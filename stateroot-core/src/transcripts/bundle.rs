//! Synthesis bundle builder (plan step 8): full-fidelity per-session
//! bundles from the harness transcript files — re-read from disk, NOT the
//! capped [`TranscriptSession`] fields.
//!
//! Windowing (compaction-aware):
//! - last `compacted` with a non-blank message → `compaction_summary` +
//!   everything AFTER it;
//! - last `compacted` with a blank/encrypted message (Codex Desktop
//!   `encrypted_content`, unreadable by design) → the last compaction's
//!   `replacement_history` rendered as cleaned text, labeled
//!   `context_window_at_last_compaction (summary encrypted by harness)`;
//! - no compaction → the whole cleaned conversation.
//!
//! Cleaning (cut only pure noise): reasoning items, token-count events,
//! base/developer instructions, injected envelopes, image blocks
//! (`[image omitted]`); tool calls/outputs kept in full; the
//! LATEST `update_plan` snapshot COMPLETE (all statuses); `create_goal`
//! arguments COMPLETE. Budget guard only elides when an explicit finite
//! max is passed (compiler uses `usize::MAX` = uncapped).

use std::path::Path;

use serde_json::{json, Value};

use super::codex::is_injected;
use super::{clean, cwd_matches, event_timestamp, walk_files, TranscriptReader, TranscriptSession};
use crate::harness_install::paths;

/// Default soft bundle cap (~900k tokens). Compiler paths pass `usize::MAX`.
pub const DEFAULT_MAX_BUNDLE_CHARS: usize = 3_500_000;
/// Chars of message text that are never elided (measured from the end).
const TAIL_PROTECT_CHARS: usize = 200_000;

/// Build synthesis bundles for the project from the harness transcript
/// stores. `session_ids`: restrict to those sessions when given.
pub fn build_bundles(
    home: &Path,
    project_dir: &Path,
    session_ids: Option<&[String]>,
    max_bundle_chars: usize,
) -> Vec<Value> {
    let mut sessions: Vec<Value> = Vec::new();

    // Codex (both stores, active-first dedupe — same as the reader).
    let rollout = |p: &Path| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
            .unwrap_or(false)
    };
    let (codex_sessions, codex_archived) = paths::codex_transcript_roots(home);
    let mut files = walk_files(&codex_sessions, &rollout);
    files.extend(walk_files(&codex_archived, &rollout));
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for file in &files {
        let Some(bundle) = bundle_rollout(file, project_dir) else {
            continue;
        };
        let id = bundle["session_id"].as_str().unwrap_or("").to_string();
        if seen.insert(id) {
            sessions.push(bundle);
        }
    }

    // Claude (whole cleaned conversation; no compaction concept).
    let claude_files = walk_files(&home.join(".claude/projects"), &|p| {
        p.extension().and_then(|e| e.to_str()) == Some("jsonl")
    });
    for file in &claude_files {
        if let Some(bundle) = bundle_claude_session(file, project_dir) {
            sessions.push(bundle);
        }
    }

    // Kimi Code wire journals (verified format; no compactions → full).
    let kimi_index = super::kimi::read_session_index(home);
    let kimi_files = walk_files(&home.join(".kimi-code/sessions"), &|p| {
        p.file_name().and_then(|n| n.to_str()) == Some("wire.jsonl")
    });
    for file in &kimi_files {
        if let Some(bundle) = bundle_kimi_wire(file, project_dir, &kimi_index) {
            sessions.push(bundle);
        }
    }

    // Cursor state.vscdb (verified format; no compactions → full).
    for db_path in super::cursor::db_candidates(home) {
        if let Ok(db) = super::cursor::open_immutable(&db_path) {
            sessions.extend(bundle_cursor_sessions(&db, project_dir));
        }
    }

    // OpenClaw + Hermes: reuse verified readers (full window from conversation tail).
    for session in super::openclaw::OpenClawReader.scan(home, project_dir) {
        sessions.push(bundle_from_transcript_session(&session));
    }
    for session in super::hermes::HermesReader.scan(home, project_dir) {
        sessions.push(bundle_from_transcript_session(&session));
    }

    if let Some(ids) = session_ids {
        sessions.retain(|s| {
            ids.iter()
                .any(|id| s["session_id"].as_str() == Some(id.as_str()))
        });
    }
    sessions.sort_by(|a, b| {
        a["started_at"]
            .as_str()
            .unwrap_or("")
            .cmp(b["started_at"].as_str().unwrap_or(""))
    });
    budget_guard(&mut sessions, max_bundle_chars);
    sessions
}

// ---------------------------------------------------------------------
// codex rollouts
// ---------------------------------------------------------------------

