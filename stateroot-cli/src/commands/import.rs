//! `stateroot import` — native transcript import (plan P1+P2).
//!
//! Scans the harness transcript stores for sessions of the current project,
//! prints a per-session summary, then imports truthfully:
//! - one server observation per session (`source: "transcript"`, stable
//!   `source_id` — re-imports dedupe server-side), batched; offline falls
//!   back to the outbox replay flow.
//! - one episodic record per session (local, deduped by source_id).
//! - ONE synthesized historical handoff from the latest session, only when
//!   no handoff exists yet (never overwrites real state). `metadata.imported`
//!   is kept in the LOCAL packet only — the server schema is `extra="forbid"`
//!   (same precedent as `accepted_by`).
//! - the state-doc objective, only when still the skeleton default (empty).
//!
//! Everything is transcript-derived (observed, never verified); nothing is
//! invented — empty fields stay empty.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use stateroot_core::local_store::now_rfc3339;
use stateroot_core::local_store::{self, SCHEMA_HANDOFF_V1};
use stateroot_core::transcripts::{self, TranscriptSession};

use super::{note, truncate, Ctx};

/// Options for `stateroot import`.
pub struct ImportOptions {
    /// Restrict to one harness (`codex`, `claude`).
    pub harness: Option<String>,
    /// Only sessions started on/after this date (YYYY-MM-DD prefix compare).
    pub since: Option<String>,
    /// Print the scan summary without writing anything.
    pub dry_run: bool,
    /// Suppress the per-session summary (init's auto-import prints its own).
    pub quiet: bool,
}

/// What an import run did (init prints per-harness counts from it).
#[derive(Debug, Default)]
pub struct ImportReport {
    /// Sessions found after filters.
    pub scanned: usize,
    /// Per-harness session counts (scanned).
    pub per_harness: BTreeMap<String, usize>,
    /// Newly ingested observations (server reply; 0 when queued offline).
    pub ingested: i64,
    /// Duplicates reported by the server (re-imports).
    pub duplicates: i64,
    /// Episodic records appended.
    pub episodic_seeded: usize,
    /// Historical handoff was synthesized.
    pub handoff_synthesized: bool,
    /// State-doc objective was seeded.
    pub objective_seeded: bool,
}

/// Run `stateroot import`.
pub async fn run(ctx: &Ctx, options: &ImportOptions) -> anyhow::Result<ImportReport> {
    let project = ctx.require_project()?;
    let home = super::install::home_dir()?;
    let mut sessions = transcripts::scan_all(&home, &ctx.cwd);

    if let Some(harness) = &options.harness {
        let wanted = harness.trim().to_lowercase();
        sessions.retain(|s| s.harness == wanted);
    }
    if let Some(since) = &options.since {
        // RFC3339 timestamps compare correctly as strings at day precision.
        sessions.retain(|s| s.started_at.as_str() >= since.as_str());
    }

    let mut report = ImportReport {
        scanned: sessions.len(),
        ..Default::default()
    };
    for session in &sessions {
        *report
            .per_harness
            .entry(session.harness.to_string())
            .or_insert(0) += 1;
    }

    if !options.quiet {
        if sessions.is_empty() {
            println!("no transcript sessions found for this project");
            for (harness, text) in transcripts::pending_reader_notes() {
                println!("  {harness}: {text}");
            }
        }
        for session in &sessions {
            let date = session.started_at.get(..10).unwrap_or(&session.started_at);
            println!(
                "  {} {} {:<12} {:<60} ({} files, {} failed, {} tool events)",
                session.harness,
                date,
                session.outcome.as_str(),
                truncate(&session.objective, 60),
                session.files_touched.len(),
                session.failed_approaches.len(),
                session.tool_events
            );
        }
    }
    if options.dry_run {
        if !options.quiet {
            println!("dry-run — nothing was imported");
        }
        return Ok(report);
    }
    if sessions.is_empty() {
        return Ok(report);
    }

    // New sessions (not yet in the episodic log) — the synthesis gate.
    seed_episodic(ctx, &sessions, &mut report);
    import_observations(ctx, &project.project_id, &sessions, &mut report).await?;
    synthesize_handoff(ctx, &project.project_id, &sessions, &mut report).await;
    seed_objective(ctx, &project.project_id, &sessions, &mut report).await;
    if !options.quiet {
        let mut parts = vec![format!("{} new", report.ingested)];
        if report.duplicates > 0 {
            parts.push(format!("{} duplicates", report.duplicates));
        }
        if report.episodic_seeded > 0 {
            parts.push(format!("{} episodic", report.episodic_seeded));
        }
        if report.handoff_synthesized {
            parts.push("historical handoff".to_string());
        }
        if report.objective_seeded {
            parts.push("objective seeded".to_string());
        }
        println!(
            "imported {} session(s): {}",
            report.scanned,
            parts.join(", ")
        );
    }
    Ok(report)
}

