//! Presentation: the seam between a platform-agnostic sync/plan engine and a
//! local view backend (mirror folder now, git-rooted worktree later).
//!
//! Trimmed during the M1 lift: the `ContentProvider for FsClient` impl was
//! server-coupled and is deliberately not lifted — there is no cloud
//! filesystem in the fork. The traits and plan/entry types remain: a backend
//! implements [`Presentation`] to apply remote-originated plan ops to its
//! local view and to scan that view for reconcile.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;

use crate::error::ApiError;

/// Errors from a presentation backend.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// Local filesystem failure.
    #[error("io error on {path}: {source}")]
    Io {
        /// Path being accessed.
        path: std::path::PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// Any other backend failure (free-form, surfaced verbatim).
    #[error("{0}")]
    Message(String),
}

/// A local file/directory entry discovered by scanning the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEntry {
    /// Workspace-relative path.
    pub path: String,
    /// True for directories.
    pub is_dir: bool,
    /// Size in bytes (0 for directories).
    pub size: u64,
    /// Modification time (unix seconds).
    pub mtime_secs: i64,
}

/// One remote→local operation produced by a plan builder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PlanOp {
    /// Create a local directory (idempotent).
    MkdirLocal {
        /// Workspace-relative path.
        path: String,
    },
    /// Download remote content into a local file.
    WriteLocal {
        /// Workspace-relative path.
        path: String,
        /// Content address for the download.
        file_identity: String,
        /// Server item id.
        item_id: String,
        /// Revision being applied.
        rev: i64,
        /// Expected size in bytes.
        size: i64,
    },
    /// Remove a local path (remote deleted it).
    DeleteLocal {
        /// Workspace-relative path.
        path: String,
        /// Whether the path is a directory.
        is_dir: bool,
    },
    /// Move a local path (remote rename — item id unchanged).
    MoveLocal {
        /// Current local path.
        from: String,
        /// New path.
        to: String,
        /// Whether the path is a directory.
        is_dir: bool,
    },
    /// Both sides changed: save the local file aside before downloading.
    ConflictCopy {
        /// Workspace-relative path.
        path: String,
        /// Conflict copy path (`<name> (conflicted copy <ts>).<ext>`).
        copy_path: String,
        /// Content address for the winning remote download.
        file_identity: String,
        /// Server item id.
        item_id: String,
        /// Winning remote revision.
        rev: i64,
    },
}

impl PlanOp {
    /// The workspace-relative path this op primarily affects.
    pub fn path(&self) -> &str {
        match self {
            PlanOp::MkdirLocal { path }
            | PlanOp::WriteLocal { path, .. }
            | PlanOp::DeleteLocal { path, .. }
            | PlanOp::ConflictCopy { path, .. } => path,
            PlanOp::MoveLocal { to, .. } => to,
        }
    }
}

/// Fetches remote file content by item id or file identity.
#[async_trait]
pub trait ContentProvider: Send + Sync {
    /// Download the full content of an item.
    async fn fetch(&self, item_id_or_identity: &str) -> Result<Vec<u8>, ApiError>;
}

/// Result of applying a plan.
#[derive(Debug, Default)]
pub struct ApplyReport {
    /// Ops applied successfully.
    pub applied: usize,
    /// Ops that failed, with the error message.
    pub failed: Vec<(PlanOp, String)>,
    /// Files whose content now matches a remote revision locally:
    /// `(path, rev, size, sha256-hex)` — recorded into the sync cache.
    pub synced_files: Vec<(String, i64, u64, String)>,
}

/// The interface a local-view backend implements for the sync engine.
#[async_trait]
pub trait Presentation: Send + Sync {
    /// Apply remote-originated ops to the local view (in plan order).
    /// Implementations must tolerate missing parents/duplicates where the op
    /// is idempotent by nature (mkdir, delete-if-absent).
    async fn apply(
        &self,
        ops: &[PlanOp],
        content: &Arc<dyn ContentProvider>,
    ) -> Result<ApplyReport, EngineError>;

    /// Scan the current local view (workspace-relative paths, ignore rules
    /// already applied by the caller or the backend).
    fn scan(&self) -> Result<Vec<LocalEntry>, EngineError>;
}
