//! Command Code transcript reader: `~/.commandcode/projects/<project-slug>/**/*.jsonl`.
//!
//! Format (verified against this machine's 0.2.x sessions and the bundled
//! sessions reference): the first line is a header
//! `{"type":"session","version":3,"id","timestamp","cwd"}`; every following
//! line is a tree entry with `id`/`parentId`/`timestamp`. Conversation entries
//! are `{"type":"message","message":{"role","content":[blocks]}}` where blocks
//! are `text`, `thinking`, `tool_use` (`name` + `input`) and `tool_result`
//! (`content` blocks). Compactions are `{"type":"compaction","summary",…}`.
//!
//! Sidecar files (`<id>.checkpoints.jsonl`, `<id>.prompts.jsonl`) share the
//! `.jsonl` extension and are NOT transcripts — they are excluded by name.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{
    clean, cwd_matches, push_unique, shell_write_targets, walk_files, LossNote, Outcome,
    TranscriptReader, TranscriptSession,
};

/// Command Code session reader.
pub struct CommandCodeReader;

/// True for a transcript file: `.jsonl` that is not a native sidecar.
fn is_transcript(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.ends_with(".jsonl")
        && !name.ends_with(".checkpoints.jsonl")
        && !name.ends_with(".prompts.jsonl")
}

/// Every Command Code transcript under the projects store.
pub(crate) fn session_files(home: &Path) -> Vec<PathBuf> {
    walk_files(
        &crate::harness_install::paths::commandcode_root(home).join("projects"),
        &is_transcript,
    )
}

/// Raw parse of one session file (unparseable lines skipped). `None` when the
/// file holds no JSON at all.
pub(crate) fn parse_session_file(path: &Path) -> Option<Vec<Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    let events: Vec<Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    (!events.is_empty()).then_some(events)
}

/// Session id + cwd from the header line (cheap project-attribution boundary).
pub(crate) fn session_identity(file: &Path, project_dir: &Path) -> Option<(String, String)> {
    let handle = std::fs::File::open(file).ok()?;
    use std::io::BufRead as _;
    for line in std::io::BufReader::new(handle).lines().take(8) {
        let Ok(line) = line else { continue };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if event.get("type").and_then(|value| value.as_str()) != Some("session") {
            continue;
        }
        let cwd = event.get("cwd").and_then(|value| value.as_str())?;
        if !cwd_matches(cwd, project_dir) {
            return None;
        }
        let session_id = event
            .get("id")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                file.file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_string)
            })?;
        return Some((session_id, cwd.to_string()));
    }
    None
}

impl TranscriptReader for CommandCodeReader {
    fn id(&self) -> &'static str {
        "commandcode"
    }

    fn scan(&self, home: &Path, project_dir: &Path) -> Vec<TranscriptSession> {
        session_files(home)
            .iter()
            .filter_map(|file| parse_session(file, project_dir))
            .collect()
    }
}

