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

impl TranscriptReader for OpenClawReader {
    fn id(&self) -> &'static str {
        "openclaw"
    }

    fn scan(&self, home: &Path, project_dir: &Path) -> Vec<TranscriptSession> {
        let mut roots = Vec::new();
        if let Ok(state) = std::env::var("OPENCLAW_STATE_DIR") {
            if !state.trim().is_empty() {
                roots.push(PathBuf::from(state.trim()));
            }
        }
        roots.push(home.join(".openclaw"));
        let mut out = Vec::new();
        for root in roots {
            let agents = root.join("agents");
            let files = walk_files(&agents, &|p| {
                p.extension().and_then(|e| e.to_str()) == Some("jsonl")
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| !n.contains(".deleted."))
                        .unwrap_or(false)
            });
            for file in &files {
                if let Some(session) = parse_session(file, project_dir) {
                    out.push(session);
                }
            }
        }
        out
    }
}

fn parse_session(file: &Path, project_dir: &Path) -> Option<TranscriptSession> {
    let text = std::fs::read_to_string(file).ok()?;
    let mut session = TranscriptSession {
        harness: "openclaw",
        ..Default::default()
    };
    let mut cwd = String::new();
    let mut saw_any = false;
    let mut saw_assistant = false;
    let mut last_ts = String::new();

    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(ts) = event_timestamp(&event).or_else(|| {
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
                        let cleaned = clean(&content, OBJECTIVE_MAX);
                        if cleaned.is_empty() {
                            continue;
                        }
                        if session.objective.is_empty() {
                            session.objective = cleaned.clone();
                        }
                        push_unique(&mut session.user_prompts, clean(&content, PROMPT_MAX));
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
                        let cleaned = clean(&content, OBJECTIVE_MAX);
                        if session.objective.is_empty() {
                            session.objective = cleaned.clone();
                        }
                        push_unique(&mut session.user_prompts, clean(&content, PROMPT_MAX));
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
        session.session_id = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
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
}