/// One episodic record per session (local source of truth), deduped by the
/// stable source_id marker.
fn seed_episodic(ctx: &Ctx, sessions: &[TranscriptSession], report: &mut ImportReport) {
    let path = local_store::root(&ctx.cwd).join(local_store::EPISODIC_PATH);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    for session in sessions {
        let source_id = transcripts::source_id(session);
        if existing.contains(&source_id) {
            continue;
        }
        let date = session.started_at.get(..10).unwrap_or(&session.started_at);
        let record = json!({
            "ts": now_rfc3339(),
            "harness": "transcript",
            "note": format!(
                "{} session {}: {} — {} files touched, {} failed attempts",
                session.harness,
                date,
                truncate(&session.objective, 120),
                session.files_touched.len(),
                session.failed_approaches.len()
            ),
            "files": session.files_touched.iter().take(20).collect::<Vec<_>>(),
            "source_id": source_id,
        });
        match local_store::append_episodic(&ctx.cwd, &record) {
            Ok(()) => report.episodic_seeded += 1,
            Err(err) => note!("warning: episodic seed failed: {err}"),
        }
    }
}

/// Extraction-loss observations for one session (B1): one per intentionally
/// excluded artifact, source_id dedup-safe via the server's UNIQUE
/// (project_id, source, source_id) constraint.
fn loss_observations_for(session: &TranscriptSession) -> Vec<Value> {
    session
        .losses
        .iter()
        .enumerate()
        .map(|(index, loss)| {
            json!({
                "source": "transcript",
                "source_id": format!("transcript:{}:{}:loss:{}", session.harness, session.session_id, index + 1),
                "kind": "extraction_loss",
                "payload": {
                    "what": loss.what,
                    "reason": loss.reason,
                    "session_id": session.session_id,
                    "harness": session.harness,
                },
                "harness": session.harness
            })
        })
        .collect()
}

/// Observation payload for one session (already-extracted fields; the rich
/// channel — everything extracted lands here, COMPLETE).
fn observation_for(session: &TranscriptSession) -> Value {
    json!({
        "source": "transcript",
        "source_id": transcripts::source_id(session),
        "kind": "imported_session",
        "payload": {
            "harness": session.harness,
            "session_id": session.session_id,
            "cwd": session.cwd,
            "started_at": session.started_at,
            "ended_at": session.ended_at,
            "outcome": session.outcome.as_str(),
            "objective": session.objective,
            "user_prompt_count": session.user_prompts.len(),
            "user_prompts": session.user_prompts,
            "files_touched": session.files_touched,
            "failed_approaches": session.failed_approaches,
            "next_steps": session.next_steps,
            "plan_state": session.plan_state.iter().map(|s| json!({
                "step": s.step,
                "status": s.status,
            })).collect::<Vec<_>>(),
            "progress_summaries": session.progress_summaries,
            "conversation_tail": session.conversation_tail.iter().map(|e| json!({
                "role": e.role,
                "text": e.text,
            })).collect::<Vec<_>>(),
            "milestones": session.milestones,
            "tool_events": session.tool_events,
            "confidence": "observed"
        },
        "harness": session.harness
    })
}

