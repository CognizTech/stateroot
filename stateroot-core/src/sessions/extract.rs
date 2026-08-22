//! Full-fidelity canonical extractors for the file- and sqlite-based
//! harnesses (claude, codex, kimi, openclaw, cursor, hermes) — the second
//! wave beside the pi/dsh reference extractors in `sessions::mod`.
//!
//! Same rules: verbatim text, NO content caps, native ids/parents kept
//! where the format has them (seq is synthesized on write), and anything
//! unmapped becomes `type:"meta"` with `native_type` — never silently
//! dropped. `None` from an extractor means the session does not belong to
//! the project (cwd filter), exactly like the summary readers.

use std::path::Path;

use serde_json::Value;

use super::{CanonicalEntry, CanonicalSession};
use crate::transcripts::{self, codex, cursor, hermes, openclaw};

fn entry(kind: &str) -> CanonicalEntry {
    CanonicalEntry {
        kind: kind.to_string(),
        ..Default::default()
    }
}

fn meta_entry(native_type: &str, content: Option<String>) -> CanonicalEntry {
    let mut e = entry("meta");
    e.native_type = Some(native_type.to_string());
    e.content = content;
    e
}

fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

// ---------------------------------------------------------------------
// claude — ~/.claude/projects/**/*.jsonl (one event per line)
// ---------------------------------------------------------------------

/// Command-wrapper prefixes claude wraps local slash-commands in (the
/// summary reader filters them as noise; canonical keeps them as meta).
fn is_command_wrapper(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("<local-command-caveat") || trimmed.starts_with("<command-name")
}

/// Canonicalize one parsed claude session file.
pub(crate) fn canonical_from_claude(
    events: &[Value],
    source_path: &Path,
    project_dir: &Path,
) -> Option<CanonicalSession> {
    let mut cwd = String::new();
    let mut session_id = String::new();
    for event in events {
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
    }
    if session_id.is_empty() || !transcripts::cwd_matches(&cwd, project_dir) {
        return None;
    }
    let mut entries = Vec::new();
    for event in events {
        let mut base = entry("meta");
        base.id = event
            .get("uuid")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        base.parent_id = event
            .get("parentUuid")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        base.ts = event
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let kind = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "user" => {
                if event.get("isMeta").and_then(|v| v.as_bool()) == Some(true) {
                    base.native_type = Some("meta event".into());
                    entries.push(base);
                    continue;
                }
                let message = event.get("message").cloned().unwrap_or(Value::Null);
                match message.get("content") {
                    Some(Value::String(text)) => {
                        if is_command_wrapper(text) {
                            base.native_type = Some("local_command".into());
                            base.content = Some(text.clone());
                            entries.push(base);
                        } else {
                            let mut e = base;
                            e.kind = "message".into();
                            e.role = Some("user".into());
                            e.content = Some(text.clone());
                            entries.push(e);
                        }
                    }
                    Some(Value::Array(blocks)) => {
                        for block in blocks {
                            let block_type =
                                block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            match block_type {
                                "tool_result" => {
                                    let mut e = base.clone();
                                    e.kind = "tool_result".into();
                                    e.parent_id = block
                                        .get("tool_use_id")
                                        .and_then(|v| v.as_str())
                                        .map(str::to_string);
                                    e.content = Some(match block.get("content") {
                                        Some(Value::String(s)) => s.clone(),
                                        Some(Value::Array(_)) => {
                                            crate::transcripts::dsh::blocks_text(
                                                block.get("content"),
                                            )
                                        }
                                        _ => String::new(),
                                    });
                                    if block.get("is_error").and_then(|v| v.as_bool()) == Some(true)
                                    {
                                        e.native_type = Some("is_error".into());
                                    }
                                    entries.push(e);
                                }
                                "text" => {
                                    let text =
                                        block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                                    if is_command_wrapper(text) {
                                        let mut e = base.clone();
                                        e.native_type = Some("local_command".into());
                                        e.content = Some(text.to_string());
                                        entries.push(e);
                                    } else {
                                        let mut e = base.clone();
                                        e.kind = "message".into();
                                        e.role = Some("user".into());
                                        e.content = Some(text.to_string());
                                        entries.push(e);
                                    }
                                }
                                other => {
                                    let mut e = base.clone();
                                    e.native_type = Some(format!("user block `{other}`"));
                                    e.content = Some(compact(block));
                                    entries.push(e);
                                }
                            }
                        }
                    }
                    _ => {
                        base.native_type = Some("user event without content".into());
                        entries.push(base);
                    }
                }
            }
            "assistant" => {
                let message = event.get("message").cloned().unwrap_or(Value::Null);
                if let Some(blocks) = message.get("content").and_then(|v| v.as_array()) {
                    for block in blocks {
                        match block.get("type").and_then(|v| v.as_str()) {
                            Some("text") => {
                                let mut e = base.clone();
                                e.kind = "message".into();
                                e.role = Some("assistant".into());
                                e.content = block
                                    .get("text")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string);
                                entries.push(e);
                            }
                            Some("thinking") => {
                                let mut e = meta_entry(
                                    "thinking",
                                    block
                                        .get("thinking")
                                        .and_then(|v| v.as_str())
                                        .map(str::to_string),
                                );
                                e.id = base.id.clone();
                                e.parent_id = base.parent_id.clone();
                                e.ts = base.ts.clone();
                                entries.push(e);
                            }
                            Some("tool_use") => {
                                let mut e = entry("tool_call");
                                e.id = block.get("id").and_then(|v| v.as_str()).map(str::to_string);
                                e.parent_id = base.id.clone();
                                e.ts = base.ts.clone();
                                e.name = block
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string);
                                e.content = block.get("input").map(compact);
                                entries.push(e);
                            }
                            other => {
                                let mut e = base.clone();
                                e.native_type =
                                    Some(format!("assistant block `{}`", other.unwrap_or("")));
                                e.content = Some(compact(block));
                                entries.push(e);
                            }
                        }
                    }
                }
            }
            "summary" => {
                // Claude's one-line session title — context, not a compaction.
                base.native_type = Some("summary".into());
                base.content = event
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                entries.push(base);
            }
            "queue-operation" => {
                base.native_type = Some("queue-operation".into());
                base.content = event
                    .get("operation")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                entries.push(base);
            }
            "" => {}
            other => {
                base.native_type = Some(other.to_string());
                base.content = Some(compact(event));
                entries.push(base);
            }
        }
    }
    Some(CanonicalSession {
        harness: "claude".into(),
        session_id,
        cwd,
        source_path: source_path.display().to_string(),
        entries,
    })
}

