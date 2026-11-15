//! DSH transcript reader: `$DSH_HOME/sessions/<projectKey>/<encoded-id>/session.jsonl`
//! (default `~/.dsh/sessions/...`).
//!
//! Format (verified against deepseek-harness `packages/core/session` and
//! `packages/session/session-persistence-jsonl`, read-only references): line
//! 1 is `{type:"session", version:0, id, createdAt (epoch ms), cwd?,
//! delegationDepth, …}`; events are `{type, seq (contiguous from 0), time
//! (epoch ms), data}` — `turn/start|end`, `step/start|end`, `user/message`,
//! `assistant/chunk`, `assistant/message`, `tool/call`, `tool/result`,
//! `todo/write`, `request/header`, `request/context`, `session/end-seed`.
//! A final line without a trailing newline is a torn tail (ignored, like
//! DSH's own scanner); a seq gap truncates the log (DSH refuses to
//! reconstruct past it — so do we).
//!
//! Both the plain `.jsonl` and the zstd `.jsonl.zstd` artifacts current DSH
//! writes are read. `assistant/chunk` stream deltas (including their packed
//! `text-chunks` / `reasoning-chunks` / `tool-call-chunks` storage rows) are
//! redundant with the assembled `assistant/message` — counted, never
//! reconstructed.

use std::path::{Path, PathBuf};

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
const TAIL_ENTRY_MAX: usize = 1500;

/// DSH session reader.
pub struct DshReader;

/// Session artifacts under the DSH store: `(plain jsonl, zstd-compressed)`.
/// Both forms are read; the split is kept so import accounting can say which
/// artifacts failed to decode.
pub(crate) fn session_files(home: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let root = paths::dsh_root(home).join("sessions");
    let plain = walk_files(&root, &|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.ends_with(".jsonl"))
            .unwrap_or(false)
    });
    let zstd = walk_files(&root, &|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.ends_with(".jsonl.zstd"))
            .unwrap_or(false)
    });
    (plain, zstd)
}

/// A raw parsed DSH session log: header line plus the contiguous event
/// prefix as verbatim JSON (chunk storage rows are expanded nowhere — they
/// are counted in `skipped_chunks`; a torn tail sets `torn`).
pub(crate) struct RawSession {
    pub(crate) header: Value,
    pub(crate) events: Vec<Value>,
    pub(crate) skipped_chunks: usize,
    pub(crate) torn: bool,
    /// Set when the contiguous prefix ended early (seq gap / corrupt line).
    pub(crate) truncated: bool,
}

/// How many events a packed chunk storage row stands for (`0` when the value
/// is not a chunk row).
fn chunk_row_members(value: &Value) -> Option<usize> {
    let kind = value.get("type").and_then(|v| v.as_str())?;
    let data = value.get("data")?;
    let members = match kind {
        "text-chunks" | "reasoning-chunks" => data.get("texts")?.as_array()?.len(),
        "tool-call-chunks" => data.get("args")?.as_array()?.len(),
        _ => return None,
    };
    let seq0 = value.get("seq0").and_then(|v| v.as_u64())? as usize;
    Some(seq0 + members) // next expected seq after this row
}

/// Read one DSH session artifact as UTF-8 text, transparently decoding the
/// zstd frames DSH writes by default. `None` when the file is unreadable or
/// not a valid zstd stream.
fn read_session_text(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if path.extension().and_then(|e| e.to_str()) == Some("zstd") {
        let decoded = decode_zstd_frames(bytes)?;
        return String::from_utf8(decoded).ok();
    }
    String::from_utf8(bytes).ok()
}

/// Decode every zstd frame in `bytes`. DSH appends compressed batches, so a
/// session artifact is commonly several concatenated frames; `ruzstd`
/// decodes one frame per decoder and a single `decode_all` would silently
/// stop at the first boundary.
fn decode_zstd_frames(bytes: Vec<u8>) -> Option<Vec<u8>> {
    let mut cursor = std::io::Cursor::new(bytes);
    let mut out = Vec::new();
    loop {
        if cursor.position() >= cursor.get_ref().len() as u64 {
            break;
        }
        let mut decoder = ruzstd::decoding::StreamingDecoder::new(&mut cursor).ok()?;
        std::io::copy(&mut decoder, &mut out).ok()?;
    }
    Some(out)
}

