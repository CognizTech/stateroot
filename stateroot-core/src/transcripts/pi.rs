//! Pi transcript reader: `$PI_CODING_AGENT_DIR/sessions/<cwd-encoded>/<ts>_<id>.jsonl`
//! (default `~/.pi/agent/sessions/...`).
//!
//! Format (verified against pi's `packages/coding-agent/src/core/
//! session-manager.ts`, a read-only reference): line 1 is the SessionHeader
//! `{type:"session", version:3, id, timestamp, cwd, parentSession?}`; entries
//! form a TREE via `{type, id, parentId, timestamp}`. `message` entries carry
//! an AgentMessage (`role` user/assistant/toolResult; content a bare string
//! or typed blocks — text/thinking/toolCall). `compaction` and
//! `branch_summary` carry summaries; `model_change`, `thinking_level_change`,
//! `label`, `session_info`, `custom`, `custom_message` are harness state.
//! The live conversation is the parentId chain walked back from the last
//! entry — pi's own loader defaults the leaf the same way.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use super::{
    clean, cwd_matches, push_unique, shell_write_targets, walk_files, LossNote, Outcome,
    TranscriptReader, TranscriptSession,
};
use crate::harness_install::paths;

/// Rich-extraction caps (same inclusion bar as the other readers).
const OBJECTIVE_MAX: usize = 8000;
const PROMPT_MAX: usize = 2000;
const FAILURE_MAX: usize = 800;
const PROGRESS_SUMMARY_MAX: usize = 6000;
const TAIL_ENTRY_MAX: usize = 1500;

/// Pi session reader.
pub struct PiReader;

/// A raw parsed Pi session file: header line plus entry lines as verbatim
/// JSON (the canonical full-tier import in `sessions` consumes this too).
pub(crate) struct RawSession {
    pub(crate) header: Value,
    pub(crate) entries: Vec<Value>,
}

/// Parse a Pi session JSONL file — lenient like pi's own loader (malformed
/// lines are skipped). `None` when line 1 is not a session header.
pub(crate) fn parse_session_file(path: &Path) -> Option<RawSession> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header: Value = serde_json::from_str(lines.next()?).ok()?;
    if header.get("type").and_then(|v| v.as_str()) != Some("session") {
        return None;
    }
    let mut entries = Vec::new();
    for line in lines {
        if let Ok(entry) = serde_json::from_str::<Value>(line) {
            entries.push(entry);
        }
    }
    Some(RawSession { header, entries })
}

/// The current branch, oldest first: pi derives the live context by walking
/// `parentId` from the leaf (default leaf = last entry in file order);
/// unknown parents and cycles terminate the walk.
pub(crate) fn linearize(entries: &[Value]) -> Vec<&Value> {
    let by_id: HashMap<&str, &Value> = entries
        .iter()
        .filter_map(|e| e.get("id").and_then(|v| v.as_str()).map(|id| (id, e)))
        .collect();
    let mut path = Vec::new();
    let mut current = entries.last();
    while let Some(entry) = current {
        path.push(entry);
        if path.len() > entries.len() {
            break; // parentId cycle — malformed session, keep what walked
        }
        current = entry
            .get("parentId")
            .and_then(|v| v.as_str())
            .and_then(|pid| by_id.get(pid))
            .copied();
    }
    path.reverse();
    path
}

