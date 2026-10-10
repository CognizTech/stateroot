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

/// Explicit recovery advances only the requested retained job.
pub async fn recover_one(ctx: &Ctx, id: &str, transcript: Option<&str>) -> Result<()> {
    let _lock = stateroot_core::safe_io::ResourceLock::acquire(lock_path(&ctx.cwd))?;
    let mut job = journal::recover(&ctx.cwd, id, transcript)?;
    if job
        .capture_watermark
        .as_ref()
        .is_some_and(|capture| capture["status"] == "unreadable")
    {
        let recovered = stateroot_core::observations::recover_session_frontier(
            &ctx.cwd,
            &job.harness,
            &job.session_id,
        )?;
        job.capture_watermark = Some(
            serde_json::json!({"status":if recovered.is_some(){"recovered"}else{"absent"},"harness":job.harness,"session_id":job.session_id,"watermark":recovered,"observed_at":now_rfc3339(),"coverage":"frontier observed during explicit recovery; not proven boundary-time capture"}),
        );
        journal::save(&ctx.cwd, &job)?;
    }
    while job.state == "active" {
        if let Err(error) = advance_one(ctx, &mut job).await {
            journal::mark_error(&ctx.cwd, &mut job, &format!("{error:#}"))?;
            return Err(error);
        }
    }
    Ok(())
}