// ---------------------------------------------------------------------
// codex — ~/.codex/sessions/**/rollout-*.jsonl (+ archived store)
// ---------------------------------------------------------------------

/// Canonicalize one parsed codex rollout.
pub(crate) fn canonical_from_codex(
    meta: &Value,
    events: &[Value],
    source_path: &Path,
    project_dir: &Path,
) -> Option<CanonicalSession> {
    let payload = meta.get("payload").cloned().unwrap_or(Value::Null);
    let cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if !transcripts::cwd_matches(&cwd, project_dir) {
        return None;
    }
    let session_id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let mut entries = vec![meta_entry("session_meta", Some(compact(&payload)))];
    for event in events {
        let ts = event
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let payload = event.get("payload").cloned().unwrap_or(Value::Null);
        let payload_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match (event_type, payload_type) {
            ("response_item", "message") => {
                let role = payload.get("role").and_then(|v| v.as_str()).unwrap_or("");
                let mut e = entry("message");
                e.ts = ts;
                e.role = Some(role.to_string());
                let text = codex_message_text(&payload);
                if role == "user" && codex::is_injected(&text) {
                    e.native_type = Some("injected".into());
                }
                e.content = Some(text);
                entries.push(e);
            }
            ("response_item", "reasoning") => {
                let mut e = meta_entry("reasoning", Some(reasoning_text(&payload)));
                e.ts = ts;
                entries.push(e);
            }
            ("response_item", "function_call") | ("response_item", "custom_tool_call") => {
                let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let arguments = if payload_type == "function_call" {
                    payload
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                } else {
                    payload
                        .get("input")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                };
                if name == "update_plan" {
                    let args: Value = serde_json::from_str(&arguments).unwrap_or(Value::Null);
                    let mut lines = Vec::new();
                    if let Some(items) = args.get("plan").and_then(|v| v.as_array()) {
                        for item in items {
                            let step = item.get("step").and_then(|v| v.as_str()).unwrap_or("");
                            let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("");
                            if !step.is_empty() {
                                lines.push(format!("- [{status}] {step}"));
                            }
                        }
                    }
                    let mut e = entry("plan");
                    e.ts = ts;
                    e.content = Some(lines.join("\n"));
                    entries.push(e);
                } else {
                    let mut e = entry("tool_call");
                    e.ts = ts;
                    e.id = payload
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    e.name = Some(name.to_string());
                    e.content = Some(arguments);
                    if payload_type == "custom_tool_call" {
                        e.native_type = Some("custom_tool_call".into());
                    }
                    entries.push(e);
                }
            }
            ("response_item", "function_call_output")
            | ("response_item", "custom_tool_call_output") => {
                let mut e = entry("tool_result");
                e.ts = ts;
                e.parent_id = payload
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                e.content = Some(match payload.get("output") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => compact(other),
                    None => String::new(),
                });
                if payload_type == "custom_tool_call_output" {
                    e.native_type = Some("custom_tool_call_output".into());
                }
                entries.push(e);
            }
            ("compacted", _) => {
                let message = payload
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let mut e = entry("compaction");
                e.ts = ts;
                if message.trim().is_empty() {
                    // Fernet-encrypted by the harness — the marker stays.
                    e.kind = "meta".into();
                    e.native_type = Some("compacted".into());
                    e.content = Some("encrypted by harness".into());
                } else {
                    e.content = Some(message.to_string());
                }
                entries.push(e);
            }
            ("response_item", other) => {
                let mut e =
                    meta_entry(&format!("response_item `{other}`"), Some(compact(&payload)));
                e.ts = ts;
                entries.push(e);
            }
            (other, _) => {
                let mut e = meta_entry(
                    &format!("{other} `{}`", payload_type),
                    Some(compact(&payload)),
                );
                e.ts = ts;
                entries.push(e);
            }
        }
    }
    Some(CanonicalSession {
        harness: "codex".into(),
        session_id,
        cwd,
        source_path: source_path.display().to_string(),
        entries,
    })
}

