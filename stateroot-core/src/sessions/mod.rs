//! Canonical session store — `stateroot.session.v1`.
//!
//! Sessions belong to StateRoot: one normalized JSONL timeline per harness
//! session, close to the strings. Line 1 is the header; every following line
//! is one full-fidelity canonical entry (NO content caps — disk is cheap;
//! the display paths cap downstream). Unmapped native types become
//! `type:"meta"` with `native_type` set — nothing silently vanishes.
//!
//! Canonical sessions live under `.stateroot/local/sessions/` — never pinned
//! into roots (same rule as `local/memory.sqlite`). Promotion into synced
//! state is a later, retention-tiered decision.

pub mod transfer;

mod extract;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::harness_install::paths;
use crate::local_store::{self, now_rfc3339};
use crate::transcripts::{self, claude, codex, cursor, dsh, hermes, kimi, openclaw, pi};

/// Schema tag on the header line.
pub const SCHEMA_SESSION_V1: &str = "stateroot.session.v1";
/// Canonical sessions dir, relative to `.stateroot/`.
pub const SESSIONS_REL: &str = "local/sessions";

/// One full-fidelity canonical timeline entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CanonicalEntry {
    /// Position in the canonical timeline (0-based, assigned on write).
    pub seq: usize,
    /// `message` | `tool_call` | `tool_result` | `compaction` | `plan` | `meta`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Native entry id (pi) or tool-call correlation id (tool_call).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Tree parent (pi message entries) or the call id (tool_result).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Source-native timestamp string (pi ISO; dsh epoch-ms as a string).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
    /// `user` | `assistant` on message entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Verbatim text (never capped); small native payloads keep their JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Tool name on tool_call / tool_result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The native type a `meta` entry (or an adapted entry) came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_type: Option<String>,
}

impl CanonicalEntry {
    fn new(kind: &str) -> Self {
        CanonicalEntry {
            kind: kind.to_string(),
            ..Default::default()
        }
    }
}

/// A canonical session in memory (pre-write).
#[derive(Debug, Clone)]
pub struct CanonicalSession {
    /// Source harness id (`pi` | `dsh`).
    pub harness: String,
    /// Harness-native session id.
    pub session_id: String,
    /// Working directory recorded by the harness.
    pub cwd: String,
    /// The native file this session was imported from.
    pub source_path: String,
    /// The full timeline (seq assigned on write).
    pub entries: Vec<CanonicalEntry>,
}

/// A canonical session read back from the store.
#[derive(Debug, Clone)]
pub struct StoredSession {
    /// Store file path.
    pub path: PathBuf,
    /// Header fields.
    pub harness: String,
    /// Header fields.
    pub session_id: String,
    /// Header fields.
    pub cwd: String,
    /// Header fields.
    pub imported_at: String,
    /// Header fields.
    pub source_path: String,
    /// The full timeline.
    pub entries: Vec<CanonicalEntry>,
}

/// The canonical store directory for one project.
pub fn store_dir(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(SESSIONS_REL)
}

/// Filename-safe session id (pi ids are uuids; dsh ids are branded strings —
/// neutralize separators before filesystem use, like DSH's own encoder).
fn sanitize_id(id: &str) -> String {
    let mut out = String::new();
    for ch in id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    out
}

