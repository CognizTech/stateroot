//! Tombstone substrate for canon-purge (repair Phase 5: project-shared).
//!
//! Scope-keyed records (`canonical_session_key` + `purged_at`) live at
//! `.stateroot/tombstones.jsonl` — NOT under `local/`. The file is
//! append-only JSONL, so it travels inside snapshots to every fork
//! worktree and synced machine, and merges by union under the existing
//! `.stateroot/.gitattributes` `*.jsonl merge=union` rule. Every write
//! goes through the provenance funnel (`local_store::report_written`).
//!
//! Doctrine: purge applies ONLY to the canonical session file + derived
//! index. Episodic, snapshots/roots, handoffs, and native harness
//! transcripts stay — the tombstone stops *resurrection* via
//! `session sync`; history keeps what it already recorded. Full
//! DAG/history redaction is a separate owner-approved design, not an
//! implicit extension of this mechanism.
//!
//! The legacy checkout-local file (`.stateroot/local/tombstones.json`,
//! WS4) is migrated on first access: its records are appended to the
//! shared file and the legacy file is renamed aside.
//!
//! Incremental indexing (deliberately not built yet): any future
//! incremental index path MUST consult the tombstone set **once per pass**
//! before inserting a transcript/session document. Full-rebuild is today's
//! resurrection shield; an incremental indexer that skipped this gate
//! would resurrect purged sessions into FTS.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::local_store::{self, now_rfc3339};

/// Binary marker so `strings` can prove WS4 is in the linked CLI.
#[used]
static WS4_TOMBSTONE_SESSION_PURGE: &str = "WS4_TOMBSTONE_SESSION_PURGE";

/// Shared (synced) tombstone file — project-wide, travels with snapshots.
pub const TOMBSTONES_REL: &str = "tombstones.jsonl";
/// Legacy checkout-local file (WS4), migrated on first access.
pub const LEGACY_TOMBSTONES_REL: &str = "local/tombstones.json";
/// Schema tag on the shared file's first line.
pub const SCHEMA_TOMBSTONES_V2: &str = "stateroot.tombstones.v2";

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
    /// Canonical session key (`harness:id` — see [`canonical_session_key`]).
    pub session_key: String,
    /// Harness id (`pi`, `dsh`, `codex`, …). Kept for reporting/scoping.
    pub harness: String,
    /// RFC3339 timestamp of the tombstone write.
    pub purged_at: String,
}

/// The one identity used by canonical import, FTS bundles, list, purge,
/// and tombstones (repair Phase 5): `<harness>:<trimmed id>`. Harness
/// alias normalization (Codex sessionId variants, Cursor SQL/JSON
/// identities, Kimi main/subagent ids) belongs to the transcript import
/// layer that assigns canonical store ids; this key is the consistent
/// join point for everything downstream of it.
pub fn canonical_session_key(harness: &str, session_id: &str) -> String {
    format!("{}:{}", harness.trim(), session_id.trim())
}

/// Errors from the tombstone store.
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

fn legacy_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(LEGACY_TOMBSTONES_REL)
}

fn io_err(path: &Path) -> impl Fn(std::io::Error) -> TombstoneError + '_ {
    move |source| TombstoneError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn lock_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join("local/tombstones.lock")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SchemaLine {
    schema: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyFile {
    #[allow(dead_code)]
    schema: String,
    records: Vec<LegacyRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyRecord {
    session_id: String,
    harness: String,
    purged_at: String,
}

/// Migrate the WS4 checkout-local file into the shared JSONL (append),
/// then rename the legacy file aside. Idempotent: no legacy file is a
/// no-op; a partial migration is safe to re-run (records dedupe on read).
fn migrate_legacy(project_dir: &Path) -> Result<(), TombstoneError> {
    let legacy = legacy_path(project_dir);
    let Ok(text) = std::fs::read_to_string(&legacy) else {
        return Ok(());
    };
    let parsed: LegacyFile =
        serde_json::from_str(&text).map_err(|source| TombstoneError::Json {
            path: legacy.clone(),
            source,
        })?;
    for record in parsed.records {
        let stone = Tombstone {
            session_key: canonical_session_key(&record.harness, &record.session_id),
            harness: record.harness,
            purged_at: record.purged_at,
        };
        append_line(project_dir, &stone)?;
    }
    let aside = legacy.with_extension(format!(
        "migrated-{}",
        now_rfc3339().replace([':', '.'], "-")
    ));
    std::fs::rename(&legacy, &aside).map_err(io_err(&legacy))?;
    Ok(())
}

fn append_line(project_dir: &Path, stone: &Tombstone) -> Result<(), TombstoneError> {
    let path = path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(&path))?;
    }
    if !path.is_file() {
        let header = serde_json::to_string(&SchemaLine {
            schema: SCHEMA_TOMBSTONES_V2.to_string(),
        })
        .map_err(|source| TombstoneError::Json {
            path: path.clone(),
            source,
        })?;
        use std::io::Write as _;
        let mut file = std::fs::File::create(&path).map_err(io_err(&path))?;
        file.write_all(format!("{header}\n").as_bytes())
            .map_err(io_err(&path))?;
    }
    let line = serde_json::to_string(stone).map_err(|source| TombstoneError::Json {
        path: path.clone(),
        source,
    })?;
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(io_err(&path))?;
    file.write_all(format!("{line}\n").as_bytes())
        .map_err(io_err(&path))?;
    file.sync_all().map_err(io_err(&path))?;
    // Synced-store write: route through the provenance funnel so the next
    // snap's write audit stays clean.
    local_store::report_written(project_dir, TOMBSTONES_REL);
    Ok(())
}