fn codex_message_text(payload: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(content) = payload.get("content").and_then(|v| v.as_array()) {
        for block in content {
            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                parts.push(text);
            }
        }
    }
    parts.join("\n")
}

fn reasoning_text(payload: &Value) -> String {
    if let Some(summary) = payload.get("summary").and_then(|v| v.as_array()) {
        let parts: Vec<&str> = summary
            .iter()
            .filter_map(|s| s.get("text").and_then(|v| v.as_str()))
            .collect();
        if !parts.is_empty() {
            return parts.join("\n");
        }
    }
    compact(payload)
}

// ---------------------------------------------------------------------
// kimi — ~/.kimi-code/sessions/**/wire.jsonl (+ session_index.jsonl cwd)
// ---------------------------------------------------------------------

/// Canonicalize one parsed kimi wire file. `cwd` comes from the session
/// index (the wire itself carries none).
pub(crate) fn canonical_from_kimi(
    meta: &Value,
    records: &[Value],
    session_id: &str,
    cwd: &str,
    source_path: &Path,
    project_dir: &Path,
) -> Option<CanonicalSession> {
    if cwd.is_empty() || !transcripts::cwd_matches(cwd, project_dir) {
        return None;
    }
    let mut entries = vec![meta_entry("metadata", Some(compact(meta)))];
    for record in records {
        let ts = record
            .get("time")
            .and_then(|v| v.as_i64())
            .map(|ms| ms.to_string());
        let record_type = record.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if record_type != "context.append_message" {
            let mut e = meta_entry(record_type, Some(compact(record)));
            e.ts = ts;
            if !record_type.is_empty() {
                entries.push(e);
            }
            continue;
        }
        let message = record.get("message").cloned().unwrap_or(Value::Null);
        // Partial records are stream state — the complete message is
        // re-appended later, so skipping is dedup, not loss (kimi format).
        if message.get("partial").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }
        let role = message.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let text = kimi_text(&message);
        match role {
            "user" => {
                let mut e = entry("message");
                e.ts = ts;
                e.role = Some("user".into());
                e.content = Some(text.clone());
                if let Some(kind) = message
                    .get("origin")
                    .and_then(|o| o.get("kind"))
                    .and_then(|v| v.as_str())
                    .filter(|kind| *kind != "user")
                {
                    e.native_type = Some(format!("origin:{kind}"));
                } else if codex::is_injected(&text) {
                    e.native_type = Some("injected".into());
                }
                entries.push(e);
            }
            "assistant" => {
                if !text.trim().is_empty() {
                    let mut e = entry("message");
                    e.ts = ts.clone();
                    e.role = Some("assistant".into());
                    e.content = Some(text);
                    entries.push(e);
                }
                if let Some(Value::Array(blocks)) = message.get("content") {
                    for block in blocks {
                        if block.get("type").and_then(|v| v.as_str()) == Some("think") {
                            let mut e = meta_entry(
                                "thinking",
                                block
                                    .get("think")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                            );
                            e.ts = ts.clone();
                            entries.push(e);
                        }
                    }
                }
                if let Some(calls) = message.get("toolCalls").and_then(|v| v.as_array()) {
                    for call in calls {
                        let function = call.get("function").unwrap_or(call);
                        let mut e = entry("tool_call");
                        e.ts = ts.clone();
                        e.id = call.get("id").and_then(|v| v.as_str()).map(str::to_string);
                        e.name = function
                            .get("name")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        e.content = function
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        entries.push(e);
                    }
                }
            }
            "tool" => {
                let mut e = entry("tool_result");
                e.ts = ts;
                e.parent_id = message
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                e.content = Some(text);
                entries.push(e);
            }
            other => {
                let mut e = meta_entry(&format!("message role `{other}`"), Some(text));
                e.ts = ts;
                entries.push(e);
            }
        }
    }
    Some(CanonicalSession {
        harness: "kimi".into(),
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
        source_path: source_path.display().to_string(),
        entries,
    })
}

