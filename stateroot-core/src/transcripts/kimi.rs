//! Kimi Code transcript reader: `~/.kimi-code/sessions/**/agents/*/wire.jsonl`.
//!
//! Format (verified against this machine's store and ai-memory's reference
//! parser): line 1 is `{"type":"metadata","protocol_version","created_at":
//! <epoch ms>}`; records are flat `{type, time?}` objects —
//! `context.append_message` with `message: {role, content: [{type:
//! "text"|"think", text}], toolCalls: [{type: "function", function: {name,
//! arguments}}], partial?, origin?}`. `~/.kimi-code/session_index.jsonl`
//! binds each session id to its `workDir` (the cwd).
//!
//! Skip rules (mirroring ai-memory's parse_kimi): partial messages (a
//! complete record is re-appended later), system messages, origin-tagged
//! injections (`origin.kind != "user"`), plus the shared INJECTED_PREFIXES
//! envelope check. `think` content blocks are reasoning — dropped.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use super::codex::is_injected;
use super::{
    clean, cwd_matches, push_unique, shell_write_targets, walk_files, Outcome, TranscriptReader,
    TranscriptSession,
};

/// Kimi Code wire reader.
pub struct KimiReader;

/// session id → workDir from `~/.kimi-code/session_index.jsonl`.
pub(crate) fn read_session_index(home: &Path) -> HashMap<String, String> {
    let mut index = HashMap::new();
    let Ok(text) = std::fs::read_to_string(home.join(".kimi-code/session_index.jsonl")) else {
        return index;
    };
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let session_id = value
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let work_dir = value.get("workDir").and_then(|v| v.as_str()).unwrap_or("");
        if !session_id.is_empty() && !work_dir.is_empty() {
            index.insert(session_id.to_string(), work_dir.to_string());
        }
    }
    index
}

impl TranscriptReader for KimiReader {
    fn id(&self) -> &'static str {
        "kimi"
    }

    fn scan(&self, home: &Path, project_dir: &Path) -> Vec<TranscriptSession> {
        let index = read_session_index(home);
        let files = walk_files(&home.join(".kimi-code/sessions"), &|p| {
            p.file_name().and_then(|n| n.to_str()) == Some("wire.jsonl")
        });
        files
            .iter()
            .filter_map(|file| parse_wire(file, project_dir, &index))
            .collect()
    }
}

