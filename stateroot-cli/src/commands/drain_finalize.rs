//! Hidden `stateroot _drain-finalize` — the detached half of the durable
//! session-boundary journal (repair Phase 3).
//!
//! The stop/session_end hook enqueues ONE composite boundary job and
//! returns inside its latency budget. Every later safe entrypoint may
//! resume delivery — this worker is an optimization, never the only
//! recovery path. Phases commit in order with atomic persistence between
//! them: snap → handoff finalized against that exact root → ingest.

use std::path::Path;

use anyhow::Result;
use serde_json::Value;
use stateroot_core::finalize_journal::{self as journal, BoundaryJob, Phase};
use stateroot_core::local_store::{self, now_rfc3339};

use super::{detached, Ctx};

const SPAWN_ERRORS_LOG: &str = "local/finalize-spawn-errors.log";

/// Kick a detached drain when work is due. Cheap: one directory scan; a
/// fresh queue never spawns. Spawn FAILURE is recorded (never silent) and
/// recovery kicks again at the next safe entrypoint.
pub fn kick(project_dir: &Path) {
    if cfg!(test) || std::env::var_os("STATEROOT_TEST_CMD_PROBES").is_some() {
        return;
    }
    if journal::due(project_dir).is_empty() {
        return;
    }
    let log = local_store::root(project_dir).join(local_store::FINALIZE_LOG_PATH);
    if let Err(err) = detached::spawn_self(&["_drain-finalize".to_string()], project_dir, &log) {
        let path = local_store::root(project_dir).join(SPAWN_ERRORS_LOG);
        let line = serde_json::json!({
            "ts": now_rfc3339(),
            "error": format!("{err:#}"),
        });
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            use std::io::Write as _;
            let _ = file.write_all(format!("{line}\n").as_bytes());
        }
    }
}

fn lock_path(project_dir: &Path) -> std::path::PathBuf {
    local_store::root(project_dir).join(local_store::FINALIZE_LOCK_PATH)
}

/// CLI entry: drain the current project's journal.
pub async fn run(ctx: &Ctx) -> Result<()> {
    run_drain(ctx).await?;
    // Post-output lifecycle path: flush any queued telemetry on the way out
    // (detached, single-flight, never blocking this worker's budget).
    crate::telemetry::kick_drain(ctx);
    Ok(())
}

/// Drain loop: single-flight (stale-recovering), legacy migration, then
/// advance every due job one phase at a time until none are due. Safe to
/// call from tests.
pub async fn run_drain(ctx: &Ctx) -> Result<()> {
    // Single-flight: another live drainer is a skip (not an error); a
    // crashed drainer's stale lock is reclaimed by ResourceLock.
    let Ok(_lock) =
        stateroot_core::safe_io::ResourceLock::acquire_with_budget(lock_path(&ctx.cwd), 1, 0)
    else {
        return Ok(());
    };
    migrate_legacy_outbox(ctx)?;
    loop {
        let due = journal::due(&ctx.cwd);
        if due.is_empty() {
            break;
        }
        for mut job in due {
            if let Err(err) = advance_one(ctx, &mut job).await {
                journal::mark_error(&ctx.cwd, &mut job, &format!("{err:#}"))?;
            }
        }
    }
    Ok(())
}

/// Advance one job through exactly one phase (the state machine persists
/// the transition; a crash between phases resumes from disk).
async fn advance_one(ctx: &Ctx, job: &mut BoundaryJob) -> Result<()> {
    match job.phase {
        Phase::Queued => {
            // 1. Snapshot FIRST — the boundary's handoff must reference the
            //    root produced for this boundary. Budget exhaustion skips
            //    only the automatic root (recorded by the engine and surfaced
            //    by `doctor`): the boundary still finalizes against the
            //    current tip instead of failing the whole finalize job.
            let root = match stateroot_core::roots::snap_if_changed(
                &ctx.cwd,
                &job.harness,
                "auto: session boundary",
                None,
            ) {
                Ok(stateroot_core::roots::SnapOutcome::Created(manifest, _)) => manifest.id,
                Ok(stateroot_core::roots::SnapOutcome::Unchanged { root }) => root,
                Err(stateroot_core::roots::RootsError::SnapshotBudget(_)) => {
                    job.last_error = Some("automatic snapshot skipped: scan budget".to_string());
                    stateroot_core::roots::latest_root(&ctx.cwd)?
                        .unwrap_or_else(|| "none".to_string())
                }
                Err(err) => return Err(err.into()),
            };
            journal::transition(&ctx.cwd, job, Phase::Snapped, Some(root), None)?;
        }
        Phase::Snapped => {
            // 2. Finalize the handoff AGAINST the boundary's exact root.
            let root = job
                .root
                .clone()
                .ok_or_else(|| anyhow::anyhow!("snapped job without a root"))?;
            let seq = super::handoff::finalize_for_boundary(ctx, &job.harness, &root)?;
            journal::transition(&ctx.cwd, job, Phase::HandoffFinalized, None, Some(seq))?;
        }
        Phase::HandoffFinalized => {
            // 3. Ingest/index (wiki inbox + FTS rebuild-if-needed).
            super::compiler::try_ingest(ctx, false).await?;
            journal::transition(&ctx.cwd, job, Phase::Ingested, None, None)?;
        }
        Phase::Ingested => {
            journal::transition(&ctx.cwd, job, Phase::Complete, None, None)?;
        }
        Phase::Complete => {
            journal::transition(&ctx.cwd, job, Phase::Complete, None, None)?;
        }
    }
    Ok(())
}