/// Canonicalize one parsed Pi session file (full fidelity, file order —
/// branches stay in the timeline with their native id/parentId links).
fn canonical_from_pi(raw: &pi::RawSession, source_path: &Path) -> Option<CanonicalSession> {
    let header = &raw.header;
    let session_id = header.get("id").and_then(|v| v.as_str())?.to_string();
    let cwd = header
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut entries = Vec::new();
    for entry in &raw.entries {
        let kind = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let mut base = CanonicalEntry::new("meta");
        base.id = entry.get("id").and_then(|v| v.as_str()).map(str::to_string);
        base.parent_id = entry
            .get("parentId")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        base.ts = entry
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        match kind {
            "message" => {
                let message = entry.get("message").cloned().unwrap_or(Value::Null);
                let role = message.get("role").and_then(|v| v.as_str()).unwrap_or("");
                match role {
                    "user" | "assistant" => {
                        let text = pi::message_text(&message);
                        if !text.is_empty() {
                            let mut e = base.clone();
                            e.kind = "message".into();
                            e.role = Some(role.to_string());
                            e.content = Some(text);
                            entries.push(e);
                        }
                        if role == "assistant" {
                            // Thinking blocks are model content — preserved
                            // as meta, same as dsh reasoning.
                            if let Some(Value::Array(blocks)) = message.get("content") {
                                for block in blocks {
                                    if block.get("type").and_then(|v| v.as_str())
                                        == Some("thinking")
                                    {
                                        let mut r = CanonicalEntry::new("meta");
                                        r.id = base.id.clone();
                                        r.parent_id = base.id.clone();
                                        r.ts = base.ts.clone();
                                        r.native_type = Some("thinking".into());
                                        r.content = block
                                            .get("thinking")
                                            .and_then(|v| v.as_str())
                                            .map(str::to_string);
                                        entries.push(r);
                                    }
                                }
                            }
                            for (call_id, name, arguments) in pi::tool_calls(&message) {
                                let mut call = CanonicalEntry::new("tool_call");
                                call.id = Some(call_id);
                                call.parent_id = base.id.clone();
                                call.ts = base.ts.clone();
                                call.name = Some(name);
                                call.content = Some(arguments);
                                entries.push(call);
                            }
                        }
                    }
                    "toolResult" => {
                        let mut e = base;
                        e.kind = "tool_result".into();
                        // The call correlation rides parent_id (writers
                        // re-chain the tree spine themselves).
                        e.parent_id = message
                            .get("toolCallId")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        e.name = message
                            .get("toolName")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        e.content = Some(pi::message_text(&message));
                        entries.push(e);
                    }
                    _ => {
                        base.native_type = Some(format!("message role `{role}`"));
                        entries.push(base);
                    }
                }
            }
            "compaction" | "branch_summary" => {
                let mut e = base;
                e.kind = "compaction".into();
                e.content = Some(
                    entry
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                );
                if kind == "branch_summary" {
                    e.native_type = Some("branch_summary".into());
                }
                entries.push(e);
            }
            "model_change" => {
                base.native_type = Some("model_change".into());
                let provider = entry.get("provider").and_then(|v| v.as_str()).unwrap_or("");
                let model = entry.get("modelId").and_then(|v| v.as_str()).unwrap_or("");
                base.content = Some(format!("{provider}/{model}"));
                entries.push(base);
            }
            "custom_message" => {
                base.native_type = Some("custom_message".into());
                let content = entry.get("content").cloned().unwrap_or(Value::Null);
                base.content = Some(match &content {
                    Value::String(s) => s.clone(),
                    other => pi::message_text(&json!({ "content": other })),
                });
                entries.push(base);
            }
            "" => {}
            other => {
                base.native_type = Some(other.to_string());
                entries.push(base);
            }
        }
    }
    Some(CanonicalSession {
        harness: "pi".into(),
        session_id,
        cwd,
        source_path: source_path.display().to_string(),
        entries,
    })
}