/// RFC3339 from epoch milliseconds (empty on garbage).
pub(crate) fn rfc3339_millis(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

fn epoch_ms(value: &Value, key: &str) -> Option<i64> {
    value
        .get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
}

/// The session id from the wire path (`…/<session-dir>/agents/<n>/wire.jsonl`
/// — the session dir is three components up from the file).
fn session_id_for(file: &Path) -> String {
    file.parent() // …/main
        .and_then(|main| main.parent()) // …/agents
        .and_then(|agents| agents.parent()) // …/<session-dir>
        .and_then(|dir| dir.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn parse_wire(
    file: &Path,
    project_dir: &Path,
    index: &HashMap<String, String>,
) -> Option<TranscriptSession> {
    let text = std::fs::read_to_string(file).ok()?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let meta: Value = serde_json::from_str(lines.next()?).ok()?;
    if meta.get("type").and_then(|v| v.as_str()) != Some("metadata") {
        return None;
    }
    let session_id = session_id_for(file);
    // The index is the only reliable cwd binding.
    let cwd = index.get(&session_id).cloned()?;
    if !cwd_matches(&cwd, project_dir) {
        return None;
    }

    let mut session = TranscriptSession {
        harness: "kimi",
        session_id,
        cwd,
        started_at: epoch_ms(&meta, "created_at")
            .map(rfc3339_millis)
            .unwrap_or_default(),
        ..Default::default()
    };
    let mut last_ts = String::new();
    let mut last_was_assistant_text = false;

    for line in lines {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(ms) = epoch_ms(&event, "time") {
            last_ts = rfc3339_millis(ms);
        }
        if event.get("type").and_then(|v| v.as_str()) != Some("context.append_message") {
            continue; // config/llm records carry harness-private data
        }
        let message = event.get("message").cloned().unwrap_or(Value::Null);
        if message.get("partial").and_then(|v| v.as_bool()) == Some(true) {
            continue; // re-appended complete later
        }
        let role = message.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let text = text_parts(&message);
        match role {
            "user" => {
                let injected = message
                    .get("origin")
                    .and_then(|o| o.get("kind"))
                    .and_then(|v| v.as_str())
                    .is_some_and(|kind| kind != "user");
                if injected || is_injected(&text) {
                    continue;
                }
                let prompt = clean(&text, 2000);
                if !prompt.is_empty() {
                    if session.objective.is_empty() {
                        session.objective = clean(&text, 8000);
                    }
                    push_unique(&mut session.user_prompts, prompt.clone());
                    super::codex::push_tail(&mut session, "user", prompt);
                }
                last_was_assistant_text = false;
            }
            "assistant" => {
                if !text.trim().is_empty() {
                    super::codex::push_tail(&mut session, "assistant", clean(&text, 1500));
                    last_was_assistant_text = true;
                }
                if let Some(calls) = message.get("toolCalls").and_then(|v| v.as_array()) {
                    for call in calls {
                        session.tool_events += 1;
                        extract_tool_call(call, &mut session);
                    }
                }
            }
            "tool" => {
                if let Some(failure) = super::codex::failure_excerpt(&text) {
                    push_unique(&mut session.failed_approaches, clean(failure, 800));
                }
                last_was_assistant_text = false;
            }
            _ => {}
        }
    }

    session.ended_at = last_ts;
    session.outcome = if last_was_assistant_text {
        Outcome::Completed
    } else {
        Outcome::Unknown
    };
    Some(session)
}

/// Join the `text` parts of a content array (`think` blocks dropped).
fn text_parts(message: &Value) -> String {
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

/// File extraction from one Kimi toolCall (function name + JSON arguments).
fn extract_tool_call(call: &Value, session: &mut TranscriptSession) {
    let function = call.get("function").unwrap_or(call);
    let name = function.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args: Value = serde_json::from_str(
        function
            .get("arguments")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    )
    .unwrap_or(Value::Null);
    match name {
        "Write" | "Edit" | "MultiEdit" | "write_file" | "edit_file" => {
            for key in ["path", "file_path", "filename"] {
                if let Some(path) = args.get(key).and_then(|v| v.as_str()) {
                    push_unique(&mut session.files_touched, clean(path, 300));
                }
            }
        }
        _ => {
            // Shell-style calls (Shell/Bash/exec): write targets only.
            let command = args
                .get("command")
                .or_else(|| args.get("cmd"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            for target in shell_write_targets(command) {
                push_unique(&mut session.files_touched, clean(&target, 300));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn write_wire(home: &Path, session_id: &str, lines: &[&str]) -> PathBuf {
        let dir = home.join(format!(
            ".kimi-code/sessions/wd_demo/{session_id}/agents/main"
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("wire.jsonl");
        let mut file = std::fs::File::create(&path).expect("create");
        writeln!(
            file,
            r#"{{"type":"metadata","protocol_version":"1.0","created_at":1784310494250}}"#
        )
        .expect("meta");
        for line in lines {
            writeln!(file, "{line}").expect("write");
        }
        path
    }

    fn write_index(home: &Path, entries: &[(&str, &str)]) {
        let mut lines = String::new();
        for (id, dir) in entries {
            lines.push_str(&format!(
                "{{\"sessionId\":\"{id}\",\"sessionDir\":\"/x/{id}\",\"workDir\":\"{dir}\"}}\n"
            ));
        }
        std::fs::create_dir_all(home.join(".kimi-code")).expect("mkdir");
        std::fs::write(home.join(".kimi-code/session_index.jsonl"), lines).expect("index");
    }

    #[test]
    fn kimi_reader_extracts_session_and_filters() {
        let project = tempfile::tempdir().expect("project");
        let cwd = project.path().to_str().expect("utf8");
        let home = tempfile::tempdir().expect("home");
        write_index(home.path(), &[("ses_demo-1", cwd)]);
        write_wire(
            home.path(),
            "ses_demo-1",
            &[
                r#"{"type":"context.append_message","message":{"role":"user","content":[{"type":"text","text":"implement the checkout flow (sk-ant-api03-AbCdEfGhIjKlMnOp)"}],"toolCalls":[]},"time":1784310495000}"#,
                r#"{"type":"context.append_message","message":{"role":"user","origin":{"kind":"hook_result"},"content":[{"type":"text","text":"INJECTED handoff delta"}]},"time":1784310496000}"#,
                r#"{"type":"context.append_message","message":{"role":"assistant","content":[{"type":"think","think":"reasoning dropped"},{"type":"text","text":"starting with the schema"}],"toolCalls":[{"type":"function","id":"t1","function":{"name":"Shell","arguments":"{\"command\": \"cat > src/checkout.rs <<'EOF'\"}"}}]},"time":1784310497000}"#,
                r#"{"type":"context.append_message","message":{"role":"tool","content":[{"type":"text","text":"Process exited with code 1"}]},"time":1784310498000}"#,
                r#"{"type":"context.append_message","message":{"role":"assistant","content":[{"type":"text","text":"done — schema migrated"}],"toolCalls":[]},"time":1784310499000}"#,
                r#"{"type":"context.append_message","message":{"role":"user","content":[{"type":"text","text":"duplicate prompt fragment"}],"partial":true},"time":1784310500000}"#,
            ],
        );
        let index = read_session_index(home.path());
        let file = home
            .path()
            .join(".kimi-code/sessions/wd_demo/ses_demo-1/agents/main/wire.jsonl");
        let session = parse_wire(&file, project.path(), &index).expect("session");

        assert_eq!(session.session_id, "ses_demo-1");
        assert_eq!(session.outcome, Outcome::Completed);
        assert!(session.objective.starts_with("implement the checkout flow"));
        // Doctrine: verbatim — credential-looking strings are NOT scrubbed.
        assert!(session.objective.contains("sk-ant-api03-AbCdEfGhIjKlMnOp"));
        assert!(!session.objective.contains("[REDACTED]"));
        assert_eq!(
            session.user_prompts.len(),
            1,
            "prompts: {:?}",
            session.user_prompts
        );
        assert_eq!(session.files_touched, vec!["src/checkout.rs"]);
        assert_eq!(session.failed_approaches.len(), 1);
        assert_eq!(session.tool_events, 1);
        assert_eq!(
            session.conversation_tail.len(),
            3,
            "tail: {:?}",
            session.conversation_tail
        );
        assert_eq!(session.started_at, rfc3339_millis(1784310494250));
        assert_eq!(session.ended_at, rfc3339_millis(1784310500000));

        // cwd filtering: a session for another workDir is excluded.
        let other_home = tempfile::tempdir().expect("other");
        write_index(other_home.path(), &[("ses_demo-1", "/elsewhere")]);
        assert!(parse_wire(
            &file,
            project.path(),
            &read_session_index(other_home.path()),
        )
        .is_none());
    }
}
