//! Transfer writers: canonical → a real, resumable native session file.
//!
//! Transfer translates strings to strings: a canonical session becomes a
//! native Pi v3 / DSH v0 JSONL log that the target harness opens and
//! resumes. The source session is never mutated, an existing target is
//! never clobbered, and fidelity is reported honestly — what mapped
//! (native), what degraded (adapted, with the mapping named), what was
//! dropped (with the native type named).
//!
//! Writer shapes verified against the read-only format references:
//! pi `packages/coding-agent/src/core/session-manager.ts` (+ pi-ai types)
//! and deepseek-harness `packages/core/session` (+ session-persistence-jsonl).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::StoredSession;
use crate::harness_install::paths;
use crate::local_store::now_rfc3339;

/// Fidelity accounting for one transfer.
#[derive(Debug, Default)]
pub struct Fidelity {
    /// Entries that mapped to their exact native counterpart.
    pub native: usize,
    /// Entries that mapped with a named degradation.
    pub adapted: usize,
    /// Entries with no target counterpart.
    pub dropped: usize,
    /// Adaptation kinds → counts (e.g. `compaction→branch_summary`).
    pub adapted_kinds: BTreeMap<String, usize>,
    /// Dropped native types → counts (e.g. `model_change`).
    pub dropped_kinds: BTreeMap<String, usize>,
}

impl Fidelity {
    fn native(&mut self) {
        self.native += 1;
    }

    fn adapt(&mut self, kind: &str) {
        self.adapted += 1;
        *self.adapted_kinds.entry(kind.to_string()).or_insert(0) += 1;
    }

    fn drop(&mut self, kind: &str) {
        self.dropped += 1;
        *self.dropped_kinds.entry(kind.to_string()).or_insert(0) += 1;
    }

