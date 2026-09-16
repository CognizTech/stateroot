//! Tombstone substrate for canon-purge.
//!
//! Scope-keyed records (`session_id` + `harness` + `purged_at`) live at
//! `.stateroot/local/tombstones.json`. A tombstone is written **before**
//! the target delete (tmp+sync+rename). Canon-purge tombstones are
//! permanent: harness transcripts outlive any TTL, and we never delete
//! other harnesses' files.
//!
//! Doctrine: purge applies ONLY to the canonical session file + derived
//! index. Episodic, snapshots/roots, and handoffs stay immutable — the
//! tombstone stops *resurrection* via `session sync`; history keeps what
//! it already recorded.
//!
//! Incremental indexing (deliberately not built yet): any future
//! incremental index path MUST consult the tombstone set **once per pass**
//! before inserting a transcript/session document. Full-rebuild is today's
//! resurrection shield; an incremental indexer that skipped this gate
//! would resurrect purged sessions into FTS.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::local_store::{self, now_rfc3339};

/// Binary marker so `strings` can prove WS4 is in the linked CLI.
#[used]
static WS4_TOMBSTONE_SESSION_PURGE: &str = "WS4_TOMBSTONE_SESSION_PURGE";

/// Relative path under `.stateroot/`.
pub const TOMBSTONES_REL: &str = "local/tombstones.json";
/// Schema tag on the tombstone file.
pub const SCHEMA_TOMBSTONES_V1: &str = "stateroot.tombstones.v1";

/// How to treat an unreadable tombstone file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TombstonePolicy {
    /// Missing or unreadable → empty set (background sync; never block).
    FailOpen,
    /// Missing → empty set; unreadable → error (explicit `session sync`).
    FailClosed,
}

/// One purged canonical session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    /// Canonical session id (the store header, not a harness filename).
    pub session_id: String,
    /// Harness id (`pi`, `dsh`, `codex`, …). Same id on another harness is
    /// a different record.
    pub harness: String,
    /// RFC3339 timestamp of the tombstone write.
    pub purged_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TombstoneFile {
    schema: String,
    records: Vec<Tombstone>,
}

impl Default for TombstoneFile {
    fn default() -> Self {
        Self {
            schema: SCHEMA_TOMBSTONES_V1.to_string(),
            records: Vec::new(),
        }
    }
}

/// Errors from the tombstone file.
#[derive(Debug, thiserror::Error)]
pub enum TombstoneError {
    /// Filesystem failure.
    #[error("io error on {path}: {source}")]
    Io {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// JSON (de)serialization failure.
    #[error("tombstones unreadable at {path}: {source}")]
    Json {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying error.
        source: serde_json::Error,
    },
}

fn path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(TOMBSTONES_REL)
}

fn lock_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join("local/tombstones.json.lock")
}

fn io_err(path: &Path) -> impl Fn(std::io::Error) -> TombstoneError + '_ {
    move |source| TombstoneError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn read_file(project_dir: &Path) -> Result<TombstoneFile, TombstoneError> {
    let path = path(project_dir);
    match fs::read_to_string(&path) {
        Ok(text) => {
            let parsed: TombstoneFile =
                serde_json::from_str(&text).map_err(|source| TombstoneError::Json {
                    path: path.clone(),
                    source,
                })?;
            Ok(parsed)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(TombstoneFile::default()),
        Err(err) => Err(io_err(&path)(err)),
    }
}

fn atomic_write(project_dir: &Path, file: &TombstoneFile) -> Result<(), TombstoneError> {
    let path = path(project_dir);
    let text = serde_json::to_string(file).map_err(|source| TombstoneError::Json {
        path: path.clone(),
        source,
    })?;
    crate::safe_io::atomic_replace(&path, text.as_bytes()).map_err(io_err(&path))
}

/// Load the tombstone set. Missing file is empty. Unreadable file follows
/// [`TombstonePolicy`].
pub fn load(project_dir: &Path, policy: TombstonePolicy) -> Result<Vec<Tombstone>, TombstoneError> {
    match read_file(project_dir) {
        Ok(file) => Ok(file.records),
        Err(err) => match policy {
            TombstonePolicy::FailOpen => Ok(Vec::new()),
            TombstonePolicy::FailClosed => Err(err),
        },
    }
}

/// True when `(session_id, harness)` is tombstoned.
pub fn contains(records: &[Tombstone], session_id: &str, harness: &str) -> bool {
    records
        .iter()
        .any(|t| t.session_id == session_id && t.harness == harness)
}

/// Write a tombstone **before** the caller deletes the canonical file.
/// Idempotent: a second record for the same pair is a no-op.
pub fn record(
    project_dir: &Path,
    session_id: &str,
    harness: &str,
) -> Result<Tombstone, TombstoneError> {
    let _ = WS4_TOMBSTONE_SESSION_PURGE;
    // Mandatory lock with stale recovery (repair Phase 1) — a failed
    // acquisition fails closed, it never proceeds unlocked.
    let _lock = crate::safe_io::ResourceLock::acquire(lock_path(project_dir)).map_err(|e| {
        TombstoneError::Io {
            path: lock_path(project_dir),
            source: std::io::Error::new(std::io::ErrorKind::ResourceBusy, e),
        }
    })?;
    let mut file = read_file(project_dir)?;
    if let Some(existing) = file
        .records
        .iter()
        .find(|t| t.session_id == session_id && t.harness == harness)
        .cloned()
    {
        return Ok(existing);
    }
    let stone = Tombstone {
        session_id: session_id.to_string(),
        harness: harness.to_string(),
        purged_at: now_rfc3339(),
    };
    file.schema = SCHEMA_TOMBSTONES_V1.to_string();
    file.records.push(stone.clone());
    atomic_write(project_dir, &file)?;
    Ok(stone)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_empty_under_both_policies() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path(), TombstonePolicy::FailOpen)
            .unwrap()
            .is_empty());
        assert!(load(dir.path(), TombstonePolicy::FailClosed)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn corrupt_file_fail_open_empty_fail_closed_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = path(dir.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{nope").unwrap();
        assert!(load(dir.path(), TombstonePolicy::FailOpen)
            .unwrap()
            .is_empty());
        assert!(load(dir.path(), TombstonePolicy::FailClosed).is_err());
    }

    #[test]
    fn record_is_idempotent_and_harness_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let a = record(dir.path(), "ses-1", "pi").unwrap();
        let again = record(dir.path(), "ses-1", "pi").unwrap();
        assert_eq!(a.purged_at, again.purged_at);
        record(dir.path(), "ses-1", "dsh").unwrap();
        let set = load(dir.path(), TombstonePolicy::FailClosed).unwrap();
        assert_eq!(set.len(), 2);
        assert!(contains(&set, "ses-1", "pi"));
        assert!(contains(&set, "ses-1", "dsh"));
        assert!(!contains(&set, "ses-1", "codex"));
    }

    #[test]
    fn tombstone_survives_abandoned_tmp() {
        let dir = tempfile::tempdir().unwrap();
        record(dir.path(), "ses-1", "pi").unwrap();
        let tmp = path(dir.path()).with_file_name("tombstones.json.tmp");
        fs::write(&tmp, "{torn").unwrap();
        let set = load(dir.path(), TombstonePolicy::FailClosed).unwrap();
        assert!(contains(&set, "ses-1", "pi"), "live file intact");
    }
}