/// Canonicalize one parsed DSH session log.
fn canonical_from_dsh(raw: &dsh::RawSession, source_path: &Path) -> Option<CanonicalSession> {
    let header = &raw.header;
    let session_id = header.get("id").and_then(|v| v.as_str())?.to_string();
    let cwd = header
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut entries = Vec::new();
    for event in &raw.events {
        let kind = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let data = event.get("data").cloned().unwrap_or(Value::Null);
        let ts = event
            .get("time")
            .and_then(|v| v.as_i64())
            .map(|ms| ms.to_string());
        let mut base = CanonicalEntry::new("meta");
        base.ts = ts;
        match kind {
            "user/message" => {
                let mut e = base;
                e.kind = "message".into();
                e.role = Some("user".into());
                e.id = data.get("id").and_then(|v| v.as_str()).map(str::to_string);
                e.content = Some(dsh::blocks_text(data.get("content")));
                let source = data.pointer("/source/kind").and_then(|v| v.as_str());
                if source != Some("user") {
                    e.native_type =
                        Some(format!("user/message source `{}`", source.unwrap_or("?")));
                }
                entries.push(e);
            }
            "assistant/message" => {
                let message = data.get("message").cloned().unwrap_or(Value::Null);
                let content = message.get("content").cloned().unwrap_or(Value::Null);
                let text = dsh::blocks_text(Some(&content));
                if !text.is_empty() {
                    let mut e = base.clone();
                    e.kind = "message".into();
                    e.role = Some("assistant".into());
                    e.id = message
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    e.content = Some(text);
                    if data.get("interrupted").and_then(|v| v.as_bool()) == Some(true) {
                        e.native_type = Some("interrupted".into());
                    }
                    entries.push(e);
                }
                // Reasoning blocks are model content too — preserved as meta.
                if let Some(Value::Array(blocks)) = message.get("content") {
                    for block in blocks {
                        if block.get("type").and_then(|v| v.as_str()) == Some("reasoning") {
                            let mut r = base.clone();
                            r.native_type = Some("assistant_reasoning".into());
                            r.content = block
                                .get("text")
                                .and_then(|v| v.as_str())
                                .map(str::to_string);
                            entries.push(r);
                        }
                    }
                }
            }
            "tool/call" => {
                let mut e = base;
                e.kind = "tool_call".into();
                e.id = data
                    .get("callId")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                e.name = data
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                e.content = data
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                entries.push(e);
            }
            "tool/result" => {
                let mut e = base;
                e.kind = "tool_result".into();
                e.parent_id = data
                    .pointer("/message/source/callId")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                e.content = Some(dsh::blocks_text(data.pointer("/message/content/0/content")));
                entries.push(e);
            }
            "todo/write" => {
                let mut lines = Vec::new();
                if let Some(todos) = data.get("todos").and_then(|v| v.as_array()) {
                    for todo in todos {
                        let content = todo.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        let status = todo.get("status").and_then(|v| v.as_str()).unwrap_or("");
                        if !content.is_empty() {
                            lines.push(format!("- [{status}] {content}"));
                        }
                    }
                }
                if !lines.is_empty() {
                    let mut e = base;
                    e.kind = "plan".into();
                    e.content = Some(lines.join("\n"));
                    entries.push(e);
                }
            }
            "" | "assistant/chunk" => {}
            other => {
                // turn/start, turn/end, step markers, request/*, end-seed, …
                base.native_type = Some(other.to_string());
                base.content = Some(serde_json::to_string(&data).unwrap_or_default());
                entries.push(base);
            }
        }
    }
    if raw.skipped_chunks > 0 {
        let mut e = CanonicalEntry::new("meta");
        e.native_type = Some("assistant/chunk".into());
        e.content = Some(format!(
            "{} stream chunk events omitted (assembled text preserved in assistant/message)",
            raw.skipped_chunks
        ));
        entries.push(e);
    }
    for (flag, native_type, note) in [
        (
            raw.torn,
            "torn_tail",
            "source log ended mid-record (crash cut)",
        ),
        (
            raw.truncated,
            "truncated_log",
            "seq gap or corrupt line — entries after it were dropped at import",
        ),
    ] {
        if flag {
            let mut e = CanonicalEntry::new("meta");
            e.native_type = Some(native_type.into());
            e.content = Some(note.into());
            entries.push(e);
        }
    }
    Some(CanonicalSession {
        harness: "dsh".into(),
        session_id,
        cwd,
        source_path: source_path.display().to_string(),
        entries,
    })
}

