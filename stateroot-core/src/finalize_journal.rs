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
    if let Some(existing) = load_active(project_dir)
        .into_iter()
        .find(|j| j.harness == harness && j.session_id == session_id)
    {
        return Ok(existing);
    }
    let now = now_rfc3339();
    let job = BoundaryJob {
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
        .filter_map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok())
        .collect();
    jobs.sort_by(|a, b| a.enqueued_at.cmp(&b.enqueued_at));
    jobs
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