fn bundle_rollout(file: &Path, project_dir: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(file).ok()?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());

    let meta: Value = serde_json::from_str(lines.next()?).ok()?;
    if meta.get("type").and_then(|v| v.as_str()) != Some("session_meta") {
        return None;
    }
    let payload = meta.get("payload").cloned().unwrap_or(Value::Null);
    let cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if !cwd_matches(&cwd, project_dir) {
        return None;
    }
    let session_id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let started_at = payload
        .get("timestamp")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Pass 1: index events, find the last compaction, collect plan/goals/meta.
    let events: Vec<Value> = lines
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();
    let mut compactions: Vec<usize> = Vec::new();
    let mut plan_state: Vec<Value> = Vec::new();
    let mut goals: Vec<String> = Vec::new();
    let mut tool_events = 0usize;
    let mut ended_at = String::new();
    for (index, event) in events.iter().enumerate() {
        if let Some(ts) = event_timestamp(event) {
            ended_at = ts;
        }
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let payload = event.get("payload").cloned().unwrap_or(Value::Null);
        let payload_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match (event_type, payload_type) {
            ("compacted", _) => compactions.push(index),
            ("response_item", "function_call") | ("response_item", "custom_tool_call") => {
                tool_events += 1;
                collect_plan_goals(
                    &payload,
                    payload_type == "custom_tool_call",
                    &mut plan_state,
                    &mut goals,
                );
            }
            _ => {}
        }
    }

    // Windowing (A1):
    // - no compactions → the whole cleaned stream;
    // - last compaction has a READABLE summary → summary + everything after;
    // - last compaction is harness-ENCRYPTED (blank message) → the WHOLE
    //   cleaned stream from the append-only file (it never deletes), with
    //   honest marker lines at each compaction point. The old
    //   `replacement_history` substitution is gone (A1).
    let last_compacted = compactions.last().copied();
    let last_message = last_compacted
        .and_then(|index| events[index].get("payload").cloned())
        .and_then(|payload| {
            payload
                .get("message")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();
    let (windowing, compaction_summary, message_start): (&str, Option<String>, usize) =
        match last_compacted {
            None => ("full", None, 0),
            Some(index) if !last_message.trim().is_empty() => (
                "compaction_summary",
                Some(clean(&last_message, 60_000)),
                index + 1,
            ),
            Some(_) => ("full_stream_encrypted_compactions", None, 0),
        };

    // Pass 2: clean the window's events into messages, inserting numbered
    // compaction markers in the full-stream mode.
    let mut messages: Vec<Value> = Vec::new();
    let mut compaction_no = 0usize;
    for (index, event) in events.iter().enumerate().skip(message_start) {
        if compactions.contains(&index) {
            compaction_no += 1;
            messages.push(json!({
                "role": "context",
                "text": format!("--- compaction #{compaction_no} (encrypted by harness) ---"),
            }));
            continue;
        }
        clean_event_message(event, &mut messages);
    }

    let mut bundle = json!({
        "session_id": session_id,
        "harness": "codex",
        "started_at": started_at,
        "ended_at": ended_at,
        "cwd": cwd,
        "outcome": "unknown",
        "tool_events": tool_events,
        "windowing": windowing,
        "plan_state": plan_state,
        "goals": goals,
        "messages": messages,
    });
    // Outcome: reuse the reader's classification (cheap recompute on the tail).
    bundle["outcome"] = json!(codex_outcome(file));
    if let Some(summary) = compaction_summary {
        bundle["compaction_summary"] = json!(summary);
    }
    Some(bundle)
}

/// Collect the LATEST update_plan snapshot and create_goal arguments.
fn collect_plan_goals(
    payload: &Value,
    custom: bool,
    plan_state: &mut Vec<Value>,
    goals: &mut Vec<String>,
) {
    let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args: Value = if custom {
        serde_json::from_str(payload.get("input").and_then(|v| v.as_str()).unwrap_or(""))
            .unwrap_or(Value::Null)
    } else {
        serde_json::from_str(
            payload
                .get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        )
        .unwrap_or(Value::Null)
    };
    match name {
        "update_plan" => {
            let mut items = Vec::new();
            if let Some(plan) = args.get("plan").and_then(|v| v.as_array()) {
                for item in plan {
                    let step = item.get("step").and_then(|v| v.as_str()).unwrap_or("");
                    let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("");
                    items.push(json!({
                        "step": clean(step, 1000),
                        "status": status,
                    }));
                }
            }
            *plan_state = items; // latest snapshot replaces entirely
        }
        "create_goal" => {
            // COMPLETE arguments (never truncated).
            let raw = if custom {
                payload.get("input").and_then(|v| v.as_str()).unwrap_or("")
            } else {
                payload
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            };
            let text = raw.to_string();
            if !text.is_empty() {
                goals.push(text);
            }
        }
        _ => {}
    }
}

/// Clean one event into the message stream (codex).
fn clean_event_message(event: &Value, messages: &mut Vec<Value>) {
    let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let payload = event.get("payload").cloned().unwrap_or(Value::Null);
    let payload_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match (event_type, payload_type) {
        ("response_item", "message") => {
            let role = payload.get("role").and_then(|v| v.as_str()).unwrap_or("");
            if role != "user" && role != "assistant" {
                return; // developer/system instructions dropped
            }
            let mut parts: Vec<String> = Vec::new();
            if let Some(content) = payload.get("content").and_then(|v| v.as_array()) {
                for block in content {
                    let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if matches!(
                        block_type,
                        "input_image" | "image" | "image_url" | "input_image_url"
                    ) {
                        parts.push("[image omitted]".to_string());
                    } else if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                        parts.push(text.to_string());
                    }
                }
            }
            let text = parts.join("\n");
            if role == "user" && is_injected(&text) {
                return;
            }
            let text = text.trim().to_string();
            if !text.is_empty() {
                messages.push(json!({"role": role, "text": text}));
            }
        }
        ("response_item", "reasoning") | ("event_msg", "token_count") => {
            // reasoning blobs + token counters are pure noise.
        }
        ("response_item", "function_call") | ("response_item", "custom_tool_call") => {
            let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if matches!(name, "update_plan" | "create_goal") {
                return; // first-class fields, not message noise
            }
            let raw = if payload_type == "custom_tool_call" {
                payload.get("input").and_then(|v| v.as_str()).unwrap_or("")
            } else {
                payload
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            };
            messages.push(json!({
                "role": "tool",
                "text": format!("[call] {name}: {}", clean(raw, usize::MAX)),
            }));
        }
        ("response_item", "function_call_output")
        | ("response_item", "custom_tool_call_output") => {
            let output = match payload.get("output") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            let text = clean(&output, usize::MAX);
            if !text.is_empty() {
                messages.push(json!({"role": "tool", "text": format!("[output] {text}")}));
            }
        }
        ("compacted", _) | ("compaction", _) => {
            // windowing handled above; encrypted blobs never render.
        }
        _ => {}
    }
}