/// Verbatim text of a pi message: user content may be a bare string or
/// text/image blocks; assistant content chains text blocks. Thinking,
/// toolCall, and image blocks carry no conversation text here.
pub(crate) fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(|v| v.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The toolCall blocks of an assistant message as `(id, name, arguments)`
/// (arguments re-serialized — pi stores them as an object, not a string).
pub(crate) fn tool_calls(message: &Value) -> Vec<(String, String, String)> {
    let mut calls = Vec::new();
    if let Some(Value::Array(blocks)) = message.get("content") {
        for block in blocks {
            if block.get("type").and_then(|v| v.as_str()) != Some("toolCall") {
                continue;
            }
            let id = block
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let name = block
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let arguments = block
                .get("arguments")
                .map(|a| serde_json::to_string(a).unwrap_or_default())
                .unwrap_or_default();
            calls.push((id, name, arguments));
        }
    }
    calls
}

/// File-write extraction from one tool call's arguments object (write/edit
/// path keys, shell command targets).
fn extract_files(arguments: &Value, session: &mut TranscriptSession) {
    for key in ["path", "file_path", "filename"] {
        if let Some(path) = arguments.get(key).and_then(|v| v.as_str()) {
            push_unique(&mut session.files_touched, clean(path, 300));
        }
    }
    let command = arguments
        .get("command")
        .or_else(|| arguments.get("cmd"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    for target in shell_write_targets(command) {
        push_unique(&mut session.files_touched, clean(&target, 300));
    }
}

/// Map a parsed Pi session into the summary tier. `None` when the header's
/// cwd does not belong to `project_dir`.
pub(crate) fn summarize(raw: &RawSession, project_dir: &Path) -> Option<TranscriptSession> {
    let header = &raw.header;
    let cwd = header
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if !cwd_matches(&cwd, project_dir) {
        return None;
    }
    let session_id = header
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let mut session = TranscriptSession {
        harness: "pi",
        session_id,
        cwd,
        started_at: header
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        ..Default::default()
    };

    let path = linearize(&raw.entries);
    let mut noted: Vec<&str> = Vec::new();
    let mut last_ts = String::new();
    // Summaries are collected file-wide: a branch_summary parents into an
    // ABANDONED branch by design, so the live path never carries it — yet it
    // is exactly the failed-approach trail the digest wants.
    for entry in &raw.entries {
        if let Some(ts) = entry.get("timestamp").and_then(|v| v.as_str()) {
            last_ts = ts.to_string();
        }
        let kind = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if kind == "compaction" || kind == "branch_summary" {
            let summary = entry.get("summary").and_then(|v| v.as_str()).unwrap_or("");
            let summary = clean(summary, PROGRESS_SUMMARY_MAX);
            if !summary.is_empty() {
                // progress_summaries are NEWEST FIRST — insert at the front.
                session.progress_summaries.insert(0, summary);
            }
        }
    }
    let mut tail_assistant_text = false;
    let mut saw_conversation = false;
    for entry in &path {
        let kind = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "message" => {
                let message = entry.get("message").cloned().unwrap_or(Value::Null);
                let role = message.get("role").and_then(|v| v.as_str()).unwrap_or("");
                let text = message_text(&message);
                match role {
                    "user" => {
                        saw_conversation = true;
                        if !super::codex::is_injected(&text) {
                            let prompt = clean(&text, PROMPT_MAX);
                            if !prompt.is_empty() {
                                if session.objective.is_empty() {
                                    session.objective = clean(&text, OBJECTIVE_MAX);
                                }
                                push_unique(&mut session.user_prompts, prompt.clone());
                                super::codex::push_tail(&mut session, "user", prompt);
                            }
                        }
                        tail_assistant_text = false;
                    }
                    "assistant" => {
                        saw_conversation = true;
                        if !text.trim().is_empty() {
                            super::codex::push_tail(
                                &mut session,
                                "assistant",
                                clean(&text, TAIL_ENTRY_MAX),
                            );
                            tail_assistant_text = true;
                        }
                        for (_id, _name, arguments) in tool_calls(&message) {
                            session.tool_events += 1;
                            let args: Value =
                                serde_json::from_str(&arguments).unwrap_or(Value::Null);
                            extract_files(&args, &mut session);
                        }
                    }
                    "toolResult" => {
                        if let Some(failure) = super::codex::failure_excerpt(&text) {
                            push_unique(
                                &mut session.failed_approaches,
                                clean(failure, FAILURE_MAX),
                            );
                        }
                        tail_assistant_text = false;
                    }
                    _ => {}
                }
            }
            "custom_message" => {
                if !noted.contains(&"custom_message") {
                    noted.push("custom_message");
                    session.losses.push(LossNote {
                        what: "custom_message".into(),
                        reason: "extension-injected context message".into(),
                    });
                }
            }
            // compaction/branch_summary were collected file-wide above.
            "compaction" | "branch_summary" | "" => {}
            other => {
                if !noted.contains(&other) {
                    noted.push(other);
                    session.losses.push(LossNote {
                        what: format!("pi entry type `{other}`"),
                        reason: "harness control/state entry, not conversation content".into(),
                    });
                }
            }
        }
    }

    session.ended_at = last_ts;
    session.outcome = if tail_assistant_text {
        Outcome::Completed
    } else if saw_conversation {
        Outcome::Interrupted
    } else {
        Outcome::Unknown
    };
    Some(session)
}

impl TranscriptReader for PiReader {
    fn id(&self) -> &'static str {
        "pi"
    }

    fn scan(&self, home: &Path, project_dir: &Path) -> Vec<TranscriptSession> {
        let root = paths::pi_agent_root(home).join("sessions");
        walk_files(&root, &|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(".jsonl"))
                .unwrap_or(false)
        })
        .iter()
        .filter_map(|file| parse_session_file(file))
        .filter_map(|raw| summarize(&raw, project_dir))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write a pi session file under `<home>/.pi/agent/sessions/<dir>/`.
    fn write_session(home: &Path, dir: &str, file: &str, lines: &[&str]) -> std::path::PathBuf {
        let dir = home.join(format!(".pi/agent/sessions/{dir}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join(file);
        let mut f = std::fs::File::create(&path).expect("create");
        for line in lines {
            writeln!(f, "{line}").expect("write");
        }
        path
    }

    #[test]
    fn pi_reader_linearizes_the_current_branch() {
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let home = tempfile::tempdir().expect("home");
        write_session(
            home.path(),
            "--tmp-demo--",
            "2026-08-20T10-00-00_ses-1.jsonl",
            &[
                &format!(
                    r#"{{"type":"session","version":3,"id":"ses-1","timestamp":"2026-08-20T10:00:00.000Z","cwd":"{cwd}"}}"#
                ),
                r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-08-20T10:00:01.000Z","message":{"role":"user","content":"implement the exporter","timestamp":1784272801000}}"#,
                r#"{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-08-20T10:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"on it"},{"type":"thinking","thinking":"reasoning dropped"},{"type":"toolCall","id":"c1","name":"bash","arguments":{"command":"cat > src/exporter.rs <<'EOF'"}}],"timestamp":1784272802000}}"#,
                r#"{"type":"message","id":"m3","parentId":"m2","timestamp":"2026-08-20T10:00:03.000Z","message":{"role":"toolResult","toolCallId":"c1","toolName":"bash","content":[{"type":"text","text":"wrote 40 lines"}],"isError":false,"timestamp":1784272803000}}"#,
                // A branch off m1 that is NOT on the current leaf path.
                r#"{"type":"message","id":"m2alt","parentId":"m1","timestamp":"2026-08-20T10:00:04.000Z","message":{"role":"assistant","content":[{"type":"text","text":"abandoned branch answer"}],"timestamp":1784272804000}}"#,
                r#"{"type":"branch_summary","id":"b1","parentId":"m2alt","timestamp":"2026-08-20T10:00:05.000Z","fromId":"m2alt","summary":"branch tried the sync exporter first"}"#,
                r#"{"type":"message","id":"m4","parentId":"m3","timestamp":"2026-08-20T10:00:06.000Z","message":{"role":"assistant","content":[{"type":"text","text":"exporter wired"}],"timestamp":1784272806000}}"#,
                r#"{"type":"model_change","id":"mc1","parentId":"m4","timestamp":"2026-08-20T10:00:07.000Z","provider":"deepseek","modelId":"deepseek-v4-flash"}"#,
            ],
        );
        let file = home
            .path()
            .join(".pi/agent/sessions/--tmp-demo--/2026-08-20T10-00-00_ses-1.jsonl");
        let raw = parse_session_file(&file).expect("raw");
        let session = summarize(&raw, project.path()).expect("session");

        assert_eq!(session.session_id, "ses-1");
        assert_eq!(session.harness, "pi");
        assert_eq!(session.objective, "implement the exporter");
        assert_eq!(session.tool_events, 1);
        assert_eq!(session.files_touched, vec!["src/exporter.rs"]);
        // The abandoned branch text is not on the current leaf path; the
        // branch summary entry is (it parents into the live spine).
        let tail_texts: Vec<&str> = session
            .conversation_tail
            .iter()
            .map(|t| t.text.as_str())
            .collect();
        assert!(
            !tail_texts.iter().any(|t| t.contains("abandoned branch")),
            "tail: {tail_texts:?}"
        );
        assert!(
            tail_texts.iter().any(|t| t.contains("exporter wired")),
            "tail: {tail_texts:?}"
        );
        assert!(
            session
                .progress_summaries
                .iter()
                .any(|s| s.contains("sync exporter")),
            "summaries: {:?}",
            session.progress_summaries
        );
        assert!(
            session
                .losses
                .iter()
                .any(|l| l.what.contains("model_change")),
            "losses: {:?}",
            session.losses
        );
        assert_eq!(session.outcome, Outcome::Completed);
        assert_eq!(session.started_at, "2026-08-20T10:00:00.000Z");
        assert_eq!(session.ended_at, "2026-08-20T10:00:07.000Z");

        // cwd filtering: a session for another project is excluded.
        assert!(summarize(&raw, Path::new("/elsewhere")).is_none());
    }

    #[test]
    fn pi_reader_flags_an_interrupted_tail() {
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let home = tempfile::tempdir().expect("home");
        write_session(
            home.path(),
            "d",
            "t_ses-2.jsonl",
            &[
                &format!(
                    r#"{{"type":"session","version":3,"id":"ses-2","timestamp":"2026-08-20T11:00:00.000Z","cwd":"{cwd}"}}"#
                ),
                r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-08-20T11:00:01.000Z","message":{"role":"user","content":"run the build","timestamp":1}}"#,
                r#"{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-08-20T11:00:02.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"c9","name":"bash","arguments":{"command":"cargo build"}}],"timestamp":2}}"#,
            ],
        );
        let file = home.path().join(".pi/agent/sessions/d/t_ses-2.jsonl");
        let raw = parse_session_file(&file).expect("raw");
        let session = summarize(&raw, project.path()).expect("session");
        assert_eq!(session.outcome, Outcome::Interrupted);
    }
}
