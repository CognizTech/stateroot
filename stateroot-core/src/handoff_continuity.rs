//! Handoff continuity: transcript finalize gates and resume overlays.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::local_store::{self, now_rfc3339, SCHEMA_HANDOFF_V1};
use crate::roots;
use crate::transcripts::{self, TranscriptSession};

/// Marker written on every explicit `handoff write` (never on finalize).
pub const EXPLICIT_HANDOFF_MARKER: &str = "local/explicit-handoff-marker.json";
/// Warning line on transcript-finalized packets.
pub const FINALIZE_WARNING: &str =
    "finalized from verified transcript — observed, not author-written handoff";

fn session_order(left: &TranscriptSession, right: &TranscriptSession) -> std::cmp::Ordering {
    let left_latest = session_latest_time(left);
    let right_latest = session_latest_time(right);
    left_latest
        .cmp(right_latest)
        .then_with(|| left.started_at.cmp(&right.started_at))
        .then_with(|| left.session_id.cmp(&right.session_id))
}

fn session_latest_time(session: &TranscriptSession) -> &str {
    if session.ended_at.is_empty() {
        &session.started_at
    } else {
        &session.ended_at
    }
}

/// Latest matching verified transcript session for one harness + project.
pub fn latest_verified_session(
    home: &Path,
    project: &Path,
    harness: &str,
) -> Option<TranscriptSession> {
    transcripts::reader_for(harness)
        .and_then(|reader| reader.scan(home, project).into_iter().max_by(session_order))
}

/// Exact verified session: newer sessions never satisfy an old boundary.
pub fn verified_session(
    home: &Path,
    project: &Path,
    harness: &str,
    session_id: &str,
    locator: Option<&str>,
) -> Option<TranscriptSession> {
    if session_id.is_empty() || session_id == "unknown" {
        return None;
    }
    let reader = transcripts::reader_for(harness)?;
    let mut sessions = reader.scan(home, project);
    if let Some(locator) = locator {
        let path = Path::new(locator);
        let located = match reader.id() {
            "codex" => transcripts::codex::parse_rollout(path, project),
            "claude" => transcripts::claude::parse_session(path, project),
            "openclaw" => transcripts::openclaw::parse_session(path, project),
            "pi" => transcripts::pi::parse_session_file(path)
                .and_then(|raw| transcripts::pi::summarize(&raw, project)),
            "dsh" => transcripts::dsh::parse_session_file(path)
                .and_then(|raw| transcripts::dsh::summarize(&raw, project)),
            _ => None,
        };
        if let Some(mut session) = located {
            session.source_path = locator.to_string();
            sessions.push(session);
        }
    }
    sessions.into_iter().find(|session| {
        session.session_id == session_id
            && crate::transcripts::cwd_matches(&session.cwd, project)
            && locator.is_none_or(|path| {
                !session.source_path.is_empty()
                    && transcripts::same_worktree(Path::new(&session.source_path), Path::new(path))
            })
    })
}

fn handoff_boundary_time(handoff: &Value) -> &str {
    handoff
        .get("written_at")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .or_else(|| handoff.get("created_at").and_then(Value::as_str))
        .unwrap_or("")
}

/// True when the session's latest timestamp is strictly after the handoff boundary.
pub fn session_newer_than_handoff(session: &TranscriptSession, handoff: &Value) -> bool {
    let boundary = handoff_boundary_time(handoff);
    if boundary.is_empty() {
        return true;
    }
    session_latest_time(session) > boundary
}

fn marker_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(EXPLICIT_HANDOFF_MARKER)
}

/// Record an explicit author handoff write (blocks finalize over that seq).
pub fn write_explicit_marker(
    project_dir: &Path,
    harness: &str,
    seq: i64,
    written_at: &str,
) -> std::io::Result<()> {
    let path = marker_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let marker = json!({
        "harness": harness,
        "seq": seq,
        "written_at": written_at,
    });
    crate::safe_io::atomic_replace_json(&path, &marker)
}

