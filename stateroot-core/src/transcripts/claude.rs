//! Claude Code transcript reader: `~/.claude/projects/**/*.jsonl`.
//!
//! Format (verified against this machine's session files): one JSON event
//! per line — `queue-operation` (skipped), `user` events
//! (`message.role == "user"`, `message.content` string or block array,
//! top-level `cwd`, `sessionId`, `timestamp`, `isMeta`), and `assistant`
//! events (`message.content[]` blocks: `thinking`, `text`, `tool_use` with
//! `name` + `input`). Tool results arrive as `user` events whose content
//! blocks are `{"type": "tool_result", "is_error": …}`.
//!
//! Skip rules for prompts: `isMeta`, `<local-command-caveat>`,
//! `<command-name>` blocks, and tool_result-only user events.

use std::path::Path;

use serde_json::Value;

use super::{
    clean, cwd_matches, event_timestamp, push_unique, shell_write_targets, walk_files, Outcome,
    TranscriptReader, TranscriptSession,
};

/// Claude Code session reader.
pub struct ClaudeReader;

/// Every claude session file under the projects store.
pub(crate) fn session_files(home: &Path) -> Vec<std::path::PathBuf> {
    walk_files(&home.join(".claude/projects"), &|p| {
        p.extension().and_then(|e| e.to_str()) == Some("jsonl")
    })
}

/// Raw parse of one session file: the event lines as verbatim JSON
/// (unparseable lines skipped, mirroring the summary reader). `None` when
/// the file holds no JSON at all.
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

impl TranscriptReader for ClaudeReader {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn scan(&self, home: &Path, project_dir: &Path) -> Vec<TranscriptSession> {
        let root = home.join(".claude/projects");
        let files = walk_files(&root, &|p| {
            p.extension().and_then(|e| e.to_str()) == Some("jsonl")
        });
        files
            .iter()
            .filter_map(|file| parse_session(file, project_dir))
            .collect()
    }
}

fn parse_session(file: &Path, project_dir: &Path) -> Option<TranscriptSession> {
    let text = std::fs::read_to_string(file).ok()?;

    let mut session = TranscriptSession {
        harness: "claude",
        ..Default::default()
    };
    let mut cwd = String::new();
    let mut session_id = String::new();
    let mut saw_assistant_text = false;
    let mut last_kind = LastKind::Other;
    let mut last_ts = String::new();
    let mut saw_any = false;

    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(ts) = event_timestamp(&event) {
            if session.started_at.is_empty() {
                session.started_at = ts.clone();
            }
            last_ts = ts;
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
                        handle_prompt(&mut session, text);
                        last_kind = LastKind::UserMessage;
                    }
                    Some(Value::Array(blocks)) => {
                        let mut had_result = false;
                        for block in blocks {
                            if block.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                                had_result = true;
                                if block.get("is_error").and_then(|v| v.as_bool()) == Some(true) {
                                    let excerpt = block
                                        .get("content")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("tool error");
                                    push_unique(
                                        &mut session.failed_approaches,
                                        clean(excerpt, 200),
                                    );
                                }
                            } else if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                    handle_prompt(&mut session, text);
                                }
                            }
                        }
                        last_kind = if had_result {
                            LastKind::ToolOutput
                        } else {
                            LastKind::UserMessage
                        };
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
                            Some("tool_use") => {
                                session.tool_events += 1;
                                extract_tool_use(block, &mut session);
                                last_kind = LastKind::ToolCall;
                            }
                            Some("text") => {
                                saw_assistant_text = true;
                                last_kind = LastKind::AssistantMessage;
                            }
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
    session.session_id = if session_id.is_empty() {
        file.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    } else {
        session_id
    };
    session.cwd = cwd;
    session.ended_at = last_ts;
    session.outcome = match last_kind {
        LastKind::AssistantMessage => Outcome::Completed,
        LastKind::ToolCall => Outcome::Interrupted,
        _ if !saw_assistant_text => Outcome::Interrupted,
        _ => Outcome::Unknown,
    };
    Some(session)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastKind {
    AssistantMessage,
    UserMessage,
    ToolCall,
    ToolOutput,
    Other,
}

fn handle_prompt(session: &mut TranscriptSession, text: &str) {
    let trimmed = text.trim_start();
    if trimmed.starts_with("<local-command-caveat") || trimmed.starts_with("<command-name") {
        return;
    }
    let prompt = clean(trimmed, 1000);
    if prompt.is_empty() {
        return;
    }
    if session.objective.is_empty() {
        session.objective = clean(trimmed, 300);
    }
    push_unique(&mut session.user_prompts, prompt);
}