fn read_records(project_dir: &Path) -> Result<Vec<Tombstone>, TombstoneError> {
    migrate_legacy(project_dir)?;
    let path = path(project_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(io_err(&path)(err)),
    };
    let mut lines = text.lines();
    // The schema line is validated, never skipped blindly — a torn file
    // must fail closed, not parse as an empty set.
    if let Some(first) = lines.next() {
        let header: SchemaLine =
            serde_json::from_str(first).map_err(|source| TombstoneError::Json {
                path: path.clone(),
                source,
            })?;
        if header.schema != SCHEMA_TOMBSTONES_V2 {
            return Err(TombstoneError::Json {
                path: path.clone(),
                source: serde_json::from_str::<serde_json::Value>("\"schema mismatch\"")
                    .expect_err("marker"),
            });
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut records = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let stone: Tombstone =
            serde_json::from_str(line).map_err(|source| TombstoneError::Json {
                path: path.clone(),
                source,
            })?;
        if seen.insert(stone.session_key.clone()) {
            records.push(stone);
        }
    }
    Ok(records)
}

/// Load the tombstone set. Missing file is empty. Unreadable file follows
/// [`TombstonePolicy`].
pub fn load(project_dir: &Path, policy: TombstonePolicy) -> Result<Vec<Tombstone>, TombstoneError> {
    match read_records(project_dir) {
        Ok(records) => Ok(records),
        Err(err) => match policy {
            TombstonePolicy::FailOpen => Ok(Vec::new()),
            TombstonePolicy::FailClosed => Err(err),
        },
    }
}

/// True when `(session_id, harness)` is tombstoned (via the canonical key).
pub fn contains(records: &[Tombstone], session_id: &str, harness: &str) -> bool {
    let key = canonical_session_key(harness, session_id);
    records.iter().any(|t| t.session_key == key)
}

/// Write a tombstone **before** the caller deletes the canonical file.
/// Idempotent: a second record for the same key is a no-op. The write is
/// serialized under the mandatory session-store lock with stale recovery
/// (fail-closed).
pub fn record(
    project_dir: &Path,
    session_id: &str,
    harness: &str,
) -> Result<Tombstone, TombstoneError> {
    let _lock = store_lock(project_dir)?;
    record_inner(project_dir, session_id, harness)
}

/// The lock-free inner of [`record`] — callers holding `store_lock` (the
/// purge transaction, the import pass) use this; it must NEVER be called
/// without the lock held.
pub fn record_inner(
    project_dir: &Path,
    session_id: &str,
    harness: &str,
) -> Result<Tombstone, TombstoneError> {
    let _ = WS4_TOMBSTONE_SESSION_PURGE;
    let stone = Tombstone {
        session_key: canonical_session_key(harness, session_id),
        harness: harness.to_string(),
        purged_at: now_rfc3339(),
    };
    let records = read_records(project_dir)?;
    if let Some(existing) = records.iter().find(|t| t.session_key == stone.session_key) {
        return Ok(existing.clone());
    }
    append_line(project_dir, &stone)?;
    Ok(stone)
}

/// The mandatory lock that serializes purge AND import/sync of the
/// canonical session store (repair Phase 5). Exposed so
/// `sessions::import` takes the same lock before canonical writes.
pub fn store_lock(project_dir: &Path) -> Result<crate::safe_io::ResourceLock, TombstoneError> {
    crate::safe_io::ResourceLock::acquire(lock_path(project_dir)).map_err(|e| TombstoneError::Io {
        path: lock_path(project_dir),
        source: std::io::Error::new(std::io::ErrorKind::ResourceBusy, e),
    })
}
