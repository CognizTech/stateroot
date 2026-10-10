//! Durable composite session-boundary journal (repair Phase 3).
//!
//! One immutable job per session boundary, advanced through explicit
//! phases with checked persistence between them:
//!
//! ```text
//! queued → snapped(root) → handoff_finalized(seq, root) → ingested → complete
//! ```
//!
//! Invariants (from the repair plan):
//! - A crash at any point leaves replayable state; every safe entrypoint
//!   (session start, checkpoint, stop, doctor, resume) may resume delivery.
//!   A detached worker is an optimization, never the only recovery path.
//! - Phases commit in order: the snapshot first, then the handoff finalized
//!   AGAINST THAT EXACT ROOT, then the ingest.
//! - "Not ready yet" is retryable, never success. Attempts persist with
//!   bounded exponential backoff and the retained last error; exhausted
//!   jobs become `manual_attention` — never dropped, never relabeled
//!   expired.
//! - Unknown/legacy queue records are preserved or explicitly migrated,
//!   never consumed as malformed work.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::local_store::{self, now_rfc3339};

/// Journal directory under `.stateroot/`.
pub const JOURNAL_DIR: &str = "local/finalize-journal";
/// Schema tag on every job file.
pub const SCHEMA: &str = "stateroot.finalize-journal.v1";
/// Attempts before a job parks as `manual_attention` (retained, never dropped).
pub const MAX_ATTEMPTS: u32 = 10;

/// One session-boundary job.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BoundaryJob {
    /// Explicit author-context references frozen at enqueue, not inventory.
    #[serde(default)]
    pub artifact_refs: Vec<crate::fidelity::ArtifactRef>,
    /// Boundary event occurrence identity (not merely session identity).
    #[serde(default)]
    pub occurrence: String,
    /// Exact checkout where this boundary was captured.
    #[serde(default)]
    pub project_path: String,
    /// Captured observation evidence watermark.
    #[serde(default)]
    pub capture_watermark: Option<serde_json::Value>,
    /// Snapshot timing and honest coverage notes.
    #[serde(default)]
    pub snapshot_timing: String,
    /// Last verified root available synchronously at enqueue; may omit pending edits.
    #[serde(default)]
    pub boundary_source_root: Option<String>,
    /// Plan reference frozen with the boundary, rather than selected at drain time.
    #[serde(default)]
    pub plan_ref: Option<serde_json::Value>,
    /// Append-only explicit recovery attempts.
    #[serde(default)]
    pub recoveries: Vec<serde_json::Value>,
    /// Schema tag.
    pub schema: String,
    /// Job id (ulid — time-ordered).
    pub id: String,
    /// Harness that ended the session.
    pub harness: String,
    /// Canonical session identity (best-known at enqueue; payload id).
    pub session_id: String,
    /// Transcript locator when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    /// Lineage ref the boundary's work belongs to.
    pub lineage_ref: String,
    /// Idempotency key for the whole boundary.
    pub ingest_key: String,
    /// Enqueue timestamp.
    pub enqueued_at: String,
    /// Current phase.
    pub phase: Phase,
    /// Root produced by the snap phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Handoff seq produced by the finalize phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_seq: Option<i64>,
    /// Work attempts so far.
    #[serde(default)]
    pub attempt: u32,
    /// Next eligible attempt time (RFC3339).
    pub next_attempt_at: String,
    /// Retained last error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// `active` | `terminal` | `manual_attention`.
    pub state: String,
}

/// Journal phases, in commit order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Enqueued, nothing committed yet.
    Queued,
    /// Snapshot committed (job.root set).
    Snapped,
    /// Handoff finalized against job.root (job.handoff_seq set).
    HandoffFinalized,
    /// Ingest/index committed.
    Ingested,
    /// All work done (terminal).
    Complete,
}

fn dir(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(JOURNAL_DIR)
}

fn job_path(project_dir: &Path, id: &str) -> PathBuf {
    dir(project_dir).join(format!("{id}.json"))
}

fn valid_job(job: &BoundaryJob, path: &Path) -> bool {
    job.schema == SCHEMA
        && !job.id.is_empty()
        && !job.ingest_key.is_empty()
        && !job.id.contains(['/', '\\'])
        && path.file_stem().and_then(|name| name.to_str()) == Some(job.id.as_str())
        && matches!(
            job.state.as_str(),
            "active" | "terminal" | "manual_attention"
        )
        && chrono::DateTime::parse_from_rfc3339(&job.enqueued_at).is_ok()
}

