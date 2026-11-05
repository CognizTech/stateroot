//! CodeBuddy transcript reader: `$CODEBUDDY_CONFIG_DIR/projects/**/*.jsonl`
//! (default `~/.codebuddy/projects/**/*.jsonl`).
//!
//! Verified against CodeBuddy Code 2.180.0 sessions and its bundled protocol
//! types: `message` events carry `input_text`/`output_text` content,
//! `function_call` uses `callId` + string `arguments`, and
//! `function_call_result` correlates with the same `callId`.

use std::collections::BTreeSet;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{
    clean, cwd_matches, push_unique, shell_write_targets, walk_files, LossNote, Outcome,
    TranscriptReader, TranscriptSession,
};
use crate::harness_install::paths;

pub struct CodeBuddyReader;

#[used]
static CODEBUDDY_NATIVE_READER_MARKER: &str = "STATEROOT_CODEBUDDY_NATIVE_READER_V1";

pub(crate) fn session_files(home: &Path) -> Vec<PathBuf> {
    walk_files(&paths::codebuddy_root(home).join("projects"), &|path| {
        path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
    })
}

pub(crate) fn parse_session_file(path: &Path) -> Option<Vec<Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    let events: Vec<Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    (!events.is_empty()).then_some(events)
}

/// Read only the session identity needed for project attribution. Native
/// messages carry both fields on the first line; later metadata is not
/// required for this cheap boundary check.
pub(crate) fn session_identity(file: &Path, project_dir: &Path) -> Option<(String, String)> {
    let handle = std::fs::File::open(file).ok()?;
    let mut cwd = None;
    let mut session_id = None;
    for line in std::io::BufReader::new(handle).lines().take(64) {
        let Ok(line) = line else { continue };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if cwd.is_none() {
            cwd = event
                .get("cwd")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
        if session_id.is_none() {
            session_id = event
                .get("sessionId")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
        if cwd.is_some() && session_id.is_some() {
            break;
        }
    }
    let cwd = cwd?;
    if !cwd_matches(&cwd, project_dir) {
        return None;
    }
    let session_id = session_id.unwrap_or_else(|| {
        file.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown")
            .to_string()
    });
    Some((session_id, cwd))
}

pub(crate) fn project_ids(home: &Path, project_dir: &Path) -> BTreeSet<String> {
    session_files(home)
        .iter()
        .filter_map(|file| {
            session_identity(file, project_dir)?;
            file.parent()?.file_name()?.to_str().map(str::to_string)
        })
        .collect()
}

impl TranscriptReader for CodeBuddyReader {
    fn id(&self) -> &'static str {
        "codebuddy"
    }

    fn scan(&self, home: &Path, project_dir: &Path) -> Vec<TranscriptSession> {
        session_files(home)
            .iter()
            .filter_map(|file| parse_session(file, project_dir))
            .collect()
    }
}

pub(crate) fn timestamp(event: &Value) -> Option<String> {
    match event.get("timestamp") {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Number(value)) => value.as_i64().map(super::kimi::rfc3339_millis),
        _ => None,
    }
}