/// True when `path` is a zstd-compressed DSH session artifact.
pub(crate) fn is_zstd_session(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("zstd")
}

/// Parse a DSH session log. `None` when line 1 is not a supported session
/// header (v0 history or the current v4 store; a foreign version is DSH's
/// own refusal — we mirror it).
pub(crate) fn parse_session_file(path: &Path) -> Option<RawSession> {
    let text = read_session_text(path)?;
    // `str::lines` yields a final partial line when the file lacks a trailing
    // newline — that is DSH's torn tail, never a complete record: drop it.
    let torn = !text.ends_with('\n');
    let mut lines: Vec<&str> = text.lines().collect();
    if torn {
        lines.pop();
    }
    let header: Value = serde_json::from_str(lines.first()?).ok()?;
    if header.get("type").and_then(|v| v.as_str()) != Some("session") {
        return None;
    }
    // v0 is the historical format this reader was written against; v4 is the
    // current store. The event envelope is unchanged between them (typed
    // `data` records with contiguous `seq`), so both decode through the same
    // path. A foreign version is DSH's own refusal boundary — mirror it.
    match header.get("version").and_then(|v| v.as_u64()) {
        Some(0..=4) => {}
        _ => {
            tracing::warn!(
                "dsh session {}: unsupported format version — skipped",
                path.display()
            );
            return None;
        }
    }
    let mut events = Vec::new();
    let mut skipped_chunks = 0;
    let mut truncated = false;
    let mut expect_seq = 0usize;
    for line in &lines[1..] {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            truncated = true;
            break; // DSH truncates at the first corrupt committed line
        };
        if let Some(next_seq) = chunk_row_members(&value) {
            if value.get("seq0").and_then(|v| v.as_u64()) != Some(expect_seq as u64) {
                truncated = true;
                break;
            }
            skipped_chunks += next_seq - expect_seq;
            expect_seq = next_seq;
            continue;
        }
        if value.get("seq").and_then(|v| v.as_u64()) != Some(expect_seq as u64) {
            truncated = true; // seq gap — DSH refuses to reconstruct past it
            break;
        }
        expect_seq += 1;
        // Stream deltas stay out of the event list too — the assembled
        // assistant/message carries their text; the count stays truthful.
        if value.get("type").and_then(|v| v.as_str()) == Some("assistant/chunk") {
            skipped_chunks += 1;
            continue;
        }
        events.push(value);
    }
    Some(RawSession {
        header,
        events,
        skipped_chunks,
        torn,
        truncated,
    })
}

/// Verbatim text of a DSH content-block array (`text` blocks only —
/// reasoning, images, and tool-call/result blocks carry no plain text here).
pub(crate) fn blocks_text(content: Option<&Value>) -> String {
    let mut parts = Vec::new();
    if let Some(Value::Array(blocks)) = content {
        for block in blocks {
            if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    parts.push(text);
                }
            }
        }
    }
    parts.join("\n")
}