/// What one canonical sync did.
#[derive(Debug, Default)]
pub struct SyncReport {
    /// Sessions written into the store.
    pub written: usize,
    /// Per-harness counts.
    pub per_harness: BTreeMap<String, usize>,
    /// DSH `.jsonl.zstd` artifacts skipped (no zstd in the dependency tree).
    pub skipped_zstd: usize,
}

/// Import every pi/DSH session belonging to `project_dir` into the canonical
/// store. Idempotent: each session file is rewritten whole.
pub fn import_from_readers(home: &Path, project_dir: &Path) -> SyncReport {
    import_from_readers_filtered(home, project_dir, None)
}

/// [`import_from_readers`] restricted to one harness (`pi` | `dsh`).
pub fn import_from_readers_filtered(
    home: &Path,
    project_dir: &Path,
    harness: Option<&str>,
) -> SyncReport {
    let mut report = SyncReport::default();
    let import_one = |session: CanonicalSession, report: &mut SyncReport| {
        if write_session(project_dir, &session).is_ok() {
            report.written += 1;
            *report
                .per_harness
                .entry(session.harness.clone())
                .or_insert(0) += 1;
        }
    };

    if harness.is_none_or(|h| h == "pi") {
        let pi_root = paths::pi_agent_root(home).join("sessions");
        for file in transcripts::walk_files(&pi_root, &|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(".jsonl"))
                .unwrap_or(false)
        }) {
            let Some(raw) = pi::parse_session_file(&file) else {
                continue;
            };
            let cwd = raw.header.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
            if !transcripts::cwd_matches(cwd, project_dir) {
                continue;
            }
            if let Some(session) = canonical_from_pi(&raw, &file) {
                import_one(session, &mut report);
            }
        }
    }

    if harness.is_none_or(|h| h == "dsh") {
        let (plain, zstd) = dsh::session_files(home);
        report.skipped_zstd = zstd.len();
        for file in &plain {
            let Some(raw) = dsh::parse_session_file(file) else {
                continue;
            };
            let cwd = raw.header.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
            if !transcripts::cwd_matches(cwd, project_dir) {
                continue;
            }
            if let Some(session) = canonical_from_dsh(&raw, file) {
                import_one(session, &mut report);
            }
        }
    }

    if harness.is_none_or(|h| h == "claude") {
        for file in claude::session_files(home) {
            let Some(events) = claude::parse_session_file(&file) else {
                continue;
            };
            if let Some(session) = extract::canonical_from_claude(&events, &file, project_dir) {
                import_one(session, &mut report);
            }
        }
    }

    if harness.is_none_or(|h| h == "codex") {
        for file in codex::session_files(home) {
            let Some((meta, events)) = codex::parse_session_file(&file) else {
                continue;
            };
            if let Some(session) = extract::canonical_from_codex(&meta, &events, &file, project_dir)
            {
                import_one(session, &mut report);
            }
        }
    }

    if harness.is_none_or(|h| h == "kimi") {
        let index = kimi::read_session_index(home);
        for file in kimi::session_files(home) {
            let Some((meta, records)) = kimi::parse_wire_raw(&file) else {
                continue;
            };
            let (session_id, agent) = kimi::ids_for(&file);
            let Some(cwd) = index.get(&session_id) else {
                continue;
            };
            // One canonical session per agent wire: `main` keeps the bare
            // session id; other agents suffix it (collision-free store names).
            let canonical_id = if agent == "main" {
                session_id.clone()
            } else {
                format!("{session_id}-{agent}")
            };
            if let Some(session) = extract::canonical_from_kimi(
                &meta,
                &records,
                &canonical_id,
                cwd,
                &file,
                project_dir,
            ) {
                import_one(session, &mut report);
            }
        }
    }

    if harness.is_none_or(|h| h == "openclaw") {
        for root in openclaw::store_roots(home) {
            for file in openclaw::session_files_in(&root) {
                let Some(events) = openclaw::parse_session_file(&file) else {
                    continue;
                };
                if let Some(session) = extract::canonical_from_openclaw(&events, &file, project_dir)
                {
                    import_one(session, &mut report);
                }
            }
        }
    }

    if harness.is_none_or(|h| h == "cursor") {
        for db_path in cursor::db_candidates(home) {
            let Ok(db) = cursor::open_immutable(&db_path) else {
                continue;
            };
            for raw in cursor::raw_sessions(&db, project_dir) {
                import_one(extract::canonical_from_cursor(&raw, &db_path), &mut report);
            }
        }
    }

    if harness.is_none_or(|h| h == "hermes") {
        for db_path in hermes::db_candidates(home) {
            let Ok(db) = cursor::open_immutable(&db_path) else {
                continue;
            };
            for raw in hermes::raw_sessions(&db, project_dir) {
                import_one(extract::canonical_from_hermes(&raw, &db_path), &mut report);
            }
        }
    }
    report
}