/// Ship one observation per session: batched POST when online, outbox ops
/// otherwise (replayed by the next online command via `flush_outbox`).
async fn import_observations(
    ctx: &Ctx,
    _project_id: &str,
    sessions: &[TranscriptSession],
    report: &mut ImportReport,
) -> anyhow::Result<()> {
    let observations: Vec<Value> = sessions
        .iter()
        .flat_map(|session| {
            let mut all = vec![observation_for(session)];
            all.extend(loss_observations_for(session));
            all
        })
        .collect();
    // Local ingest: append to the observations spool — the same journal
    // the session hooks write. (The readers return only NEW sessions via the
    // import cursor, so identical replays are rare and tolerated.)
    let spool = local_store::root(&ctx.cwd).join("spool/observations.jsonl");
    if let Some(parent) = spool.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut appended = 0i64;
    {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&spool)?;
        for observation in &observations {
            writeln!(file, "{}", serde_json::to_string(observation)?)?;
            appended += 1;
        }
    }
    report.ingested = appended;
    Ok(())
}

/// The synthesized historical handoff: from the LATEST session, only when
/// no handoff exists (never overwrites real state).
async fn synthesize_handoff(
    ctx: &Ctx,
    project_id: &str,
    sessions: &[TranscriptSession],
    report: &mut ImportReport,
) {
    let Some(latest) = sessions.last() else {
        return;
    };

    // Gate + upgrade rule: an existing handoff with an EMPTY objective is a
    // shell, not content (phase still `init` with no objective is the same
    // thing) — the imported handoff wins when it has a real objective. A
    // handoff with real content is NEVER overwritten.
    let has_objective = |packet: &Value| {
        !packet
            .get("objective")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .is_empty()
    };
    let imported_wins = !latest.objective.is_empty();
    let local_current = local_store::read_handoff_local(&ctx.cwd).ok().flatten();
    if let Some(existing) = &local_current {
        if has_objective(existing) || !imported_wins {
            println!("handoff exists with real content — imported handoff skipped");
            return;
        }
        println!(
            "handoff upgrade: existing handoff has an empty objective — imported handoff wins"
        );
    }
    let failed: Vec<String> = sessions
        .iter()
        .flat_map(|s| s.failed_approaches.clone())
        .fold(Vec::new(), |mut acc, item| {
            if !acc.contains(&item) {
                acc.push(item);
            }
            acc
        });
    // Transcript readers carry canonical harness ids. That observed source is
    // the packet author; the local CLI must never replace it with StateSmith.
    let from = latest.harness;
    let date = latest.started_at.get(..10).unwrap_or(&latest.started_at);
    // Actual progress content: the harness's own NEWEST compacted summary
    // when the transcript has one; otherwise the plain import statement.
    // (The full rich set also travels in the optional HandoffV1 fields
    // below and in the observation payload; this field is the digest.)
    let mut context_summary = if let Some(newest) = latest.progress_summaries.first() {
        newest.clone()
    } else {
        // Plain provenance line — the objective has its own field; do NOT
        // echo it here (it rendered twice in resume otherwise).
        format!(
            "Imported {} session(s) from native transcripts (latest: {} {}).",
            sessions.len(),
            latest.harness,
            date
        )
    };
    // Rich pack fields (HandoffV1 additive/optional, direction §4.8):
    // latest NON-EMPTY source session wins per field — same rule as the
    // objective/context_summary selection. Omitted entirely when empty.
    let plan_source = sessions.iter().rev().find(|s| !s.plan_state.is_empty());
    let progress_source = sessions
        .iter()
        .rev()
        .find(|s| s.progress_summaries.iter().any(|t| !t.trim().is_empty()));
    let tail_source = sessions
        .iter()
        .rev()
        .find(|s| !s.conversation_tail.is_empty());
    let milestone_source = sessions.iter().rev().find(|s| !s.milestones.is_empty());
    let task = latest
        .user_prompts
        .iter()
        .rev()
        .find(|text| !text.trim().is_empty())
        .cloned()
        .or_else(|| latest.next_steps.first().cloned())
        .unwrap_or_else(|| format!("Review imported {} session {date}", latest.harness));
    let implementation_status = format!(
        "Transcript outcome: {}; {} file(s) changed, {} failure(s), {} next action(s), {} tool event(s).",
        latest.outcome.as_str(),
        latest.files_touched.len(),
        failed.len(),
        latest.next_steps.len(),
        latest.tool_events
    );
    if task.trim().eq_ignore_ascii_case(context_summary.trim()) {
        context_summary = implementation_status.clone();
    }
    let mut warnings = vec!["imported from transcripts — observed, not verified".to_string()];
    let summary_max = stateroot_core::handoff_bounds::CONTEXT_SUMMARY_MAX;
    if context_summary.chars().count() > summary_max {
        warnings.push(format!(
            "context_summary truncated to {summary_max} characters"
        ));
    }
    let mut packet = json!({
        "schema_version": SCHEMA_HANDOFF_V1,
        "project_id": project_id,
        "seq": 1,
        "task": task,
        "current_phase": "",
        "last_harness": from,
        "recommended_next_harness": null,
        "objective": latest.objective,
        "implementation_status": implementation_status,
        "decisions": [],
        "changed_files": latest.files_touched,
        "tests_run": [],
        "failures": failed,
        "bugs_found": [],
        "blockers": [],
        "open_questions": [],
        "next_actions": latest.next_steps,
        "warnings": warnings,
        "relevant_memories": [],
        "relevant_skills": [],
        "artifacts": [],
        "traces": [],
        "context_summary": truncate(&context_summary, summary_max),
        "created_at": now_rfc3339(),
        "written_at": now_rfc3339(),
        "created_by_harness": from,
        // LOCAL ONLY — the server schema is extra="forbid" (accepted_by
        // precedent); stripped before POSTing.
        "metadata": {"imported": true, "source": "transcript", "source_harness": latest.harness, "sessions": sessions.len()},
    });
    if let Some(source) = plan_source {
        packet["plan_state"] = json!(source
            .plan_state
            .iter()
            .map(|s| json!({"step": s.step, "status": s.status}))
            .collect::<Vec<_>>());
    }
    if let Some(source) = progress_source {
        packet["progress_summaries"] = json!(source.progress_summaries);
    }
    if let Some(source) = tail_source {
        packet["conversation_tail"] = json!(super::handoff::compact_tail(source));
    }
    if let Some(source) = milestone_source {
        packet["milestones"] = json!(source.milestones);
    }

    match local_store::write_handoff_local(&ctx.cwd, &packet) {
        Ok(()) => {
            report.handoff_synthesized = true;
        }
        Err(err) => note!("warning: could not write the imported handoff: {err}"),
    }
}

