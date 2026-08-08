//! Codex transcript reader: `~/.codex/sessions/**/rollout-*.jsonl`.
//!
//! Format (verified against this machine's rollouts): line 1 is
//! `session_meta` (`payload.id`, `payload.cwd`, `payload.timestamp`);
//! events are `response_item` lines whose `payload.type` is `message`
//! (`role` user/developer/assistant, `content[].text`), `function_call`
//! (`name`, `arguments` as a JSON string, `call_id`), or
//! `function_call_output` (`call_id`, `output`). `event_msg` lines carry
//! turn lifecycle (`task_complete`).
//!
//! Skip rules for prompts: developer role, `<environment_context>` /
//! `<permissions …>` wrappers, injected context blocks (`# AGENTS.md
//! instructions for …`, `# Context from my IDE setup:`).

use std::path::Path;

use serde_json::Value;

use super::{
    clean, cwd_matches, event_timestamp, push_unique, shell_write_targets, walk_files, Outcome,
    TranscriptReader, TranscriptSession,
};

/// Codex rollout reader.
pub struct CodexReader;

impl TranscriptReader for CodexReader {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn scan(&self, home: &Path, project_dir: &Path) -> Vec<TranscriptSession> {
        // Two roots: the active store (recursive) and the archived store
        // (flat). Same session id can exist in both — the ACTIVE copy wins.
        let rollout = |p: &Path| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
                .unwrap_or(false)
        };
        let mut files = walk_files(&home.join(".codex/sessions"), &rollout);
        files.extend(walk_files(&home.join(".codex/archived_sessions"), &rollout));
        let mut by_id: std::collections::HashMap<String, TranscriptSession> =
            std::collections::HashMap::new();
        for file in &files {
            let Some(session) = parse_rollout(file, project_dir) else {
                continue;
            };
            // Files are walked active-store-first; keep the first copy seen
            // for a given session id.
            by_id.entry(session.session_id.clone()).or_insert(session);
        }
        by_id.into_values().collect()
    }
}

/// Prompt prefixes that are harness-injected context, not user intent
/// (all observed in real desktop/CLI rollouts).
pub(crate) const INJECTED_PREFIXES: &[&str] = &[
    "<environment_context",
    "<permissions",
    "<recommended_plugins",
    "<turn_aborted",
    "# AGENTS.md instructions",
    "# Context from my IDE setup:",
    "# Files mentioned by the user:",
];

/// Rich-extraction caps (generous inclusion — cut only pure noise).
const OBJECTIVE_MAX: usize = 8000;
const PROMPT_MAX: usize = 2000;
const FAILURE_MAX: usize = 800;
const PROGRESS_SUMMARY_MAX: usize = 6000;
const PROGRESS_SUMMARIES_MAX: usize = 8;
const TAIL_ENTRY_MAX: usize = 1500;
const TAIL_ENTRIES_MAX: usize = 24;
const PLAN_STEP_MAX: usize = 1000;
const MILESTONE_MAX: usize = 1200;
const MILESTONES_MAX: usize = 30;
/// Texts shorter than this are conversational filler, not accomplishments.
const MILESTONE_MIN: usize = 40;
/// An opener repeated at least this many times proves a heartbeat pattern.
const HEARTBEAT_MIN_REPEATS: usize = 3;
/// The known goal-heartbeat opener (seeded as pre-proven boilerplate).
const HEARTBEAT_OPENER_SEED: &str = "work continues under the active goal";

/// Heartbeat-dedup key: the first sentence's first ~60 chars, lowercased.
fn opener_of(text: &str) -> String {
    let trimmed = text.trim();
    let end = trimmed.find(['.', '!', '?', '\n']).unwrap_or(trimmed.len());
    trimmed[..end]
        .chars()
        .take(60)
        .collect::<String>()
        .to_lowercase()
}