/// Write one canonical session (rewrite whole — idempotent re-import).
pub fn write_session(project_dir: &Path, session: &CanonicalSession) -> std::io::Result<PathBuf> {
    let dir = store_dir(project_dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "{}-{}.jsonl",
        session.harness,
        sanitize_id(&session.session_id)
    ));
    let header = json!({
        "schema_version": SCHEMA_SESSION_V1,
        "harness": session.harness,
        "session_id": session.session_id,
        "cwd": session.cwd,
        "imported_at": now_rfc3339(),
        "source_path": session.source_path,
        "entry_count": session.entries.len(),
    });
    let mut body = serde_json::to_string(&header)?;
    body.push('\n');
    for (seq, entry) in session.entries.iter().enumerate() {
        let mut entry = entry.clone();
        entry.seq = seq;
        body.push_str(&serde_json::to_string(&entry)?);
        body.push('\n');
    }
    std::fs::write(&path, body)?;
    Ok(path)
}

/// Parse one canonical store file.
fn read_store_file(path: &Path) -> Option<StoredSession> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header: Value = serde_json::from_str(lines.next()?).ok()?;
    if header.get("schema_version").and_then(|v| v.as_str()) != Some(SCHEMA_SESSION_V1) {
        return None;
    }
    let mut entries = Vec::new();
    for line in lines {
        if let Ok(entry) = serde_json::from_str::<CanonicalEntry>(line) {
            entries.push(entry);
        }
    }
    Some(StoredSession {
        path: path.to_path_buf(),
        harness: header
            .get("harness")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        session_id: header
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        cwd: header
            .get("cwd")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        imported_at: header
            .get("imported_at")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        source_path: header
            .get("source_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        entries,
    })
}

/// Every canonical session in the store, sorted by harness then id.
pub fn list(project_dir: &Path) -> Vec<StoredSession> {
    let mut out: Vec<StoredSession> = transcripts::walk_files(&store_dir(project_dir), &|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.ends_with(".jsonl"))
            .unwrap_or(false)
    })
    .iter()
    .filter_map(|p| read_store_file(p))
    .collect();
    out.sort_by(|a, b| (&a.harness, &a.session_id).cmp(&(&b.harness, &b.session_id)));
    out
}

/// Load one canonical session by id (exact, or a unique prefix).
pub fn load(project_dir: &Path, id: &str) -> Option<StoredSession> {
    resolve(project_dir, id).ok()
}