/// Migrate the WS2 outbox into composite journal jobs — explicitly, never
/// consuming unknown kinds as malformed work. The legacy file is renamed
/// aside (preserved), and unknown-kind ops are written back to it.
fn migrate_legacy_outbox(ctx: &Ctx) -> Result<()> {
    let path = local_store::root(&ctx.cwd).join(local_store::OUTBOX_PATH);
    if !path.is_file() {
        return Ok(());
    }
    let ops = local_store::outbox_pending(&ctx.cwd)?;
    if ops.is_empty() {
        // An empty outbox is just clutter; rename it aside anyway.
        let aside = local_store::root(&ctx.cwd).join(format!(
            "outbox.migrated-{}.jsonl",
            now_rfc3339().replace([':', '.'], "-")
        ));
        std::fs::rename(&path, aside)?;
        return Ok(());
    }
    let mut known: Vec<&Value> = Vec::new();
    let mut unknown: Vec<&Value> = Vec::new();
    for op in &ops {
        let kind = op.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        if local_store::FINALIZE_KINDS.contains(&kind) {
            known.push(op);
        } else {
            unknown.push(op);
        }
    }
    // One composite job per distinct (harness, enqueued_at) trio.
    let mut seen: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    for op in known {
        let harness = op
            .get("harness")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let enqueued_at = op.get("enqueued_at").and_then(|v| v.as_str()).unwrap_or("");
        if seen.insert((harness.to_string(), enqueued_at.to_string())) {
            journal::enqueue(
                &ctx.cwd,
                harness,
                "legacy-outbox",
                None,
                "refs/stateroot/latest",
            )?;
        }
    }
    let aside = local_store::root(&ctx.cwd).join(format!(
        "outbox.migrated-{}.jsonl",
        now_rfc3339().replace([':', '.'], "-")
    ));
    std::fs::rename(&path, &aside)?;
    // Unknown kinds are preserved in a fresh outbox — never consumed.
    if !unknown.is_empty() {
        for op in unknown {
            local_store::outbox_append(&ctx.cwd, op)?;
        }
    }
    Ok(())
}

/// The doctor-visible journal report: grouped by state, errors retained.
pub fn report(project_dir: &Path) -> Vec<String> {
    let jobs = journal::load_all(project_dir);
    let mut lines = Vec::new();
    let active: Vec<&BoundaryJob> = jobs.iter().filter(|j| j.state == "active").collect();
    let attention: Vec<&BoundaryJob> = jobs
        .iter()
        .filter(|j| j.state == "manual_attention")
        .collect();
    let terminal: Vec<&BoundaryJob> = jobs.iter().filter(|j| j.state == "terminal").collect();
    for job in active {
        lines.push(format!(
            "  active: {} {} · phase {:?} · attempt {}{}",
            job.harness,
            job.session_id,
            job.phase,
            job.attempt,
            job.last_error
                .as_deref()
                .map(|e| format!(" · last error: {e}"))
                .unwrap_or_default()
        ));
    }
    for job in attention {
        lines.push(format!(
            "  manual_attention: {} {} · attempt {} · last error: {}",
            job.harness,
            job.session_id,
            job.attempt,
            job.last_error.as_deref().unwrap_or("(none retained)")
        ));
    }
    if !terminal.is_empty() {
        lines.push(format!("  complete: {}", terminal.len()));
    }
    if lines.is_empty() {
        lines.push("  no boundary jobs".to_string());
    }
    lines
}