fn kimi_text(message: &Value) -> String {
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
    parts.join("\n")
}

// ---------------------------------------------------------------------
// openclaw — ~/.openclaw/agents/<agent>/sessions/*.jsonl
// ---------------------------------------------------------------------

/// Canonicalize one parsed openclaw session file.
pub(crate) fn canonical_from_openclaw(
    events: &[Value],
    source_path: &Path,
    project_dir: &Path,
) -> Option<CanonicalSession> {
    let mut cwd = String::new();
    let mut session_id = String::new();
    for event in events {
        if event.get("type").and_then(|v| v.as_str()) == Some("session") {
            if let Some(value) = event.get("cwd").and_then(|v| v.as_str()) {
                cwd = value.to_string();
            }
            if let Some(value) = event.get("id").and_then(|v| v.as_str()) {
                session_id = value.to_string();
            }
        }
    }
    if session_id.is_empty() {
        session_id = source_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
    }
    if !transcripts::cwd_matches(&cwd, project_dir) {
        return None;
    }
    let mut entries = Vec::new();
    for event in events {
        let mut base = entry("meta");
        base.id = event.get("id").and_then(|v| v.as_str()).map(str::to_string);
        base.parent_id = event
            .get("parentId")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        base.ts = event
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let kind = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "session" => {} // the header line — fields live in the canonical header
            "message" | "custom_message" | "" => {
                let message = event.get("message").cloned().unwrap_or(Value::Null);
                let role = message.get("role").and_then(|v| v.as_str()).unwrap_or("");
                let text = openclaw::extract_text_pub(message.get("content"));
                match role {
                    "user" | "assistant" => {
                        let mut e = base;
                        e.kind = "message".into();
                        e.role = Some(role.to_string());
                        e.content = Some(text);
                        if kind == "custom_message" {
                            e.native_type = Some("custom_message".into());
                        }
                        entries.push(e);
                    }
                    "toolResult" | "tool" => {
                        let mut e = base;
                        e.kind = "tool_result".into();
                        e.parent_id = message
                            .get("toolCallId")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        e.content = Some(text);
                        entries.push(e);
                    }
                    _ => {
                        base.native_type = Some(format!("message role `{role}`"));
                        base.content = Some(text);
                        entries.push(base);
                    }
                }
            }
            "compaction" | "branch_summary" => {
                let summary = event
                    .get("summary")
                    .or_else(|| event.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if summary.is_empty() {
                    base.native_type = Some(kind.to_string());
                    base.content = Some("empty or encrypted by harness".into());
                } else {
                    base.kind = "compaction".into();
                    base.content = Some(summary.to_string());
                    if kind == "branch_summary" {
                        base.native_type = Some("branch_summary".into());
                    }
                }
                entries.push(base);
            }
            other => {
                base.native_type = Some(other.to_string());
                base.content = Some(compact(event));
                entries.push(base);
            }
        }
    }
    Some(CanonicalSession {
        harness: "openclaw".into(),
        session_id,
        cwd,
        source_path: source_path.display().to_string(),
        entries,
    })
}

// ---------------------------------------------------------------------
// cursor — state.vscdb (composerHeaders + cursorDiskKV bubbles)
// ---------------------------------------------------------------------