/// Text of one message's content blocks of `kind` (text by default).
pub(crate) fn message_text(message: &Value, kind: &str) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(|value| value.as_str()) == Some(kind))
            .filter_map(|block| block.get("text").and_then(|value| value.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Text carried by a tool_result block (`content` string or text blocks).
pub(crate) fn tool_result_text(block: &Value) -> Option<String> {
    match block.get("content") {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(blocks)) => {
            let text = blocks
                .iter()
                .filter(|part| part.get("type").and_then(|value| value.as_str()) == Some("text"))
                .filter_map(|part| part.get("text").and_then(|value| value.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

/// Prompt text that Command Code itself injected (system reminders, slash
/// command envelopes) rather than the human typing it.
pub(crate) fn is_injected_prompt(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("<system-reminder")
        || trimmed.starts_with("<command-name")
        || trimmed.starts_with("<local-command")
        || super::codex::is_injected(trimmed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastKind {
    AssistantMessage,
    UserMessage,
    ToolCall,
    ToolOutput,
    Incomplete,
    Other,
}

pub(crate) fn parse_session(file: &Path, project_dir: &Path) -> Option<TranscriptSession> {
    let events = parse_session_file(file)?;
    let header = events
        .iter()
        .find(|event| event.get("type").and_then(|value| value.as_str()) == Some("session"));
    let cwd = header
        .and_then(|event| event.get("cwd").and_then(|value| value.as_str()))
        .filter(|value| !value.is_empty())
        .map(str::to_string)?;
    if !cwd_matches(&cwd, project_dir) {
        return None;
    }
    let session_id = header
        .and_then(|event| event.get("id").and_then(|value| value.as_str()))
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            file.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("unknown")
                .to_string()
        });
    let mut session = TranscriptSession {
        harness: "commandcode",
        source_path: file.to_string_lossy().into_owned(),
        session_id,
        cwd,
        ..Default::default()
    };
    let mut pending_calls = std::collections::BTreeSet::new();
    let mut saw_assistant = false;
    let mut last_kind = LastKind::Other;
    let mut branches = std::collections::BTreeSet::new();
    let mut compactions = Vec::new();

    for event in &events {
        if let Some(ts) = event.get("timestamp").and_then(|value| value.as_str()) {
            if session.started_at.is_empty() {
                session.started_at = ts.to_string();
            }
            session.ended_at = ts.to_string();
        }
        match event.get("type").and_then(|value| value.as_str()) {
            Some("message") => {
                let Some(message) = event.get("message") else {
                    continue;
                };
                let role = message
                    .get("role")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                if let Some(parent) = event.get("parentId").and_then(|value| value.as_str()) {
                    branches.insert(parent.to_string());
                }
                match role {
                    "user" => {
                        let mut had_result = false;
                        if let Some(blocks) = message.get("content").and_then(|v| v.as_array()) {
                            for block in blocks {
                                match block.get("type").and_then(|value| value.as_str()) {
                                    Some("tool_result") => {
                                        had_result = true;
                                        if let Some(call_id) =
                                            block.get("tool_use_id").and_then(|v| v.as_str())
                                        {
                                            pending_calls.remove(call_id);
                                        }
                                        if let Some(text) = tool_result_text(block) {
                                            if let Some(failure) =
                                                super::codex::failure_excerpt(&text)
                                            {
                                                push_unique(
                                                    &mut session.failed_approaches,
                                                    clean(failure, 800),
                                                );
                                            }
                                        } else {
                                            session.losses.push(LossNote {
                                                what: "tool_result.content".to_string(),
                                                reason:
                                                    "no text part; retained as canonical metadata"
                                                        .to_string(),
                                            });
                                        }
                                    }
                                    Some("text") => {
                                        if let Some(text) =
                                            block.get("text").and_then(|v| v.as_str())
                                        {
                                            if !is_injected_prompt(text) {
                                                handle_prompt(&mut session, text);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        } else if let Some(text) = message.get("content").and_then(|v| v.as_str()) {
                            if !is_injected_prompt(text) {
                                handle_prompt(&mut session, text);
                            }
                        }
                        last_kind = if had_result {
                            LastKind::ToolOutput
                        } else {
                            LastKind::UserMessage
                        };
                    }
                    "assistant" => {
                        saw_assistant = true;
                        let text = message_text(message, "text");
                        if !text.trim().is_empty() {
                            super::codex::push_tail(&mut session, "assistant", clean(&text, 1500));
                        }
                        if message.get("status").and_then(|value| value.as_str())
                            == Some("incomplete")
                        {
                            last_kind = LastKind::Incomplete;
                        } else if let Some(blocks) =
                            message.get("content").and_then(|v| v.as_array())
                        {
                            for block in blocks {
                                match block.get("type").and_then(|value| value.as_str()) {
                                    Some("tool_use") => {
                                        session.tool_events += 1;
                                        extract_tool_use(block, &mut session);
                                        if let Some(call_id) =
                                            block.get("id").and_then(|v| v.as_str())
                                        {
                                            pending_calls.insert(call_id.to_string());
                                        }
                                        last_kind = LastKind::ToolCall;
                                    }
                                    Some("text") => {
                                        if text.trim().chars().count() >= 40 {
                                            push_unique(
                                                &mut session.milestones,
                                                clean(&text, 1200),
                                            );
                                        }
                                        last_kind = LastKind::AssistantMessage;
                                    }
                                    _ => {}
                                }
                            }
                        } else {
                            last_kind = LastKind::AssistantMessage;
                        }
                    }
                    _ => {}
                }
            }
            Some("compaction") => {
                if let Some(summary) = event.get("summary").and_then(|value| value.as_str()) {
                    compactions.push(clean(summary, 6000));
                }
            }
            _ => {}
        }
    }

    // Newest first, capped like the other readers' progress summaries.
    compactions.reverse();
    compactions.truncate(8);
    session.progress_summaries = compactions;
    if !branches.is_empty() {
        session.losses.push(LossNote {
            what: "rewound_branches".to_string(),
            reason: "entries are appended in tree order; rewound branches are retained as the \
                     superseded-history mainline only"
                .to_string(),
        });
    }
    session.outcome = if !pending_calls.is_empty() || last_kind == LastKind::Incomplete {
        Outcome::Interrupted
    } else if last_kind == LastKind::AssistantMessage {
        Outcome::Completed
    } else if !saw_assistant || last_kind == LastKind::ToolCall {
        Outcome::Interrupted
    } else {
        Outcome::Unknown
    };
    Some(session)
}

fn handle_prompt(session: &mut TranscriptSession, text: &str) {
    let prompt = clean(text, 2000);
    if prompt.is_empty() {
        return;
    }
    if session.objective.is_empty() {
        session.objective = clean(text, 8000);
    }
    push_unique(&mut session.user_prompts, prompt.clone());
    super::codex::push_tail(session, "user", prompt);
}

/// Files from tool_use inputs: Command Code tool ids are `write_file` /
/// `edit_file` (`file_path`) and `shell_command` (`command`).
fn extract_tool_use(block: &Value, session: &mut TranscriptSession) {
    let name = block
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let input = block.get("input").cloned().unwrap_or(Value::Null);
    match name {
        "write_file" | "edit_file" => {
            if let Some(path) = input.get("file_path").and_then(|value| value.as_str()) {
                push_unique(&mut session.files_touched, clean(path, 300));
            }
        }
        "shell_command" => {
            if let Some(command) = input.get("command").and_then(|value| value.as_str()) {
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
    use serde_json::json;
    use std::io::Write;

    fn write_events(path: &Path, events: &[Value]) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let mut file = std::fs::File::create(path).expect("create");
        for event in events {
            writeln!(file, "{}", serde_json::to_string(event).expect("json")).expect("write");
        }
    }

    fn fixture(home: &Path, project_path: &Path) -> PathBuf {
        let cwd = project_path.to_string_lossy().to_string();
        let touched = project_path.join("src/lib.rs");
        let session_id = "52e6f092-17d2-4ade-9253-d00fac70105d";
        let events = vec![
            json!({"type":"session","version":3,"id":session_id,"timestamp":"2026-11-18T10:11:19.153Z","cwd":cwd}),
            json!({"type":"message","id":"m1","parentId":null,"timestamp":"2026-11-18T10:11:20.000Z","message":{"role":"user","content":[{"type":"text","text":"Implement the parser"}],"meta":{"source":"user"}}}),
            json!({"type":"message","id":"m2","parentId":"m1","timestamp":"2026-11-18T10:11:21.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"secret reasoning"},{"type":"tool_use","id":"call_1","name":"write_file","input":{"file_path":touched}}]}}),
            json!({"type":"message","id":"m3","parentId":"m2","timestamp":"2026-11-18T10:11:22.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":[{"type":"text","text":"Error: disk full"}],"meta":{"source":"tool"}}]}}),
            json!({"type":"compaction","id":"c1","parentId":"m3","timestamp":"2026-11-18T10:11:23.000Z","summary":"Compacted: parser work started","tokensBefore":1000}),
            json!({"type":"message","id":"m4","parentId":"c1","timestamp":"2026-11-18T10:11:24.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Implemented the parser and recorded the verification result."}]}}),
        ];
        let file = home
            .join(".commandcode/projects")
            .join(project_path.file_name().expect("name"))
            .join(format!("{session_id}.jsonl"));
        write_events(&file, &events);
        // Sidecars share the extension and must never be read as transcripts.
        write_events(
            &file.with_extension("checkpoints.jsonl"),
            &[json!({"type":"checkpoint"})],
        );
        write_events(
            &file.with_extension("prompts.jsonl"),
            &[json!({"type":"prompt"})],
        );
        file
    }

    #[test]
    fn reads_native_messages_tools_compaction_and_sidecar_exclusion() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let file = fixture(home.path(), project.path());

        assert_eq!(session_files(home.path()), vec![file.clone()]);
        let session = parse_session(&file, project.path()).expect("session");
        assert_eq!(session.harness, "commandcode");
        assert_eq!(session.session_id, "52e6f092-17d2-4ade-9253-d00fac70105d");
        assert_eq!(session.user_prompts, vec!["Implement the parser"]);
        assert_eq!(session.tool_events, 1);
        assert_eq!(session.files_touched.len(), 1);
        assert!(session.files_touched[0].ends_with("src/lib.rs"));
        assert!(session
            .failed_approaches
            .iter()
            .any(|item| item.contains("disk full")));
        assert_eq!(
            session.progress_summaries,
            vec!["Compacted: parser work started"],
            "compaction summaries are transcript content, not losses"
        );
        assert!(session.milestones.iter().any(|m| m.contains("parser")));
        assert_eq!(session.outcome, Outcome::Completed);
        assert_eq!(session.started_at, "2026-11-18T10:11:19.153Z");
        assert_eq!(session.ended_at, "2026-11-18T10:11:24.000Z");
        // The rewind note is truthful about tree ordering.
        assert!(session
            .losses
            .iter()
            .any(|loss| loss.what == "rewound_branches"));
        assert_eq!(session_identity(&file, project.path()).map(|id| id.0), {
            Some("52e6f092-17d2-4ade-9253-d00fac70105d".to_string())
        });
    }

    #[test]
    fn injected_prompts_are_skipped_and_foreign_cwd_excluded() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let cwd = project.path().to_string_lossy().to_string();
        let file = home.path().join(".commandcode/projects/demo/s1.jsonl");
        write_events(
            &file,
            &[
                json!({"type":"session","version":3,"id":"s1","timestamp":"2026-11-18T10:00:00.000Z","cwd":cwd}),
                json!({"type":"message","id":"m1","timestamp":"2026-11-18T10:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"<system-reminder>hooks</system-reminder>"}]}}),
                json!({"type":"message","id":"m2","timestamp":"2026-11-18T10:00:02.000Z","message":{"role":"user","content":[{"type":"text","text":"<command-name>/plan</command-name>"}]}}),
                json!({"type":"message","id":"m3","timestamp":"2026-11-18T10:00:03.000Z","message":{"role":"user","content":[{"type":"text","text":"real request"}]}}),
                json!({"type":"message","id":"m4","timestamp":"2026-11-18T10:00:04.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"call_open","name":"shell_command","input":{"command":"make test"}}]}}),
            ],
        );
        let session = parse_session(&file, project.path()).expect("session");
        assert_eq!(session.user_prompts, vec!["real request"]);
        assert_eq!(session.outcome, Outcome::Interrupted, "dangling tool call");

        let other = home.path().join(".commandcode/projects/other/s2.jsonl");
        write_events(
            &other,
            &[
                json!({"type":"session","version":3,"id":"s2","timestamp":"2026-11-18T10:00:00.000Z","cwd":"/elsewhere"}),
            ],
        );
        assert!(parse_session(&other, project.path()).is_none());
        assert!(session_identity(&other, project.path()).is_none());
    }
}