/// Cheap outcome classification for a rollout (reader semantics on the tail).
fn codex_outcome(file: &Path) -> &'static str {
    let Ok(text) = std::fs::read_to_string(file) else {
        return "unknown";
    };
    let mut saw_assistant = false;
    let mut dangling_call = false;
    let mut last_assistant = false;
    let mut completed = false;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let payload = event.get("payload").cloned().unwrap_or(Value::Null);
        let payload_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match (event_type, payload_type) {
            ("response_item", "message") => {
                let role = payload.get("role").and_then(|v| v.as_str()).unwrap_or("");
                saw_assistant |= role == "assistant";
                last_assistant = role == "assistant";
                dangling_call = false;
            }
            ("response_item", "function_call") | ("response_item", "custom_tool_call") => {
                dangling_call = true;
                last_assistant = false;
            }
            ("response_item", "function_call_output")
            | ("response_item", "custom_tool_call_output") => {
                dangling_call = false;
                last_assistant = false;
            }
            ("event_msg", "task_complete") => completed = true,
            _ => {}
        }
    }
    if completed || last_assistant {
        "completed"
    } else if dangling_call || !saw_assistant {
        "interrupted"
    } else {
        "unknown"
    }
}

// ---------------------------------------------------------------------
// claude sessions (whole cleaned conversation)
// ---------------------------------------------------------------------