/// Canonicalize one cursor composer session.
pub(crate) fn canonical_from_cursor(
    raw: &cursor::RawSession,
    source_path: &Path,
) -> CanonicalSession {
    let head = &raw.head;
    let session_id = head
        .get("composerId")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let cwd = head
        .pointer("/workspaceIdentifier/uri/fsPath")
        .or_else(|| head.pointer("/workspaceIdentifier/uri/path"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut entries = vec![meta_entry("head", Some(compact(head)))];
    for (bubble_id, created, bubble) in &raw.bubbles {
        let bubble_type = bubble.get("type").and_then(|v| v.as_i64()).unwrap_or(0);
        let text = bubble.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let mut e = entry("meta");
        e.id = Some(bubble_id.clone());
        e.ts = Some(created.clone());
        match bubble_type {
            1 | 2 => {
                e.kind = "message".into();
                e.role = Some(
                    if bubble_type == 1 {
                        "user"
                    } else {
                        "assistant"
                    }
                    .into(),
                );
                e.content = Some(text.to_string());
            }
            other => {
                e.native_type = Some(format!("bubble type {other}"));
                e.content = Some(text.to_string());
            }
        }
        entries.push(e);
        // The toolResults shape is unverified (only empty arrays observed) —
        // preserved raw as meta, never parsed into calls.
        if let Some(results) = bubble.get("toolResults").and_then(|v| v.as_array()) {
            if !results.is_empty() {
                let mut e = meta_entry(
                    "cursor toolResults (shape unverified)",
                    Some(compact(&Value::Array(results.clone()))),
                );
                e.ts = Some(created.clone());
                entries.push(e);
            }
        }
    }
    CanonicalSession {
        harness: "cursor".into(),
        session_id,
        cwd,
        source_path: source_path.display().to_string(),
        entries,
    }
}

// ---------------------------------------------------------------------
// hermes — ~/.hermes/state.db (sessions + messages tables)
// ---------------------------------------------------------------------

/// Canonicalize one hermes session's raw rows.
pub(crate) fn canonical_from_hermes(
    raw: &hermes::RawSession,
    source_path: &Path,
) -> CanonicalSession {
    // Session-row span as provenance (entry timestamps come from the rows).
    let mut entries = vec![meta_entry(
        "session",
        Some(compact(&serde_json::json!({
            "started_at": raw.started,
            "ended_at": raw.ended,
        }))),
    )];
    for (role, content, tool_calls, tool_name, ts) in &raw.messages {
        let ts = if *ts > 0.0 {
            Some(if ts.fract() == 0.0 {
                format!("{}", *ts as i64)
            } else {
                format!("{ts}")
            })
        } else {
            None
        };
        match role.to_lowercase().as_str() {
            "user" | "human" => {
                let mut e = entry("message");
                e.ts = ts.clone();
                e.role = Some("user".into());
                e.content = Some(hermes::content_text_pub(content));
                entries.push(e);
            }
            "assistant" | "model" => {
                let mut e = entry("message");
                e.ts = ts.clone();
                e.role = Some("assistant".into());
                e.content = Some(hermes::content_text_pub(content));
                entries.push(e);
            }
            "tool" | "toolresult" | "function" => {
                let mut e = entry("tool_result");
                e.ts = ts.clone();
                e.name = if tool_name.trim().is_empty() {
                    None
                } else {
                    Some(tool_name.clone())
                };
                e.content = Some(hermes::content_text_pub(content));
                entries.push(e);
            }
            other => {
                let mut e = meta_entry(
                    &format!("message role `{other}`"),
                    Some(hermes::content_text_pub(content)),
                );
                e.ts = ts.clone();
                entries.push(e);
            }
        }
        if !tool_calls.trim().is_empty() {
            // tool_calls is a JSON array of calls when well-formed; the raw
            // string stays the content either way.
            let parsed: Value = serde_json::from_str(tool_calls).unwrap_or(Value::Null);
            if let Some(calls) = parsed.as_array() {
                for call in calls {
                    let function = call.get("function").unwrap_or(call);
                    let mut e = entry("tool_call");
                    e.ts = ts.clone();
                    e.id = call.get("id").and_then(|v| v.as_str()).map(str::to_string);
                    e.name = function
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .or_else(|| (!tool_name.trim().is_empty()).then(|| tool_name.clone()));
                    e.content = Some(
                        function
                            .get("arguments")
                            .map(|a| match a {
                                Value::String(s) => s.clone(),
                                other => compact(other),
                            })
                            .unwrap_or_else(|| compact(call)),
                    );
                    entries.push(e);
                }
            } else {
                let mut e = entry("tool_call");
                e.ts = ts;
                e.name = if tool_name.trim().is_empty() {
                    None
                } else {
                    Some(tool_name.clone())
                };
                e.content = Some(tool_calls.clone());
                entries.push(e);
            }
        }
    }
    CanonicalSession {
        harness: "hermes".into(),
        session_id: raw.id.clone(),
        cwd: raw.cwd.clone(),
        source_path: source_path.display().to_string(),
        entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kinds(session: &CanonicalSession) -> Vec<&str> {
        session.entries.iter().map(|e| e.kind.as_str()).collect()
    }

    #[test]
    fn claude_extracts_messages_tools_and_meta() {
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let events = vec![
            json!({"type":"queue-operation","operation":"enqueue","timestamp":"2026-07-10T09:00:00Z","sessionId":"s1","cwd":cwd}),
            json!({"type":"user","isMeta":true,"uuid":"u0","parentUuid":null,"message":{"role":"user","content":"caveat"},"timestamp":"2026-07-10T09:00:01Z"}),
            json!({"type":"user","uuid":"u1","parentUuid":"u0","message":{"role":"user","content":"<command-name>/model</command-name>"},"timestamp":"2026-07-10T09:00:02Z"}),
            json!({"type":"user","uuid":"u2","parentUuid":"u1","message":{"role":"user","content":"build the exporter"},"timestamp":"2026-07-10T09:00:03Z"}),
            json!({"type":"assistant","uuid":"u3","parentUuid":"u2","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"on it"},{"type":"tool_use","id":"tu1","name":"Write","input":{"file_path":"src/exporter.rs"}}]},"timestamp":"2026-07-10T09:00:04Z"}),
            json!({"type":"user","uuid":"u4","parentUuid":"u3","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","is_error":true,"content":"Permission denied"}]},"timestamp":"2026-07-10T09:00:05Z"}),
            json!({"type":"summary","summary":"exporter session","timestamp":"2026-07-10T09:00:06Z"}),
            json!({"type":"future_thing","timestamp":"2026-07-10T09:00:07Z","data":1}),
        ];
        let session = canonical_from_claude(&events, Path::new("/x/s1.jsonl"), project.path())
            .expect("session");
        assert_eq!(session.harness, "claude");
        assert_eq!(session.session_id, "s1");
        assert_eq!(
            kinds(&session),
            [
                "meta",        // queue-operation
                "meta",        // isMeta
                "meta",        // local_command
                "message",     // user
                "meta",        // thinking
                "message",     // assistant text
                "tool_call",   // Write
                "tool_result", // tu1
                "meta",        // summary
                "meta",        // future_thing
            ]
        );
        let e = &session.entries;
        assert_eq!(e[3].role.as_deref(), Some("user"));
        assert_eq!(e[3].id.as_deref(), Some("u2"));
        assert_eq!(e[3].parent_id.as_deref(), Some("u1"));
        assert_eq!(e[3].content.as_deref(), Some("build the exporter"));
        assert_eq!(e[4].native_type.as_deref(), Some("thinking"));
        assert_eq!(e[6].name.as_deref(), Some("Write"));
        assert_eq!(
            e[6].content.as_deref(),
            Some("{\"file_path\":\"src/exporter.rs\"}")
        );
        assert_eq!(e[7].parent_id.as_deref(), Some("tu1"));
        assert_eq!(e[7].native_type.as_deref(), Some("is_error"));
        assert_eq!(e[7].content.as_deref(), Some("Permission denied"));
        assert_eq!(e[9].native_type.as_deref(), Some("future_thing"));

        // cwd filter.
        let other = vec![
            json!({"type":"user","message":{"role":"user","content":"hi"},"cwd":"/elsewhere","sessionId":"s2"}),
        ];
        assert!(canonical_from_claude(&other, Path::new("/x/s2.jsonl"), project.path()).is_none());
    }

    #[test]
    fn codex_extracts_messages_plans_tools_and_compactions() {
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let meta = json!({"type":"session_meta","payload":{"id":"c-1","cwd":cwd,"timestamp":"2026-07-01T10:00:00Z"}});
        let events = vec![
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"<environment_context>injected</environment_context>"}]},"timestamp":"2026-07-01T10:00:01Z"}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"write the doc"}]},"timestamp":"2026-07-01T10:00:02Z"}),
            json!({"type":"response_item","payload":{"type":"reasoning","summary":[{"text":"planning"}]},"timestamp":"2026-07-01T10:00:03Z"}),
            json!({"type":"response_item","payload":{"type":"function_call","name":"update_plan","arguments":"{\"plan\":[{\"step\":\"write\",\"status\":\"completed\"},{\"step\":\"review\",\"status\":\"pending\"}]}","call_id":"c1"},"timestamp":"2026-07-01T10:00:04Z"}),
            json!({"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":\"ls\"}","call_id":"c2"},"timestamp":"2026-07-01T10:00:05Z"}),
            json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** patch","call_id":"c3"},"timestamp":"2026-07-01T10:00:06Z"}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c2","output":"ok"},"timestamp":"2026-07-01T10:00:07Z"}),
            json!({"type":"compacted","payload":{"message":"progress so far"},"timestamp":"2026-07-01T10:00:08Z"}),
            json!({"type":"compacted","payload":{"message":"","encrypted_content":"blob"},"timestamp":"2026-07-01T10:00:09Z"}),
            json!({"type":"event_msg","payload":{"type":"task_complete"},"timestamp":"2026-07-01T10:00:10Z"}),
        ];
        let session = canonical_from_codex(
            &meta,
            &events,
            Path::new("/x/rollout-1.jsonl"),
            project.path(),
        )
        .expect("session");
        assert_eq!(
            kinds(&session),
            [
                "meta",
                "message",
                "message",
                "meta",
                "plan",
                "tool_call",
                "tool_call",
                "tool_result",
                "compaction",
                "meta",
                "meta",
            ]
        );
        let e = &session.entries;
        assert_eq!(e[1].native_type.as_deref(), Some("injected"));
        assert_eq!(e[2].content.as_deref(), Some("write the doc"));
        assert_eq!(e[3].native_type.as_deref(), Some("reasoning"));
        assert_eq!(
            e[4].content.as_deref(),
            Some("- [completed] write\n- [pending] review")
        );
        assert_eq!(e[5].id.as_deref(), Some("c2"));
        assert_eq!(e[5].content.as_deref(), Some("{\"command\":\"ls\"}"));
        assert_eq!(e[6].native_type.as_deref(), Some("custom_tool_call"));
        assert_eq!(e[7].parent_id.as_deref(), Some("c2"));
        assert_eq!(e[7].content.as_deref(), Some("ok"));
        assert_eq!(e[8].content.as_deref(), Some("progress so far"));
        assert_eq!(e[9].native_type.as_deref(), Some("compacted"));
        assert_eq!(e[9].content.as_deref(), Some("encrypted by harness"));
        assert_eq!(
            e[10].native_type.as_deref(),
            Some("event_msg `task_complete`")
        );
    }

    #[test]
    fn kimi_extracts_messages_skips_partials_and_marks_origins() {
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let meta =
            json!({"type":"metadata","protocol_version":"1.0","created_at":1784310494250i64});
        let records = vec![
            json!({"type":"context.append_message","message":{"role":"user","content":[{"type":"text","text":"real prompt"}]},"time":1784310495000i64}),
            json!({"type":"context.append_message","message":{"role":"user","origin":{"kind":"hook_result"},"content":[{"type":"text","text":"injected delta"}]},"time":1784310496000i64}),
            json!({"type":"context.append_message","message":{"role":"user","content":[{"type":"text","text":"stream fragment"}],"partial":true},"time":1784310496100i64}),
            json!({"type":"context.append_message","message":{"role":"assistant","content":[{"type":"think","think":"reasoning"},{"type":"text","text":"answer"}],"toolCalls":[{"type":"function","id":"t1","function":{"name":"Shell","arguments":"{\"command\":\"ls\"}"}}]},"time":1784310497000i64}),
            json!({"type":"context.append_message","message":{"role":"tool","content":[{"type":"text","text":"exit 0"}]},"time":1784310498000i64}),
            json!({"type":"llm.config","time":1784310499000i64,"data":{}}),
        ];
        let session = canonical_from_kimi(
            &meta,
            &records,
            "ses-1",
            &cwd,
            Path::new("/x/wire.jsonl"),
            project.path(),
        )
        .expect("session");
        assert_eq!(
            kinds(&session),
            [
                "meta",    // metadata line
                "message", // real prompt
                "message", // origin-tagged (kept, marked)
                // partial skipped entirely
                "message",     // assistant text
                "meta",        // thinking
                "tool_call",   // Shell
                "tool_result", // tool message
                "meta",        // llm.config
            ]
        );
        let e = &session.entries;
        assert_eq!(e[1].content.as_deref(), Some("real prompt"));
        assert_eq!(e[1].native_type, None);
        assert_eq!(e[2].native_type.as_deref(), Some("origin:hook_result"));
        assert_eq!(e[4].native_type.as_deref(), Some("thinking"));
        assert_eq!(e[5].name.as_deref(), Some("Shell"));
        assert_eq!(e[5].content.as_deref(), Some("{\"command\":\"ls\"}"));
        assert_eq!(e[6].content.as_deref(), Some("exit 0"));
        assert_eq!(e[7].native_type.as_deref(), Some("llm.config"));
        assert_eq!(e[1].ts.as_deref(), Some("1784310495000"));

        // No cwd binding → excluded.
        assert!(canonical_from_kimi(
            &meta,
            &records,
            "ses-1",
            "",
            Path::new("/x/wire.jsonl"),
            project.path()
        )
        .is_none());
    }

    #[test]
    fn openclaw_extracts_all_entry_kinds() {
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let events = vec![
            json!({"type":"session","version":1,"id":"oc-1","timestamp":"2026-07-10T09:00:00Z","cwd":cwd}),
            json!({"type":"message","id":"m1","parentId":null,"timestamp":"2026-07-10T09:00:01Z","message":{"role":"user","content":[{"type":"text","text":"inbox summary"}]}}),
            json!({"type":"message","id":"m2","parentId":"m1","timestamp":"2026-07-10T09:00:02Z","message":{"role":"assistant","content":"done — 3 unread"}}),
            json!({"type":"message","id":"m3","parentId":"m2","timestamp":"2026-07-10T09:00:03Z","message":{"role":"toolResult","toolCallId":"c1","content":[{"type":"text","text":"tool out"}]}}),
            json!({"type":"custom_message","id":"m4","parentId":"m3","timestamp":"2026-07-10T09:00:04Z","message":{"role":"user","content":"extension context"}}),
            json!({"type":"compaction","id":"co","parentId":"m4","timestamp":"2026-07-10T09:00:05Z","summary":"compacted text"}),
            json!({"type":"branch_summary","id":"bs","parentId":"m4","timestamp":"2026-07-10T09:00:06Z","summary":"branch text"}),
            json!({"type":"custom","id":"cu","parentId":"bs","timestamp":"2026-07-10T09:00:07Z","customType":"x","data":{"k":1}}),
            json!({"id":"m5","parentId":"bs","timestamp":"2026-07-10T09:00:08Z","message":{"role":"user","content":"legacy untyped"}}),
        ];
        let session = canonical_from_openclaw(&events, Path::new("/x/oc-1.jsonl"), project.path())
            .expect("session");
        assert_eq!(session.session_id, "oc-1");
        assert_eq!(
            kinds(&session),
            [
                "message",
                "message",
                "tool_result",
                "message",
                "compaction",
                "compaction",
                "meta",
                "message"
            ]
        );
        let e = &session.entries;
        assert_eq!(e[0].content.as_deref(), Some("inbox summary"));
        assert_eq!(e[1].content.as_deref(), Some("done — 3 unread"));
        assert_eq!(e[2].parent_id.as_deref(), Some("c1"));
        assert_eq!(e[3].native_type.as_deref(), Some("custom_message"));
        assert_eq!(e[5].native_type.as_deref(), Some("branch_summary"));
        assert_eq!(e[6].native_type.as_deref(), Some("custom"));
        assert!(e[6].content.as_deref().unwrap_or("").contains("\"k\":1"));
        assert_eq!(e[7].content.as_deref(), Some("legacy untyped"));
    }

    #[test]
    fn cursor_extracts_bubbles_and_preserves_tool_results_raw() {
        let raw = cursor::RawSession {
            head: json!({"type":"head","composerId":"cmp-1","name":"demo","workspaceIdentifier":{"uri":{"fsPath":"/work/demo"}}}),
            bubbles: vec![
                (
                    "b-1".into(),
                    "2026-07-01T10:00:01Z".into(),
                    json!({"type":1,"text":"do the thing"}),
                ),
                (
                    "b-2".into(),
                    "2026-07-01T10:00:02Z".into(),
                    json!({"type":2,"text":"done","toolResults":[{"opaque":1}]}),
                ),
                (
                    "b-3".into(),
                    "2026-07-01T10:00:03Z".into(),
                    json!({"type":9,"text":"weird bubble"}),
                ),
            ],
        };
        let session = canonical_from_cursor(&raw, Path::new("/x/state.vscdb"));
        assert_eq!(session.session_id, "cmp-1");
        assert_eq!(session.cwd, "/work/demo");
        assert_eq!(
            kinds(&session),
            ["meta", "message", "message", "meta", "meta"]
        );
        let e = &session.entries;
        assert_eq!(e[1].role.as_deref(), Some("user"));
        assert_eq!(e[1].id.as_deref(), Some("b-1"));
        assert_eq!(e[2].role.as_deref(), Some("assistant"));
        assert_eq!(
            e[3].native_type.as_deref(),
            Some("cursor toolResults (shape unverified)")
        );
        assert!(e[3].content.as_deref().unwrap_or("").contains("opaque"));
        assert_eq!(e[4].native_type.as_deref(), Some("bubble type 9"));
    }

    #[test]
    fn hermes_extracts_roles_tools_and_span() {
        let raw = hermes::RawSession {
            id: "h-1".into(),
            cwd: "/work/demo".into(),
            started: 1700000000.0,
            ended: 1700000060.0,
            messages: vec![
                (
                    "user".into(),
                    "draft the brief".into(),
                    "".into(),
                    "".into(),
                    1700000001.0,
                ),
                (
                    "assistant".into(),
                    "calling the tool".into(),
                    r#"[{"id":"c1","function":{"name":"search","arguments":"{\"q\":\"x\"}"}}]"#
                        .into(),
                    "".into(),
                    1700000002.0,
                ),
                (
                    "tool".into(),
                    "results here".into(),
                    "".into(),
                    "search".into(),
                    1700000003.0,
                ),
                (
                    "model".into(),
                    "final answer".into(),
                    "".into(),
                    "".into(),
                    1700000004.0,
                ),
                (
                    "mystery".into(),
                    "opaque".into(),
                    "".into(),
                    "".into(),
                    1700000005.0,
                ),
            ],
        };
        let session = canonical_from_hermes(&raw, Path::new("/x/state.db"));
        assert_eq!(
            kinds(&session),
            [
                "meta",
                "message",
                "message",
                "tool_call",
                "tool_result",
                "message",
                "meta"
            ]
        );
        let e = &session.entries;
        assert_eq!(e[0].native_type.as_deref(), Some("session"));
        assert!(e[0].content.as_deref().unwrap_or("").contains("1700000000"));
        assert_eq!(e[1].role.as_deref(), Some("user"));
        assert_eq!(e[1].ts.as_deref(), Some("1700000001"));
        assert_eq!(e[2].role.as_deref(), Some("assistant"));
        assert_eq!(e[3].name.as_deref(), Some("search"));
        assert_eq!(e[3].content.as_deref(), Some("{\"q\":\"x\"}"));
        assert_eq!(e[4].name.as_deref(), Some("search"));
        assert_eq!(e[4].content.as_deref(), Some("results here"));
        assert_eq!(e[6].native_type.as_deref(), Some("message role `mystery`"));
    }
}
