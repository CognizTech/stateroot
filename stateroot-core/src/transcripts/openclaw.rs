//! OpenClaw transcript reader: `~/.openclaw/agents/<agent>/sessions/*.jsonl`.
//!
//! Format (docs + OpenClaw session-management reference):
//! - Line 1 header: `{type:"session", id, timestamp, cwd, …}`
//! - Entries: `{type:"message", message:{role, content}}` where role is
//!   `user` | `assistant` | `toolResult`; content is string or
//!   `[{type:"text", text}]` blocks.
//! - Also: `compaction`, `custom_message`, `custom`, `branch_summary` —
//!   extract text when known; record `LossNote` for unverified shapes.
//! - `files_touched` stays empty unless a verified tool path is present.
//!
//! Project filter: header `cwd` via `cwd_matches`. Gateway/DM sessions with
//! workspace cwd won't match a coding project — expected.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{
    clean, cwd_matches, event_timestamp, push_unique, walk_files, LossNote, Outcome, TailEntry,
    TranscriptReader, TranscriptSession,
};

const OBJECTIVE_MAX: usize = 8000;
const PROMPT_MAX: usize = 2000;
const TAIL_ENTRY_MAX: usize = 1500;
const TAIL_ENTRIES_MAX: usize = 24;
const PROGRESS_SUMMARY_MAX: usize = 6000;
const PROGRESS_SUMMARIES_MAX: usize = 8;

/// OpenClaw JSONL session reader.
pub struct OpenClawReader;

/// Session store roots: `$OPENCLAW_STATE_DIR` when set, then `~/.openclaw`.
pub(crate) fn store_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(state) = std::env::var("OPENCLAW_STATE_DIR") {
        if !state.trim().is_empty() {
            roots.push(PathBuf::from(state.trim()));
        }
    }
    roots.push(home.join(".openclaw"));
    roots
}

/// Every session file under `<root>/agents/**` (deleted sessions excluded).
pub(crate) fn session_files_in(root: &Path) -> Vec<PathBuf> {
    walk_files(&root.join("agents"), &|p| {
        p.extension().and_then(|e| e.to_str()) == Some("jsonl")
            && p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| !n.contains(".deleted."))
                .unwrap_or(false)
    })
}

/// Raw parse of one session file: the entry lines as verbatim JSON
/// (unparseable lines skipped, mirroring the summary reader).
pub(crate) fn parse_session_file(path: &Path) -> Option<Vec<Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    let events: Vec<Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if events.is_empty() {
        None
    } else {
        Some(events)
    }
}

/// OpenClaw's current transcript store: `<root>/agents/<agent>/agent/*.sqlite`.
/// Trajectory companions and lock files are not session stores.
pub(crate) fn sqlite_stores_in(root: &Path) -> Vec<PathBuf> {
    walk_files(&root.join("agents"), &|p| {
        p.extension().and_then(|e| e.to_str()) == Some("sqlite")
            && p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| !n.contains("-lock") && !n.contains("media-persistence"))
                .unwrap_or(false)
    })
}

/// One stored session per row group: `(session_id, events)` in `seq` order.
/// `transcript_events.event_json` holds the same event objects the legacy
/// JSONL files carried; compressed-only rows are skipped honestly.
pub(crate) fn sessions_from_db(path: &Path) -> Vec<(String, Vec<Value>)> {
    let Ok(db) = super::cursor::open_readonly(path) else {
        return Vec::new();
    };
    let Ok(mut stmt) = db.prepare(
        "SELECT session_id, event_json FROM transcript_events          WHERE event_json IS NOT NULL ORDER BY session_id, seq",
    ) else {
        return Vec::new();
    };
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map(|mapped| mapped.flatten().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut out: Vec<(String, Vec<Value>)> = Vec::new();
    for (session_id, json) in rows {
        let Ok(event) = serde_json::from_str::<Value>(&json) else {
            continue;
        };
        match out.last_mut() {
            Some((last_id, events)) if *last_id == session_id => events.push(event),
            _ => out.push((session_id, vec![event])),
        }
    }
    out
}

impl TranscriptReader for OpenClawReader {
    fn id(&self) -> &'static str {
        "openclaw"
    }

    fn scan(&self, home: &Path, project_dir: &Path) -> Vec<TranscriptSession> {
        let mut out = Vec::new();
        let mut seen_session_ids = std::collections::HashSet::new();
        for root in store_roots(home) {
            // Current store first: SQLite is the live source, legacy JSONL
            // files are only history and must not shadow the same session.
            for db_path in sqlite_stores_in(&root) {
                for (session_id, events) in sessions_from_db(&db_path) {
                    let source = format!("{}#{}", db_path.display(), session_id);
                    if let Some(session) =
                        summary_from_events(&events, &source, Some(&session_id), project_dir)
                    {
                        seen_session_ids.insert(session.session_id.clone());
                        out.push(session);
                    }
                }
            }
            let files = walk_files(&root.join("agents"), &|p| {
                p.extension().and_then(|e| e.to_str()) == Some("jsonl")
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| !n.contains(".deleted."))
                        .unwrap_or(false)
            });
            for file in &files {
                if let Some(session) = parse_session(file, project_dir) {
                    if seen_session_ids.insert(session.session_id.clone()) {
                        out.push(session);
                    }
                }
            }
        }
        out
    }
}