/// Seed `project/state.json`'s objective from the latest session — only
/// when it is still the skeleton default (empty). Local file + server PATCH.
async fn seed_objective(
    ctx: &Ctx,
    project_id: &str,
    sessions: &[TranscriptSession],
    report: &mut ImportReport,
) {
    let Some(latest) = sessions.last() else {
        return;
    };
    if latest.objective.is_empty() {
        return;
    }
    let objective = latest.objective.clone();

    // Local mirror first (source of truth for offline reads).
    let path = local_store::root(&ctx.cwd).join(local_store::STATE_PATH);
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(mut state) = serde_json::from_str::<Value>(&text) {
            let current = state
                .get("objective")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if current.trim().is_empty() {
                state["objective"] = Value::String(objective.clone());
                if let Ok(pretty) = serde_json::to_string_pretty(&state) {
                    if let Err(err) = std::fs::write(&path, format!("{pretty}\n")) {
                        note!("warning: could not write local state.json: {err}");
                    } else {
                        report.objective_seeded = true;
                    }
                }
            } else {
                // real objective present — never overwrite.
            }
        }
    } else {
        // No state file at all (init always creates one) — write fresh.
        let state = json!({
            "schema_version": "stateroot.project_state.v1",
            "project_id": project_id,
            "objective": objective,
            "current_phase": "build",
            "status": "active",
            "last_harness": Value::Null,
            "recommended_next_harness": Value::Null,
        });
        if let Ok(pretty) = serde_json::to_string_pretty(&state) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(err) = std::fs::write(&path, format!("{pretty}\n")) {
                note!("warning: could not write local state.json: {err}");
            } else {
                report.objective_seeded = true;
            }
        }
    }
}