    fn kinds(kinds: &BTreeMap<String, usize>) -> String {
        kinds
            .iter()
            .map(|(k, n)| {
                if *n > 1 {
                    format!("{k} ×{n}")
                } else {
                    k.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `84 native · 6 adapted (compaction→branch_summary) · 3 dropped (model_change)`.
    pub fn line(&self) -> String {
        let mut out = format!("{} native", self.native);
        out.push_str(&format!(" · {} adapted", self.adapted));
        if !self.adapted_kinds.is_empty() {
            out.push_str(&format!(" ({})", Self::kinds(&self.adapted_kinds)));
        }
        out.push_str(&format!(" · {} dropped", self.dropped));
        if !self.dropped_kinds.is_empty() {
            out.push_str(&format!(" ({})", Self::kinds(&self.dropped_kinds)));
        }
        out
    }
}

/// A planned transfer: target path plus the exact lines to write.
#[derive(Debug)]
pub struct TransferPlan {
    /// The native session file to create.
    pub target_path: PathBuf,
    /// Header line + entry lines (no trailing newlines).
    pub lines: Vec<String>,
    /// Fidelity accounting.
    pub fidelity: Fidelity,
    /// How to resume the transferred session (`pi (in /cwd)`).
    pub resume_hint: String,
}

/// Write the plan, REFUSING to clobber an existing file (`create_new`).
pub fn write(plan: &TransferPlan) -> Result<(), String> {
    if let Some(parent) = plan.target_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&plan.target_path)
        .map_err(|e| format!("cannot create {}: {e}", plan.target_path.display()))?;
    use std::io::Write as _;
    for line in &plan.lines {
        writeln!(file, "{line}")
            .map_err(|e| format!("write {}: {e}", plan.target_path.display()))?;
    }
    Ok(())
}

/// Canonical ts → epoch milliseconds (dsh-native integer string, or pi ISO).
fn ts_to_ms(ts: Option<&str>, fallback: i64) -> i64 {
    match ts {
        Some(s) => {
            if let Ok(ms) = s.parse::<i64>() {
                ms
            } else {
                chrono::DateTime::parse_from_rfc3339(s)
                    .map(|dt| dt.timestamp_millis())
                    .unwrap_or(fallback)
            }
        }
        None => fallback,
    }
}

/// Canonical ts → an ISO-8601 string for pi entry timestamps.
fn ts_to_iso(ts: Option<&str>, fallback_ms: i64) -> String {
    if let Some(s) = ts {
        if let Ok(ms) = s.parse::<i64>() {
            return chrono::DateTime::from_timestamp_millis(ms)
                .map(|d| d.to_rfc3339())
                .unwrap_or_default();
        }
        return s.to_string(); // already ISO (pi-native)
    }
    chrono::DateTime::from_timestamp_millis(fallback_ms)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ---------------------------------------------------------------------
// Pi writer
// ---------------------------------------------------------------------

/// Pi's sessions-dir encoding (session-manager.ts `getDefaultSessionDirPath`):
/// strip one leading slash, then every `/ \ :` becomes `-`, wrapped `--…--`.
fn pi_session_dir(home: &Path, cwd: &Path) -> PathBuf {
    let raw = cwd.to_string_lossy().replace('\\', "/");
    let stripped = raw.strip_prefix('/').unwrap_or(&raw);
    let encoded: String = stripped
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    paths::pi_agent_root(home)
        .join("sessions")
        .join(format!("--{encoded}--"))
}

/// Zeroed Usage object — pi's AssistantMessage requires the field.
fn pi_zero_usage() -> Value {
    json!({
        "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0},
    })
}

/// Assistant-message envelope pi expects (provider/api are open strings;
/// provenance is labeled `stateroot`).
fn pi_assistant_message(content: Value, timestamp: i64) -> Value {
    json!({
        "role": "assistant",
        "content": content,
        "api": "stateroot",
        "provider": "stateroot",
        "model": "transferred",
        "usage": pi_zero_usage(),
        "stopReason": "stop",
        "timestamp": timestamp,
    })
}

/// Canonical → Pi v3 session plan: fresh header, entries re-chained as one
/// linear id/parentId spine (branches flatten to the imported timeline).
pub fn plan_pi(
    session: &StoredSession,
    home: &Path,
    cwd: &Path,
    new_id: &str,
) -> Result<TransferPlan, String> {
    let started = now_rfc3339();
    let file_ts = started.replace([':', '.'], "-");
    let target = pi_session_dir(home, cwd).join(format!("{file_ts}_{new_id}.jsonl"));
    let created_ms = now_ms();
    let mut lines = vec![serde_json::to_string(&json!({
        "type": "session",
        "version": 3,
        "id": new_id,
        "timestamp": started,
        "cwd": cwd.display().to_string(),
    }))
    .map_err(|e| e.to_string())?];
    let mut fidelity = Fidelity::default();
    let mut parent: Option<String> = None;
    let provenance = format!("imported from {} by stateroot", session.harness);

    let push = |entry: Value, lines: &mut Vec<String>, parent: &mut Option<String>| {
        let id = uuid::Uuid::new_v4().to_string();
        let mut entry = entry;
        entry["id"] = json!(id);
        entry["parentId"] = match parent.as_ref() {
            Some(p) => json!(p),
            None => Value::Null,
        };
        lines.push(serde_json::to_string(&entry).map_err(|e| e.to_string())?);
        *parent = Some(id);
        Ok::<(), String>(())
    };

    for (idx, entry) in session.entries.iter().enumerate() {
        let ms = ts_to_ms(entry.ts.as_deref(), created_ms + idx as i64);
        let iso = ts_to_iso(entry.ts.as_deref(), created_ms + idx as i64);
        match entry.kind.as_str() {
            "message" => {
                let role = entry.role.as_deref().unwrap_or("user");
                let text = entry.content.as_deref().unwrap_or("");
                let message = if role == "assistant" {
                    pi_assistant_message(json!([{ "type": "text", "text": text }]), ms)
                } else {
                    json!({ "role": "user", "content": text, "timestamp": ms })
                };
                push(
                    json!({ "type": "message", "timestamp": iso, "message": message }),
                    &mut lines,
                    &mut parent,
                )?;
                fidelity.native();
            }
            "tool_call" => {
                let raw_args = entry.content.as_deref().unwrap_or("{}");
                let arguments = match serde_json::from_str::<Value>(raw_args) {
                    Ok(Value::Object(map)) => Value::Object(map),
                    _ => {
                        fidelity.adapt("tool_call arguments rewrapped");
                        json!({ "_stateroot_raw": raw_args })
                    }
                };
                let call_id = entry
                    .id
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let message = pi_assistant_message(
                    json!([{
                        "type": "toolCall",
                        "id": call_id,
                        "name": entry.name.as_deref().unwrap_or("tool"),
                        "arguments": arguments,
                    }]),
                    ms,
                );
                push(
                    json!({ "type": "message", "timestamp": iso, "message": message }),
                    &mut lines,
                    &mut parent,
                )?;
                fidelity.native();
            }
            "tool_result" => {
                let message = json!({
                    "role": "toolResult",
                    "toolCallId": entry.parent_id.as_deref().unwrap_or(""),
                    "toolName": entry.name.as_deref().unwrap_or("tool"),
                    "content": [{ "type": "text", "text": entry.content.as_deref().unwrap_or("") }],
                    "isError": false,
                    "timestamp": ms,
                });
                push(
                    json!({ "type": "message", "timestamp": iso, "message": message }),
                    &mut lines,
                    &mut parent,
                )?;
                fidelity.native();
            }
            "compaction" => {
                let summary = format!(
                    "[{provenance}]\n\n{}",
                    entry.content.as_deref().unwrap_or("")
                );
                push(
                    json!({
                        "type": "branch_summary",
                        "timestamp": iso,
                        "fromId": parent.clone().unwrap_or_default(),
                        "summary": summary,
                    }),
                    &mut lines,
                    &mut parent,
                )?;
                fidelity.adapt("compaction→branch_summary");
            }
            "plan" => fidelity.drop("plan"),
            other => {
                let native = entry.native_type.as_deref().unwrap_or(other);
                fidelity.drop(native);
            }
        }
    }
    Ok(TransferPlan {
        target_path: target,
        lines,
        fidelity,
        resume_hint: format!("pi (in {})", cwd.display()),
    })
}

// ---------------------------------------------------------------------
// DSH writer
// ---------------------------------------------------------------------

/// DSH `encodeSegment` (session-persistence-jsonl/format.ts): safe units stay
/// literal, everything else becomes `~XXXX` (uppercase hex of the code unit);
/// `.`/`..` are special-cased against traversal.
fn dsh_encode_segment(raw: &str) -> String {
    if raw == "." {
        return "~002E".to_string();
    }
    if raw == ".." {
        return "~002E~002E".to_string();
    }
    let mut out = String::new();
    for ch in raw.chars() {
        if ch != '~' && (ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')) {
            out.push(ch);
        } else {
            out.push_str(&format!("~{:04X}", ch as u32));
        }
    }
    out
}

/// DSH `projectKey` (same reference): separators collapse to single `-` runs,
/// unsafe units escape, leading `-` stripped, `--slug--` bounded to 251.
fn dsh_project_key(cwd: &str) -> String {
    let mut readable = String::new();
    let mut separator_run = false;
    for ch in cwd.chars() {
        match ch {
            '/' | '\\' | ':' => {
                if !separator_run {
                    readable.push('-');
                }
                separator_run = true;
            }
            c if c != '~' && (c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) => {
                readable.push(c);
                separator_run = false;
            }
            c => {
                readable.push_str(&format!("~{:04X}", c as u32));
                separator_run = false;
            }
        }
    }
    let stripped = readable.trim_start_matches('-');
    let slug: String = if stripped.is_empty() {
        "root".into()
    } else {
        stripped.chars().take(251).collect()
    };
    format!("--{slug}--")
}

/// DSH turn/step state machine over the canonical timeline. Events carry
/// contiguous seq from 0; turns and steps are 1-based (DSH invariant).
struct DshWriter {
    lines: Vec<String>,
    seq: usize,
    fidelity: Fidelity,
    turn: usize,
    step: usize,
    turn_open: bool,
    step_open: bool,
    pending: Vec<String>,
}

impl DshWriter {
    fn new() -> Self {
        DshWriter {
            lines: Vec::new(),
            seq: 0,
            fidelity: Fidelity::default(),
            turn: 0,
            step: 0,
            turn_open: false,
            step_open: false,
            pending: Vec::new(),
        }
    }

    fn emit(&mut self, time: i64, kind: &str, data: Value, surface: bool) {
        let mut event = json!({
            "type": kind,
            "seq": self.seq,
            "time": time,
            "data": data,
        });
        // surfaceOp is mandatory on surface-eligible events (the surface fold
        // throws without it); log-only events must NOT carry it.
        if surface {
            event["surfaceOp"] = json!("append");
        }
        self.lines
            .push(serde_json::to_string(&event).unwrap_or_default());
        self.seq += 1;
    }

    fn open_turn(&mut self, time: i64) {
        self.turn += 1;
        self.step = 0;
        self.emit(time, "turn/start", json!({ "turn": self.turn }), false);
        self.turn_open = true;
    }

    fn close_step(&mut self, time: i64) {
        if self.step_open {
            self.emit(
                time,
                "step/end",
                json!({ "turn": self.turn, "step": self.step }),
                false,
            );
            self.step_open = false;
            self.pending.clear();
        }
    }

    fn close_turn(&mut self, time: i64, reason: &str) {
        self.close_step(time);
        if self.turn_open {
            self.emit(
                time,
                "turn/end",
                json!({ "turn": self.turn, "reason": { "kind": reason } }),
                false,
            );
            self.turn_open = false;
        }
    }

    fn ensure_step(&mut self, time: i64) {
        if !self.turn_open {
            self.open_turn(time);
        }
        if !self.step_open {
            self.step += 1;
            self.emit(
                time,
                "step/start",
                json!({ "turn": self.turn, "step": self.step }),
                false,
            );
            self.step_open = true;
        }
    }
}

/// Canonical → DSH v0 session plan (plain JSONL; the target harness loads it
/// as completed/resumable — every turn closes with a clean tail).
pub fn plan_dsh(
    session: &StoredSession,
    home: &Path,
    cwd: &Path,
    new_id: &str,
) -> Result<TransferPlan, String> {
    let created = now_ms();
    let cwd_string = cwd.display().to_string();
    let target = paths::dsh_root(home)
        .join("sessions")
        .join(dsh_project_key(&cwd_string))
        .join(dsh_encode_segment(new_id))
        .join("session.jsonl");
    let mut lines = vec![serde_json::to_string(&json!({
        "type": "session",
        "version": 0,
        "id": new_id,
        "createdAt": created,
        "cwd": cwd_string,
        "delegationDepth": 0,
    }))
    .map_err(|e| e.to_string())?];

    let mut w = DshWriter::new();
    for (idx, entry) in session.entries.iter().enumerate() {
        let time = ts_to_ms(entry.ts.as_deref(), created + idx as i64);
        match entry.kind.as_str() {
            "message" if entry.role.as_deref() == Some("user") => {
                // A fresh user prompt starts a new turn.
                if w.turn_open {
                    w.close_turn(time, "completed");
                }
                w.open_turn(time);
                let message = json!({
                    "id": uuid::Uuid::new_v4().to_string(),
                    "role": "user",
                    "content": [{ "type": "text", "text": entry.content.as_deref().unwrap_or("") }],
                    "source": { "kind": "user" },
                });
                w.emit(time, "user/message", message, true);
                w.fidelity.native();
            }
            "message" => {
                // assistant (or role-less) message — inside the current step.
                w.ensure_step(time);
                let message = json!({
                    "id": uuid::Uuid::new_v4().to_string(),
                    "role": "assistant",
                    "content": [{ "type": "text", "text": entry.content.as_deref().unwrap_or("") }],
                    "source": { "kind": "model", "provider": session.harness, "model": "transferred" },
                });
                w.emit(
                    time,
                    "assistant/message",
                    json!({
                        "turn": w.turn,
                        "step": w.step,
                        "message": message,
                    }),
                    true,
                );
                w.fidelity.native();
            }
            "tool_call" => {
                w.ensure_step(time);
                let call_id = entry
                    .id
                    .clone()
                    .unwrap_or_else(|| format!("call_{}", w.seq));
                w.emit(
                    time,
                    "tool/call",
                    json!({
                        "turn": w.turn,
                        "step": w.step,
                        "callId": call_id,
                        "name": entry.name.as_deref().unwrap_or("tool"),
                        "arguments": entry.content.as_deref().unwrap_or("{}"),
                    }),
                    false,
                );
                w.pending.push(call_id);
                w.fidelity.native();
            }
            "tool_result" => {
                w.ensure_step(time);
                let call_id = entry
                    .parent_id
                    .clone()
                    .unwrap_or_else(|| format!("call_{}", w.seq));
                if !w.pending.contains(&call_id) {
                    // Orphan result (source was truncated mid-flight): pair it
                    // with a synthetic call so the DSH pending-call invariant
                    // holds — the degradation is counted, not hidden.
                    w.emit(
                        time,
                        "tool/call",
                        json!({
                            "turn": w.turn,
                            "step": w.step,
                            "callId": call_id,
                            "name": entry.name.as_deref().unwrap_or("tool"),
                            "arguments": "{}",
                        }),
                        false,
                    );
                    w.pending.push(call_id.clone());
                    w.fidelity
                        .adapt("orphan tool_result paired with a synthetic call");
                }
                let message = json!({
                    "id": uuid::Uuid::new_v4().to_string(),
                    "role": "user",
                    "content": [{
                        "type": "tool-result",
                        "toolCallId": call_id,
                        "content": [{ "type": "text", "text": entry.content.as_deref().unwrap_or("") }],
                        "isError": false,
                    }],
                    "source": { "kind": "tool", "callId": call_id },
                });
                w.emit(
                    time,
                    "tool/result",
                    json!({
                        "turn": w.turn,
                        "step": w.step,
                        "message": message,
                    }),
                    true,
                );
                w.pending.retain(|c| c != &call_id);
                w.fidelity.native();
            }
            "compaction" => {
                // Injected-context note: model-visible, honestly sourced.
                if !w.turn_open {
                    w.open_turn(time);
                }
                w.close_step(time);
                let message = json!({
                    "id": uuid::Uuid::new_v4().to_string(),
                    "role": "user",
                    "content": [{
                        "type": "text",
                        "text": format!(
                            "[stateroot transfer — {} compaction summary]\n\n{}",
                            session.harness,
                            entry.content.as_deref().unwrap_or("")
                        ),
                    }],
                    "source": { "kind": "plugin", "plugin": "stateroot" },
                });
                w.emit(time, "user/message", message, true);
                w.fidelity.adapt("compaction→user/message");
            }
            "plan" => {
                // Reconstruct the todo snapshot (latest-wins whole list).
                if !w.turn_open {
                    w.open_turn(time);
                }
                let mut todos = Vec::new();
                for line in entry.content.as_deref().unwrap_or("").lines() {
                    let parsed = line
                        .strip_prefix("- [")
                        .and_then(|rest| rest.split_once(']'))
                        .map(|(status, content)| (status.trim(), content.trim()));
                    let (status, content) = match parsed {
                        Some((s, c)) => (s.to_string(), c.to_string()),
                        None => ("pending".to_string(), line.trim().to_string()),
                    };
                    if !content.is_empty() {
                        todos.push(json!({ "content": content, "status": status }));
                    }
                }
                if !todos.is_empty() {
                    w.emit(time, "todo/write", json!({ "todos": todos }), false);
                    w.fidelity.native();
                }
            }
            other => {
                let native = entry.native_type.as_deref().unwrap_or(other);
                w.fidelity.drop(native);
            }
        }
    }
    // Clean tail: the last turn closes completed when the last
    // conversational entry is an assistant message with no pending calls,
    // interrupted otherwise (honest crash-orphan semantics, per DSH's own
    // repair markers). plan/meta entries don't affect the judgment.
    let last_conversational = session
        .entries
        .iter()
        .rev()
        .find(|e| matches!(e.kind.as_str(), "message" | "tool_call" | "tool_result"));
    let clean_tail = matches!(
        last_conversational,
        Some(e) if e.kind == "message" && e.role.as_deref() == Some("assistant")
    ) && w.pending.is_empty();
    if w.turn_open {
        let time = ts_to_ms(
            session.entries.last().and_then(|e| e.ts.as_deref()),
            created + session.entries.len() as i64,
        );
        w.close_turn(
            time,
            if clean_tail {
                "completed"
            } else {
                "interrupted"
            },
        );
    }
    lines.extend(w.lines);
    Ok(TransferPlan {
        target_path: target,
        lines,
        fidelity: w.fidelity,
        resume_hint: format!("dsh (in {})", cwd.display()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions;
    use std::io::Write;

    fn write_file(path: &Path, lines: &[&str]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        let mut f = std::fs::File::create(path).expect("create");
        for line in lines {
            writeln!(f, "{line}").expect("write");
        }
    }

    /// pi fixture → canonical → pi writer → re-read with our own pi reader.
    #[test]
    fn pi_roundtrip_preserves_conversation_and_reports_fidelity() {
        let home = tempfile::tempdir().expect("home");
        let target_home = tempfile::tempdir().expect("target home");
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        write_file(
            &home
                .path()
                .join(".pi/agent/sessions/--tmp-demo--/t_rt.jsonl"),
            &[
                &format!(
                    r#"{{"type":"session","version":3,"id":"rt-1","timestamp":"2026-08-20T10:00:00.000Z","cwd":"{cwd}"}}"#
                ),
                r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-08-20T10:00:01.000Z","message":{"role":"user","content":"port the parser","timestamp":1784272801000}}"#,
                r#"{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-08-20T10:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"ported"},{"type":"toolCall","id":"c1","name":"bash","arguments":{"command":"cargo test"}}],"timestamp":1784272802000}}"#,
                r#"{"type":"message","id":"m3","parentId":"m2","timestamp":"2026-08-20T10:00:03.000Z","message":{"role":"toolResult","toolCallId":"c1","toolName":"bash","content":[{"type":"text","text":"all green"}],"isError":false,"timestamp":1784272803000}}"#,
                r#"{"type":"model_change","id":"mc","parentId":"m3","timestamp":"2026-08-20T10:00:04.000Z","provider":"deepseek","modelId":"m"}"#,
                r#"{"type":"compaction","id":"co","parentId":"m3","timestamp":"2026-08-20T10:00:05.000Z","summary":"parser halfway ported","firstKeptEntryId":"m3","tokensBefore":1234}"#,
            ],
        );
        sessions::import_from_readers(home.path(), project.path());
        let stored = sessions::load(project.path(), "rt-1").expect("stored");

        let plan =
            plan_pi(&stored, target_home.path(), project.path(), "fresh-pi-id").expect("plan");
        assert_eq!(plan.fidelity.native, 4, "{}", plan.fidelity.line());
        assert_eq!(plan.fidelity.adapted, 1, "compaction→branch_summary");
        assert_eq!(plan.fidelity.dropped, 1, "model_change dropped");
        assert!(
            plan.fidelity.line().contains("model_change"),
            "{}",
            plan.fidelity.line()
        );
        write(&plan).expect("write");
        assert!(plan.target_path.is_file());

        // Never-clobber: a second write to the same path refuses.
        assert!(write(&plan).is_err(), "existing target must refuse");

        // Re-read with our own pi reader: same conversation texts.
        let raw = crate::transcripts::pi::parse_session_file(&plan.target_path).expect("raw");
        let reread = crate::transcripts::pi::summarize(&raw, project.path()).expect("session");
        assert_eq!(reread.session_id, "fresh-pi-id");
        assert_eq!(reread.objective, "port the parser");
        let tail: Vec<&str> = reread
            .conversation_tail
            .iter()
            .map(|t| t.text.as_str())
            .collect();
        assert!(tail.iter().any(|t| t.contains("ported")), "tail: {tail:?}");
        // The tool result survives as a real pi toolResult message (tails
        // are user/assistant-only by reader design).
        let tool_result_text = raw
            .entries
            .iter()
            .filter(|e| e.pointer("/message/role").and_then(|v| v.as_str()) == Some("toolResult"))
            .filter_map(|e| {
                e.pointer("/message/content/0/text")
                    .and_then(|v| v.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(tool_result_text, ["all green"]);
        assert!(
            reread
                .progress_summaries
                .iter()
                .any(|s| s.contains("parser halfway ported")
                    && s.contains("imported from pi by stateroot")),
            "summaries: {:?}",
            reread.progress_summaries
        );
        // The header is a v3 pi header at the pi-encoded dir for the cwd.
        assert_eq!(raw.header["version"], 3);
        assert_eq!(raw.header["cwd"].as_str().expect("cwd"), cwd);
        assert!(plan
            .target_path
            .display()
            .to_string()
            .contains("sessions/--tmp-"));
    }

    /// dsh fixture → canonical → dsh writer → re-read with our dsh reader.
    #[test]
    fn dsh_roundtrip_preserves_conversation_and_closes_clean() {
        let home = tempfile::tempdir().expect("home");
        let target_home = tempfile::tempdir().expect("target home");
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        write_file(
            &home
                .path()
                .join(".dsh/sessions/--tmp-demo--/rt-2/session.jsonl"),
            &[
                &format!(
                    r#"{{"type":"session","version":0,"id":"rt-2","createdAt":1784272800000,"cwd":"{cwd}","delegationDepth":0}}"#
                ),
                r#"{"type":"turn/start","seq":0,"time":1784272801000,"data":{"turn":1}}"#,
                r#"{"type":"user/message","seq":1,"time":1784272801001,"data":{"id":"u1","role":"user","content":[{"type":"text","text":"port the parser"}],"source":{"kind":"user"}},"surfaceOp":"append"}"#,
                r#"{"type":"step/start","seq":2,"time":1784272801002,"data":{"turn":1,"step":1}}"#,
                r#"{"type":"assistant/message","seq":3,"time":1784272801003,"data":{"turn":1,"step":1,"message":{"id":"a1","role":"assistant","content":[{"type":"text","text":"ported"}],"source":{"kind":"model","provider":"deepseek","model":"m"}}},"surfaceOp":"append"}"#,
                r#"{"type":"tool/call","seq":4,"time":1784272801004,"data":{"turn":1,"step":1,"callId":"c1","name":"bash","arguments":"{\"command\":\"cargo test\"}"}}"#,
                r#"{"type":"tool/result","seq":5,"time":1784272801005,"data":{"turn":1,"step":1,"message":{"id":"r1","role":"user","content":[{"type":"tool-result","toolCallId":"c1","content":[{"type":"text","text":"all green"}]}],"source":{"kind":"tool","callId":"c1"}},"surfaceOp":"append"}}"#,
                r#"{"type":"assistant/message","seq":6,"time":1784272801006,"data":{"turn":1,"step":1,"message":{"id":"a2","role":"assistant","content":[{"type":"text","text":"done — all green"}],"source":{"kind":"model","provider":"deepseek","model":"m"}}},"surfaceOp":"append"}"#,
                r#"{"type":"todo/write","seq":7,"time":1784272801007,"data":{"todos":[{"content":"merge it","status":"pending"}]}}"#,
                r#"{"type":"step/end","seq":8,"time":1784272801008,"data":{"turn":1,"step":1}}"#,
                r#"{"type":"turn/end","seq":9,"time":1784272801009,"data":{"turn":1,"reason":{"kind":"completed"}}}"#,
            ],
        );
        sessions::import_from_readers(home.path(), project.path());
        let stored = sessions::load(project.path(), "rt-2").expect("stored");

        let plan =
            plan_dsh(&stored, target_home.path(), project.path(), "fresh-dsh-id").expect("plan");
        // meta entries (turn/step markers) drop; everything else is native.
        assert_eq!(plan.fidelity.dropped, 4, "{}", plan.fidelity.line());
        assert_eq!(plan.fidelity.adapted, 0, "{}", plan.fidelity.line());
        write(&plan).expect("write");
        assert!(plan.target_path.is_file());
        assert!(write(&plan).is_err(), "existing target must refuse");

        // The written log parses under our own (DSH-verified) parser: header
        // shape, contiguous seq, and a clean completed tail.
        let raw = crate::transcripts::dsh::parse_session_file(&plan.target_path).expect("raw");
        assert!(!raw.torn && !raw.truncated, "clean log expected");
        assert_eq!(raw.header["version"], 0);
        assert_eq!(raw.header["delegationDepth"], 0);
        let reread = crate::transcripts::dsh::summarize(&raw, project.path()).expect("session");
        assert_eq!(reread.objective, "port the parser");
        assert_eq!(reread.tool_events, 1);
        assert_eq!(
            reread
                .plan_state
                .iter()
                .map(|p| p.step.as_str())
                .collect::<Vec<_>>(),
            vec!["merge it"]
        );
        assert_eq!(reread.outcome, crate::transcripts::Outcome::Completed);
        // Every event carries a contiguous seq from 0.
        for (i, event) in raw.events.iter().enumerate() {
            assert_eq!(event["seq"].as_u64(), Some(i as u64), "event {i}");
        }
        // Surface events carry the mandatory surfaceOp marker.
        for event in &raw.events {
            let kind = event["type"].as_str().unwrap_or("");
            if matches!(kind, "user/message" | "assistant/message" | "tool/result") {
                assert_eq!(event["surfaceOp"].as_str(), Some("append"), "{kind}");
            }
        }
        // Exact DSH dir layout: sessions/<projectKey>/<encoded id>/session.jsonl.
        assert!(plan.target_path.ends_with(
            Path::new("sessions")
                .join(dsh_project_key(&cwd))
                .join("fresh-dsh-id")
                .join("session.jsonl")
        ));
    }

    #[test]
    fn dsh_path_encoding_matches_the_reference() {
        assert_eq!(dsh_project_key("/home/u/proj"), "--home-u-proj--");
        assert_eq!(dsh_project_key("D:\\SAAS\\App"), "--D-SAAS-App--");
        assert_eq!(dsh_project_key("/"), "--root--");
        assert_eq!(dsh_encode_segment("abc-123_X.Y"), "abc-123_X.Y");
        assert_eq!(dsh_encode_segment(".."), "~002E~002E");
        assert_eq!(dsh_encode_segment("a/b"), "a~002Fb");
        assert_eq!(dsh_encode_segment("a~b"), "a~007Eb");
    }
}