/// File-write extraction from one `tool/call` arguments JSON string.
fn extract_files(arguments: &str, session: &mut TranscriptSession) {
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    for key in ["path", "file_path", "filename"] {
        if let Some(path) = args.get(key).and_then(|v| v.as_str()) {
            push_unique(&mut session.files_touched, clean(path, 300));
        }
    }
    let command = args
        .get("command")
        .or_else(|| args.get("cmd"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    for target in shell_write_targets(command) {
        push_unique(&mut session.files_touched, clean(&target, 300));
    }
}

/// Map a parsed DSH session into the summary tier. `None` when the header's
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
    let started_at = header
        .get("createdAt")
        .and_then(|v| v.as_i64())
        .map(super::kimi::rfc3339_millis)
        .unwrap_or_default();
    let mut session = TranscriptSession {
        harness: "dsh",
        session_id,
        cwd,
        started_at,
        ..Default::default()
    };
    if raw.torn {
        session.losses.push(LossNote {
            what: "torn tail".into(),
            reason: "final record without newline (crash cut) — ignored, like DSH".into(),
        });
    }
    if raw.truncated {
        session.losses.push(LossNote {
            what: "truncated log".into(),
            reason: "seq gap or corrupt line — the event prefix before it is intact".into(),
        });
    }
    if raw.skipped_chunks > 0 {
        session.losses.push(LossNote {
            what: format!("{} assistant/chunk events", raw.skipped_chunks),
            reason: "stream deltas redundant with assistant/message".into(),
        });
    }

    let mut open_turn = false;
    let mut completed = false;
    let mut interrupted = false;
    let mut last_ms: Option<i64> = None;
    let mut noted: Vec<String> = Vec::new();

    for event in &raw.events {
        if let Some(ms) = event.get("time").and_then(|v| v.as_i64()) {
            last_ms = Some(ms);
        }
        let kind = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let data = event.get("data").cloned().unwrap_or(Value::Null);
        match kind {
            "turn/start" => open_turn = true,
            "turn/end" => {
                open_turn = false;
                match data.pointer("/reason/kind").and_then(|v| v.as_str()) {
                    Some("completed") => completed = true,
                    Some("interrupted") | Some("aborted") => interrupted = true,
                    Some("error") => {
                        if let Some(failure) = data
                            .pointer("/reason/error/message")
                            .and_then(|v| v.as_str())
                        {
                            push_unique(
                                &mut session.failed_approaches,
                                clean(failure, FAILURE_MAX),
                            );
                        }
                    }
                    _ => {}
                }
            }
            "user/message" => {
                let source = data.pointer("/source/kind").and_then(|v| v.as_str());
                let text = blocks_text(data.get("content"));
                let text = super::codex::user_assertion(&text).unwrap_or_default();
                if source == Some("user") && !super::codex::is_injected(text) {
                    let prompt = clean(text, PROMPT_MAX);
                    if !prompt.is_empty() {
                        if session.objective.is_empty() {
                            session.objective = clean(text, OBJECTIVE_MAX);
                        }
                        push_unique(&mut session.user_prompts, prompt.clone());
                        super::codex::push_tail(&mut session, "user", prompt);
                    }
                }
            }
            "assistant/message" => {
                let text = blocks_text(data.pointer("/message/content"));
                if !text.trim().is_empty() {
                    super::codex::push_tail(
                        &mut session,
                        "assistant",
                        clean(&text, TAIL_ENTRY_MAX),
                    );
                }
                if data.get("interrupted").and_then(|v| v.as_bool()) == Some(true) {
                    interrupted = true;
                }
            }
            "tool/call" => {
                session.tool_events += 1;
                extract_files(
                    data.get("arguments").and_then(|v| v.as_str()).unwrap_or(""),
                    &mut session,
                );
            }
            "tool/result" => {
                let text = blocks_text(data.pointer("/message/content/0/content"));
                if let Some(failure) = super::codex::failure_excerpt(&text) {
                    push_unique(&mut session.failed_approaches, clean(failure, FAILURE_MAX));
                }
            }
            "todo/write" => {
                let mut plan = Vec::new();
                if let Some(todos) = data.get("todos").and_then(|v| v.as_array()) {
                    for todo in todos {
                        let content = todo.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        let status = todo.get("status").and_then(|v| v.as_str()).unwrap_or("");
                        if !content.is_empty() {
                            plan.push(super::PlanStep {
                                step: clean(content, 1000),
                                status: status.to_string(),
                            });
                        }
                    }
                }
                session.plan_state = plan; // whole-list snapshot: latest wins
            }
            "assistant/attempt" => {
                // Current stores emit this on a failed attempt: the finish
                // chunk carries the provider failure instead of a message.
                if let Some(failure) =
                    data.get("stream")
                        .and_then(|v| v.as_array())
                        .and_then(|stream| {
                            stream.iter().find_map(|entry| {
                                entry
                                    .pointer("/chunk/reason/failure/message")
                                    .and_then(|v| v.as_str())
                            })
                        })
                {
                    push_unique(&mut session.failed_approaches, clean(failure, FAILURE_MAX));
                }
            }
            // Store/driver control records: they carry no conversation text
            // and are not losses.
            "step/start"
            | "step/end"
            | "request/header"
            | "request/context"
            | "session/end-seed"
            | "assistant/chunk"
            | "agent/inbox/spliced"
            | "job/registered"
            | "job/settled"
            | "session-log-deepseek/delivery-accepted"
            | "permission/preset"
            | "sandbox/mode"
            | "approval/policy"
            | "system/message"
            | "session/title"
            | "session/title-llm-request"
            | "model/selection"
            | "command/run"
            | "command/done"
            | "goal/change"
            | "developer/message" => {}
            other => {
                if !other.is_empty() && !noted.iter().any(|n| n == other) {
                    noted.push(other.to_string());
                    session.losses.push(LossNote {
                        what: format!("dsh event type `{other}`"),
                        reason: "not mapped by this reader".into(),
                    });
                }
            }
        }
    }

    session.ended_at = last_ms.map(super::kimi::rfc3339_millis).unwrap_or_default();
    session.outcome = if open_turn || interrupted {
        Outcome::Interrupted
    } else if completed {
        Outcome::Completed
    } else {
        Outcome::Unknown
    };
    Some(session)
}

impl TranscriptReader for DshReader {
    fn id(&self) -> &'static str {
        "dsh"
    }

    fn scan(&self, home: &Path, project_dir: &Path) -> Vec<TranscriptSession> {
        let (plain, zstd) = session_files(home);
        plain
            .iter()
            .chain(zstd.iter())
            .filter_map(|file| {
                let mut session = summarize(&parse_session_file(file)?, project_dir)?;
                session.source_path = file.to_string_lossy().into_owned();
                Some(session)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write a DSH session log under `<home>/.dsh/sessions/<proj>/<id>/`.
    fn write_session(
        home: &Path,
        proj: &str,
        id: &str,
        lines: &[&str],
        trailing_nl: bool,
    ) -> PathBuf {
        let dir = home.join(format!(".dsh/sessions/{proj}/{id}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("session.jsonl");
        let mut f = std::fs::File::create(&path).expect("create");
        for (i, line) in lines.iter().enumerate() {
            if i + 1 == lines.len() && !trailing_nl {
                write!(f, "{line}").expect("write");
            } else {
                writeln!(f, "{line}").expect("write");
            }
        }
        path
    }

    fn header(cwd: &str) -> String {
        format!(
            r#"{{"type":"session","version":0,"id":"d-1","createdAt":1784272800000,"cwd":"{cwd}","delegationDepth":0}}"#
        )
    }

    #[test]
    fn dsh_reader_maps_events_and_a_torn_tail() {
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let home = tempfile::tempdir().expect("home");
        let file = write_session(
            home.path(),
            "--tmp-demo--",
            "d-1",
            &[
                &header(&cwd),
                r#"{"type":"turn/start","seq":0,"time":1784272801000,"data":{"turn":1}}"#,
                r#"{"type":"user/message","seq":1,"time":1784272801001,"data":{"id":"u1","role":"user","content":[{"type":"text","text":"implement the exporter"}],"source":{"kind":"user"}},"surfaceOp":"append"}"#,
                r#"{"type":"step/start","seq":2,"time":1784272801002,"data":{"turn":1,"step":1}}"#,
                r#"{"type":"assistant/chunk","seq":3,"time":1784272801003,"data":{"turn":1,"step":1,"chunk":{"type":"text-delta","index":0,"text":"on"}}}"#,
                r#"{"type":"text-chunks","seq0":4,"time0":1784272801004,"data":{"turn":1,"step":1,"index":0,"dt":[1,1],"texts":[" it"," now","!"]}}"#,
                r#"{"type":"assistant/message","seq":7,"time":1784272801010,"data":{"turn":1,"step":1,"message":{"id":"a1","role":"assistant","content":[{"type":"reasoning","text":"thinking dropped"},{"type":"text","text":"on it now!"}],"source":{"kind":"model","provider":"deepseek","model":"deepseek-v4-flash"}}},"surfaceOp":"append"}"#,
                r#"{"type":"tool/call","seq":8,"time":1784272801011,"data":{"turn":1,"step":1,"callId":"c1","name":"bash","arguments":"{\"command\":\"cat > src/exporter.rs <<'EOF'\"}"}}"#,
                r#"{"type":"tool/result","seq":9,"time":1784272801012,"data":{"turn":1,"step":1,"message":{"id":"r1","role":"user","content":[{"type":"tool-result","toolCallId":"c1","content":[{"type":"text","text":"Process exited with code 1: linker exploded"}],"isError":true}],"source":{"kind":"tool","callId":"c1"}},"surfaceOp":"append"}}"#,
                r#"{"type":"todo/write","seq":10,"time":1784272801013,"data":{"todos":[{"content":"wire the exporter","status":"in_progress"},{"content":"test it","status":"pending"}]}}"#,
                r#"{"type":"step/end","seq":11,"time":1784272801014,"data":{"turn":1,"step":1}}"#,
                r#"{"type":"turn/end","seq":12,"time":1784272801015,"data":{"turn":1,"reason":{"kind":"completed"}}}"#,
                r#"{"type":"turn/start","seq":13,"time":1784272802000,"data":{"turn":2}"#, // torn tail: no newline, invalid JSON fragment
            ],
            false,
        );
        let raw = parse_session_file(&file).expect("raw");
        assert!(raw.torn, "torn tail detected");
        assert_eq!(
            raw.skipped_chunks, 4,
            "one chunk event + one packed row of 3"
        );
        let session = summarize(&raw, project.path()).expect("session");

        assert_eq!(session.session_id, "d-1");
        assert_eq!(session.harness, "dsh");
        assert_eq!(session.objective, "implement the exporter");
        assert_eq!(session.tool_events, 1);
        assert_eq!(session.files_touched, vec!["src/exporter.rs"]);
        assert_eq!(session.failed_approaches.len(), 1);
        assert_eq!(
            session
                .plan_state
                .iter()
                .map(|p| p.step.as_str())
                .collect::<Vec<_>>(),
            vec!["wire the exporter", "test it"]
        );
        assert!(
            session.losses.iter().any(|l| l.what == "torn tail"),
            "losses: {:?}",
            session.losses
        );
        assert!(
            session.losses.iter().any(|l| l.what.contains("chunk")),
            "losses: {:?}",
            session.losses
        );
        // Turn 2 never closed in the preserved prefix? It was torn away —
        // turn 1 ended completed and nothing is open.
        assert_eq!(session.outcome, Outcome::Completed);
        assert_eq!(
            session.started_at,
            super::super::kimi::rfc3339_millis(1784272800000)
        );

        // cwd filtering.
        assert!(summarize(&raw, Path::new("/elsewhere")).is_none());
    }

    #[test]
    fn dsh_reader_stops_at_a_seq_gap() {
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let home = tempfile::tempdir().expect("home");
        let file = write_session(
            home.path(),
            "p",
            "d-2",
            &[
                &header(&cwd),
                r#"{"type":"turn/start","seq":0,"time":1,"data":{"turn":1}}"#,
                r#"{"type":"user/message","seq":1,"time":2,"data":{"id":"u1","role":"user","content":[{"type":"text","text":"before the gap"}],"source":{"kind":"user"}},"surfaceOp":"append"}"#,
                r#"{"type":"user/message","seq":5,"time":3,"data":{"id":"u2","role":"user","content":[{"type":"text","text":"after the gap"}],"source":{"kind":"user"}},"surfaceOp":"append"}"#,
            ],
            true,
        );
        let raw = parse_session_file(&file).expect("raw");
        assert!(raw.truncated);
        let session = summarize(&raw, project.path()).expect("session");
        assert_eq!(session.user_prompts, vec!["before the gap"]);
        assert!(session.losses.iter().any(|l| l.what == "truncated log"));
    }
}

#[cfg(test)]
mod zstd_tests {
    use super::*;

    fn write_zstd(home: &Path, proj: &str, id: &str, body: &str, frames: usize) -> PathBuf {
        let dir = home.join(format!(".dsh/sessions/{proj}/{id}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("session.v4.jsonl.zstd");
        let mut bytes = Vec::new();
        if frames <= 1 {
            bytes = zstd::stream::encode_all(body.as_bytes(), 3).expect("encode");
        } else {
            // DSH appends compressed batches; exercise concatenated frames.
            let lines: Vec<&str> = body.lines().collect();
            let chunk = lines.len().div_ceil(frames);
            for group in lines.chunks(chunk.max(1)) {
                let mut part = group.join("\n");
                part.push('\n');
                bytes.extend(zstd::stream::encode_all(part.as_bytes(), 3).expect("encode"));
            }
        }
        std::fs::write(&path, bytes).expect("write");
        path
    }

    #[test]
    fn v4_zstd_session_reads_and_summarizes() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let body = format!(
            r#"{{"type":"session","version":4,"id":"s-4","createdAt":1794674498468,"cwd":"{cwd}","isSeeded":false,"delegationDepth":0}}
{{"type":"permission/preset","seq":0,"time":1794674498478,"data":{{"preset":"workspace-write"}}}}
{{"type":"turn/start","seq":1,"time":1794674498484,"data":{{"turn":1}}}}
{{"type":"user/message","seq":2,"time":1794674498485,"data":{{"id":"u1","role":"user","content":[{{"type":"text","text":"ship the v4 reader"}}],"source":{{"kind":"user"}}}}}}
{{"type":"step/start","seq":3,"time":1794674498486,"data":{{"turn":1,"step":1}}}}
{{"type":"assistant/message","seq":4,"time":1794674498487,"data":{{"turn":1,"step":1,"message":{{"id":"a1","role":"assistant","content":[{{"type":"text","text":"shipping it"}}],"source":{{"kind":"model","provider":"deepseek","model":"m"}}}}}}}}
{{"type":"turn/end","seq":5,"time":1794674498488,"data":{{"turn":1,"reason":{{"kind":"completed"}}}}}}
"#
        );
        let path = write_zstd(home.path(), "--tmp-proj--", "s-4", &body, 1);
        let raw = parse_session_file(&path).expect("v4 zstd parses");
        assert_eq!(raw.header.get("version").and_then(|v| v.as_u64()), Some(4));
        assert!(raw
            .events
            .iter()
            .any(|e| e.get("type").and_then(|v| v.as_str()) == Some("user/message")));

        let summary = summarize(&raw, project.path()).expect("summary");
        assert_eq!(summary.session_id, "s-4");
        assert_eq!(summary.objective, "ship the v4 reader");
        assert_eq!(summary.outcome, Outcome::Completed);
        assert!(
            summary
                .losses
                .iter()
                .all(|l| !l.what.contains("permission/preset")),
            "control records must not be reported as losses: {:?}",
            summary.losses
        );

        let scanned = DshReader.scan(home.path(), project.path());
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].session_id, "s-4");
    }

    #[test]
    fn concatenated_zstd_frames_decode() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let body = format!(
            r#"{{"type":"session","version":4,"id":"s-frames","createdAt":1794674498468,"cwd":"{cwd}"}}
{{"type":"turn/start","seq":0,"time":1794674498484,"data":{{"turn":1}}}}
{{"type":"user/message","seq":1,"time":1794674498485,"data":{{"role":"user","content":[{{"type":"text","text":"frame one"}}],"source":{{"kind":"user"}}}}}}
{{"type":"assistant/message","seq":2,"time":1794674498487,"data":{{"message":{{"role":"assistant","content":[{{"type":"text","text":"frame two"}}],"source":{{"kind":"model"}}}}}}}}
{{"type":"turn/end","seq":3,"time":1794674498488,"data":{{"turn":1,"reason":{{"kind":"completed"}}}}}}
"#
        );
        let path = write_zstd(home.path(), "--tmp-proj--", "s-frames", &body, 3);
        let raw = parse_session_file(&path).expect("concatenated zstd frames parse");
        assert_eq!(raw.header.get("version").and_then(|v| v.as_u64()), Some(4));
        // Every event of the single logical log must survive the frame join.
        assert!(raw
            .events
            .iter()
            .any(|e| e.get("type").and_then(|v| v.as_str()) == Some("turn/end")));
    }

    #[test]
    fn corrupt_zstd_is_not_fabricated() {
        let home = tempfile::tempdir().expect("home");
        let dir = home.path().join(".dsh/sessions/p/x");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("session.jsonl.zstd");
        std::fs::write(&path, b"not a zstd frame").expect("write");
        assert!(parse_session_file(&path).is_none());
        assert!(is_zstd_session(&path));
    }
}