/// Advance one job through exactly one phase (the state machine persists
/// the transition; a crash between phases resumes from disk).
async fn advance_one(ctx: &Ctx, job: &mut BoundaryJob) -> Result<()> {
    if let Some(capture) = &job.capture_watermark {
        anyhow::ensure!(
            capture["status"] != "unreadable",
            "boundary capture frontier unavailable: {}; explicit handoff recover --job required",
            capture["reason"].as_str().unwrap_or("unknown source error")
        );
    }
    match job.phase {
        Phase::Queued => {
            anyhow::ensure!(
                job.project_path.is_empty()
                    || stateroot_core::transcripts::same_worktree(
                        Path::new(&job.project_path),
                        &ctx.cwd
                    ),
                "boundary checkout mismatch"
            );
            anyhow::ensure!(
                job.lineage_ref == stateroot_core::roots::lineage_refname(&ctx.cwd),
                "boundary lineage mismatch"
            );
            job.snapshot_timing = format!("delayed automatic snapshot at {}; may include post-boundary edits; boundary_source_root is the prior verified state", now_rfc3339());
            // 1. Snapshot FIRST — the boundary's handoff must reference the
            //    root produced for this boundary. Budget exhaustion skips
            //    only the automatic root (recorded by the engine and surfaced
            //    by `doctor`): the boundary still finalizes against the
            //    current tip instead of failing the whole finalize job.
            let home = stateroot_core::harness_install::home_dir().map_err(anyhow::Error::msg)?;
            let components = super::roots::integration_environment(ctx, &home);
            let snap_context = stateroot_core::snap_context::SnapContext {
                home,
                harness: Some(job.harness.clone()),
                session_id: Some(job.session_id.clone()),
                transcript: job.transcript.clone(),
                capture_watermark: job.capture_watermark.clone(),
                artifact_refs: job.artifact_refs.clone(),
                components,
            };
            let root = match stateroot_core::roots::snap_if_changed(
                &ctx.cwd,
                &job.harness,
                "auto: session boundary",
                Some(&snap_context),
            ) {
                Ok(stateroot_core::roots::SnapOutcome::Created(manifest, _)) => manifest.id,
                Ok(stateroot_core::roots::SnapOutcome::Unchanged { root }) => root,
                Err(stateroot_core::roots::RootsError::SnapshotBudget(_)) => {
                    job.snapshot_timing.push_str(
                        "; snapshot skipped: scan budget; root coverage excludes unsnapped edits",
                    );
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
            let _ = root;
            let seq = super::handoff::finalize_bound_job(ctx, job)?;
            journal::transition(&ctx.cwd, job, Phase::HandoffFinalized, None, Some(seq))?;
        }
        Phase::HandoffFinalized => {
            // 3. Ingest/index (wiki inbox + FTS rebuild-if-needed).
            super::compiler::try_ingest(ctx, false).await?;
            if let Some(capture) = &job.capture_watermark {
                if matches!(capture["status"].as_str(), Some("present" | "recovered")) {
                    anyhow::ensure!(
                        capture["harness"].as_str() == Some(job.harness.as_str())
                            && capture["session_id"].as_str() == Some(job.session_id.as_str()),
                        "capture frontier/session binding mismatch"
                    );
                    let watermark: stateroot_core::observations::CaptureWatermark =
                        serde_json::from_value(capture["watermark"].clone())?;
                    stateroot_core::observations::acknowledge_frontier(
                        &ctx.cwd,
                        &job.harness,
                        &job.session_id,
                        &job.ingest_key,
                        &watermark,
                    )?;
                }
            }
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

/// The doctor-visible journal report: grouped by state, errors retained.
pub fn report(project_dir: &Path) -> Vec<String> {
    let jobs = journal::load_all(project_dir);
    let mut lines = Vec::new();
    if local_store::root(project_dir)
        .join(local_store::OUTBOX_PATH)
        .is_file()
    {
        lines.push("  legacy outbox retained in place; no native session binding inferred or silently migrated".into());
    }
    for path in journal::invalid_records(project_dir) {
        lines.push(format!(
            "  quarantined in place: corrupt/unsupported boundary record {}",
            path.display()
        ));
    }
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
    // Presentation grouping only (WS3 C2): dozens of identical parked rows
    // must not bury the report. Every underlying job stays loadable by id
    // (`stateroot handoff inspect --job <id>`); nothing is reclassified or
    // removed here.
    let mut groups: std::collections::BTreeMap<(String, String), Vec<&BoundaryJob>> =
        Default::default();
    for job in attention {
        groups
            .entry((
                job.harness.clone(),
                job.last_error
                    .clone()
                    .unwrap_or_else(|| "(none retained)".into()),
            ))
            .or_default()
            .push(job);
    }
    for ((harness, error), grouped) in &groups {
        let ids: Vec<&str> = grouped.iter().map(|j| j.id.as_str()).collect();
        let shown = ids.len().min(3);
        let id_list = format!(
            "{}{}",
            ids[..shown].join(", "),
            if ids.len() > shown {
                format!(" · +{} more", ids.len() - shown)
            } else {
                String::new()
            }
        );
        if grouped.len() == 1 {
            let job = grouped[0];
            lines.push(format!(
                "  manual_attention: {} {} · attempt {} · last error: {} · job: {}",
                job.harness, job.session_id, job.attempt, error, job.id
            ));
        } else {
            lines.push(format!(
                "  manual_attention: {} ×{} · last error: {} · jobs: {}",
                harness,
                grouped.len(),
                error,
                id_list
            ));
        }
    }
    if !terminal.is_empty() {
        lines.push(format!("  complete: {}", terminal.len()));
    }
    if lines.is_empty() {
        lines.push("  no boundary jobs".to_string());
    }
    lines
}

#[cfg(test)]
mod tests {
    /// WS3 C2: dozens of identical parked rows collapse to one grouped line
    /// with ids navigable; distinct errors stay separate. Presentation only
    /// — the journal files are untouched.
    #[test]
    fn report_groups_identical_manual_attention_rows() {
        let project = tempfile::tempdir().expect("project");
        let dir = stateroot_core::local_store::root(project.path()).join("local/finalize-journal");
        std::fs::create_dir_all(&dir).expect("journal dir");
        let job = |id: &str, session: &str, error: Option<&str>| {
            serde_json::json!({
                "schema": "stateroot.finalize-journal.v1",
                "id": id,
                "harness": "codex",
                "session_id": session,
                "lineage_ref": "refs/stateroot/main",
                "ingest_key": format!("k-{id}"),
                "enqueued_at": "2026-10-09T00:00:00Z",
                "phase": "queued",
                "next_attempt_at": "2026-10-09T00:00:00Z",
                "state": "manual_attention",
                "last_error": error,
            })
        };
        // 62 identical rows (the owner's real pile shape) + one distinct.
        for i in 0..62 {
            let body = job(&format!("job-{i:03}"), "sess-a", Some("transcript missing"));
            std::fs::write(dir.join(format!("job-{i:03}.json")), body.to_string()).expect("write");
        }
        let other = job("job-x", "sess-b", Some("root write failed"));
        std::fs::write(dir.join("job-x.json"), other.to_string()).expect("write");

        let lines = super::report(project.path());
        let attention: Vec<&String> = lines
            .iter()
            .filter(|l| l.contains("manual_attention"))
            .collect();
        assert_eq!(attention.len(), 2, "grouped: {lines:?}");
        let grouped = attention
            .iter()
            .find(|l| l.contains("×62"))
            .expect("grouped row");
        assert!(grouped.contains("codex"), "{grouped}");
        assert!(grouped.contains("transcript missing"), "{grouped}");
        assert!(grouped.contains("job-"), "{grouped}");
        assert!(grouped.contains("+59 more"), "{grouped}");
        assert!(attention.iter().any(|l| l.contains("root write failed")));
    }
}