pub(crate) fn message_text(event: &Value) -> String {
    match event.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(
                |block| match block.get("type").and_then(|kind| kind.as_str()) {
                    Some("input_text" | "output_text" | "text") => {
                        block.get("text").and_then(|text| text.as_str())
                    }
                    _ => None,
                },
            )
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub(crate) fn output_text(output: &Value) -> Option<String> {
    match output {
        Value::String(text) => Some(text.clone()),
        Value::Object(block)
            if block.get("type").and_then(|kind| kind.as_str()) == Some("text") =>
        {
            block
                .get("text")
                .and_then(|text| text.as_str())
                .map(str::to_string)
        }
        Value::Array(blocks) => {
            let text = blocks
                .iter()
                .filter_map(
                    |block| match block.get("type").and_then(|kind| kind.as_str()) {
                        Some("input_text" | "output_text" | "text") => {
                            block.get("text").and_then(|text| text.as_str())
                        }
                        _ => None,
                    },
                )
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

pub(crate) fn is_injected_prompt(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("<system-reminder")
        || trimmed.starts_with("<command-name")
        || trimmed.starts_with("<local-command")
        || super::codex::is_injected(text)
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
    let cwd = events
        .iter()
        .filter_map(|event| event.get("cwd").and_then(|value| value.as_str()))
        .find(|value| !value.is_empty())?
        .to_string();
    if !cwd_matches(&cwd, project_dir) {
        return None;
    }
    let session_id = events
        .iter()
        .filter_map(|event| event.get("sessionId").and_then(|value| value.as_str()))
        .find(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            file.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("unknown")
                .to_string()
        });
    let mut session = TranscriptSession {
        harness: "codebuddy",
        source_path: file.to_string_lossy().into_owned(),
        session_id,
        cwd,
        ..Default::default()
    };
    let mut pending_calls = BTreeSet::new();
    let mut saw_assistant = false;
    let mut last_kind = LastKind::Other;

    for event in &events {
        if let Some(ts) = timestamp(event) {
            if session.started_at.is_empty() {
                session.started_at = ts.clone();
            }
            session.ended_at = ts;
        }
        match event.get("type").and_then(|kind| kind.as_str()) {
            Some("message") => {
                let role = event
                    .get("role")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let text = message_text(event);
                let skipped = event
                    .pointer("/providerData/skipRun")
                    .and_then(|value| value.as_bool())
                    == Some(true);
                if let Some(error) = event
                    .pointer("/providerData/error/message")
                    .and_then(|value| value.as_str())
                {
                    push_unique(&mut session.failed_approaches, clean(error, 800));
                }
                match role {
                    "user" => {
                        if !skipped && !is_injected_prompt(&text) {
                            let prompt = clean(&text, 2000);
                            if !prompt.is_empty() {
                                if session.objective.is_empty() {
                                    session.objective = clean(&text, 8000);
                                }
                                push_unique(&mut session.user_prompts, prompt.clone());
                                super::codex::push_tail(&mut session, "user", prompt);
                            }
                        }
                        last_kind = LastKind::UserMessage;
                    }
                    "assistant" => {
                        saw_assistant = true;
                        let reply = clean(&text, 1500);
                        if !reply.is_empty() {
                            super::codex::push_tail(&mut session, "assistant", reply);
                        }
                        if event.get("status").and_then(|value| value.as_str())
                            == Some("incomplete")
                        {
                            last_kind = LastKind::Incomplete;
                        } else {
                            if text.trim().chars().count() >= 40 {
                                push_unique(&mut session.milestones, clean(&text, 1200));
                            }
                            last_kind = LastKind::AssistantMessage;
                        }
                    }
                    _ => {}
                }
            }
            Some("function_call") => {
                session.tool_events += 1;
                extract_function_call(event, &mut session);
                if let Some(call_id) = event.get("callId").and_then(|value| value.as_str()) {
                    pending_calls.insert(call_id.to_string());
                }
                last_kind = LastKind::ToolCall;
            }
            Some("function_call_result") => {
                if let Some(call_id) = event.get("callId").and_then(|value| value.as_str()) {
                    pending_calls.remove(call_id);
                }
                let output = event.get("output").unwrap_or(&Value::Null);
                match output_text(output) {
                    Some(text) => {
                        if event.get("status").and_then(|value| value.as_str())
                            == Some("incomplete")
                        {
                            push_unique(&mut session.failed_approaches, clean(&text, 800));
                        } else if let Some(failure) = super::codex::failure_excerpt(&text) {
                            push_unique(&mut session.failed_approaches, clean(failure, 800));
                        }
                    }
                    None => session.losses.push(LossNote {
                        what: "function_call_result.output".to_string(),
                        reason: "shape unverified; retained as canonical metadata".to_string(),
                    }),
                }
                last_kind = LastKind::ToolOutput;
            }
            _ => {}
        }
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

fn extract_function_call(event: &Value, session: &mut TranscriptSession) {
    let name = event
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let arguments = event
        .get("arguments")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    match name {
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            if let Some(path) = args.get("file_path").and_then(|value| value.as_str()) {
                push_unique(&mut session.files_touched, clean(path, 300));
            }
        }
        "Bash" => {
            if let Some(command) = args.get("command").and_then(|value| value.as_str()) {
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

    #[test]
    fn reads_native_messages_tools_and_failures() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let cwd = project.path().to_string_lossy().to_string();
        let touched = project.path().join("src/lib.rs");
        let session_id = "01a1a9f5-3264-70b5-aa96-6c6f786b5307";
        let events = vec![
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"<command-name>/model</command-name>"}],"providerData":{"skipRun":true},"sessionId":session_id,"cwd":cwd,"timestamp":1793852781215i64}),
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"Implement the exporter"}],"sessionId":session_id,"cwd":cwd,"timestamp":1793852781216i64}),
            json!({"type":"function_call","callId":"call-1","name":"Write","arguments":serde_json::to_string(&json!({"file_path":touched})).unwrap(),"sessionId":session_id,"cwd":cwd,"timestamp":1793852781217i64}),
            json!({"type":"function_call_result","callId":"call-1","name":"Write","status":"completed","output":{"type":"text","text":"Error: disk full"},"sessionId":session_id,"cwd":cwd,"timestamp":1793852781218i64}),
            json!({"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Implemented the exporter and recorded the verification result."}],"sessionId":session_id,"cwd":cwd,"timestamp":1793852781219i64}),
        ];
        let file = home
            .path()
            .join(".codebuddy/projects/tmp-demo")
            .join(format!("{session_id}.jsonl"));
        write_events(&file, &events);

        let session = parse_session(&file, project.path()).expect("session");
        assert_eq!(session.session_id, session_id);
        assert_eq!(session.user_prompts, vec!["Implement the exporter"]);
        assert_eq!(session.tool_events, 1);
        assert_eq!(session.files_touched.len(), 1);
        assert_eq!(Path::new(&session.files_touched[0]), touched.as_path());
        assert!(session
            .failed_approaches
            .iter()
            .any(|item| item.contains("disk full")));
        assert_eq!(session.outcome, Outcome::Completed);
        assert!(session.started_at.contains('T'));
        assert_eq!(project_ids(home.path(), project.path()).len(), 1);
    }

    #[test]
    fn dangling_call_is_interrupted_and_foreign_cwd_is_excluded() {
        let project = tempfile::tempdir().expect("project");
        let cwd = project.path().to_string_lossy().to_string();
        let file = project.path().join("session.jsonl");
        write_events(
            &file,
            &[
                json!({"type":"message","role":"user","content":[{"type":"input_text","text":"start"}],"sessionId":"s1","cwd":cwd,"timestamp":1i64}),
                json!({"type":"function_call","callId":"call-open","name":"Bash","arguments":"{\"command\":\"pwd\"}","sessionId":"s1","cwd":cwd,"timestamp":2i64}),
            ],
        );
        assert_eq!(
            parse_session(&file, project.path())
                .expect("session")
                .outcome,
            Outcome::Interrupted
        );

        let other = project.path().join("other.jsonl");
        write_events(
            &other,
            &[
                json!({"type":"message","role":"user","content":[{"type":"input_text","text":"other"}],"sessionId":"s2","cwd":"/elsewhere","timestamp":1i64}),
            ],
        );
        assert!(parse_session(&other, project.path()).is_none());
    }
}