fn new_id() -> String {
    // uuid v7: time-ordered, no new dependency for the journal.
    uuid::Uuid::now_v7().to_string()
}

/// Enqueue one boundary job. Idempotent per (harness, session_id): an
/// active job for the same boundary is returned instead of duplicated.
pub fn enqueue(
    project_dir: &Path,
    harness: &str,
    session_id: &str,
    transcript: Option<&str>,
    lineage_ref: &str,
) -> std::io::Result<BoundaryJob> {
    enqueue_occurrence(
        project_dir,
        harness,
        session_id,
        transcript,
        lineage_ref,
        session_id,
    )
}

/// Serialize event enqueue, including terminal replay recognition.
pub fn enqueue_occurrence(
    project_dir: &Path,
    harness: &str,
    session_id: &str,
    transcript: Option<&str>,
    lineage_ref: &str,
    occurrence: &str,
) -> std::io::Result<BoundaryJob> {
    enqueue_with_capture(
        project_dir,
        harness,
        session_id,
        transcript,
        lineage_ref,
        occurrence,
        None,
    )
}

/// Capture-aware enqueue: the caller freezes a verified session frontier before
/// publishing the job, so a drainer never races a later metadata attachment.
pub fn enqueue_with_capture(
    project_dir: &Path,
    harness: &str,
    session_id: &str,
    transcript: Option<&str>,
    lineage_ref: &str,
    occurrence: &str,
    capture_watermark: Option<serde_json::Value>,
) -> std::io::Result<BoundaryJob> {
    let _lock = crate::safe_io::ResourceLock::acquire_with_budget(
        dir(project_dir).join("enqueue.lock"),
        40,
        25,
    )
    .map_err(std::io::Error::other)?;
    if let Some(existing) = load_all(project_dir).into_iter().find(|j| {
        j.harness == harness
            && j.session_id == session_id
            && j.occurrence == occurrence
            && j.lineage_ref == lineage_ref
    }) {
        return Ok(existing);
    }
    let now = now_rfc3339();
    let job = BoundaryJob {
        artifact_refs: crate::harness_install::home_dir()
            .ok()
            .and_then(|home| {
                local_store::read_handoff_local(project_dir)
                    .ok()
                    .flatten()
                    .map(|packet| crate::fidelity::boundary_references(project_dir, &home, &packet))
            })
            .unwrap_or_default(),
        occurrence: occurrence.to_string(),
        project_path: project_dir.to_string_lossy().into_owned(),
        capture_watermark,
        snapshot_timing: "unresolved".into(),
        boundary_source_root: crate::roots::latest_root(project_dir).ok().flatten(),
        plan_ref: crate::plans::active(project_dir).map(
            |(plan, _)| serde_json::json!({"id":plan.id,"title":plan.title,"status":plan.status}),
        ),
        recoveries: Vec::new(),
        schema: SCHEMA.to_string(),
        id: new_id(),
        harness: harness.to_string(),
        session_id: session_id.to_string(),
        transcript: transcript.map(str::to_string),
        lineage_ref: lineage_ref.to_string(),
        ingest_key: new_id(),
        enqueued_at: now.clone(),
        phase: Phase::Queued,
        root: None,
        handoff_seq: None,
        attempt: 0,
        next_attempt_at: now,
        last_error: None,
        state: "active".to_string(),
    };
    save(project_dir, &job)?;
    Ok(job)
}

/// Persist the job (checked atomic replace between every phase).
pub fn save(project_dir: &Path, job: &BoundaryJob) -> std::io::Result<()> {
    let value = serde_json::to_value(job)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    crate::safe_io::atomic_replace_json(&job_path(project_dir, &job.id), &value)
}

/// All job files that parse (unknown/corrupt files are skipped, never
/// consumed — preserved on disk for inspection or explicit migration).
pub fn load_all(project_dir: &Path) -> Vec<BoundaryJob> {
    let mut jobs: Vec<BoundaryJob> = std::fs::read_dir(dir(project_dir))
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            let job: BoundaryJob = serde_json::from_str(&text).ok()?;
            valid_job(&job, &e.path()).then_some(job)
        })
        .collect();
    jobs.sort_by(|a, b| a.enqueued_at.cmp(&b.enqueued_at));
    jobs
}