/// Files from tool_use inputs (Write/Edit/NotebookEdit paths; Bash write
/// targets via the shared shell extractor).
fn extract_tool_use(block: &Value, session: &mut TranscriptSession) {
    let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let input = block.get("input").cloned().unwrap_or(Value::Null);
    match name {
        "Write" | "Edit" | "NotebookEdit" | "MultiEdit" => {
            if let Some(path) = input.get("file_path").and_then(|v| v.as_str()) {
                push_unique(&mut session.files_touched, clean(path, 300));
            }
        }
        "Bash" => {
            if let Some(command) = input.get("command").and_then(|v| v.as_str()) {
                for target in shell_write_targets(command) {
                    push_unique(&mut session.files_touched, clean(&target, 300));
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn write_session(dir: &Path, name: &str, lines: &[&str]) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let mut file = std::fs::File::create(&path).expect("create");
        for line in lines {
            writeln!(file, "{line}").expect("write");
        }
        path
    }

    #[test]
    fn claude_reader_extracts_full_session_and_skips_non_prompts() {
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let home = tempfile::tempdir().expect("home");
        let file = write_session(
            &home.path().join(".claude/projects/-work-demo"),
            "session-uuid-1.jsonl",
            &[
                r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-07-10T09:00:00Z","sessionId":"session-uuid-1"}"#,
                &format!(
                    r#"{{"type":"user","isMeta":true,"message":{{"role":"user","content":"<local-command-caveat>caveat</local-command-caveat>"}},"timestamp":"2026-07-10T09:00:01Z","cwd":"{cwd}","sessionId":"session-uuid-1"}}"#
                ),
                &format!(
                    r#"{{"type":"user","message":{{"role":"user","content":"<command-name>/model</command-name>"}},"timestamp":"2026-07-10T09:00:02Z","cwd":"{cwd}","sessionId":"session-uuid-1"}}"#
                ),
                &format!(
                    r#"{{"type":"user","message":{{"role":"user","content":"build a knowledge graph of the repo (key sk-ant-api03-AbCdEfGhIjKlMnOp)"}},"timestamp":"2026-07-10T09:00:03Z","cwd":"{cwd}","sessionId":"session-uuid-1"}}"#
                ),
                &format!(
                    r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"thinking","thinking":"hmm"}},{{"type":"tool_use","id":"Write_0","name":"Write","input":{{"file_path":"{cwd}/project-kg/README.md"}}}}]}},"timestamp":"2026-07-10T09:00:04Z","cwd":"{cwd}","sessionId":"session-uuid-1"}}"#
                ),
                &format!(
                    r#"{{"type":"user","message":{{"role":"user","content":[{{"tool_use_id":"Bash_9","type":"tool_result","is_error":true,"content":"rm: cannot remove 'x': Permission denied"}}]}},"timestamp":"2026-07-10T09:00:05Z","cwd":"{cwd}","sessionId":"session-uuid-1"}}"#
                ),
                &format!(
                    r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"done — graph written"}}]}},"timestamp":"2026-07-10T09:00:06Z","cwd":"{cwd}","sessionId":"session-uuid-1"}}"#
                ),
            ],
        );
        let session = parse_session(&file, project.path()).expect("session");

        assert_eq!(session.session_id, "session-uuid-1");
        assert_eq!(session.outcome, Outcome::Completed);
        assert_eq!(
            session.user_prompts.len(),
            1,
            "prompts: {:?}",
            session.user_prompts
        );
        assert!(session.objective.starts_with("build a knowledge graph"));
        // Doctrine: verbatim — credential-looking strings are NOT scrubbed.
        assert!(session.objective.contains("sk-ant-api03-AbCdEfGhIjKlMnOp"));
        assert!(!session.objective.contains("[REDACTED]"));
        assert_eq!(session.files_touched.len(), 1);
        assert!(session.files_touched[0].ends_with("project-kg/README.md"));
        assert_eq!(session.failed_approaches.len(), 1);
        assert!(session.failed_approaches[0].contains("Permission denied"));
        assert_eq!(session.tool_events, 1);
        assert_eq!(session.started_at, "2026-07-10T09:00:00Z");
        assert_eq!(session.ended_at, "2026-07-10T09:00:06Z");
    }

    #[test]
    fn claude_reader_interrupted_on_dangling_tool_use_and_cwd_filter() {
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let home = tempfile::tempdir().expect("home");
        let file = write_session(
            &home.path().join(".claude/projects/-work-demo"),
            "session-uuid-2.jsonl",
            &[
                &format!(
                    r#"{{"type":"user","message":{{"role":"user","content":"start"}},"timestamp":"2026-07-11T09:00:00Z","cwd":"{cwd}","sessionId":"session-uuid-2"}}"#
                ),
                &format!(
                    r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"Bash_1","name":"Bash","input":{{"command":"make"}}}}]}},"timestamp":"2026-07-11T09:00:01Z","cwd":"{cwd}","sessionId":"session-uuid-2"}}"#
                ),
            ],
        );
        let session = parse_session(&file, project.path()).expect("session");
        assert_eq!(session.outcome, Outcome::Interrupted);

        // Non-matching cwd → excluded.
        let other = write_session(
            &home.path().join(".claude/projects/-elsewhere"),
            "session-uuid-3.jsonl",
            &[
                r#"{"type":"user","message":{"role":"user","content":"hi"},"timestamp":"2026-07-11T09:00:00Z","cwd":"/elsewhere","sessionId":"session-uuid-3"}"#,
            ],
        );
        assert!(parse_session(&other, project.path()).is_none());
    }
}