/// [`load`] with a truthful error: unknown id vs ambiguous prefix (candidate
/// ids listed, capped at five).
pub fn resolve(project_dir: &Path, id: &str) -> Result<StoredSession, String> {
    let all = list(project_dir);
    if let Some(exact) = all.iter().find(|s| s.session_id == id) {
        return Ok(exact.clone());
    }
    let matches: Vec<&StoredSession> = all
        .iter()
        .filter(|s| s.session_id.starts_with(id))
        .collect();
    match matches.len() {
        0 => Err(format!(
            "no canonical session matches `{id}` — run `stateroot session list`"
        )),
        1 => Ok(matches[0].clone()),
        n => {
            let mut ids: Vec<&str> = matches.iter().map(|s| s.session_id.as_str()).collect();
            ids.sort_unstable();
            let preview = ids
                .iter()
                .take(5)
                .map(|i| format!("  {i}"))
                .collect::<Vec<_>>()
                .join("\n");
            let more = if n > 5 {
                format!("\n  … and {} more", n - 5)
            } else {
                String::new()
            };
            Err(format!(
                "`{id}` is ambiguous — {n} canonical sessions match:\n{preview}{more}"
            ))
        }
    }
}

/// Display-oriented summary of one stored session (list/show): span from the
/// first/last entry timestamps, plus a tail-shape outcome heuristic.
pub fn summarize_stored(session: &StoredSession) -> (String, String, &'static str) {
    let first_ts = session
        .entries
        .iter()
        .find_map(|e| e.ts.clone())
        .unwrap_or_default();
    let last_ts = session
        .entries
        .iter()
        .rev()
        .find_map(|e| e.ts.clone())
        .unwrap_or_default();
    let outcome = match session.entries.last() {
        Some(e) if e.kind == "message" && e.role.as_deref() == Some("assistant") => "completed",
        Some(e) if e.kind == "tool_call" => "interrupted",
        Some(_) => "unknown",
        None => "empty",
    };
    (first_ts, last_ts, outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn pi_canonical_roundtrip_through_the_store() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        write_file(
            &home
                .path()
                .join(".pi/agent/sessions/--tmp-demo--/t_ses-9.jsonl"),
            &[
                &format!(
                    r#"{{"type":"session","version":3,"id":"ses-9","timestamp":"2026-08-20T10:00:00.000Z","cwd":"{cwd}"}}"#
                ),
                r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-08-20T10:00:01.000Z","message":{"role":"user","content":"ship the exporter","timestamp":1}}"#,
                r#"{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-08-20T10:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"on it"},{"type":"toolCall","id":"c1","name":"bash","arguments":{"command":"cargo build"}}],"timestamp":2}}"#,
                r#"{"type":"message","id":"m3","parentId":"m2","timestamp":"2026-08-20T10:00:03.000Z","message":{"role":"toolResult","toolCallId":"c1","toolName":"bash","content":[{"type":"text","text":"build ok"}],"isError":false,"timestamp":3}}"#,
                r#"{"type":"model_change","id":"mc","parentId":"m3","timestamp":"2026-08-20T10:00:04.000Z","provider":"deepseek","modelId":"deepseek-v4-flash"}"#,
            ],
        );
        let report = import_from_readers(home.path(), project.path());
        assert_eq!(report.written, 1);
        assert_eq!(report.per_harness.get("pi"), Some(&1));

        let stored = load(project.path(), "ses-9").expect("stored");
        assert_eq!(stored.harness, "pi");
        let kinds: Vec<&str> = stored.entries.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            ["message", "message", "tool_call", "tool_result", "meta"]
        );
        assert_eq!(stored.entries[0].role.as_deref(), Some("user"));
        assert_eq!(
            stored.entries[0].content.as_deref(),
            Some("ship the exporter")
        );
        assert_eq!(stored.entries[2].name.as_deref(), Some("bash"));
        // tool_result correlates to its call via parent_id.
        assert_eq!(stored.entries[3].parent_id.as_deref(), Some("c1"));
        assert_eq!(
            stored.entries[4].native_type.as_deref(),
            Some("model_change")
        );
        // seq assigned contiguously on write.
        assert_eq!(stored.entries[4].seq, 4);

        // Re-import is idempotent (rewrite whole, no duplication).
        let report = import_from_readers(home.path(), project.path());
        assert_eq!(report.written, 1);
        assert_eq!(
            load(project.path(), "ses-9").expect("stored").entries.len(),
            5
        );
    }

    #[test]
    fn dsh_canonical_maps_events_and_notes_losses() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        write_file(
            &home
                .path()
                .join(".dsh/sessions/--tmp-demo--/d-9/session.jsonl"),
            &[
                &format!(
                    r#"{{"type":"session","version":0,"id":"d-9","createdAt":1784272800000,"cwd":"{cwd}","delegationDepth":0}}"#
                ),
                r#"{"type":"turn/start","seq":0,"time":1784272801000,"data":{"turn":1}}"#,
                r#"{"type":"user/message","seq":1,"time":1784272801001,"data":{"id":"u1","role":"user","content":[{"type":"text","text":"build it"}],"source":{"kind":"user"}},"surfaceOp":"append"}"#,
                r#"{"type":"step/start","seq":2,"time":1784272801002,"data":{"turn":1,"step":1}}"#,
                r#"{"type":"assistant/message","seq":3,"time":1784272801003,"data":{"turn":1,"step":1,"message":{"id":"a1","role":"assistant","content":[{"type":"reasoning","text":"thinking"},{"type":"text","text":"done"}],"source":{"kind":"model","provider":"deepseek","model":"m"}}},"surfaceOp":"append"}"#,
                r#"{"type":"tool/call","seq":4,"time":1784272801004,"data":{"turn":1,"step":1,"callId":"c1","name":"bash","arguments":"{\"command\":\"ls\"}"}}"#,
                r#"{"type":"tool/result","seq":5,"time":1784272801005,"data":{"turn":1,"step":1,"message":{"id":"r1","role":"user","content":[{"type":"tool-result","toolCallId":"c1","content":[{"type":"text","text":"ok"}]}],"source":{"kind":"tool","callId":"c1"}},"surfaceOp":"append"}}"#,
                r#"{"type":"todo/write","seq":6,"time":1784272801006,"data":{"todos":[{"content":"ship","status":"pending"}]}}"#,
                r#"{"type":"step/end","seq":7,"time":1784272801007,"data":{"turn":1,"step":1}}"#,
                r#"{"type":"turn/end","seq":8,"time":1784272801008,"data":{"turn":1,"reason":{"kind":"completed"}}}"#,
            ],
        );
        let report = import_from_readers(home.path(), project.path());
        assert_eq!(report.written, 1);

        let stored = load(project.path(), "d-9").expect("stored");
        let kinds: Vec<&str> = stored.entries.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "meta", // turn/start
                "message",
                "meta", // step/start
                "message",
                "meta", // assistant reasoning preserved
                "tool_call",
                "tool_result",
                "plan",
                "meta", // step/end
                "meta", // turn/end
            ]
        );
        assert_eq!(stored.entries[1].role.as_deref(), Some("user"));
        assert_eq!(stored.entries[3].content.as_deref(), Some("done"));
        assert_eq!(
            stored.entries[4].native_type.as_deref(),
            Some("assistant_reasoning")
        );
        assert_eq!(
            stored.entries[5].content.as_deref(),
            Some("{\"command\":\"ls\"}")
        );
        assert_eq!(stored.entries[6].parent_id.as_deref(), Some("c1"));
        assert_eq!(
            stored.entries[7].content.as_deref(),
            Some("- [pending] ship")
        );
        // epoch-ms timestamps stay native strings.
        assert_eq!(stored.entries[1].ts.as_deref(), Some("1784272801001"));

        let (first, last, outcome) = summarize_stored(&stored);
        assert_eq!(first, "1784272801000");
        assert_eq!(last, "1784272801008");
        assert_eq!(outcome, "unknown"); // tail is a meta entry
    }

    #[test]
    fn zstd_artifacts_are_counted_and_skipped() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        write_file(
            &home.path().join(".dsh/sessions/p/x/session.jsonl.zstd"),
            &["not really zstd — we must never try to read it"],
        );
        let report = import_from_readers(home.path(), project.path());
        assert_eq!(report.skipped_zstd, 1);
        assert_eq!(report.written, 0);
    }
}