fn bundle_claude_session(file: &Path, project_dir: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(file).ok()?;
    let mut cwd = String::new();
    let mut session_id = String::new();
    let mut started_at = String::new();
    let mut ended_at = String::new();
    let mut messages: Vec<Value> = Vec::new();
    let mut tool_events = 0usize;
    let mut saw_any = false;

    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(ts) = event_timestamp(&event) {
            if started_at.is_empty() {
                started_at = ts.clone();
            }
            ended_at = ts;
        }
        if cwd.is_empty() {
            if let Some(value) = event.get("cwd").and_then(|v| v.as_str()) {
                cwd = value.to_string();
            }
        }
        if session_id.is_empty() {
            if let Some(value) = event.get("sessionId").and_then(|v| v.as_str()) {
                session_id = value.to_string();
            }
        }
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match event_type {
            "user" => {
                saw_any = true;
                if event.get("isMeta").and_then(|v| v.as_bool()) == Some(true) {
                    continue;
                }
                let Some(message) = event.get("message") else {
                    continue;
                };
                if message.get("role").and_then(|v| v.as_str()) != Some("user") {
                    continue;
                }
                match message.get("content") {
                    Some(Value::String(text)) => {
                        if !is_injected(text) {
                            let text = text.trim().to_string();
                            if !text.is_empty() {
                                messages.push(json!({"role": "user", "text": text}));
                            }
                        }
                    }
                    Some(Value::Array(blocks)) => {
                        for block in blocks {
                            let block_type =
                                block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            if block_type == "tool_result" {
                                let content =
                                    block.get("content").and_then(|v| v.as_str()).unwrap_or("");
                                let text = clean(content, usize::MAX);
                                if !text.is_empty() {
                                    messages.push(
                                        json!({"role": "tool", "text": format!("[output] {text}")}),
                                    );
                                }
                            } else if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                if !is_injected(text) {
                                    let text = text.trim().to_string();
                                    if !text.is_empty() {
                                        messages.push(json!({"role": "user", "text": text}));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            "assistant" => {
                saw_any = true;
                let Some(message) = event.get("message") else {
                    continue;
                };
                if let Some(blocks) = message.get("content").and_then(|v| v.as_array()) {
                    for block in blocks {
                        match block.get("type").and_then(|v| v.as_str()) {
                            Some("text") => {
                                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                    let text = text.trim().to_string();
                                    if !text.is_empty() {
                                        messages.push(json!({"role": "assistant", "text": text}));
                                    }
                                }
                            }
                            Some("tool_use") => {
                                tool_events += 1;
                                let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                let input = block
                                    .get("input")
                                    .map(|v| v.to_string())
                                    .unwrap_or_default();
                                messages.push(json!({
                                    "role": "tool",
                                    "text": format!("[call] {name}: {}", clean(&input, usize::MAX)),
                                }));
                            }
                            Some("thinking") => {} // reasoning dropped
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if !saw_any || !cwd_matches(&cwd, project_dir) {
        return None;
    }
    Some(json!({
        "session_id": session_id,
        "harness": "claude",
        "started_at": started_at,
        "ended_at": ended_at,
        "cwd": cwd,
        "outcome": "unknown",
        "tool_events": tool_events,
        "windowing": "full",
        "plan_state": [],
        "goals": [],
        "messages": messages,
    }))
}

// ---------------------------------------------------------------------
// kimi wire journals + cursor state.vscdb (both verified; no compactions)
// ---------------------------------------------------------------------

/// Kimi Code wire journal → bundle (whole cleaned conversation, `full`).
fn bundle_kimi_wire(
    file: &Path,
    project_dir: &Path,
    index: &std::collections::HashMap<String, String>,
) -> Option<Value> {
    let text = std::fs::read_to_string(file).ok()?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let meta: Value = serde_json::from_str(lines.next()?).ok()?;
    if meta.get("type").and_then(|v| v.as_str()) != Some("metadata") {
        return None;
    }
    let session_id = file
        .parent()
        .and_then(|main| main.parent())
        .and_then(|agents| agents.parent())
        .and_then(|dir| dir.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let cwd = index.get(&session_id).cloned()?;
    if !cwd_matches(&cwd, project_dir) {
        return None;
    }

    let mut messages: Vec<Value> = Vec::new();
    let mut tool_events = 0usize;
    let mut started_at =
        super::kimi::rfc3339_millis(meta.get("created_at").and_then(|v| v.as_i64()).unwrap_or(0));
    let mut ended_at = String::new();
    for line in lines {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(ms) = event.get("time").and_then(|v| v.as_i64()) {
            ended_at = super::kimi::rfc3339_millis(ms);
        }
        if event.get("type").and_then(|v| v.as_str()) != Some("context.append_message") {
            continue; // config/llm records carry harness-private data
        }
        let message = event.get("message").cloned().unwrap_or(Value::Null);
        if message.get("partial").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }
        let role = message.get("role").and_then(|v| v.as_str()).unwrap_or("");
        match role {
            "user" => {
                let injected = message
                    .get("origin")
                    .and_then(|o| o.get("kind"))
                    .and_then(|v| v.as_str())
                    .is_some_and(|kind| kind != "user");
                let mut parts = Vec::new();
                if let Some(content) = message.get("content").and_then(|v| v.as_array()) {
                    for block in content {
                        if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                parts.push(text);
                            }
                        }
                    }
                }
                let text = parts.join("\n");
                if !injected && !is_injected(&text) {
                    let text = text.trim().to_string();
                    if !text.is_empty() {
                        messages.push(json!({"role": "user", "text": text}));
                    }
                }
            }
            "assistant" => {
                let mut parts = Vec::new();
                if let Some(content) = message.get("content").and_then(|v| v.as_array()) {
                    for block in content {
                        if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                parts.push(text);
                            }
                        }
                    }
                }
                let text = parts.join("\n").trim().to_string();
                if !text.is_empty() {
                    messages.push(json!({"role": "assistant", "text": text}));
                }
                if let Some(calls) = message.get("toolCalls").and_then(|v| v.as_array()) {
                    for call in calls {
                        tool_events += 1;
                        let function = call.get("function").unwrap_or(call);
                        let name = function.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let arguments = function
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        messages.push(json!({
                            "role": "tool",
                            "text": format!("[call] {name}: {}", clean(arguments, usize::MAX)),
                        }));
                    }
                }
            }
            "tool" => {
                let mut parts = Vec::new();
                if let Some(content) = message.get("content").and_then(|v| v.as_array()) {
                    for block in content {
                        if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                            parts.push(text);
                        }
                    }
                }
                let text = clean(&parts.join("\n"), usize::MAX);
                if !text.is_empty() {
                    messages.push(json!({"role": "tool", "text": format!("[output] {text}")}));
                }
            }
            _ => {}
        }
    }
    if started_at.is_empty() {
        started_at = ended_at.clone();
    }
    Some(json!({
        "session_id": session_id,
        "harness": "kimi",
        "started_at": started_at,
        "ended_at": ended_at,
        "cwd": cwd,
        "outcome": "unknown",
        "tool_events": tool_events,
        "windowing": "full",
        "plan_state": [],
        "goals": [],
        "messages": messages,
    }))
}

/// All project-matched Cursor composer sessions in one state db → bundles.
fn bundle_cursor_sessions(db: &rusqlite::Connection, project_dir: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(mut stmt) = db.prepare("SELECT composerId, value FROM composerHeaders") else {
        return out;
    };
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map(|mapped| mapped.flatten().collect::<Vec<_>>())
        .unwrap_or_default();
    for (composer_id, head_json) in rows {
        let Ok(head) = serde_json::from_str::<Value>(&head_json) else {
            continue;
        };
        let cwd = head
            .pointer("/workspaceIdentifier/uri/fsPath")
            .or_else(|| head.pointer("/workspaceIdentifier/uri/path"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if cwd.is_empty() || !cwd_matches(&cwd, project_dir) {
            continue;
        }
        let Ok(mut bubbles_stmt) = db.prepare("SELECT value FROM cursorDiskKV WHERE key GLOB ?1")
        else {
            continue;
        };
        let bubbles: Vec<Value> = bubbles_stmt
            .query_map([format!("bubbleId:{composer_id}:*")], |row| {
                row.get::<_, String>(0)
            })
            .map(|mapped| {
                mapped
                    .flatten()
                    .filter_map(|text| serde_json::from_str::<Value>(&text).ok())
                    .collect()
            })
            .unwrap_or_default();
        let mut bubbles: Vec<(String, Value)> = bubbles
            .into_iter()
            .map(|bubble| {
                let created = bubble
                    .get("createdAt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                (created, bubble)
            })
            .collect();
        bubbles.sort_by(|a, b| a.0.cmp(&b.0));

        let mut messages: Vec<Value> = Vec::new();
        let mut tool_events = 0usize;
        for (_, bubble) in &bubbles {
            let bubble_type = bubble.get("type").and_then(|v| v.as_i64()).unwrap_or(0);
            let text = bubble.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let role = match bubble_type {
                1 => "user",
                2 => "assistant",
                _ => continue,
            };
            if !text.trim().is_empty() && (role != "user" || !is_injected(text)) {
                messages.push(json!({
                    "role": role,
                    "text": text.trim().to_string(),
                }));
            }
            if bubble_type == 2 {
                tool_events += bubble
                    .get("toolResults")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
            }
        }
        if bubbles.is_empty() {
            continue;
        }
        out.push(json!({
            "session_id": composer_id,
            "harness": "cursor",
            "started_at": bubbles.first().map(|(ts, _)| ts.clone()).unwrap_or_default(),
            "ended_at": bubbles.last().map(|(ts, _)| ts.clone()).unwrap_or_default(),
            "cwd": cwd,
            "outcome": "unknown",
            "tool_events": tool_events,
            "windowing": "full",
            "plan_state": [],
            "goals": [],
            "messages": messages,
        }));
    }
    out
}

fn bundle_from_transcript_session(session: &TranscriptSession) -> Value {
    let messages: Vec<Value> = session
        .conversation_tail
        .iter()
        .map(|entry| {
            json!({
                "role": entry.role,
                "text": entry.text,
            })
        })
        .collect();
    json!({
        "session_id": session.session_id,
        "harness": session.harness,
        "started_at": session.started_at,
        "ended_at": session.ended_at,
        "cwd": session.cwd,
        "outcome": session.outcome.as_str(),
        "tool_events": session.tool_events,
        "windowing": "conversation_tail",
        "plan_state": [],
        "goals": [],
        "messages": messages,
        "objective": session.objective,
    })
}

// ---------------------------------------------------------------------
// budget guard
// ---------------------------------------------------------------------

/// Soft cap: when the assembled bundle exceeds `max_chars`, elide the
/// MIDDLE of the message stream with a marker — never touching plan_state,
/// goals, compaction summaries, or the final ~200k chars of message text.
fn budget_guard(sessions: &mut [Value], max_chars: usize) {
    if max_chars == usize::MAX {
        return;
    }
    let total = serde_json::to_string(&sessions)
        .map(|s| s.chars().count())
        .unwrap_or(0);
    if total <= max_chars {
        return;
    }
    // Mark which messages are tail-protected (walk from the end).
    let mut protected: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut tail_chars = 0usize;
    'outer: for (si, session) in sessions.iter().enumerate().rev() {
        if let Some(messages) = session["messages"].as_array() {
            for (mi, message) in messages.iter().enumerate().rev() {
                let chars = message["text"].as_str().unwrap_or("").chars().count();
                if tail_chars + chars > TAIL_PROTECT_CHARS {
                    break 'outer;
                }
                tail_chars += chars;
                protected.insert((si, mi));
            }
        }
    }

    let mut excess = total - max_chars;
    let mut elided = 0usize;
    for (si, session) in sessions.iter_mut().enumerate() {
        if excess == 0 {
            break;
        }
        let Some(messages) = session["messages"].as_array_mut() else {
            continue;
        };
        let mut kept: Vec<Value> = Vec::with_capacity(messages.len());
        let mut dropped_here = 0usize;
        for (mi, message) in std::mem::take(messages).into_iter().enumerate() {
            let chars = message["text"].as_str().unwrap_or("").chars().count() + 40;
            if excess > 0 && !protected.contains(&(si, mi)) {
                excess = excess.saturating_sub(chars);
                elided += chars;
                dropped_here += 1;
            } else {
                kept.push(message);
            }
        }
        if dropped_here > 0 {
            kept.insert(
                0,
                json!({"role": "context", "text": format!("… [elided {elided} chars] …")}),
            );
        }
        *messages = kept;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_rollout(dir: &Path, name: &str, lines: &[&str]) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let mut file = std::fs::File::create(&path).expect("create");
        for line in lines {
            writeln!(file, "{line}").expect("write");
        }
        path
    }

    fn meta(cwd: &str, id: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-07-01T09:59:00Z","type":"session_meta","payload":{{"id":"{id}","timestamp":"2026-07-01T10:00:00Z","cwd":"{cwd}","originator":"codex_cli"}}}}"#
        )
    }

    #[test]
    fn bundle_no_compaction_is_full_cleaned_conversation() {
        let project = tempfile::tempdir().expect("project");
        let home = tempfile::tempdir().expect("home");
        write_rollout(
            &home.path().join(".codex/sessions/2026/07/01"),
            "rollout-2026-07-01T10-00-00-s-b1.jsonl",
            &[
                &meta(&crate::transcripts::path_for_json(project.path()), "s-b1"),
                r#"{"timestamp":"2026-07-01T10:00:01Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>drop me</permissions instructions>"}]}}"#,
                r#"{"timestamp":"2026-07-01T10:00:02Z","type":"response_item","payload":{"type":"reasoning","summary":[]}}"#,
                r#"{"timestamp":"2026-07-01T10:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{}}}"#,
                r#"{"timestamp":"2026-07-01T10:00:04Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"real question"},{"type":"input_image","image_url":"blob"}]}}"#,
                r#"{"timestamp":"2026-07-01T10:00:05Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"real answer"}]}}"#,
                r#"{"timestamp":"2026-07-01T10:00:06Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"long command here\"}","call_id":"c1"}}"#,
                r#"{"timestamp":"2026-07-01T10:00:07Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"some output"}}"#,
            ],
        );
        let bundles = build_bundles(home.path(), project.path(), None, DEFAULT_MAX_BUNDLE_CHARS);
        assert_eq!(bundles.len(), 1);
        let bundle = &bundles[0];
        assert_eq!(bundle["windowing"], "full");
        let messages = bundle["messages"].as_array().expect("messages");
        let texts: Vec<&str> = messages
            .iter()
            .map(|m| m["text"].as_str().unwrap_or(""))
            .collect();
        // developer/reasoning/token_count dropped; image marked; tool capped.
        assert!(!texts.iter().any(|t| t.contains("drop me")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("real question")));
        assert!(texts.iter().any(|t| t.contains("[image omitted]")));
        assert!(texts.iter().any(|t| t.contains("real answer")));
        assert!(texts.iter().any(|t| t.starts_with("[call] exec_command:")));
        assert!(texts.iter().any(|t| t.starts_with("[output] some output")));
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn bundle_plaintext_compaction_summary_plus_after_only() {
        let project = tempfile::tempdir().expect("project");
        let home = tempfile::tempdir().expect("home");
        write_rollout(
            &home.path().join(".codex/sessions/2026/07/01"),
            "rollout-2026-07-01T10-00-00-s-b2.jsonl",
            &[
                &meta(&crate::transcripts::path_for_json(project.path()), "s-b2"),
                r#"{"timestamp":"2026-07-01T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"BEFORE compaction"}]}}"#,
                r#"{"timestamp":"2026-07-01T10:00:02Z","type":"compacted","payload":{"message":"progress so far: schema done"}}"#,
                r#"{"timestamp":"2026-07-01T10:00:03Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"AFTER compaction"}]}}"#,
            ],
        );
        let bundles = build_bundles(home.path(), project.path(), None, DEFAULT_MAX_BUNDLE_CHARS);
        let bundle = &bundles[0];
        assert_eq!(bundle["windowing"], "compaction_summary");
        assert_eq!(bundle["compaction_summary"], "progress so far: schema done");
        let texts: Vec<&str> = bundle["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .map(|m| m["text"].as_str().unwrap_or(""))
            .collect();
        assert!(!texts.iter().any(|t| t.contains("BEFORE compaction")));
        assert!(texts.iter().any(|t| t.contains("AFTER compaction")));
    }

    #[test]
    fn bundle_encrypted_compaction_streams_whole_file_with_markers() {
        let project = tempfile::tempdir().expect("project");
        let home = tempfile::tempdir().expect("home");
        write_rollout(
            &home.path().join(".codex/sessions/2026/07/01"),
            "rollout-2026-07-01T10-00-00-s-b3.jsonl",
            &[
                &meta(&crate::transcripts::path_for_json(project.path()), "s-b3"),
                r#"{"timestamp":"2026-07-01T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"ERA ONE work"}]}}"#,
                r#"{"timestamp":"2026-07-01T10:00:02Z","type":"compacted","payload":{"message":""}}"#,
                r#"{"timestamp":"2026-07-01T10:00:03Z","type":"compaction","payload":{"encrypted_content":"gAAAAABmF6b2NlZDE"}}"#,
                r#"{"timestamp":"2026-07-01T10:00:04Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ERA TWO work"}]}}"#,
                r#"{"timestamp":"2026-07-01T10:00:05Z","type":"compacted","payload":{"message":"   "}}"#,
                r#"{"timestamp":"2026-07-01T10:00:06Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"ERA THREE work"}]}}"#,
            ],
        );
        let bundles = build_bundles(home.path(), project.path(), None, DEFAULT_MAX_BUNDLE_CHARS);
        let bundle = &bundles[0];
        // A1: whole append-only stream, numbered markers at each compaction.
        assert_eq!(bundle["windowing"], "full_stream_encrypted_compactions");
        assert!(bundle.get("compaction_summary").is_none());
        let texts: Vec<&str> = bundle["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .map(|m| m["text"].as_str().unwrap_or(""))
            .collect();
        assert!(
            texts.iter().any(|t| t.contains("ERA ONE work")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("ERA TWO work")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("ERA THREE work")),
            "{texts:?}"
        );
        let markers: Vec<&&str> = texts
            .iter()
            .filter(|t| t.contains("(encrypted by harness)"))
            .collect();
        assert_eq!(markers.len(), 2, "{texts:?}");
        assert!(texts
            .iter()
            .any(|t| t.contains("--- compaction #1 (encrypted by harness) ---")));
        assert!(texts
            .iter()
            .any(|t| t.contains("--- compaction #2 (encrypted by harness) ---")));
        // Marker positions: #1 sits between era one and era two.
        let era1 = texts
            .iter()
            .position(|t| t.contains("ERA ONE"))
            .expect("era1");
        let m1 = texts
            .iter()
            .position(|t| t.contains("compaction #1"))
            .expect("m1");
        let era2 = texts
            .iter()
            .position(|t| t.contains("ERA TWO"))
            .expect("era2");
        assert!(era1 < m1 && m1 < era2, "{texts:?}");
        // The encrypted blob itself never renders.
        assert!(!texts.iter().any(|t| t.contains("gAAAAAB")), "{texts:?}");
    }

    #[test]
    fn bundle_plan_and_goals_complete_budget_elides_middle() {
        let project = tempfile::tempdir().expect("project");
        let home = tempfile::tempdir().expect("home");
        let long = "y".repeat(2000);
        let mut lines = vec![
            meta(&crate::transcripts::path_for_json(project.path()), "s-b4"),
            r#"{"timestamp":"2026-07-01T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"start"}]}}"#.to_string(),
            r#"{"timestamp":"2026-07-01T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"create_goal","arguments":"{\"objective\":\"the complete goal text, unabridged\"}","call_id":"g1"}}"#.to_string(),
            r#"{"timestamp":"2026-07-01T10:00:03Z","type":"response_item","payload":{"type":"function_call","name":"update_plan","arguments":"{\"plan\":[{\"step\":\"done step\",\"status\":\"completed\"},{\"step\":\"next step\",\"status\":\"pending\"}]}","call_id":"p1"}}"#.to_string(),
        ];
        // Many filler messages to exceed the tail-protection span (elision
        // can only fire beyond it, by design).
        for i in 1..=150 {
            lines.push(format!(
                r#"{{"timestamp":"2026-07-01T10:{i:02}:00Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"filler {i} {long}"}}]}}}}"#
            ));
        }
        lines.push(r#"{"timestamp":"2026-07-01T11:00:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"THE PROTECTED TAIL MESSAGE"}]}}"#.to_string());
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        write_rollout(
            &home.path().join(".codex/sessions/2026/07/01"),
            "rollout-2026-07-01T10-00-00-s-b4.jsonl",
            &refs,
        );
        let bundles = build_bundles(home.path(), project.path(), None, 250_000);
        let bundle = &bundles[0];
        // Plan + goals COMPLETE despite elision.
        let plan = bundle["plan_state"].as_array().expect("plan");
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0]["status"], "completed");
        assert_eq!(plan[1]["status"], "pending");
        assert_eq!(
            bundle["goals"][0],
            "{\"objective\":\"the complete goal text, unabridged\"}"
        );
        // Elision marker present; tail message protected.
        let texts: Vec<&str> = bundle["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .map(|m| m["text"].as_str().unwrap_or(""))
            .collect();
        assert!(texts.iter().any(|t| t.contains("[elided")), "{texts:?}");
        assert!(texts
            .iter()
            .any(|t| t.contains("THE PROTECTED TAIL MESSAGE")));
    }
}

#[cfg(test)]
mod new_harness_bundle_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn kimi_and_cursor_sessions_bundle_as_full() {
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let home = tempfile::tempdir().expect("home");

        // Kimi fixture (verified shape).
        std::fs::create_dir_all(home.path().join(".kimi-code")).expect("mkdir");
        std::fs::write(
            home.path().join(".kimi-code/session_index.jsonl"),
            format!("{{\"sessionId\":\"ses-k1\",\"sessionDir\":\"/x\",\"workDir\":\"{cwd}\"}}\n"),
        )
        .expect("index");
        let wire_dir = home
            .path()
            .join(".kimi-code/sessions/wd_demo/ses-k1/agents/main");
        std::fs::create_dir_all(&wire_dir).expect("mkdir");
        let mut wire = std::fs::File::create(wire_dir.join("wire.jsonl")).expect("wire");
        writeln!(
            wire,
            r#"{{"type":"metadata","protocol_version":"1.0","created_at":1784310494250}}"#
        )
        .expect("meta");
        writeln!(wire, r#"{{"type":"context.append_message","message":{{"role":"user","content":[{{"type":"text","text":"kimi build step"}}],"toolCalls":[]}},"time":1784310495000}}"#).expect("u");
        writeln!(wire, r#"{{"type":"context.append_message","message":{{"role":"assistant","content":[{{"type":"text","text":"kimi answer"}}],"toolCalls":[{{"type":"function","id":"t1","function":{{"name":"Shell","arguments":"{{\"command\": \"ls\"}}"}}}}]}},"time":1784310496000}}"#).expect("a");

        // Cursor fixture (verified shape).
        let db_dir = home.path().join(".config/Cursor/User/globalStorage");
        std::fs::create_dir_all(&db_dir).expect("mkdir");
        let db = rusqlite::Connection::open(db_dir.join("state.vscdb")).expect("db");
        db.execute_batch(
            "CREATE TABLE composerHeaders (composerId TEXT, workspaceId TEXT, createdAt INTEGER, lastUpdatedAt INTEGER, isArchived INTEGER, isSubagent INTEGER, recency INTEGER, checkpointAt INTEGER, value TEXT);
             CREATE TABLE cursorDiskKV (key TEXT, value TEXT);",
        )
        .expect("schema");
        let head = serde_json::json!({
            "type": "head", "composerId": "cmp-1", "createdAt": 1781508855958i64,
            "workspaceIdentifier": {"id": "w1", "uri": {"fsPath": cwd}}
        });
        db.execute(
            "INSERT INTO composerHeaders (composerId, value) VALUES ('cmp-1', ?1)",
            [serde_json::to_string(&head).expect("head")],
        )
        .expect("insert");
        for (id, ty, text) in [("b1", 1, "cursor build step"), ("b2", 2, "cursor answer")] {
            let bubble = serde_json::json!({
                "_v": 3, "type": ty, "text": text, "createdAt": "2026-07-01T10:00:01Z", "toolResults": []
            });
            db.execute(
                "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
                [
                    format!("bubbleId:cmp-1:{id}"),
                    serde_json::to_string(&bubble).expect("b"),
                ],
            )
            .expect("insert");
        }
        drop(db);

        let bundles = build_bundles(home.path(), project.path(), None, DEFAULT_MAX_BUNDLE_CHARS);
        assert_eq!(bundles.len(), 2, "bundles: {bundles:?}");
        let kimi = bundles
            .iter()
            .find(|b| b["harness"] == "kimi")
            .expect("kimi");
        assert_eq!(kimi["windowing"], "full");
        assert_eq!(kimi["session_id"], "ses-k1");
        assert!(kimi["messages"]
            .as_array()
            .expect("m")
            .iter()
            .any(|m| m["text"] == "kimi build step"));
        assert!(kimi["messages"]
            .as_array()
            .expect("m")
            .iter()
            .any(|m| m["text"].as_str().expect("t").starts_with("[call] Shell:")));
        let cursor = bundles
            .iter()
            .find(|b| b["harness"] == "cursor")
            .expect("cursor");
        assert_eq!(cursor["windowing"], "full");
        assert_eq!(cursor["session_id"], "cmp-1");
        assert!(cursor["messages"]
            .as_array()
            .expect("m")
            .iter()
            .any(|m| m["text"] == "cursor answer"));
    }
}