fn parse_rollout(file: &Path, project_dir: &Path) -> Option<TranscriptSession> {
    let text = std::fs::read_to_string(file).ok()?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());

    // Line 1: session_meta (tolerate leading blank lines).
    let meta_line = lines.next()?;
    let meta: Value = serde_json::from_str(meta_line).ok()?;
    if meta.get("type").and_then(|v| v.as_str()) != Some("session_meta") {
        return None;
    }
    let payload = meta.get("payload").cloned().unwrap_or(Value::Null);
    let cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if !cwd_matches(&cwd, project_dir) {
        return None;
    }
    let session_id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            file.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string()
        });

    let mut session = TranscriptSession {
        harness: "codex",
        session_id,
        cwd,
        started_at: payload
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        ..Default::default()
    };

    let mut last_function_call: Option<String> = None; // call_id without output
    let mut saw_assistant_message = false;
    let mut last_kind: LastKind = LastKind::Other;
    let mut last_ts = String::new();
    // Last assistant text since the previous task_complete (milestone source).
    let mut last_assistant_text: Option<String> = None;
    // Milestone candidates as (heartbeat-dedup key, cleaned text); the
    // heartbeat filter runs as a second pass over the whole session (A3).
    let mut milestone_candidates: Vec<(String, String)> = Vec::new();

    for line in lines {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(ts) = event_timestamp(&event) {
            last_ts = ts;
        }
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let payload = event.get("payload").cloned().unwrap_or(Value::Null);
        let payload_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match (event_type, payload_type) {
            ("response_item", "message") => {
                let role = payload.get("role").and_then(|v| v.as_str()).unwrap_or("");
                let text = message_text(&payload);
                match role {
                    "user" => {
                        if !is_injected(&text) {
                            let prompt = clean(&text, PROMPT_MAX);
                            if !prompt.is_empty() {
                                if session.objective.is_empty() {
                                    session.objective = clean(&text, OBJECTIVE_MAX);
                                }
                                push_unique(&mut session.user_prompts, prompt.clone());
                                push_tail(&mut session, "user", prompt);
                            }
                        }
                        last_kind = LastKind::UserMessage;
                    }
                    "assistant" => {
                        saw_assistant_message = true;
                        // response_item assistant messages carry the reply
                        // text (264 event_msg agent_message vs 265 of these
                        // in the reference desktop session — response_item
                        // is the complete channel; prefer it, per plan).
                        let reply = clean(&text, TAIL_ENTRY_MAX);
                        if !reply.is_empty() {
                            push_tail(&mut session, "assistant", reply);
                        }
                        if !text.trim().is_empty() {
                            last_assistant_text = Some(text);
                        }
                        last_kind = LastKind::AssistantMessage;
                    }
                    _ => {}
                }
            }
            ("response_item", "function_call") => {
                session.tool_events += 1;
                extract_function_call(&payload, &mut session);
                last_function_call = payload
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                last_kind = LastKind::ToolCall;
            }
            ("response_item", "custom_tool_call") => {
                // Desktop sessions carry tool calls in BOTH forms; the
                // custom form carries raw `input` (not a JSON `arguments`
                // string) — apply_patch arrives this way too.
                session.tool_events += 1;
                extract_custom_tool_call(&payload, &mut session);
                last_function_call = payload
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                last_kind = LastKind::ToolCall;
            }
            ("response_item", "function_call_output")
            | ("response_item", "custom_tool_call_output") => {
                // `output` may be a string or an object — be defensive.
                let output = match payload.get("output") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                if let Some(failure) = failure_excerpt(&output) {
                    push_unique(&mut session.failed_approaches, clean(failure, FAILURE_MAX));
                }
                last_function_call = None;
                last_kind = LastKind::ToolOutput;
            }
            ("compacted", _) => {
                // The harness's own running summaries of progress/decisions:
                // keep ALL of them, newest first, capped per entry and count.
                // Desktop/VS Code variants carry an EMPTY message plus a
                // Fernet-encrypted blob (`encrypted_content`) — unreadable
                // by design; blank payloads are skipped entirely (never an
                // empty string in progress_summaries) and recorded as an
                // extraction loss (B1) so the gap is truthful.
                let message = payload
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if message.trim().is_empty() {
                    session.losses.push(super::LossNote {
                        what: "compaction_summary".to_string(),
                        reason: "encrypted by harness".to_string(),
                    });
                } else {
                    session
                        .progress_summaries
                        .insert(0, clean(message, PROGRESS_SUMMARY_MAX));
                    session.progress_summaries.truncate(PROGRESS_SUMMARIES_MAX);
                }
                last_kind = LastKind::Other;
            }
            ("event_msg", "task_complete") => {
                // Milestone candidate: the assistant text closing this task —
                // an accomplishment summary, when substantial and real.
                if let Some(text) = last_assistant_text.take() {
                    if !is_injected(&text) && text.trim().chars().count() >= MILESTONE_MIN {
                        milestone_candidates.push((opener_of(&text), clean(&text, MILESTONE_MAX)));
                    }
                }
                last_kind = LastKind::TaskComplete;
            }
            _ => {}
        }
    }

    // A3 heartbeat filter: an opener repeated ≥ HEARTBEAT_MIN_REPEATS times
    // across the session marks a boilerplate heartbeat pattern (the seeded
    // goal-heartbeat opener is pre-proven). Every candidate sharing a proven
    // heartbeat opener is dropped; messages appearing once or twice always
    // survive. Order preserved, cap kept.
    let mut opener_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    opener_counts.insert(HEARTBEAT_OPENER_SEED.to_string(), HEARTBEAT_MIN_REPEATS);
    for (opener, _) in &milestone_candidates {
        *opener_counts.entry(opener.clone()).or_insert(0) += 1;
    }
    for (opener, text) in milestone_candidates {
        if opener_counts.get(&opener).copied().unwrap_or(0) >= HEARTBEAT_MIN_REPEATS {
            continue;
        }
        session.milestones.push(text);
        if session.milestones.len() > MILESTONES_MAX {
            session.milestones.remove(0);
        }
    }

    session.ended_at = last_ts;
    session.outcome = classify(
        last_kind,
        last_function_call.is_some(),
        saw_assistant_message,
    );
    Some(session)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastKind {
    AssistantMessage,
    UserMessage,
    ToolCall,
    ToolOutput,
    TaskComplete,
    Other,
}