/// Preserve and name corrupt or unsupported files rather than treating them as an empty queue.
pub fn invalid_records(project_dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir(project_dir))
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|entry| {
            let valid = std::fs::read(entry.path())
                .ok()
                .and_then(|bytes| serde_json::from_slice::<BoundaryJob>(&bytes).ok())
                .is_some_and(|job| valid_job(&job, &entry.path()));
            (!valid).then(|| entry.path())
        })
        .collect()
}

/// Small read-only health interface for existing status/doctor projections.
#[derive(Debug, serde::Serialize)]
pub struct JournalHealth {
    /// Active replayable jobs.
    pub active: usize,
    /// Parked jobs retained for explicit recovery.
    pub manual_attention: usize,
    /// Unsupported/corrupt records retained in place.
    pub invalid: usize,
}

/// Health never silently removes production jobs.
pub fn health(project_dir: &Path) -> JournalHealth {
    let jobs = load_all(project_dir);
    JournalHealth {
        active: jobs.iter().filter(|job| job.state == "active").count(),
        manual_attention: jobs
            .iter()
            .filter(|job| job.state == "manual_attention")
            .count(),
        invalid: invalid_records(project_dir).len(),
    }
}

/// Retry one parked job with a permanent recovery trail, preserving its original errors.
pub fn recover(
    project_dir: &Path,
    id: &str,
    transcript: Option<&str>,
) -> std::io::Result<BoundaryJob> {
    let mut job = load_all(project_dir)
        .into_iter()
        .find(|job| job.id == id)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "boundary job unavailable or quarantined",
            )
        })?;
    if job.state == "terminal" {
        return Ok(job);
    }
    job.recoveries.push(serde_json::json!({"at": now_rfc3339(), "phase": job.phase, "attempt": job.attempt, "last_error": job.last_error, "transcript": transcript}));
    if let Some(locator) = transcript {
        job.transcript = Some(locator.to_string());
    }
    job.state = "active".into();
    job.attempt = 0;
    job.next_attempt_at = now_rfc3339();
    save(project_dir, &job)?;
    Ok(job)
}

/// Active jobs (state == `active`).
pub fn load_active(project_dir: &Path) -> Vec<BoundaryJob> {
    load_all(project_dir)
        .into_iter()
        .filter(|j| j.state == "active")
        .collect()
}

/// Active jobs eligible for an attempt right now, oldest first.
pub fn due(project_dir: &Path) -> Vec<BoundaryJob> {
    let now = now_rfc3339();
    load_active(project_dir)
        .into_iter()
        .filter(|j| j.next_attempt_at <= now)
        .collect()
}

/// Bounded exponential backoff: 5s, 10s, 20s, … capped at 30 minutes.
pub fn backoff_secs(attempt: u32) -> u64 {
    let shift = attempt.min(9);
    (5u64 << shift).min(1800)
}

fn rfc3339_in(secs: u64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs as i64))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Record a phase transition with its payload, persisted atomically.
pub fn transition(
    project_dir: &Path,
    job: &mut BoundaryJob,
    phase: Phase,
    root: Option<String>,
    handoff_seq: Option<i64>,
) -> std::io::Result<()> {
    job.phase = phase;
    if let Some(root) = root {
        job.root = Some(root);
    }
    if let Some(seq) = handoff_seq {
        job.handoff_seq = Some(seq);
    }
    // A phase that advanced cleared whatever error preceded it — the journal
    // must not keep displaying a superseded failure as if it were live.
    job.last_error = None;
    if phase == Phase::Complete {
        job.state = "terminal".to_string();
    }
    save(project_dir, job)
}