fn read_explicit_marker(project_dir: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(marker_path(project_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Finalize must not clobber an explicit handoff at the current seq.
pub fn explicit_blocks_finalize(project_dir: &Path, harness: &str, current_seq: i64) -> bool {
    let Some(marker) = read_explicit_marker(project_dir) else {
        return false;
    };
    marker.get("seq").and_then(Value::as_i64) == Some(current_seq)
        && marker.get("harness").and_then(Value::as_str) == Some(harness)
}

/// Automatic boundary work must preserve authored decisions from any actor.
pub fn any_explicit_blocks_finalize(project_dir: &Path, current_seq: i64) -> bool {
    read_explicit_marker(project_dir)
        .is_some_and(|marker| marker["seq"].as_i64() == Some(current_seq))
}

/// Newest verified session (any harness) strictly newer than the handoff boundary.
pub fn gap_session_after_handoff(
    home: &Path,
    project: &Path,
    handoff: &Value,
) -> Option<TranscriptSession> {
    let mut sessions: Vec<TranscriptSession> = transcripts::scan_all(home, project)
        .into_iter()
        .filter(|session| session_newer_than_handoff(session, handoff))
        .collect();
    sessions.sort_by(session_order);
    sessions.pop()
}

pub fn should_finalize(
    project_dir: &Path,
    home: &Path,
    harness: &str,
    current: Option<&Value>,
) -> bool {
    let Some(handoff) = current else {
        return false;
    };
    let Some(session) = latest_verified_session(home, project_dir, harness) else {
        return false;
    };
    if !session_newer_than_handoff(&session, handoff) {
        return false;
    }
    let current_seq = handoff.get("seq").and_then(Value::as_i64).unwrap_or(0);
    !explicit_blocks_finalize(project_dir, harness, current_seq)
}

pub fn transcript_digest(session: &TranscriptSession) -> String {
    let mut parts = vec![format!("Transcript outcome: {}", session.outcome.as_str())];
    parts.push(format!(
        "{} file(s) changed, {} failure(s), {} next action(s), {} tool event(s)",
        session.files_touched.len(),
        session.failed_approaches.len(),
        session.next_steps.len(),
        session.tool_events
    ));
    format!("{}.", parts.join("; "))
}

fn full_tail(session: &TranscriptSession) -> Vec<Value> {
    session
        .conversation_tail
        .iter()
        .map(|entry| json!({"role": entry.role, "text": entry.text}))
        .collect()
}

/// Build a transcript-finalized handoff packet (caller persists + bounds).
pub fn build_finalize_packet(
    project_id: &str,
    project_dir: &Path,
    harness: &str,
    current_seq: i64,
    session: &TranscriptSession,
    state_objective: &str,
    state_phase: &str,
) -> Value {
    let task = session
        .user_prompts
        .iter()
        .rev()
        .find(|text| !text.trim().is_empty())
        .cloned()
        .or_else(|| session.next_steps.first().cloned())
        .unwrap_or_else(|| format!("Continue from observed {} session", harness));

    let objective = if !state_objective.trim().is_empty() {
        state_objective.to_string()
    } else {
        task.clone()
    };

    let mut context_summary = session
        .progress_summaries
        .first()
        .filter(|text| !text.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| transcript_digest(session));
    if task.trim().eq_ignore_ascii_case(context_summary.trim()) {
        context_summary = format!(
            "{context_summary}\n\n(Observed session continuity beyond the immediate task.)"
        );
    }

    let mut warnings = vec![FINALIZE_WARNING.to_string()];
    warnings.push(format!(
        "transcript enrichment is observed from verified {} session {}",
        harness, session.session_id
    ));

    let now = now_rfc3339();
    let mut packet = json!({
        "schema_version": SCHEMA_HANDOFF_V1,
        "project_id": project_id,
        "seq": current_seq + 1,
        "task": task,
        "current_phase": state_phase,
        "last_harness": harness,
        "recommended_next_harness": Value::Null,
        "objective": objective,
        "implementation_status": transcript_digest(session),
        "decisions": [],
        "changed_files": session.files_touched,
        "tests_run": [],
        "failures": session.failed_approaches,
        "bugs_found": [],
        "blockers": [],
        "open_questions": [],
        "next_actions": session.next_steps,
        "warnings": warnings,
        "relevant_memories": [],
        "relevant_skills": [],
        "artifacts": [],
        "traces": [],
        "context_summary": context_summary,
        "created_at": now,
        "written_at": now,
        "created_by_harness": harness,
        "evidence_ref": { "session_id": session.session_id, "harness": session.harness, "transcript": session.source_path },
        "history_ref": { "session_id": session.session_id, "harness": session.harness, "transcript": session.source_path, "prompt_count": session.user_prompts.len(), "projection": "bounded reader summary; native source retains history" },
    });

    if !session.plan_state.is_empty() {
        packet["plan_state"] = json!(session
            .plan_state
            .iter()
            .map(|item| json!({"step": item.step, "status": item.status}))
            .collect::<Vec<_>>());
    }
    if !session.progress_summaries.is_empty() {
        packet["progress_summaries"] = json!(session.progress_summaries);
    }
    if !session.milestones.is_empty() {
        packet["milestones"] = json!(session.milestones);
    }
    let tail = full_tail(session);
    if !tail.is_empty() {
        packet["conversation_tail"] = Value::Array(tail);
    }
    if let Ok(Some(root)) = roots::latest_root(project_dir) {
        if !root.is_empty() {
            packet["latest_root"] = json!(root);
        }
    }
    packet
}

fn short_hash(hash: &str) -> String {
    if hash.is_empty() {
        return "∅".into();
    }
    hash.chars().take(12).collect()
}

/// Overlay tail bounds (the token razor): the LAST 8 entries, each ≤ 400
/// chars with an ellipsis when cut. Extraction already trims/caps, so small
/// tails render identically to before.
const OVERLAY_TAIL_ENTRIES: usize = 8;
/// Per-entry char cap for the overlay tail.
const OVERLAY_TAIL_ENTRY_CHARS: usize = 400;

/// Read-only overlay when a gap remains after the formal handoff packet.
pub fn compose_since_handoff_overlay(
    project_dir: &Path,
    handoff: &Value,
    session: &TranscriptSession,
) -> String {
    let seq = handoff.get("seq").and_then(Value::as_i64).unwrap_or(0);
    let author = handoff
        .get("created_by_harness")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let boundary = handoff_boundary_time(handoff);
    let mut out = format!(
        "## Work since handoff #{seq} (observed — {})\n\n",
        session.harness
    );
    out.push_str(&format!(
        "Last formal handoff: #{seq} by {author} at {boundary}.\n"
    ));
    out.push_str(&format!(
        "Observed {} session {} (outcome: {}).\n",
        session.harness,
        session.session_id,
        session.outcome.as_str()
    ));
    if !session.files_touched.is_empty() {
        out.push_str(&format!(
            "Files touched ({}): {}\n",
            session.files_touched.len(),
            session.files_touched.join(", ")
        ));
    }
    if !session.failed_approaches.is_empty() {
        out.push_str("Failed approaches:\n");
        for item in session.failed_approaches.iter().take(6) {
            out.push_str(&format!("- {item}\n"));
        }
    }
    if !session.plan_state.is_empty() {
        out.push_str("\nPlan state:\n");
        for item in &session.plan_state {
            out.push_str(&format!("- [{}] {}\n", item.status, item.step));
        }
    }
    if let Some(stored) = handoff.get("latest_root").and_then(Value::as_str) {
        if !stored.is_empty() {
            out.push_str(&format!(
                "\nHandoff stamped root: `{}`.\n",
                short_hash(stored)
            ));
        }
    }
    if let Ok(Some(latest)) = roots::latest_root(project_dir) {
        let handoff_root = handoff
            .get("latest_root")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !latest.is_empty() && latest != handoff_root {
            out.push_str(&format!(
                "Latest root since handoff: `{}`.\n",
                short_hash(&latest)
            ));
        }
    }
    let tail = full_tail(session);
    if !tail.is_empty() {
        out.push_str("\nConversation tail (observed):\n");
        let start = tail.len().saturating_sub(OVERLAY_TAIL_ENTRIES);
        for entry in &tail[start..] {
            let role = entry.get("role").and_then(Value::as_str).unwrap_or("?");
            let text = entry.get("text").and_then(Value::as_str).unwrap_or("");
            out.push_str(&format!(
                "- [{role}] {}\n",
                crate::transcripts::clean(text, OVERLAY_TAIL_ENTRY_CHARS)
            ));
        }
    }
    out.push_str("\nThis is NOT a formal handoff packet.\n");
    out
}

/// Whether resume should append an overlay (gap exists and handoff wasn't finalized from it).
pub fn overlay_for_handoff(
    home: &Path,
    project_dir: &Path,
    handoff: &Value,
) -> Option<TranscriptSession> {
    let gap = gap_session_after_handoff(home, project_dir, handoff)?;
    if handoff
        .get("warnings")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.as_str() == Some(FINALIZE_WARNING))
        })
        && handoff.get("created_by_harness").and_then(Value::as_str) == Some(gap.harness)
    {
        return None;
    }
    Some(gap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_aliases_resolve_capability_without_borrowing_unknown_readers() {
        for (alias, expected) in [
            ("claude", "claude"),
            ("claude-code", "claude"),
            ("kimi", "kimi"),
            ("kimi-code", "kimi"),
            ("copilot", "copilot"),
            ("vscode-copilot", "copilot"),
            ("github-copilot", "copilot"),
            ("dsh", "dsh"),
        ] {
            assert_eq!(transcripts::reader_for(alias).unwrap().id(), expected);
        }
        assert!(transcripts::reader_for("unsupported-harness").is_none());
    }

    #[test]
    fn exact_native_identity_and_explicit_locator_never_choose_a_newer_session() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let sibling = tempfile::tempdir().unwrap();
        let store = home.path().join(".claude/projects/example");
        std::fs::create_dir_all(&store).unwrap();
        let write = |id: &str, cwd: &Path, timestamp: &str| {
            let file = store.join(format!("{id}.jsonl"));
            std::fs::write(&file, serde_json::json!({"type":"user", "sessionId":id, "cwd":cwd, "timestamp":timestamp, "message":{"role":"user", "content":id}}).to_string()).unwrap();
            file
        };
        let old = write("older", project.path(), "2026-10-08T01:00:00Z");
        write("newer", project.path(), "2026-10-08T02:00:00Z");
        write("sibling", sibling.path(), "2026-10-08T03:00:00Z");
        let found = verified_session(
            home.path(),
            project.path(),
            "claude-code",
            "older",
            Some(old.to_str().unwrap()),
        )
        .unwrap();
        assert_eq!(found.session_id, "older");
        assert!(verified_session(home.path(), project.path(), "claude", "missing", None).is_none());
        assert!(verified_session(home.path(), project.path(), "claude", "sibling", None).is_none());
        assert!(verified_session(home.path(), sibling.path(), "claude", "older", None).is_none());
    }

    #[test]
    fn historical_first_prompt_does_not_replace_current_objective() {
        let session = TranscriptSession {
            objective: "Receive handoff 70".into(),
            user_prompts: vec!["Review competitors".into()],
            ..Default::default()
        };
        let packet = build_finalize_packet(
            "p",
            Path::new("/unavailable"),
            "codex",
            1,
            &session,
            "Current competitive review",
            "working",
        );
        assert_eq!(packet["objective"], "Current competitive review");
        assert_eq!(packet["task"], "Review competitors");
    }

    #[test]
    fn session_newer_compares_rfc3339_strings() {
        let handoff = json!({"written_at": "2026-08-12T10:00:00Z"});
        let older = TranscriptSession {
            harness: "codex",
            session_id: "s1".into(),
            ended_at: "2026-08-12T09:00:00Z".into(),
            ..Default::default()
        };
        let newer = TranscriptSession {
            ended_at: "2026-08-12T11:00:00Z".into(),
            ..older.clone()
        };
        assert!(!session_newer_than_handoff(&older, &handoff));
        assert!(session_newer_than_handoff(&newer, &handoff));
    }

    #[test]
    fn overlay_tail_is_bounded_to_last_8_at_400_chars() {
        let handoff =
            json!({"seq": 2, "created_by_harness": "kimi", "written_at": "2026-08-12T10:00:00Z"});
        let tail: Vec<crate::transcripts::TailEntry> = (0..12)
            .map(|i| crate::transcripts::TailEntry {
                role: "user",
                text: format!("entry-{i} {}", "x".repeat(1000)),
            })
            .collect();
        let session = TranscriptSession {
            harness: "kimi",
            session_id: "s1".into(),
            conversation_tail: tail,
            ..Default::default()
        };
        let dir = tempfile::tempdir().expect("dir");
        let out = compose_since_handoff_overlay(dir.path(), &handoff, &session);
        let lines: Vec<&str> = out.lines().filter(|l| l.starts_with("- [user]")).collect();
        assert_eq!(lines.len(), 8, "last 8 only: {out}");
        assert!(lines[0].contains("entry-4"), "oldest shown is #4: {out}");
        assert!(lines[7].contains("entry-11"), "out: {out}");
        for line in lines {
            assert!(
                line.chars().count() <= 410,
                "line: {} chars",
                line.chars().count()
            );
            assert!(line.ends_with('…'), "cut lines carry the ellipsis: {line}");
        }

        // A small tail renders identically to the unbounded form.
        let small = TranscriptSession {
            harness: "kimi",
            session_id: "s2".into(),
            conversation_tail: vec![crate::transcripts::TailEntry {
                role: "assistant",
                text: "short answer".into(),
            }],
            ..Default::default()
        };
        let out = compose_since_handoff_overlay(dir.path(), &handoff, &small);
        assert!(out.contains("- [assistant] short answer\n"), "out: {out}");
    }
}