/// Outcome from the tail of the session (see module docs).
fn classify(last: LastKind, dangling_tool_call: bool, saw_assistant: bool) -> Outcome {
    if last == LastKind::TaskComplete {
        return Outcome::Completed;
    }
    if dangling_tool_call {
        return Outcome::Interrupted;
    }
    if last == LastKind::AssistantMessage {
        return Outcome::Completed;
    }
    if !saw_assistant {
        // No assistant finale at all — the session never completed a turn.
        return Outcome::Interrupted;
    }
    Outcome::Unknown
}

fn message_text(payload: &Value) -> String {
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

pub(crate) fn is_injected(text: &str) -> bool {
    let trimmed = text.trim_start();
    INJECTED_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// Extract file paths from raw apply_patch text (shared by the
/// `function_call` and `custom_tool_call` forms).
fn extract_patch_paths(patch: &str, session: &mut TranscriptSession) {
    for line in patch.lines() {
        for marker in ["*** Update File:", "*** Add File:", "*** Delete File:"] {
            if let Some(path) = line.strip_prefix(marker) {
                push_unique(&mut session.files_touched, clean(path.trim(), 300));
            }
        }
    }
}

/// Append to the conversation tail with a rolling window (chronological).
pub(crate) fn push_tail(session: &mut TranscriptSession, role: &'static str, text: String) {
    session
        .conversation_tail
        .push(super::TailEntry { role, text });
    if session.conversation_tail.len() > TAIL_ENTRIES_MAX {
        session.conversation_tail.remove(0);
    }
}

/// Snapshot the LATEST `update_plan` call: `plan_state` holds every item
/// with its verbatim status; `next_steps` is recomputed from it
/// (pending + in_progress titles). Earlier calls are superseded entirely.
fn apply_update_plan(args: &Value, session: &mut TranscriptSession) {
    let mut plan_state = Vec::new();
    let mut next_steps = Vec::new();
    if let Some(items) = args.get("plan").and_then(|v| v.as_array()) {
        for item in items {
            let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let Some(step) = item.get("step").and_then(|v| v.as_str()) else {
                continue;
            };
            let step = clean(step, PLAN_STEP_MAX);
            if matches!(status, "pending" | "in_progress") {
                next_steps.push(step.clone());
            }
            plan_state.push(super::PlanStep {
                step,
                status: status.to_string(),
            });
        }
    }
    session.plan_state = plan_state;
    session.next_steps = next_steps;
}

/// Extract files/next-steps from one function_call payload.
fn extract_function_call(payload: &Value, session: &mut TranscriptSession) {
    let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = payload
        .get("arguments")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    match name {
        "apply_patch" => {
            let patch = args
                .get("input")
                .or_else(|| args.get("patch"))
                .and_then(|v| v.as_str())
                .unwrap_or(arguments);
            extract_patch_paths(patch, session);
        }
        "write_file" | "edit_file" | "create_file" | "write" | "edit" => {
            for key in ["path", "file_path", "filename"] {
                if let Some(path) = args.get(key).and_then(|v| v.as_str()) {
                    push_unique(&mut session.files_touched, clean(path, 300));
                }
            }
        }
        "update_plan" => apply_update_plan(&args, session),
        _ => {
            // Shell-style calls (exec_command, shell_command, write_stdin):
            // extract write targets only — never file content.
            let command = args
                .get("cmd")
                .or_else(|| args.get("command"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !command.is_empty() {
                for target in shell_write_targets(command) {
                    push_unique(&mut session.files_touched, clean(&target, 300));
                }
            }
        }
    }
}

/// Extract files/next-steps from one `custom_tool_call` payload — the
/// desktop form: raw `input` (a string), NOT a JSON `arguments` blob.
fn extract_custom_tool_call(payload: &Value, session: &mut TranscriptSession) {
    let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let input = payload.get("input").and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "apply_patch" => extract_patch_paths(input, session),
        "write_file" | "edit_file" | "create_file" | "write" | "edit" => {
            // Defensive: these may carry raw JSON in `input` too.
            let args: Value = serde_json::from_str(input).unwrap_or(Value::Null);
            for key in ["path", "file_path", "filename"] {
                if let Some(path) = args.get(key).and_then(|v| v.as_str()) {
                    push_unique(&mut session.files_touched, clean(path, 300));
                }
            }
        }
        "update_plan" => {
            let args: Value = serde_json::from_str(input).unwrap_or(Value::Null);
            apply_update_plan(&args, session);
        }
        _ => {
            for target in shell_write_targets(input) {
                push_unique(&mut session.files_touched, clean(&target, 300));
            }
        }
    }
}

/// Non-zero exits and hard error shapes from a tool output.
pub(crate) fn failure_excerpt(output: &str) -> Option<&str> {
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("Process exited with code ") {
            if !rest.starts_with('0') {
                return Some(line);
            }
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("Traceback (most recent call last)")
            || trimmed.starts_with("Error:")
            || trimmed.starts_with("error:")
        {
            return Some(line);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn write_rollout(dir: &Path, name: &str, lines: &[&str]) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let mut file = std::fs::File::create(&path).expect("create");
        for line in lines {
            writeln!(file, "{line}").expect("write");
        }
        path
    }

    fn project() -> tempfile::TempDir {
        tempfile::tempdir().expect("project")
    }

    fn meta(cwd: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-07-01T09:59:00Z","type":"session_meta","payload":{{"id":"s-1","timestamp":"2026-07-01T10:00:00Z","cwd":"{cwd}","originator":"codex_cli","cli_version":"0.1","model_provider":"openai"}}}}"#
        )
    }

    #[test]
    fn codex_reader_extracts_full_session() {
        let project = project();
        let home = tempfile::tempdir().expect("home");
        let rollout = write_rollout(
            &home.path().join(".codex/sessions/2026/07/01"),
            "rollout-2026-07-01T10-00-00-s-1.jsonl",
            &[
                &meta(project.path().to_str().expect("utf8")),
                r#"{"timestamp":"2026-07-01T10:00:01Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>…</permissions instructions>"}]}}"#,
                r#"{"timestamp":"2026-07-01T10:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>cwd</environment_context>"}]}}"#,
                r#"{"timestamp":"2026-07-01T10:00:03Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"create the handoff notes doc (token sk-proj-AbCdEfGhIjKlMnOpQrStUvWx)"}]}}"#,
                r#"{"timestamp":"2026-07-01T10:00:04Z","type":"response_item","payload":{"type":"function_call","name":"update_plan","arguments":"{\"plan\":[{\"step\":\"write the doc\",\"status\":\"completed\"},{\"step\":\"review with the user\",\"status\":\"pending\"}]}","call_id":"c1"}}"#,
                r#"{"timestamp":"2026-07-01T10:00:05Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cat > docs/handoff-notes.md <<'EOF'\"}","call_id":"c2"}}"#,
                r#"{"timestamp":"2026-07-01T10:00:06Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c2","output":"Process exited with code 0"}}"#,
                r#"{"timestamp":"2026-07-01T10:00:07Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"pytest\"}","call_id":"c3"}}"#,
                r#"{"timestamp":"2026-07-01T10:00:08Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c3","output":"Process exited with code 1\nFAILED tests/test_x.py"}}"#,
                r#"{"timestamp":"2026-07-01T10:00:09Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"fixed, rerunning"}]}}"#,
                r#"{"timestamp":"2026-07-01T10:00:10Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}"#,
            ],
        );
        let session = parse_rollout(&rollout, project.path()).expect("session");

        assert_eq!(session.session_id, "s-1");
        assert_eq!(session.outcome, Outcome::Completed);
        // Objective = first real prompt. DOCTRINE: no secret-pattern
        // scrubbing — a credential-looking string passes through VERBATIM.
        assert!(session
            .objective
            .starts_with("create the handoff notes doc"));
        assert!(session
            .objective
            .contains("sk-proj-AbCdEfGhIjKlMnOpQrStUvWx"));
        assert!(!session.objective.contains("[REDACTED]"));
        assert_eq!(session.user_prompts.len(), 1);
        // Files + failures + next steps + tool events.
        assert_eq!(session.files_touched, vec!["docs/handoff-notes.md"]);
        assert_eq!(session.failed_approaches.len(), 1);
        assert!(session.failed_approaches[0].contains("exited with code 1"));
        assert_eq!(session.next_steps, vec!["review with the user"]);
        assert_eq!(session.tool_events, 3);
        assert_eq!(session.started_at, "2026-07-01T10:00:00Z");
        assert_eq!(session.ended_at, "2026-07-01T10:00:10Z");
    }

    #[test]
    fn codex_reader_interrupted_tail_and_cwd_filter() {
        let project = project();
        let home = tempfile::tempdir().expect("home");
        // Interrupted: last event is a function_call with no output.
        let interrupted = write_rollout(
            &home.path().join(".codex/sessions/2026/07/02"),
            "rollout-2026-07-02T10-00-00-s-2.jsonl",
            &[
                &meta(project.path().to_str().expect("utf8")).replace("s-1", "s-2"),
                r#"{"timestamp":"2026-07-02T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"do the thing"}]}}"#,
                r#"{"timestamp":"2026-07-02T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"make\"}","call_id":"c9"}}"#,
            ],
        );
        // Other project entirely → excluded.
        let other = write_rollout(
            &home.path().join(".codex/sessions/2026/07/03"),
            "rollout-2026-07-03T10-00-00-s-3.jsonl",
            &[&meta("/elsewhere").replace("s-1", "s-3")],
        );
        let session = parse_rollout(&interrupted, project.path()).expect("interrupted");
        assert_eq!(session.outcome, Outcome::Interrupted);
        assert!(parse_rollout(&other, project.path()).is_none());

        // scan() merges both files but filters by cwd.
        let sessions = CodexReader.scan(home.path(), project.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "s-2");
    }

    #[test]
    fn codex_reader_nested_cwd_included() {
        let project = project();
        let nested = project.path().join("subdir");
        std::fs::create_dir_all(&nested).expect("mkdir");
        let home = tempfile::tempdir().expect("home");
        let rollout = write_rollout(
            &home.path().join(".codex/sessions/2026/07/04"),
            "rollout-2026-07-04T10-00-00-s-4.jsonl",
            &[&meta(nested.to_str().expect("utf8")).replace("s-1", "s-4")],
        );
        assert!(parse_rollout(&rollout, project.path()).is_some());
    }

    #[test]
    fn desktop_junk_prefixes_do_not_become_objective() {
        // The Laiq bug: `<recommended_plugins>` and `# Files mentioned…`
        // wrappers preceded the real objective in the desktop session.
        let project = project();
        let home = tempfile::tempdir().expect("home");
        let rollout = write_rollout(
            &home.path().join(".codex/sessions/2026/07/26"),
            "rollout-2026-07-26T10-00-00-s-5.jsonl",
            &[
                &meta(project.path().to_str().expect("utf8")).replace("s-1", "s-5"),
                r#"{"timestamp":"2026-07-26T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<recommended_plugins>\nHere is a list of plugins that are available but not installed"}]}}"#,
                r##"{"timestamp":"2026-07-26T10:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# Files mentioned by the user:\n- docs/plan.md"}]}}"##,
                r#"{"timestamp":"2026-07-26T10:00:03Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"PLEASE IMPLEMENT THIS PLAN:\n# LAIQ Production Marketplace Implementation"}]}}"#,
                r#"{"timestamp":"2026-07-26T10:00:04Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"on it"}]}}"#,
            ],
        );
        let session = parse_rollout(&rollout, project.path()).expect("session");
        assert_eq!(
            session.objective,
            "PLEASE IMPLEMENT THIS PLAN:\n# LAIQ Production Marketplace Implementation"
        );
        assert_eq!(
            session.user_prompts.len(),
            1,
            "prompts: {:?}",
            session.user_prompts
        );
    }

    #[test]
    fn turn_aborted_marker_is_not_an_objective() {
        let project = project();
        let home = tempfile::tempdir().expect("home");
        let aborted = r#"{"timestamp":"2026-07-26T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<turn_aborted>\nThe user interrupted the previous turn on purpose. Any running unified exec processes may still be running in the background."}]}}"#;
        // Only the control marker → EMPTY objective (truth contract: empty,
        // never junk).
        let only_marker = write_rollout(
            &home.path().join(".codex/sessions/2026/07/26"),
            "rollout-2026-07-26T10-00-00-s-9.jsonl",
            &[
                &meta(project.path().to_str().expect("utf8")).replace("s-1", "s-9"),
                aborted,
            ],
        );
        let session = parse_rollout(&only_marker, project.path()).expect("session");
        assert!(
            session.objective.is_empty(),
            "objective: {:?}",
            session.objective
        );
        assert!(session.user_prompts.is_empty());

        // Marker followed by a real prompt → the real one wins.
        let with_prompt = write_rollout(
            &home.path().join(".codex/sessions/2026/07/27"),
            "rollout-2026-07-27T10-00-00-s-10.jsonl",
            &[
                &meta(project.path().to_str().expect("utf8")).replace("s-1", "s-10"),
                aborted,
                r#"{"timestamp":"2026-07-27T10:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"actually, ship the marketplace plan"}]}}"#,
            ],
        );
        let session = parse_rollout(&with_prompt, project.path()).expect("session");
        assert_eq!(session.objective, "actually, ship the marketplace plan");
    }

    #[test]
    fn custom_tool_call_apply_patch_extracts_files_and_counts() {
        let project = project();
        let home = tempfile::tempdir().expect("home");
        let rollout = write_rollout(
            &home.path().join(".codex/sessions/2026/07/26"),
            "rollout-2026-07-26T10-00-00-s-6.jsonl",
            &[
                &meta(project.path().to_str().expect("utf8")).replace("s-1", "s-6"),
                r#"{"timestamp":"2026-07-26T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"build it"}]}}"#,
                r#"{"timestamp":"2026-07-26T10:00:02Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n*** Add File: src/lib.rs\n*** End Patch","call_id":"cc1"}}"#,
                r#"{"timestamp":"2026-07-26T10:00:03Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"cc1","output":{"text":"Patch applied"}}}"#,
                r#"{"timestamp":"2026-07-26T10:00:04Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Delete File: src/old.rs\n*** End Patch","call_id":"cc2"}}"#,
                r#"{"timestamp":"2026-07-26T10:00:05Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"cc2","output":"Process exited with code 1\napply_patch failed"}}"#,
                r#"{"timestamp":"2026-07-26T10:00:06Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}"#,
            ],
        );
        let session = parse_rollout(&rollout, project.path()).expect("session");
        assert_eq!(session.tool_events, 2);
        assert_eq!(
            session.files_touched,
            vec!["src/main.rs", "src/lib.rs", "src/old.rs"],
            "files: {:?}",
            session.files_touched
        );
        // Object-shaped output is tolerated; the string error line matches.
        assert_eq!(session.failed_approaches.len(), 1);
        assert!(session.failed_approaches[0].contains("exited with code 1"));
        assert_eq!(session.outcome, Outcome::Completed);
    }

    #[test]
    fn archived_sessions_scanned_and_active_copy_wins() {
        let project = project();
        let home = tempfile::tempdir().expect("home");
        // Session only in the archived store → found.
        write_rollout(
            &home.path().join(".codex/archived_sessions"),
            "rollout-2026-07-20T10-00-00-arch-only.jsonl",
            &[&meta(project.path().to_str().expect("utf8")).replace("s-1", "arch-only")],
        );
        // Same id in BOTH stores → one session, and the ACTIVE copy wins
        // (distinguished by a different objective in each copy).
        write_rollout(
            &home.path().join(".codex/sessions/2026/07/21"),
            "rollout-2026-07-21T10-00-00-dup.jsonl",
            &[
                &meta(project.path().to_str().expect("utf8")).replace("s-1", "dup"),
                r#"{"timestamp":"2026-07-21T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"ACTIVE copy objective"}]}}"#,
            ],
        );
        write_rollout(
            &home.path().join(".codex/archived_sessions"),
            "rollout-2026-07-21T10-00-00-dup.jsonl",
            &[
                &meta(project.path().to_str().expect("utf8")).replace("s-1", "dup"),
                r#"{"timestamp":"2026-07-21T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"archived copy objective"}]}}"#,
            ],
        );
        let sessions = CodexReader.scan(home.path(), project.path());
        assert_eq!(
            sessions.len(),
            2,
            "sessions: {:?}",
            sessions.iter().map(|s| &s.session_id).collect::<Vec<_>>()
        );
        let dup = sessions
            .iter()
            .find(|s| s.session_id == "dup")
            .expect("dup session");
        assert_eq!(dup.objective, "ACTIVE copy objective");
        assert!(sessions.iter().any(|s| s.session_id == "arch-only"));
    }

    #[test]
    fn compacted_events_populate_summary_from_the_last_one() {
        let project = project();
        let home = tempfile::tempdir().expect("home");
        let rollout = write_rollout(
            &home.path().join(".codex/sessions/2026/07/26"),
            "rollout-2026-07-26T10-00-00-s-7.jsonl",
            &[
                &meta(project.path().to_str().expect("utf8")).replace("s-1", "s-7"),
                r#"{"timestamp":"2026-07-26T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"do the work"}]}}"#,
                r#"{"timestamp":"2026-07-26T10:00:02Z","type":"compacted","payload":{"message":"EARLIER summary — superseded"}}"#,
                r#"{"timestamp":"2026-07-26T10:00:03Z","type":"compacted","payload":{"message":"LATEST summary: 3 files written, plan at step 2 of 4"}}"#,
                r#"{"timestamp":"2026-07-26T10:00:04Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"continuing"}]}}"#,
            ],
        );
        let session = parse_rollout(&rollout, project.path()).expect("session");
        // ALL compacted summaries kept, newest first.
        assert_eq!(
            session.progress_summaries,
            vec![
                "LATEST summary: 3 files written, plan at step 2 of 4".to_string(),
                "EARLIER summary — superseded".to_string(),
            ]
        );
        // No compacted events → empty, never invented.
        let plain = write_rollout(
            &home.path().join(".codex/sessions/2026/07/27"),
            "rollout-2026-07-27T10-00-00-s-8.jsonl",
            &[&meta(project.path().to_str().expect("utf8")).replace("s-1", "s-8")],
        );
        let plain = parse_rollout(&plain, project.path()).expect("plain");
        assert!(plain.progress_summaries.is_empty());

        // Desktop/VS Code variants: EMPTY message (encrypted blob alongside)
        // and whitespace-only are skipped — never an empty string stored.
        let encrypted = write_rollout(
            &home.path().join(".codex/sessions/2026/07/28"),
            "rollout-2026-07-28T10-00-00-s-9.jsonl",
            &[
                &meta(project.path().to_str().expect("utf8")).replace("s-1", "s-9"),
                r#"{"timestamp":"2026-07-28T10:00:02Z","type":"compacted","payload":{"message":""}}"#,
                r#"{"timestamp":"2026-07-28T10:00:03Z","type":"compacted","payload":{"message":"   \n  "}}"#,
                r#"{"timestamp":"2026-07-28T10:00:04Z","type":"compaction","payload":{"encrypted_content":"gAAAAABmF6b2NlZDE"}}"#,
                r#"{"timestamp":"2026-07-28T10:00:05Z","type":"compacted","payload":{"message":"real summary survives"}}"#,
            ],
        );
        let session = parse_rollout(&encrypted, project.path()).expect("session");
        assert_eq!(session.progress_summaries, vec!["real summary survives"]);
        assert!(session
            .progress_summaries
            .iter()
            .all(|s| !s.trim().is_empty()));
    }

    #[test]
    fn milestones_captured_at_task_complete_boundaries() {
        let project = project();
        let home = tempfile::tempdir().expect("home");
        let mut lines = vec![
            meta(project.path().to_str().expect("utf8")).replace("s-1", "s-14"),
            r#"{"timestamp":"2026-07-26T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"do three tasks"}]}}"#.to_string(),
        ];
        // Task 1: substantial assistant text → milestone.
        lines.push(r#"{"timestamp":"2026-07-26T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Fixed. The error was Celery reusing asyncpg connections across event loops. Both suites completed successfully."}]}}"#.to_string());
        lines.push(r#"{"timestamp":"2026-07-26T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}"#.to_string());
        // Task 2: noise text (< 40 chars) → filtered.
        lines.push(r#"{"timestamp":"2026-07-26T10:00:04Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}}"#.to_string());
        lines.push(r#"{"timestamp":"2026-07-26T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t-2"}}"#.to_string());
        // Task 3: no assistant text since t-2 → nothing.
        lines.push(r#"{"timestamp":"2026-07-26T10:00:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t-3"}}"#.to_string());
        // Task 4: another real milestone.
        lines.push(r#"{"timestamp":"2026-07-26T10:00:07Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done. Migrated the marketplace schema and verified every endpoint against the staging database snapshot."}]}}"#.to_string());
        lines.push(r#"{"timestamp":"2026-07-26T10:00:08Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t-4"}}"#.to_string());
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let rollout = write_rollout(
            &home.path().join(".codex/sessions/2026/07/26"),
            "rollout-2026-07-26T10-00-00-s-14.jsonl",
            &refs,
        );
        let session = parse_rollout(&rollout, project.path()).expect("session");
        assert_eq!(
            session.milestones.len(),
            2,
            "milestones: {:?}",
            session.milestones
        );
        assert!(session.milestones[0].starts_with("Fixed. The error was Celery"));
        assert!(session.milestones[1].starts_with("Done. Migrated the marketplace schema"));

        // Cap: 40 tasks → LAST 30 kept, oldest-of-kept first.
        let mut lines = vec![meta(project.path().to_str().expect("utf8")).replace("s-1", "s-15")];
        for i in 1..=40 {
            lines.push(format!(
                r#"{{"timestamp":"2026-07-26T10:{i:02}:00Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Completed task number {i} with a sufficiently long summary text."}}]}}}}"#
            ));
            lines.push(format!(
                r#"{{"timestamp":"2026-07-26T10:{i:02}:30Z","type":"event_msg","payload":{{"type":"task_complete","turn_id":"t-{i}"}}}}"#
            ));
        }
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let rollout = write_rollout(
            &home.path().join(".codex/sessions/2026/07/27"),
            "rollout-2026-07-27T10-00-00-s-15.jsonl",
            &refs,
        );
        let session = parse_rollout(&rollout, project.path()).expect("session");
        assert_eq!(session.milestones.len(), 30);
        assert!(
            session.milestones[0].contains("task number 11"),
            "oldest kept: {}",
            session.milestones[0]
        );
        assert!(session.milestones[29].contains("task number 40"));
    }

    #[test]
    fn milestone_heartbeat_filter_drops_boilerplate_openers() {
        let project = project();
        let home = tempfile::tempdir().expect("home");
        let mut lines = vec![
            meta(project.path().to_str().expect("utf8")).replace("s-1", "s-16"),
            r#"{"timestamp":"2026-07-26T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"long-running goal work"}]}}"#.to_string(),
        ];
        // 5 heartbeat messages (seeded opener + a session-proven one)…
        for i in 1..=3 {
            lines.push(format!(
                r#"{{"timestamp":"2026-07-26T10:0{i}:00Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Work continues under the active goal. Heartbeat {i} details vary here."}}]}}}}"#
            ));
            lines.push(format!(
                r#"{{"timestamp":"2026-07-26T10:0{i}:05Z","type":"event_msg","payload":{{"type":"task_complete","turn_id":"h-{i}"}}}}"#
            ));
        }
        for i in 4..=6 {
            lines.push(format!(
                r#"{{"timestamp":"2026-07-26T10:0{i}:00Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Still pushing the same rock up the same hill toward the goal state — detail variant {i}."}}]}}}}"#
            ));
            lines.push(format!(
                r#"{{"timestamp":"2026-07-26T10:0{i}:05Z","type":"event_msg","payload":{{"type":"task_complete","turn_id":"h-{i}"}}}}"#
            ));
        }
        // …plus two REAL accomplishments with distinct openers.
        lines.push(r#"{"timestamp":"2026-07-26T10:06:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Fixed. The Celery asyncpg event-loop reuse bug is resolved and the full suite passes."}]}}"#.to_string());
        lines.push(r#"{"timestamp":"2026-07-26T10:06:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t-6"}}"#.to_string());
        lines.push(r#"{"timestamp":"2026-07-26T10:07:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Shipped the marketplace schema migration; every endpoint verified against staging data."}]}}"#.to_string());
        lines.push(r#"{"timestamp":"2026-07-26T10:07:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t-7"}}"#.to_string());
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let rollout = write_rollout(
            &home.path().join(".codex/sessions/2026/07/26"),
            "rollout-2026-07-26T10-00-00-s-16.jsonl",
            &refs,
        );
        let session = parse_rollout(&rollout, project.path()).expect("session");
        // Heartbeats gone (seeded opener AND the 3x-proven one), real kept in order.
        assert_eq!(
            session.milestones.len(),
            2,
            "milestones: {:?}",
            session.milestones
        );
        assert!(session.milestones[0].starts_with("Fixed. The Celery"));
        assert!(session.milestones[1].starts_with("Shipped the marketplace schema"));
        assert!(!session
            .milestones
            .iter()
            .any(|m| m.contains("Work continues under the active goal")));
        assert!(!session
            .milestones
            .iter()
            .any(|m| m.contains("Still pushing the same rock")));
    }

    #[test]
    fn latest_update_plan_supersedes_earlier_snapshots() {
        let project = project();
        let home = tempfile::tempdir().expect("home");
        let rollout = write_rollout(
            &home.path().join(".codex/sessions/2026/07/26"),
            "rollout-2026-07-26T10-00-00-s-11.jsonl",
            &[
                &meta(project.path().to_str().expect("utf8")).replace("s-1", "s-11"),
                r#"{"timestamp":"2026-07-26T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"build the thing"}]}}"#,
                r#"{"timestamp":"2026-07-26T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"update_plan","arguments":"{\"plan\":[{\"step\":\"old step A\",\"status\":\"pending\"},{\"step\":\"old step B\",\"status\":\"pending\"}]}","call_id":"c1"}}"#,
                r#"{"timestamp":"2026-07-26T10:00:03Z","type":"response_item","payload":{"type":"function_call","name":"update_plan","arguments":"{\"plan\":[{\"step\":\"write the code\",\"status\":\"completed\"},{\"step\":\"run the tests\",\"status\":\"in_progress\"},{\"step\":\"ship it\",\"status\":\"pending\"}]}","call_id":"c2"}}"#,
            ],
        );
        let session = parse_rollout(&rollout, project.path()).expect("session");
        // Only the LATEST snapshot, statuses verbatim.
        assert_eq!(
            session.plan_state.len(),
            3,
            "plan: {:?}",
            session.plan_state
        );
        assert_eq!(session.plan_state[0].step, "write the code");
        assert_eq!(session.plan_state[0].status, "completed");
        assert_eq!(session.plan_state[1].status, "in_progress");
        assert_eq!(session.plan_state[2].step, "ship it");
        assert_eq!(session.plan_state[2].status, "pending");
        // next_steps = latest pending + in_progress titles (old steps gone).
        assert_eq!(session.next_steps, vec!["run the tests", "ship it"]);
    }

    #[test]
    fn conversation_tail_is_chronological_last_window() {
        let project = project();
        let home = tempfile::tempdir().expect("home");
        let mut lines = vec![meta(project.path().to_str().expect("utf8")).replace("s-1", "s-12")];
        // 15 user+assistant pairs = 30 messages → tail keeps the LAST 24.
        for i in 1..=15 {
            lines.push(format!(
                r#"{{"timestamp":"2026-07-26T10:{i:02}:00Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"question {i}"}}]}}}}"#
            ));
            lines.push(format!(
                r#"{{"timestamp":"2026-07-26T10:{i:02}:30Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"answer {i}"}}]}}}}"#
            ));
        }
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let rollout = write_rollout(
            &home.path().join(".codex/sessions/2026/07/26"),
            "rollout-2026-07-26T10-00-00-s-12.jsonl",
            &refs,
        );
        let session = parse_rollout(&rollout, project.path()).expect("session");
        assert_eq!(session.conversation_tail.len(), 24);
        // Chronological: first kept = question 4, last = answer 15.
        assert_eq!(session.conversation_tail[0].role, "user");
        assert_eq!(session.conversation_tail[0].text, "question 4");
        assert_eq!(session.conversation_tail[23].role, "assistant");
        assert_eq!(session.conversation_tail[23].text, "answer 15");
    }

    #[test]
    fn rich_caps_respected_and_lists_untruncated() {
        let project = project();
        let home = tempfile::tempdir().expect("home");
        let long_prompt = "x".repeat(9000);
        let mut patch = String::from("*** Begin Patch\n");
        for i in 1..=40 {
            patch.push_str(&format!("*** Add File: src/file_{i}.rs\n"));
        }
        patch.push_str("*** End Patch");
        let patch_json = patch
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        let mut lines = vec![
            meta(project.path().to_str().expect("utf8")).replace("s-1", "s-13"),
            format!(
                r#"{{"timestamp":"2026-07-26T10:00:01Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"{long_prompt}"}}]}}}}"#
            ),
            format!(
                r#"{{"timestamp":"2026-07-26T10:00:02Z","type":"response_item","payload":{{"type":"custom_tool_call","name":"apply_patch","input":"{patch_json}","call_id":"cc1"}}}}"#
            ),
        ];
        // 10 compacted events → 8 kept, newest first.
        for i in 1..=10 {
            lines.push(format!(
                r#"{{"timestamp":"2026-07-26T10:{i:02}:00Z","type":"compacted","payload":{{"message":"summary {i}"}}}}"#
            ));
        }
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let rollout = write_rollout(
            &home.path().join(".codex/sessions/2026/07/26"),
            "rollout-2026-07-26T10-00-00-s-13.jsonl",
            &refs,
        );
        let session = parse_rollout(&rollout, project.path()).expect("session");
        // Objective capped at 8000 (input was 9000).
        assert_eq!(session.objective.chars().count(), 8000);
        // File list COMPLETE (no count cap in the struct).
        assert_eq!(session.files_touched.len(), 40);
        // Summaries capped at 8, newest first.
        assert_eq!(session.progress_summaries.len(), 8);
        assert_eq!(session.progress_summaries[0], "summary 10");
        assert_eq!(session.progress_summaries[7], "summary 3");
    }
}