/// Record a failed attempt: attempt+1, retained error, backoff — and
/// `manual_attention` at the cap. Never a silent drop.
pub fn mark_error(project_dir: &Path, job: &mut BoundaryJob, error: &str) -> std::io::Result<()> {
    job.attempt += 1;
    job.last_error = Some(error.chars().take(500).collect());
    job.next_attempt_at = rfc3339_in(backoff_secs(job.attempt));
    if job.attempt >= MAX_ATTEMPTS {
        job.state = "manual_attention".to_string();
    }
    save(project_dir, job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_replay_and_distinct_occurrences_preserve_one_result() {
        let project = project();
        let mut original = enqueue_occurrence(
            project.path(),
            "codex",
            "session",
            None,
            "refs/stateroot/latest",
            "event-1",
        )
        .unwrap();
        transition(
            project.path(),
            &mut original,
            Phase::Complete,
            None,
            Some(42),
        )
        .unwrap();
        let replay = enqueue_occurrence(
            project.path(),
            "codex",
            "session",
            None,
            "refs/stateroot/latest",
            "event-1",
        )
        .unwrap();
        assert_eq!(replay.id, original.id);
        assert_eq!(replay.handoff_seq, Some(42));
        assert_ne!(
            enqueue_occurrence(
                project.path(),
                "codex",
                "session",
                None,
                "refs/stateroot/latest",
                "event-2"
            )
            .unwrap()
            .id,
            original.id
        );
    }

    #[test]
    fn unsupported_records_are_preserved_and_visible() {
        let project = project();
        std::fs::create_dir_all(dir(project.path())).unwrap();
        let path = dir(project.path()).join("unknown.json");
        std::fs::write(&path, b"{bad").unwrap();
        assert_eq!(health(project.path()).invalid, 1);
        assert_eq!(std::fs::read(path).unwrap(), b"{bad");
    }

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(dir.path().join(".stateroot")).expect("stateroot");
        dir
    }

    #[test]
    fn enqueue_is_idempotent_per_boundary_and_persists() {
        let dir = project();
        let a = enqueue(dir.path(), "kimi", "s-1", None, "refs/stateroot/latest").expect("a");
        let b = enqueue(dir.path(), "kimi", "s-1", None, "refs/stateroot/latest").expect("b");
        assert_eq!(a.id, b.id, "same boundary must not duplicate");
        let c = enqueue(dir.path(), "kimi", "s-2", None, "refs/stateroot/latest").expect("c");
        assert_ne!(a.id, c.id);
        let active = load_active(dir.path());
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].phase, Phase::Queued);
    }

    #[test]
    fn transitions_persist_across_reloads() {
        let dir = project();
        let mut job =
            enqueue(dir.path(), "codex", "s-9", None, "refs/stateroot/latest").expect("enqueue");
        transition(
            dir.path(),
            &mut job,
            Phase::Snapped,
            Some("root-abc".into()),
            None,
        )
        .expect("snap");
        // Simulate a crash: drop the in-memory copy, reload from disk.
        let mut reloaded = load_active(dir.path())
            .into_iter()
            .find(|j| j.id == job.id)
            .expect("reloaded");
        assert_eq!(reloaded.phase, Phase::Snapped);
        assert_eq!(reloaded.root.as_deref(), Some("root-abc"));
        transition(
            dir.path(),
            &mut reloaded,
            Phase::HandoffFinalized,
            None,
            Some(7),
        )
        .expect("finalize");
        transition(dir.path(), &mut reloaded, Phase::Ingested, None, None).expect("ingest");
        transition(dir.path(), &mut reloaded, Phase::Complete, None, None).expect("complete");
        let reloaded = load_all(dir.path())
            .into_iter()
            .find(|j| j.id == job.id)
            .expect("final");
        assert_eq!(reloaded.state, "terminal");
        assert_eq!(reloaded.handoff_seq, Some(7));
        assert!(due(dir.path()).is_empty(), "terminal jobs are never due");
    }

    #[test]
    fn errors_back_off_and_park_as_manual_attention_never_dropped() {
        let dir = project();
        let mut job =
            enqueue(dir.path(), "cursor", "s-e", None, "refs/stateroot/latest").expect("enqueue");
        for i in 1..=MAX_ATTEMPTS {
            mark_error(dir.path(), &mut job, &format!("boom {i}")).expect("mark");
        }
        let reloaded = load_all(dir.path())
            .into_iter()
            .find(|j| j.id == job.id)
            .expect("reloaded");
        assert_eq!(reloaded.state, "manual_attention");
        assert!(reloaded
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("boom"));
        assert_eq!(reloaded.attempt, MAX_ATTEMPTS);
        assert!(
            load_active(dir.path()).is_empty(),
            "manual_attention jobs leave the active set but stay on disk"
        );
        assert!(dir
            .path()
            .join(".stateroot/local/finalize-journal")
            .join(format!("{}.json", job.id))
            .is_file());
    }

    #[test]
    fn due_respects_next_attempt_and_orders_oldest_first() {
        let dir = project();
        let mut older = enqueue(dir.path(), "a", "s-1", None, "r").expect("a");
        let _newer = enqueue(dir.path(), "a", "s-2", None, "r").expect("b");
        mark_error(dir.path(), &mut older, "not yet").expect("mark");
        let due_now = due(dir.path());
        // The errored job is in backoff; only the fresh one is due.
        assert_eq!(due_now.len(), 1);
        assert_eq!(due_now[0].session_id, "s-2");
    }
}