pub(crate) fn parse_session(file: &Path, project_dir: &Path) -> Option<TranscriptSession> {
    let text = std::fs::read_to_string(file).ok()?;
    let events: Vec<Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    summary_from_events(&events, &file.to_string_lossy(), None, project_dir)
}

/// Summary projection over one session's event sequence. Shared by the legacy
/// JSONL files and the current SQLite store, which persists the same event
/// objects in `transcript_events.event_json`.
pub(crate) fn summary_from_events(
    events: &[Value],
    source_path: &str,
    session_id_hint: Option<&str>,
    project_dir: &Path,
) -> Option<TranscriptSession> {
    let mut session = TranscriptSession {
        harness: "openclaw",
        source_path: source_path.to_string(),
        ..Default::default()
    };
    let mut cwd = String::new();
    let mut saw_any = false;
    let mut saw_assistant = false;
    let mut last_ts = String::new();

    for event in events {
        if let Some(ts) = event_timestamp(event).or_else(|| {
            event
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        }) {
            if session.started_at.is_empty() {
                session.started_at = ts.clone();
            }
            last_ts = ts;
        }
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match event_type {
            "session" => {
                if let Some(id) = event.get("id").and_then(|v| v.as_str()) {
                    session.session_id = id.to_string();
                }
                if let Some(value) = event.get("cwd").and_then(|v| v.as_str()) {
                    cwd = value.to_string();
                }
            }
            "message" | "custom_message" => {
                saw_any = true;
                let Some(message) = event.get("message") else {
                    continue;
                };
                let role = message.get("role").and_then(|v| v.as_str()).unwrap_or("");
                let content = extract_text(message.get("content"));
                if content.trim().is_empty() {
                    continue;
                }
                match role {
                    "user" => {
                        let content = super::codex::user_assertion(&content).unwrap_or_default();
                        if super::codex::is_injected(content) {
                            continue;
                        }
                        let cleaned = clean(content, OBJECTIVE_MAX);
                        if cleaned.is_empty() {
                            continue;
                        }
                        if session.objective.is_empty() {
                            session.objective = cleaned.clone();
                        }
                        push_unique(&mut session.user_prompts, clean(content, PROMPT_MAX));
                        push_tail(&mut session.conversation_tail, "user", &cleaned);
                    }
                    "assistant" => {
                        saw_assistant = true;
                        let cleaned = clean(&content, TAIL_ENTRY_MAX);
                        if !cleaned.is_empty() {
                            push_tail(&mut session.conversation_tail, "assistant", &cleaned);
                        }
                    }
                    "toolResult" | "tool" => {
                        session.tool_events += 1;
                        if content.to_lowercase().contains("error") {
                            push_unique(&mut session.failed_approaches, clean(&content, 800));
                        }
                    }
                    _ => {}
                }
            }
            "compaction" | "branch_summary" => {
                saw_any = true;
                let summary = event
                    .get("summary")
                    .or_else(|| event.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if summary.is_empty() {
                    session.losses.push(LossNote {
                        what: event_type.to_string(),
                        reason: "empty or encrypted by harness".into(),
                    });
                } else {
                    let cleaned = clean(summary, PROGRESS_SUMMARY_MAX);
                    if !cleaned.is_empty() {
                        session.progress_summaries.insert(0, cleaned);
                        session.progress_summaries.truncate(PROGRESS_SUMMARIES_MAX);
                    }
                }
            }
            "custom" => {
                session.losses.push(LossNote {
                    what: "custom".into(),
                    reason: "extension state excluded by design".into(),
                });
            }
            "" => {
                // Tolerant legacy: `{timestamp, message:{role,content}}` without type.
                if let Some(message) = event.get("message") {
                    saw_any = true;
                    let role = message.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    let content = extract_text(message.get("content"));
                    if role == "user" && !content.trim().is_empty() {
                        let content = super::codex::user_assertion(&content).unwrap_or_default();
                        if super::codex::is_injected(content) {
                            continue;
                        }
                        let cleaned = clean(content, OBJECTIVE_MAX);
                        if session.objective.is_empty() {
                            session.objective = cleaned.clone();
                        }
                        push_unique(&mut session.user_prompts, clean(content, PROMPT_MAX));
                        push_tail(&mut session.conversation_tail, "user", &cleaned);
                    } else if role == "assistant" && !content.trim().is_empty() {
                        saw_assistant = true;
                        push_tail(
                            &mut session.conversation_tail,
                            "assistant",
                            &clean(&content, TAIL_ENTRY_MAX),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    if session.session_id.is_empty() {
        session.session_id = session_id_hint
            .map(str::to_string)
            .or_else(|| {
                std::path::Path::new(source_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "unknown".to_string());
    }
    session.cwd = cwd.clone();
    session.ended_at = last_ts;
    if !saw_any || !cwd_matches(&cwd, project_dir) {
        return None;
    }
    session.outcome = if saw_assistant {
        Outcome::Completed
    } else {
        Outcome::Interrupted
    };
    Some(session)
}

/// Text of an openclaw message content field (bare string or text blocks;
/// shared with the canonical extractor).
pub(crate) fn extract_text_pub(content: Option<&Value>) -> String {
    extract_text(content)
}

fn extract_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => {
            let mut parts = Vec::new();
            for block in blocks {
                if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                        parts.push(t.to_string());
                    }
                } else if let Some(t) = block.as_str() {
                    parts.push(t.to_string());
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

fn push_tail(tail: &mut Vec<TailEntry>, role: &'static str, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    tail.push(TailEntry {
        role,
        text: text.chars().take(TAIL_ENTRY_MAX).collect(),
    });
    if tail.len() > TAIL_ENTRIES_MAX {
        let drop = tail.len() - TAIL_ENTRIES_MAX;
        tail.drain(0..drop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_session(dir: &Path, name: &str, lines: &[&str]) -> PathBuf {
        std::fs::create_dir_all(dir).expect("mkdir");
        let path = dir.join(name);
        std::fs::write(&path, lines.join("\n") + "\n").expect("write");
        path
    }

    #[test]
    fn openclaw_reader_parses_header_and_messages() {
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let home = tempfile::tempdir().expect("home");
        let file = write_session(
            &home.path().join(".openclaw/agents/main/sessions"),
            "sess-1.jsonl",
            &[
                &format!(
                    r#"{{"type":"session","version":1,"id":"sess-1","timestamp":"2026-07-10T09:00:00Z","cwd":"{cwd}"}}"#
                ),
                r#"{"type":"message","id":"m1","message":{"role":"user","content":[{"type":"text","text":"summarize the inbox"}]}}"#,
                r#"{"type":"message","id":"m2","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
            ],
        );
        let session = parse_session(&file, project.path()).expect("session");
        assert_eq!(session.session_id, "sess-1");
        assert_eq!(session.harness, "openclaw");
        assert!(session.objective.contains("summarize the inbox"));
        assert_eq!(session.outcome, Outcome::Completed);
        assert_eq!(session.conversation_tail.len(), 2);
    }

    #[test]
    fn openclaw_reader_filters_cwd() {
        let project = tempfile::tempdir().expect("project");
        let home = tempfile::tempdir().expect("home");
        let file = write_session(
            &home.path().join(".openclaw/agents/main/sessions"),
            "other.jsonl",
            &[
                r#"{"type":"session","id":"other","timestamp":"2026-07-10T09:00:00Z","cwd":"/unrelated"}"#,
                r#"{"type":"message","message":{"role":"user","content":"hi"}}"#,
            ],
        );
        assert!(parse_session(&file, project.path()).is_none());
    }
    #[test]
    fn openclaw_sqlite_store_reads_current_sessions() {
        let project = tempfile::tempdir().expect("project");
        let home = tempfile::tempdir().expect("home");
        let db_dir = home.path().join(".openclaw/agents/main/agent");
        std::fs::create_dir_all(&db_dir).expect("db dir");
        let db_path = db_dir.join("openclaw-agent.sqlite");
        let cwd = crate::transcripts::path_for_json(project.path());
        {
            let db = rusqlite::Connection::open(&db_path).expect("db");
            db.execute_batch(
                "CREATE TABLE transcript_events (session_id TEXT, seq INTEGER, event_json TEXT);",
            )
            .expect("schema");
            let header = format!(
                r#"{{"type":"session","version":3,"id":"sess-sqlite","timestamp":"2026-11-23T08:00:00Z","cwd":"{cwd}"}}"#
            );
            let user = r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"port the watcher"}]}}"#.to_string();
            let assistant = r#"{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"ported"}]}}"#.to_string();
            for (seq, event) in [(1_i64, header), (2, user), (3, assistant)] {
                db.execute(
                    "INSERT INTO transcript_events (session_id, seq, event_json) VALUES (?1, ?2, ?3)",
                    rusqlite::params!["sess-sqlite", seq, event],
                )
                .expect("insert");
            }
        }

        let stores = sqlite_stores_in(&home.path().join(".openclaw"));
        assert_eq!(stores.len(), 1, "one store discovered: {stores:?}");
        let sessions = sessions_from_db(&stores[0]);
        assert_eq!(sessions.len(), 1);
        let (id, events) = &sessions[0];
        assert_eq!(id, "sess-sqlite");
        assert_eq!(events.len(), 3);
        let summary = summary_from_events(events, "db#sess-sqlite", Some(id), project.path())
            .expect("summary");
        assert_eq!(summary.session_id, "sess-sqlite");
        assert!(summary.objective.contains("port the watcher"));
        assert_eq!(summary.outcome, Outcome::Completed);
    }
}
