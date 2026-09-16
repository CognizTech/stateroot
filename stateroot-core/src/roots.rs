//! Git-plumbing roots (M2) — the files-first centerpiece.
//!
//! A **root** is a `git commit-tree` of the working state: project files
//! (honoring root `.gitignore` and `.staterootignore` via
//! [`crate::sync_engine::ignore`], plus hardcoded `.git/` / `.stateroot/local/`)
//! plus the `.stateroot/` tree itself. Commits live under
//! `refs/stateroot/roots/<hash>` with `refs/stateroot/latest` as the head
//! pointer; the user's branch log and index are never touched (plumbing
//! only — no checkout, no `refs/heads/*`, no index writes).
//!
//! Revert is append-only: a NEW root whose tree equals the target root's
//! tree. Forks are branch refs under `refs/stateroot/forks/` (kept out of
//! `refs/heads/` so the user's branch list stays clean; the report prints
//! the worktree command for real materialization).
//!
//! Lineage self-containment: the transition + root-manifest files are
//! written right after their commit, so every snapshot carries the full
//! history *up to its predecessor* (a root cannot contain its own hash —
//! the egg comes after the chicken by construction).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::local_store::{self, now_rfc3339};
use crate::sync_engine::ignore::IgnoreRules;

/// Binary marker so `strings` can prove WS3(1-3) is in the linked CLI.
#[used]
static WS3_ROOTS_COMMIT_LOCK: &str = "WS3_ROOTS_COMMIT_LOCK";

/// Ref namespace for root commits.
pub const ROOTS_REF_PREFIX: &str = "refs/stateroot/roots/";
/// Head pointer to the latest root.
pub const LATEST_REF: &str = "refs/stateroot/latest";
/// Ref namespace for fork materializations.
pub const FORKS_REF_PREFIX: &str = "refs/stateroot/forks/";
/// `.stateroot/roots/<hash>.json`.
pub const ROOTS_DIR: &str = "roots";
/// `.stateroot/transitions/<id>.json`.
pub const TRANSITIONS_DIR: &str = "transitions";
/// `.stateroot/forks/<name>.json`.
pub const FORKS_DIR: &str = "forks";
/// Root manifest schema.
pub const ROOT_SCHEMA: &str = "stateroot.root.local.v1";
/// Transition schema (same shape family as the server variant).
pub const TRANSITION_SCHEMA: &str = "stateroot.transition.local.v1";

/// Errors from the roots engine.
#[derive(Debug, thiserror::Error)]
pub enum RootsError {
    /// git2 plumbing failure.
    #[error(transparent)]
    Git(#[from] git2::Error),
    /// Local filesystem failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Local store failure.
    #[error(transparent)]
    Store(#[from] local_store::LocalStoreError),
    /// JSON (de)serialization failure.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// A named object does not exist.
    #[error("{0}")]
    NotFound(String),
    /// A merge cannot complete cleanly (conflicts or nothing to fold).
    #[error("{0}")]
    Merge(String),
    /// Ref CAS / mandatory-lock failure.
    #[error(transparent)]
    RefCas(#[from] crate::safe_io::RefCasError),
}

/// Persisted root manifest (`.stateroot/roots/<hash>.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RootManifest {
    /// Schema id.
    #[serde(default)]
    pub schema_version: String,
    /// Commit hash.
    #[serde(default)]
    pub id: String,
    /// Parent root hashes (0 = genesis, 1 = previous root).
    #[serde(default)]
    pub parents: Vec<String>,
    /// Creation timestamp (RFC 3339).
    #[serde(default)]
    pub created_at: String,
    /// Harness that created the root.
    #[serde(default)]
    pub created_by_harness: String,
    /// Free-text creation reason.
    #[serde(default)]
    pub created_reason: String,
    /// Project files pinned (the `.stateroot/` tree is not counted).
    #[serde(default)]
    pub files_pinned: i64,
    /// `full` | `state_only` (empty/ignored project tree).
    #[serde(default)]
    pub coverage: String,
    /// Total pinned bytes (large-repo guard; sync warns past the cap).
    #[serde(default)]
    pub tree_bytes: u64,
}

/// A transition linking from-root → to-root.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Transition {
    /// Schema id.
    #[serde(default)]
    pub schema_version: String,
    /// Transition id (uuid v7 — time-ordered).
    #[serde(default)]
    pub id: String,
    /// Source root (empty for genesis).
    #[serde(default)]
    pub from_root: String,
    /// Destination root.
    #[serde(default)]
    pub to_root: String,
    /// `snapshot` | `revert`.
    #[serde(default)]
    pub kind: String,
    /// Objective at transition time (from project state).
    #[serde(default)]
    pub objective: String,
    /// Harness that drove it.
    #[serde(default)]
    pub harness: String,
    /// Evidence bag (reason, revert_to, …).
    #[serde(default)]
    pub evidence: Value,
    /// Creation timestamp.
    #[serde(default)]
    pub created_at: String,
}

/// Open the repo at `dir`, or `git init` it (M1 rule: non-git folders get a
/// silent repo). A parent repo is NOT reused — the project dir is the sync
/// root, and snapshotting an ancestor's whole tree would be wrong.
pub fn ensure_repo(dir: &Path) -> Result<Repository, RootsError> {
    if dir.join(".git").exists() {
        return Ok(git2::Repository::open(dir)?);
    }
    Ok(git2::Repository::init(dir)?)
}

use git2::Repository;

fn signature() -> Result<git2::Signature<'static>, git2::Error> {
    git2::Signature::now("StateRoot", "local@stateroot")
}

/// Build a git tree from the working directory honoring the ignore rules.
/// Returns the tree oid and the number of project (non-`.stateroot`) files.
/// Large-repo guard threshold (sync warn): trees beyond this get a
/// `.staterootignore` hint.
pub const TREE_SIZE_WARN_BYTES: u64 = 200 * 1024 * 1024;

/// The outcome of one `build_tree` run (tree + counters for proof tests).
#[derive(Debug, Clone)]
pub(crate) struct TreeBuild {
    pub(crate) tree: git2::Oid,
    pub(crate) pinned: i64,
    pub(crate) bytes: u64,
    /// Files reused from the stat index without re-hashing.
    #[allow(dead_code)]
    pub(crate) index_hits: u64,
    /// Files read + hashed this run.
    #[allow(dead_code)]
    pub(crate) index_misses: u64,
}

/// Machine-local stat index (`.stateroot/local/blob_index.json` — never
/// pinned into roots, never synced). Maps rel path → ((mtime secs, nanos),
/// len, oid).
///
/// The reuse rule is git's racy-clean rule, not naive stat matching: a
/// stored blob is reused only when the file's mtime is STRICTLY OLDER than
/// the index's own write time. Anything newer-or-equal is re-hashed — on
/// filesystems with lying or coarse mtimes (DrvFs) that costs extra hashes
/// (safe), never stale roots (correct). A missing or corrupt index is the
/// recovery path: one full re-hash, then the index is rewritten.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct BlobIndex {
    written_at: (u64, u32),
    entries: std::collections::HashMap<String, ((u64, u32), u64, String)>,
}

fn blob_index_path(dir: &Path) -> PathBuf {
    dir.join(".stateroot/local/blob_index.json")
}

fn load_blob_index(dir: &Path) -> BlobIndex {
    let text = std::fs::read_to_string(blob_index_path(dir)).unwrap_or_default();
    serde_json::from_str(&text).unwrap_or_default()
}

fn write_blob_index(dir: &Path, index: &BlobIndex) {
    let path = blob_index_path(dir);
    let Ok(text) = serde_json::to_string(index) else {
        return;
    };
    let _ = crate::safe_io::atomic_replace(&path, text.as_bytes());
}

fn now_stamp() -> (u64, u32) {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs(), d.subsec_nanos()))
        .unwrap_or((0, 0))
}

/// Build the working tree; returns the tree, pin count, bytes, and index
/// hit/miss counters (proof of the stat-cache contract in tests).
fn build_tree(repo: &Repository, dir: &Path) -> Result<TreeBuild, RootsError> {
    let rules = IgnoreRules::load(dir);
    let index_written_at = now_stamp();
    let index = load_blob_index(dir);
    let mut next_index = BlobIndex {
        written_at: index_written_at,
        ..Default::default()
    };
    let mut hits = 0u64;
    let mut misses = 0u64;
    #[allow(clippy::too_many_arguments)]
    fn walk(
        repo: &Repository,
        root: &Path,
        dir: &Path,
        rules: &IgnoreRules,
        index: &BlobIndex,
        next_index: &mut BlobIndex,
        hits: &mut u64,
        misses: &mut u64,
        pinned: &mut i64,
        total_bytes: &mut u64,
    ) -> Result<Option<git2::Oid>, RootsError> {
        let mut builder = repo.treebuilder(None)?;
        let mut any = false;
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
            .flatten()
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for path in entries {
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };
            let rel = path
                .strip_prefix(root)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if path.is_dir() {
                // `.stateroot/local/` is the quarantine lane (sync state,
                // machine-local notes) — it NEVER enters roots, and thus
                // never syncs. (Files-first doctrine.)
                if rel == ".stateroot/local" || rel.starts_with(".stateroot/local/") {
                    continue;
                }
                if rules.is_ignored(&rel, true) {
                    continue;
                }
                if let Some(sub) = walk(
                    repo,
                    root,
                    &path,
                    rules,
                    index,
                    next_index,
                    hits,
                    misses,
                    pinned,
                    total_bytes,
                )? {
                    builder.insert(&name, sub, 0o040000)?;
                    any = true;
                }
            } else if path.is_file() {
                if rel.starts_with(".stateroot/local/") {
                    continue;
                }
                if rules.is_ignored(&rel, false) {
                    continue;
                }
                let meta = std::fs::metadata(&path)?;
                let len = meta.len();
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| (d.as_secs(), d.subsec_nanos()))
                    .unwrap_or((0, 0));
                // Racy-clean reuse: only strictly-older mtimes trust the index.
                let reused = index
                    .entries
                    .get(&rel)
                    .filter(|(stored_mtime, stored_len, _)| {
                        *stored_mtime == mtime && *stored_len == len && mtime < index.written_at
                    })
                    .and_then(|(_, _, oid)| git2::Oid::from_str(oid).ok());
                let blob = match reused {
                    Some(oid) => {
                        *hits += 1;
                        *total_bytes += len;
                        next_index
                            .entries
                            .insert(rel.clone(), (mtime, len, oid.to_string()));
                        oid
                    }
                    None => {
                        *misses += 1;
                        let bytes = read_validated(&path, len, mtime)?;
                        *total_bytes += bytes.len() as u64;
                        let oid = repo.blob(&bytes)?;
                        next_index
                            .entries
                            .insert(rel.clone(), (mtime, len, oid.to_string()));
                        oid
                    }
                };
                builder.insert(&name, blob, 0o100644)?;
                any = true;
                if !rel.starts_with(".stateroot/") && rel != ".stateroot" {
                    *pinned += 1;
                }
            }
            // Symlinks and special files are skipped (M2 minimal).
        }
        if any {
            Ok(Some(builder.write()?))
        } else {
            Ok(None)
        }
    }
    let mut pinned = 0i64;
    let mut total_bytes = 0u64;
    let tree = walk(
        repo,
        dir,
        dir,
        &rules,
        &index,
        &mut next_index,
        &mut hits,
        &mut misses,
        &mut pinned,
        &mut total_bytes,
    )?;
    // An empty tree is legal (state_only roots before any project file).
    let tree = match tree {
        Some(oid) => oid,
        None => repo.treebuilder(None)?.write()?,
    };
    write_blob_index(dir, &next_index);
    Ok(TreeBuild {
        tree,
        pinned,
        bytes: total_bytes,
        index_hits: hits,
        index_misses: misses,
    })
}

/// Read a file for a blob with metadata validation BEFORE and AFTER the
/// read (repair Phase 4): a short/torn or mid-write read retries with
/// backoff; a file that never stabilizes is an honest error, never a
/// silently wrong blob.
fn read_validated(
    path: &Path,
    expected_len: u64,
    expected_mtime: (u64, u32),
) -> std::io::Result<Vec<u8>> {
    for attempt in 0..4 {
        let bytes = crate::fs_lock::read_with_retry(path)?;
        let after = std::fs::metadata(path)?;
        let after_len = after.len();
        let after_mtime = after
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| (d.as_secs(), d.subsec_nanos()))
            .unwrap_or((0, 0));
        if after_len == expected_len
            && after_mtime == expected_mtime
            && bytes.len() as u64 == expected_len
        {
            return Ok(bytes);
        }
        if attempt < 3 {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("file kept changing mid-read: {}", path.display()),
    ))
}

/// The ref this checkout's lineage hangs on (WS5): the fork ref inside a
/// fork worktree (machine-local fork-context.json), else
/// `refs/stateroot/latest`. Worktrees share the git dir, so the fork ref is
/// readable and writable from inside the worktree — but everything else on
/// the trunk keeps following `latest`.
///
/// Fail closed (repair Phase 4): a worktree stamped as a fork checkout
/// whose context is unreadable, or whose fork ref is missing, is an ERROR
/// for lineage WRITES — never a silent fallback to the trunk.
pub fn lineage_refname(project_dir: &Path) -> String {
    lineage_refname_checked(project_dir).unwrap_or_else(|_| LATEST_REF.to_string())
}

/// The checked form of [`lineage_refname`] for lineage writes.
pub fn lineage_refname_checked(project_dir: &Path) -> Result<String, RootsError> {
    let context_path = local_store::root(project_dir).join("local/fork-context.json");
    if !context_path.is_file() {
        return Ok(LATEST_REF.to_string());
    }
    let context = local_store::fork_context(project_dir).ok_or_else(|| {
        RootsError::Merge(format!(
            "fork context at {} is unreadable — refusing to fall back to the trunk",
            context_path.display()
        ))
    })?;
    Ok(format!("{FORKS_REF_PREFIX}{}", context.fork))
}

fn latest_oid_for(repo: &Repository, project_dir: &Path) -> Result<Option<git2::Oid>, RootsError> {
    let refname = lineage_refname_checked(project_dir)?;
    match repo.refname_to_id(&refname) {
        Ok(oid) => Ok(Some(oid)),
        Err(_) if refname == LATEST_REF => Ok(None),
        Err(err) => Err(RootsError::Merge(format!(
            "fork lineage ref {refname} is missing ({err}) — refusing to fall back to the trunk"
        ))),
    }
}

fn read_objective(project_dir: &Path) -> String {
    let path = local_store::root(project_dir).join(local_store::STATE_PATH);
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|state| {
            state
                .get("objective")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), RootsError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let pretty = serde_json::to_string_pretty(value)?;
    std::fs::write(path, format!("{pretty}\n"))?;
    Ok(())
}

fn commit_root(
    repo: &Repository,
    tree: git2::Oid,
    parents: &[git2::Oid],
    message: &str,
) -> Result<git2::Oid, RootsError> {
    let sig = signature()?;
    let tree = repo.find_tree(tree)?;
    let parent_commits: Vec<git2::Commit> = parents
        .iter()
        .map(|oid| repo.find_commit(*oid))
        .collect::<Result<_, _>>()?;
    let parent_refs: Vec<&git2::Commit> = parent_commits.iter().collect();
    Ok(repo.commit(None, &sig, &sig, message, &tree, &parent_refs)?)
}

#[allow(clippy::too_many_arguments)]
fn persist_root(
    repo: &Repository,
    project_dir: &Path,
    oid: git2::Oid,
    parent_hashes: Vec<String>,
    harness: &str,
    reason: &str,
    files_pinned: i64,
    tree_bytes: u64,
    kind: &str,
    evidence: Value,
) -> Result<(RootManifest, Transition), RootsError> {
    // The lineage write span — per-hash ref, tip check, tip write — holds
    // the ref's resource lock for the WHOLE span (repair Phase 4): two
    // simultaneous identical commits can no longer collide on libgit2's
    // per-ref lock file, and the caller's parent must still be the tip
    // (CAS). Acquisition failure fails closed.
    let hash = oid.to_string();
    let expected_parent: Option<git2::Oid> = parent_hashes
        .first()
        .and_then(|h| git2::Oid::from_str(h).ok());
    let lock_dir = local_store::root(project_dir).join("local/locks");
    let lineage = lineage_refname(project_dir);
    let _lock =
        crate::safe_io::ResourceLock::acquire(crate::safe_io::ref_lock_path(&lock_dir, &lineage))
            .map_err(crate::safe_io::RefCasError::from)?;
    // WS5: inside a fork worktree this advances the fork's tip ref, so the
    // trunk's `latest` is untouched by fork-side work (and vice versa).
    repo.reference(&format!("{ROOTS_REF_PREFIX}{hash}"), oid, true, "root")?;
    let current = repo.refname_to_id(&lineage).ok();
    if current != expected_parent {
        return Err(crate::safe_io::RefCasError::Moved {
            refname: lineage,
            expected: expected_parent
                .map(|o| o.to_string())
                .unwrap_or_else(|| "<none>".into()),
            current: current
                .map(|o| o.to_string())
                .unwrap_or_else(|| "<none>".into()),
        }
        .into());
    }
    repo.reference(&lineage, oid, true, "latest root")?;

    let from = parent_hashes.first().cloned().unwrap_or_default();
    let transition = Transition {
        schema_version: TRANSITION_SCHEMA.into(),
        id: uuid::Uuid::now_v7().to_string(),
        from_root: from,
        to_root: hash.clone(),
        kind: kind.into(),
        objective: read_objective(project_dir),
        harness: harness.into(),
        evidence,
        created_at: now_rfc3339(),
    };
    let root = local_store::root(project_dir);
    let transition_rel = format!("{TRANSITIONS_DIR}/{}.json", transition.id);
    write_json(&root.join(&transition_rel), &transition)?;
    // Root-generated writes go through the same provenance funnel
    // (Phase 4) — the next snap's audit must be empty for a clean flow.
    local_store::report_written(project_dir, &transition_rel);

    let coverage = if files_pinned == 0 {
        "state_only"
    } else {
        "full"
    };
    let manifest = RootManifest {
        schema_version: ROOT_SCHEMA.into(),
        id: hash,
        parents: parent_hashes,
        created_at: now_rfc3339(),
        created_by_harness: harness.into(),
        created_reason: reason.into(),
        files_pinned,
        coverage: coverage.into(),
        tree_bytes,
    };
    let manifest_rel = format!("{ROOTS_DIR}/{}.json", manifest.id);
    write_json(&root.join(&manifest_rel), &manifest)?;
    local_store::report_written(project_dir, &manifest_rel);
    Ok((manifest, transition))
}

/// `snap`: commit-tree the working state, link to the previous root.
pub fn create_root(
    project_dir: &Path,
    harness: &str,
    reason: &str,
    snap_ctx: Option<&crate::snap_context::SnapContext>,
) -> Result<(RootManifest, Transition), RootsError> {
    let repo = ensure_repo(project_dir)?;
    let build = build_tree(&repo, project_dir)?;
    commit_new_root(
        &repo,
        project_dir,
        build.tree,
        build.pinned,
        build.bytes,
        harness,
        reason,
        snap_ctx,
    )
}

/// Outcome of an automatic snap attempt (`snap_if_changed`).
pub enum SnapOutcome {
    /// The project tree moved since the last root — a new root was created.
    /// Transition is boxed to keep the enum small next to `Unchanged`.
    Created(RootManifest, Box<Transition>),
    /// Project files are identical to the last root — no root created.
    Unchanged {
        /// The root that still describes the current project tree.
        root: String,
    },
}

/// Automatic snap for agent-independent surfaces (checkpoint, turn end).
/// Lineage must never depend on an agent remembering to run `snap`: a root
/// is created ONLY when project files changed since the last root.
/// Bookkeeping churn inside `.stateroot/` is not work and never creates one.
pub fn snap_if_changed(
    project_dir: &Path,
    harness: &str,
    reason: &str,
    snap_ctx: Option<&crate::snap_context::SnapContext>,
) -> Result<SnapOutcome, RootsError> {
    let repo = ensure_repo(project_dir)?;
    let build = build_tree(&repo, project_dir)?;
    if let Some(parent) = latest_oid_for(&repo, project_dir)? {
        if !project_files_changed(&repo, parent, build.tree)? {
            return Ok(SnapOutcome::Unchanged {
                root: parent.to_string(),
            });
        }
    }
    let (manifest, transition) = commit_new_root(
        &repo,
        project_dir,
        build.tree,
        build.pinned,
        build.bytes,
        harness,
        reason,
        snap_ctx,
    )?;
    Ok(SnapOutcome::Created(manifest, Box::new(transition)))
}

/// True when `new_tree` differs from `parent_root`'s tree anywhere outside
/// `.stateroot/`. The store's own bookkeeping (checkpoints, handoff stamps)
/// must not fabricate lineage.
fn project_files_changed(
    repo: &Repository,
    parent_root: git2::Oid,
    new_tree: git2::Oid,
) -> Result<bool, RootsError> {
    let old_tree = repo.find_commit(parent_root)?.tree()?;
    let new_tree = repo.find_tree(new_tree)?;
    let diff = repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), None)?;
    Ok(diff.deltas().any(|delta| {
        let path = delta.new_file().path().or_else(|| delta.old_file().path());
        path.map(|p| !p.starts_with(".stateroot")).unwrap_or(true)
    }))
}

// ---------------------------------------------------------------------------
// Write audit (WS3.4)
//
// Two checks run at every parented root creation, recorded into the
// transition's evidence:
// 1. **Funnel drift** — `.stateroot/` paths (minus `local/`) that changed
//    since the parent root but were never reported through
//    `local_store::report_written`. The funnel only tracks our own
//    high-traffic writers (episodic, handoffs, plans); project code is
//    out of scope by design.
// 2. **Privacy re-verification** — every path in the NEW tree is re-run
//    through the ignore rules. The build walk applies them on the way in;
//    this is the independent net for a rule bug ever letting one through.
// ---------------------------------------------------------------------------

/// Audit outcome for the transition evidence (empty vecs → nothing recorded).
pub struct WriteAudit {
    /// Changed-but-unreported `.stateroot/` paths (capped).
    pub unreported: Vec<String>,
    /// Total unreported count before capping.
    pub unreported_total: usize,
    /// Tree paths that fail the ignore rules (capped).
    pub privacy_violations: Vec<String>,
    /// Total violation count before capping.
    pub privacy_violations_total: usize,
}

const AUDIT_CAP: usize = 20;

fn audit_writes(
    repo: &Repository,
    project_dir: &Path,
    parent: git2::Oid,
    new_tree: git2::Oid,
) -> WriteAudit {
    // Changed `.stateroot/` paths since the parent, with content identity
    // (Phase 4): a path reported once can never mask a later direct
    // rewrite — the tree's content must match the ledger's latest report.
    let mut changed: Vec<(String, u64, String)> = Vec::new();
    if let (Ok(old), Ok(new)) = (
        repo.find_commit(parent).and_then(|c| c.tree()),
        repo.find_tree(new_tree),
    ) {
        if let Ok(diff) = repo.diff_tree_to_tree(Some(&old), Some(&new), None) {
            for delta in diff.deltas() {
                let path = delta.new_file().path();
                let Some(p) = path else { continue };
                let rel = p.to_string_lossy().replace('\\', "/");
                if !(rel.starts_with(".stateroot/") && !rel.starts_with(".stateroot/local/")) {
                    continue;
                }
                let blob_id = delta.new_file().id();
                let identity = repo
                    .find_blob(blob_id)
                    .map(|blob| {
                        let content = blob.content();
                        (content.len() as u64, local_store::fnv128_tail(content))
                    })
                    .unwrap_or((0, String::new()));
                changed.push((rel, identity.0, identity.1));
            }
        }
    }
    // Watermark: the parent commit's time (ledger entries at/after it are
    // "since the last root"). Same format as local_store::now_rfc3339
    // (Zulu seconds) so string comparison is exact.
    let since = repo
        .find_commit(parent)
        .ok()
        .and_then(|c| chrono::DateTime::from_timestamp(c.time().seconds(), 0))
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default();
    let reported = local_store::reported_writes_since(project_dir, &since);
    let mut unreported: Vec<String> = changed
        .into_iter()
        .filter(|(path, len, id)| match reported.get(path) {
            Some(reported_id) if !reported_id.is_null() => {
                reported_id.get("id").and_then(|v| v.as_str()) != Some(id.as_str())
                    || reported_id.get("len").and_then(|v| v.as_u64()) != Some(*len)
            }
            _ => true,
        })
        .map(|(path, _, _)| path)
        .collect();
    unreported.sort();
    unreported.dedup();
    let unreported_total = unreported.len();
    unreported.truncate(AUDIT_CAP);

    let privacy_violations = tree_violations(repo, project_dir, new_tree);
    let privacy_violations_total = privacy_violations.len();
    WriteAudit {
        unreported,
        unreported_total,
        privacy_violations: privacy_violations.into_iter().take(AUDIT_CAP).collect(),
        privacy_violations_total,
    }
}

/// Re-run the ignore rules over every blob path in a tree (metadata-only;
/// reads no file contents). Any hit is a privacy violation: that path must
/// never be in a root.
fn tree_violations(repo: &Repository, project_dir: &Path, tree_oid: git2::Oid) -> Vec<String> {
    let rules = crate::sync_engine::ignore::IgnoreRules::load(project_dir);
    let mut violations = Vec::new();
    let Ok(tree) = repo.find_tree(tree_oid) else {
        return violations;
    };
    let _ = tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() == Some(git2::ObjectType::Blob) {
            let name = entry.name().unwrap_or("");
            let full = format!("{dir}{name}");
            if rules.is_ignored(&full, false) {
                violations.push(full);
            }
        }
        git2::TreeWalkResult::Ok
    });
    violations
}

/// The audit's evidence value, or None when everything is clean.
fn audit_evidence(audit: &WriteAudit) -> Option<serde_json::Value> {
    if audit.unreported_total == 0 && audit.privacy_violations_total == 0 {
        return None;
    }
    Some(serde_json::json!({
        "unreported_writes": audit.unreported,
        "unreported_total": audit.unreported_total,
        "privacy_violations": audit.privacy_violations,
        "privacy_violations_total": audit.privacy_violations_total,
    }))
}

#[allow(clippy::too_many_arguments)]
fn commit_new_root(
    repo: &Repository,
    project_dir: &Path,
    tree: git2::Oid,
    pinned: i64,
    tree_bytes: u64,
    harness: &str,
    reason: &str,
    snap_ctx: Option<&crate::snap_context::SnapContext>,
) -> Result<(RootManifest, Transition), RootsError> {
    // Same-ref CAS span (repair Phase 4): tip read → parent selection →
    // commit construction → ref update CAS, retried against the new tip on
    // a Moved conflict. The tree is parent-independent, so a retry only
    // re-selects the parent and re-commits — never a silent sibling.
    const CAS_RETRIES: usize = 8;
    let mut attempt = 0;
    loop {
        let parent = latest_oid_for(repo, project_dir)?;
        let parents: Vec<git2::Oid> = parent.into_iter().collect();
        let parent_hashes: Vec<String> = parents.iter().map(|o| o.to_string()).collect();
        let from_root = parent_hashes.first().cloned().unwrap_or_default();
        let message = match reason {
            "" => format!("root by {harness}"),
            r => format!("root: {r} (by {harness})"),
        };
        let oid = commit_root(repo, tree, &parents, &message)?;
        let to_root = oid.to_string();
        let mut evidence = crate::snap_context::build_snap_evidence(
            project_dir,
            harness,
            reason,
            &from_root,
            &to_root,
            snap_ctx,
        );
        // WS3.4 write audit: parented roots only — genesis has no baseline
        // and its whole-tree diff would be pure noise.
        if let Some(p) = parent {
            let audit = audit_writes(repo, project_dir, p, tree);
            // Privacy enforcement (Phase 4): a forbidden path BLOCKS the ref
            // advance and surfaces the exact problem — never a hidden
            // evidence note on a published root.
            if audit.privacy_violations_total > 0 {
                return Err(RootsError::Merge(format!(
                    "privacy: forbidden path(s) in the new tree — ref not advanced: {}",
                    audit.privacy_violations.join(", ")
                )));
            }
            if let Some(value) = audit_evidence(&audit) {
                evidence["write_audit"] = value;
            }
        }
        match persist_root(
            repo,
            project_dir,
            oid,
            parent_hashes,
            harness,
            reason,
            pinned,
            tree_bytes,
            "snapshot",
            evidence,
        ) {
            Err(RootsError::RefCas(crate::safe_io::RefCasError::Moved { .. }))
                if attempt < CAS_RETRIES =>
            {
                attempt += 1;
                continue;
            }
            other => return other,
        }
    }
}

/// The latest root hash, if any.
pub fn latest_root(project_dir: &Path) -> Result<Option<String>, RootsError> {
    let repo = ensure_repo(project_dir)?;
    Ok(latest_oid_for(&repo, project_dir)?.map(|oid| oid.to_string()))
}

/// Load a root manifest by hash (prefix match allowed, git-style).
pub fn get_root(project_dir: &Path, hash_prefix: &str) -> Result<RootManifest, RootsError> {
    let id = resolve_hash(project_dir, hash_prefix)?;
    let path = local_store::root(project_dir)
        .join(ROOTS_DIR)
        .join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path)
        .map_err(|_| RootsError::NotFound(format!("no root manifest for {hash_prefix}")))?;
    Ok(serde_json::from_str(&text)?)
}

/// Resolve a hash prefix to a full root id (manifest files are the index).
pub fn resolve_hash(project_dir: &Path, hash_prefix: &str) -> Result<String, RootsError> {
    let dir = local_store::root(project_dir).join(ROOTS_DIR);
    let entries: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    e.file_name()
                        .to_str()
                        .and_then(|n| n.strip_suffix(".json").map(str::to_string))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut matches: Vec<&String> = entries
        .iter()
        .filter(|id| id.starts_with(hash_prefix))
        .collect();
    match matches.len() {
        0 => Err(RootsError::NotFound(format!(
            "no root matching '{hash_prefix}'"
        ))),
        1 => Ok(matches.remove(0).clone()),
        _ => Err(RootsError::NotFound(format!(
            "ambiguous hash prefix '{hash_prefix}' ({} matches)",
            matches.len()
        ))),
    }
}

fn commit_for<'r>(
    repo: &'r Repository,
    project_dir: &Path,
    hash_prefix: &str,
) -> Result<git2::Commit<'r>, RootsError> {
    let id = resolve_hash(project_dir, hash_prefix)?;
    let oid = git2::Oid::from_str(&id)
        .map_err(|e| RootsError::NotFound(format!("bad hash {id}: {e}")))?;
    Ok(repo.find_commit(oid)?)
}

/// One lineage entry: manifest + whether it is on the latest first-parent chain.
#[derive(Debug)]
pub struct LineageEntry {
    /// The manifest (commit-derived fallback when the file is missing).
    pub manifest: RootManifest,
    /// True when on the mainline from `refs/stateroot/latest`.
    pub mainline: bool,
    /// True when another root branches off this one (fork point).
    pub fork_point: bool,
}

/// Walk the root lineage: mainline from latest, then side roots as forks.
pub fn lineage(project_dir: &Path) -> Result<Vec<LineageEntry>, RootsError> {
    let repo = ensure_repo(project_dir)?;
    let mut mainline: Vec<String> = Vec::new();
    let mut children: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    if let Some(tip) = latest_oid_for(&repo, project_dir)? {
        let mut current = Some(tip);
        while let Some(oid) = current {
            mainline.push(oid.to_string());
            let commit = repo.find_commit(oid)?;
            for parent in commit.parents() {
                *children.entry(parent.id().to_string()).or_insert(0) += 1;
            }
            current = commit.parent(0).ok().map(|c| c.id());
        }
    }
    let mainline_set: std::collections::BTreeSet<String> = mainline.iter().cloned().collect();
    // All root refs not on the mainline are side branches.
    let mut side: Vec<String> = Vec::new();
    for reference in repo
        .references_glob(&format!("{ROOTS_REF_PREFIX}*"))?
        .flatten()
    {
        if let Some(oid) = reference.target() {
            let id = oid.to_string();
            if !mainline_set.contains(&id) {
                side.push(id);
            }
        }
    }
    side.sort();
    // Fork points: a side branch's first-parent chain credits one child edge
    // to each ancestor it crosses (bounded walk until the mainline or genesis).
    for tip in &side {
        let mut current = repo.find_commit(git2::Oid::from_str(tip)?).ok();
        while let Some(commit) = current {
            match commit.parent(0).ok() {
                Some(parent) => {
                    *children.entry(parent.id().to_string()).or_insert(0) += 1;
                    if mainline_set.contains(&parent.id().to_string()) {
                        break;
                    }
                    current = Some(parent);
                }
                None => break,
            }
        }
    }
    let mut out = Vec::new();
    for id in mainline.into_iter().chain(side) {
        let manifest = get_root(project_dir, &id).unwrap_or_else(|_| RootManifest {
            schema_version: ROOT_SCHEMA.into(),
            id: id.clone(),
            coverage: "unknown".into(),
            ..Default::default()
        });
        out.push(LineageEntry {
            manifest,
            mainline: mainline_set.contains(&id),
            fork_point: children.get(&id).copied().unwrap_or(0) > 1,
        });
    }
    Ok(out)
}

/// A file delta entry for `diff` (name + status).
#[derive(Debug, Clone)]
pub struct FileDelta {
    /// Project-relative path (new path for renames).
    pub path: String,
    /// `A` | `M` | `D` | `R` | `T`.
    pub status: char,
    /// True when the path is under `.stateroot/`.
    pub internal: bool,
}

fn collect_deltas(diff: &git2::Diff) -> Vec<FileDelta> {
    diff.deltas()
        .map(|delta| {
            let status = match delta.status() {
                git2::Delta::Added => 'A',
                git2::Delta::Deleted => 'D',
                git2::Delta::Modified => 'M',
                git2::Delta::Renamed => 'R',
                git2::Delta::Typechange => 'T',
                _ => '?',
            };
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let internal = path.starts_with(".stateroot/");
            FileDelta {
                path,
                status,
                internal,
            }
        })
        .collect()
}

/// `diff a b`: names+status, or unified content diffs with caps and honest
/// binary/unavailable markers. Returns (files-section, state-section, content).
pub fn diff_roots(
    project_dir: &Path,
    from: &str,
    to: &str,
    content: bool,
    max_files: usize,
    max_lines_per_file: usize,
) -> Result<Value, RootsError> {
    let repo = ensure_repo(project_dir)?;
    let a = commit_for(&repo, project_dir, from)?;
    let b = commit_for(&repo, project_dir, to)?;
    let diff = repo.diff_tree_to_tree(Some(&a.tree()?), Some(&b.tree()?), None)?;
    let deltas = collect_deltas(&diff);
    let files: Vec<&FileDelta> = deltas.iter().filter(|d| !d.internal).collect();
    let state: Vec<&FileDelta> = deltas.iter().filter(|d| d.internal).collect();

    let mut contents = Vec::new();
    let mut truncated = false;
    if content {
        for (idx, delta) in deltas.iter().enumerate() {
            if contents.len() >= max_files {
                truncated = true;
                break;
            }
            if delta.internal {
                continue; // state docs: names+status only in M2
            }
            let entry = if diff
                .get_delta(idx)
                .map(|d| d.flags().is_binary())
                .unwrap_or(false)
            {
                json!({"path": delta.path, "binary": true})
            } else if let Some(mut patch) = git2::Patch::from_diff(&diff, idx)? {
                let text = String::from_utf8_lossy(patch.to_buf()?.as_ref()).to_string();
                let lines: Vec<&str> = text.lines().collect();
                let (body, cut) = if lines.len() > max_lines_per_file {
                    (lines[..max_lines_per_file].join("\n"), true)
                } else {
                    (text.clone(), false)
                };
                json!({"path": delta.path, "diff": body, "truncated": cut})
            } else {
                json!({"path": delta.path, "content_available": false, "reason": "no patch"})
            };
            contents.push(entry);
        }
    }
    Ok(json!({
        "from_root": a.id().to_string(),
        "to_root": b.id().to_string(),
        "files": files.iter().map(|d| json!({"path": d.path, "status": d.status.to_string()})).collect::<Vec<_>>(),
        "state": state.iter().map(|d| json!({"path": d.path, "status": d.status.to_string()})).collect::<Vec<_>>(),
        "contents": contents,
        "truncated": truncated,
    }))
}

/// `revert <hash>`: append-only — a NEW root whose tree equals the target's.
pub fn revert_to_root(
    project_dir: &Path,
    hash_prefix: &str,
    harness: &str,
) -> Result<(RootManifest, Transition), RootsError> {
    let repo = ensure_repo(project_dir)?;
    let target = commit_for(&repo, project_dir, hash_prefix)?;
    let target_id = target.id().to_string();
    let target_tree = target.tree()?.id();
    let manifest = get_root(project_dir, &target_id).unwrap_or_default();
    // Same CAS span as snap (Phase 4): re-read the tip and re-commit on a
    // Moved conflict; the target tree is parent-independent.
    const CAS_RETRIES: usize = 8;
    let mut attempt = 0;
    loop {
        let parent = latest_oid_for(&repo, project_dir)?;
        let parents: Vec<git2::Oid> = parent.into_iter().collect();
        let parent_hashes: Vec<String> = parents.iter().map(|o| o.to_string()).collect();
        let message = format!("revert to {} (by {harness})", &target_id[..12]);
        let oid = commit_root(&repo, target_tree, &parents, &message)?;
        match persist_root(
            &repo,
            project_dir,
            oid,
            parent_hashes,
            harness,
            &format!("revert to {}", &target_id[..12]),
            manifest.files_pinned,
            manifest.tree_bytes,
            "revert",
            json!({"revert_to": target_id}),
        ) {
            Err(RootsError::RefCas(crate::safe_io::RefCasError::Moved { .. }))
                if attempt < CAS_RETRIES =>
            {
                attempt += 1;
                continue;
            }
            other => return other,
        }
    }
}

/// `fork <hash> --branch <name>`: fork ref + record at the root commit.
/// `fork_materialize` turns the ref into an isolated worktree on demand.
pub fn fork_root(
    project_dir: &Path,
    hash_prefix: &str,
    branch: Option<&str>,
    harness: &str,
) -> Result<(String, String), RootsError> {
    let repo = ensure_repo(project_dir)?;
    let commit = commit_for(&repo, project_dir, hash_prefix)?;
    let name = branch
        .map(str::to_string)
        .unwrap_or_else(|| format!("f-{}", &uuid::Uuid::now_v7().to_string()[..8]));
    let refname = format!("{FORKS_REF_PREFIX}{name}");
    repo.reference(&refname, commit.id(), true, "fork root")?;
    let record = json!({
        "schema_version": "stateroot.fork.local.v1",
        "name": name,
        "root": commit.id().to_string(),
        "ref": refname,
        "created_at": now_rfc3339(),
        "created_by_harness": harness,
    });
    write_json(
        &local_store::root(project_dir)
            .join(FORKS_DIR)
            .join(format!("{name}.json")),
        &record,
    )?;
    Ok((name, refname))
}

/// Materialize a fork ref into a real worktree (WS5 / repair Phase 6A):
/// TRANSACTIONAL — destination validated before any mutation, rollback on
/// every partial failure, and `--branch` is gone: no user branch is ever
/// created, moved, or pointed at a synthetic root.
///
/// Validated up front: the fork exists, the destination does NOT exist and
/// is NOT inside the project tree, and a claimed plan exists and is not
/// already claimed by another fork.
///
/// The checkout contains the root's full tree — including `.stateroot/`
/// (minus `local/`). A plan can have been recorded after that root, so a
/// claimed plan is copied into the new worktree before its fork context is
/// stamped. HEAD is detached at the root commit so plumbing ref writes never
/// move a checked-out branch under the worktree. Absolute paths live only in
/// the machine-local registry (`.stateroot/local/`); shared state carries the
/// opaque fork id.
pub fn fork_materialize(
    project_dir: &Path,
    name: &str,
    worktree_path: &Path,
    plan: Option<&str>,
) -> Result<(), RootsError> {
    let repo = ensure_repo(project_dir)?;
    let refname = format!("{FORKS_REF_PREFIX}{name}");

    // -- Validation (before ANY mutation) ----------------------------------
    let tip = repo
        .refname_to_id(&refname)
        .map_err(|_| RootsError::NotFound(format!("no fork named {name}")))?;
    if worktree_path.exists() {
        return Err(RootsError::Merge(format!(
            "worktree destination {} already exists",
            worktree_path.display()
        )));
    }
    let project_canon = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());
    let dest_canon = worktree_path
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.join(worktree_path.file_name().unwrap_or_default()))
        .unwrap_or_else(|| worktree_path.to_path_buf());
    if dest_canon.starts_with(&project_canon) {
        return Err(RootsError::Merge(format!(
            "worktree destination {} is inside the project tree — use a sibling directory",
            worktree_path.display()
        )));
    }
    if let Some(plan_id) = plan {
        crate::plans::load(project_dir, plan_id)
            .ok_or_else(|| RootsError::NotFound(format!("no plan named {plan_id}")))?;
        // Duplicate-claim rejection: another fork already holds this plan.
        if let Some(holder) = fork_holding_plan(project_dir, plan_id) {
            return Err(RootsError::Merge(format!(
                "plan {plan_id} is already claimed by fork {holder}"
            )));
        }
    }

    // -- Execute with rollback ----------------------------------------------
    let tmp_branch = format!("refs/heads/stateroot-forktmp-{name}");
    // libgit2's worktree-add accepts only branch refs — check out via a
    // throwaway branch, then detach HEAD and delete it so no refs/heads
    // entry survives.
    repo.reference(&tmp_branch, tip, true, "fork tmp branch")?;
    let result = (|| -> Result<(), RootsError> {
        {
            let reference = repo.find_reference(&tmp_branch)?;
            let mut opts = git2::WorktreeAddOptions::new();
            opts.reference(Some(&reference));
            repo.worktree(name, worktree_path, Some(&opts))?;
        }
        let wt_repo = git2::Repository::open(worktree_path)?;
        wt_repo.set_head_detached(tip)?;
        if let Ok(mut branch) = repo.find_branch(&tmp_branch, git2::BranchType::Local) {
            branch.delete()?;
        }
        if let Some(plan_id) = plan {
            copy_plan_to_worktree(project_dir, worktree_path, plan_id)?;
        }
        local_store::write_fork_context(
            worktree_path,
            &local_store::ForkContext {
                schema: "stateroot.fork-context.v1".into(),
                fork: name.to_string(),
                parent_root: tip.to_string(),
                plan: plan.map(str::to_string),
            },
        )?;
        register_worktree(project_dir, name, worktree_path)?;
        if let Some(plan_id) = plan {
            claim_plan(project_dir, plan_id, name)?;
            let record_path = local_store::root(project_dir)
                .join(FORKS_DIR)
                .join(format!("{name}.json"));
            if let Ok(text) = std::fs::read_to_string(&record_path) {
                if let Ok(mut record) = serde_json::from_str::<Value>(&text) {
                    record["plan"] = json!(plan_id);
                    write_json(&record_path, &record)?;
                }
            }
        }
        Ok(())
    })();
    if result.is_err() {
        // Rollback in reverse: registry entry, plan claim, worktree, branch.
        let _ = unregister_worktree(project_dir, name);
        if let Some(plan_id) = plan {
            let _ = release_plan_claim(project_dir, plan_id, name);
        }
        let _ = std::fs::remove_dir_all(worktree_path);
        if let Ok(mut branch) = repo.find_branch(&tmp_branch, git2::BranchType::Local) {
            let _ = branch.delete();
        }
        if let Ok(wt) = repo.find_worktree(name) {
            let _ = wt.prune(None);
        }
    }
    result
}

/// Copy the authoritative plan body and sidecar into a newly materialized
/// fork. The root being forked may predate the plan, so relying on the Git
/// snapshot alone leaves the executor with a fork context that names a plan
/// it cannot read or activate.
fn copy_plan_to_worktree(
    project_dir: &Path,
    worktree_path: &Path,
    plan_id: &str,
) -> Result<(), RootsError> {
    let (_meta, body_path) = crate::plans::load(project_dir, plan_id)
        .ok_or_else(|| RootsError::NotFound(format!("no plan named {plan_id}")))?;
    let body = std::fs::read(&body_path)?;
    let meta = std::fs::read(
        local_store::root(project_dir)
            .join("plans")
            .join(format!("{plan_id}.json")),
    )?;
    let destination = local_store::root(worktree_path).join("plans");
    crate::safe_io::atomic_replace(&destination.join(format!("{plan_id}.md")), &body)?;
    crate::safe_io::atomic_replace(&destination.join(format!("{plan_id}.json")), &meta)?;
    Ok(())
}

fn fork_holding_plan(project_dir: &Path, plan_id: &str) -> Option<String> {
    let dir = local_store::root(project_dir).join(FORKS_DIR);
    std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let text = std::fs::read_to_string(entry.path()).ok()?;
        let record: Value = serde_json::from_str(&text).ok()?;
        (record.get("plan").and_then(|p| p.as_str()) == Some(plan_id)).then(|| {
            record
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("?")
                .to_string()
        })
    })
}

fn registry_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join("local/fork-worktrees.json")
}

fn read_registry(project_dir: &Path) -> Value {
    std::fs::read_to_string(registry_path(project_dir))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| json!({"schema": "stateroot.fork-worktrees.v1", "entries": {}}))
}

fn write_registry(project_dir: &Path, registry: &Value) -> Result<(), RootsError> {
    crate::safe_io::atomic_replace_json(&registry_path(project_dir), registry)?;
    Ok(())
}

/// Machine-local fork id → absolute worktree path (shared state carries
/// only the opaque fork id; paths never leave `local/`).
pub fn registered_worktree_path(project_dir: &Path, name: &str) -> Option<PathBuf> {
    let registry = read_registry(project_dir);
    registry
        .get("entries")
        .and_then(|e| e.get(name))
        .and_then(|v| v.get("path"))
        .and_then(|p| p.as_str())
        .map(PathBuf::from)
}

fn register_worktree(project_dir: &Path, name: &str, path: &Path) -> Result<(), RootsError> {
    let mut registry = read_registry(project_dir);
    registry["entries"][name] = json!({
        "path": path.to_string_lossy(),
        "created_at": now_rfc3339(),
    });
    write_registry(project_dir, &registry)
}

fn unregister_worktree(project_dir: &Path, name: &str) -> Result<(), RootsError> {
    let mut registry = read_registry(project_dir);
    if let Some(entries) = registry.get_mut("entries").and_then(|e| e.as_object_mut()) {
        entries.remove(name);
    }
    write_registry(project_dir, &registry)
}

fn claim_plan(project_dir: &Path, plan_id: &str, fork: &str) -> Result<(), RootsError> {
    let path = local_store::root(project_dir)
        .join("plans")
        .join(format!("{plan_id}.json"));
    let text = std::fs::read_to_string(&path)
        .map_err(|_| RootsError::NotFound(format!("no plan named {plan_id}")))?;
    let mut meta: Value = serde_json::from_str(&text)?;
    meta["claimed_by"] = json!({"fork": fork, "at": now_rfc3339()});
    crate::safe_io::atomic_replace_json(&path, &meta)?;
    local_store::report_written(project_dir, &format!("plans/{plan_id}.json"));
    Ok(())
}

fn release_plan_claim(project_dir: &Path, plan_id: &str, fork: &str) -> Result<(), RootsError> {
    let path = local_store::root(project_dir)
        .join("plans")
        .join(format!("{plan_id}.json"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let mut meta: Value = serde_json::from_str(&text)?;
    let ours = meta
        .get("claimed_by")
        .and_then(|c| c.get("fork"))
        .and_then(|f| f.as_str())
        == Some(fork);
    if ours {
        if let Some(obj) = meta.as_object_mut() {
            obj.remove("claimed_by");
        }
        crate::safe_io::atomic_replace_json(&path, &meta)?;
        local_store::report_written(project_dir, &format!("plans/{plan_id}.json"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Merge (WS5 batch B)
//
// Fold fork lineages back into the trunk with a git 3-way merge. Executions
// were never serialized; only this step is (the WS3 roots lock inside
// persist_root). Clean fold across all forks → ONE merge root with N
// parents [trunk, fork tips…]. Any conflict → per-path report, NO merge
// root — nothing is ever half-applied (immutability makes that free).
// ---------------------------------------------------------------------------

/// One fork's contribution to a merge: name + tip at merge time.
#[derive(Debug)]
pub struct MergedFork {
    /// Fork name.
    pub name: String,
    /// Tip root merged in (empty when the fork was already contained).
    pub tip: String,
}

/// `stateroot merge <fork>…` — fold fork tips into `refs/stateroot/latest`.
/// Returns the merge root and the forks that contributed.
///
/// 6C contract: runs ONLY from the trunk checkout (a fork worktree fails
/// closed with the exact recovery); the trunk ref transaction is CAS from
/// base selection through publication (a moved trunk recomputes the fold);
/// the merged tree is materialized into the trunk filesystem and verified
/// BEFORE the ref advances (dirty workspaces refuse; a failed
/// materialization restores the base); merged fork worktrees are cleaned up
/// automatically, with cleanup failures left as visible retryable state.
pub fn merge_forks(
    project_dir: &Path,
    forks: &[String],
    harness: &str,
) -> Result<(RootManifest, Transition, Vec<MergedFork>), RootsError> {
    if forks.is_empty() {
        return Err(RootsError::Merge(
            "no forks named — pass at least one fork to merge".into(),
        ));
    }
    if local_store::fork_context(project_dir).is_some() {
        return Err(RootsError::Merge(
            "merge must run from the trunk checkout (this directory is a fork worktree) — run it from the project trunk"
                .into(),
        ));
    }
    const CAS_RETRIES: usize = 8;
    let mut attempt = 0;
    loop {
        match merge_forks_once(project_dir, forks, harness) {
            Err(RootsError::RefCas(crate::safe_io::RefCasError::Moved { .. }))
                if attempt < CAS_RETRIES =>
            {
                attempt += 1;
                continue;
            }
            other => return other,
        }
    }
}

fn merge_forks_once(
    project_dir: &Path,
    forks: &[String],
    harness: &str,
) -> Result<(RootManifest, Transition, Vec<MergedFork>), RootsError> {
    let repo = ensure_repo(project_dir)?;
    let base_oid = repo
        .refname_to_id(LATEST_REF)
        .map_err(|_| RootsError::NotFound("no roots yet — nothing to merge into".into()))?;
    // The accumulated fold: `current_tree` is the union so far, `head_oid`
    // is the line's head for the NEXT merge_base. After folding fork A the
    // union contains A's work, so B must merge against (A's tip) as the
    // base line — never against the original trunk, or A's edits are
    // silently dropped (and cross-fork conflicts go undetected).
    let mut head_oid = base_oid;
    let mut current_tree = repo.find_commit(base_oid)?.tree()?;
    let mut parents: Vec<git2::Oid> = vec![base_oid];
    let mut merged: Vec<MergedFork> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for name in forks {
        let refname = format!("{FORKS_REF_PREFIX}{name}");
        let tip = repo
            .refname_to_id(&refname)
            .map_err(|_| RootsError::NotFound(format!("no fork named {name}")))?;
        let fork_commit = repo.find_commit(tip)?;
        let ancestor_oid = repo.merge_base(head_oid, tip)?;
        if ancestor_oid == tip {
            // The accumulated line already contains this fork entirely.
            skipped.push(name.clone());
            continue;
        }
        let ancestor_tree = repo.find_commit(ancestor_oid)?.tree()?;
        let mut index =
            repo.merge_trees(&ancestor_tree, &current_tree, &fork_commit.tree()?, None)?;
        if index.has_conflicts() {
            let mut paths: Vec<String> = index
                .conflicts()?
                .filter_map(|c| {
                    let c = c.ok()?;
                    c.their
                        .or(c.our)
                        .or(c.ancestor)
                        .and_then(|f| String::from_utf8(f.path).ok())
                })
                .collect();
            paths.sort();
            paths.dedup();
            return Err(RootsError::Merge(format!(
                "merge conflict in fork `{name}` — no merge root created; resolve and retry. Conflicting paths: {}",
                paths.join(", ")
            )));
        }
        let tree_oid = index.write_tree_to(&repo)?;
        current_tree = repo.find_tree(tree_oid)?;
        head_oid = tip;
        parents.push(tip);
        merged.push(MergedFork {
            name: name.clone(),
            tip: tip.to_string(),
        });
    }

    if merged.is_empty() {
        return Err(RootsError::Merge(format!(
            "nothing to merge — every named fork is already contained in the trunk ({})",
            skipped.join(", ")
        )));
    }

    // Materialize BEFORE publication (6C): the merged tree must land on
    // disk, verified, before the ref advances. The workspace protection is
    // content-based and covers EVERY base-tracked path (a force checkout
    // overwrites all of them, not just the merge's changed paths);
    // untracked extras and store bookkeeping never block — the next
    // checkpoint capturing them is honest new work, not a merge lie.
    let base_tree = repo.find_commit(base_oid)?.tree()?;
    let dirty = workspace_conflicts(&repo, project_dir, &base_tree, &current_tree)?;
    if !dirty.is_empty() {
        return Err(RootsError::Merge(format!(
            "trunk workspace has uncommitted change(s) the merge would overwrite — refusing: snap, revert, or clean first. Paths: {}",
            dirty.join(", ")
        )));
    }
    let checkout = |tree: git2::Oid| -> Result<(), RootsError> {
        let object = repo.find_object(tree, Some(git2::ObjectType::Tree))?;
        // Filters disabled: roots are a content-addressed byte store —
        // build_tree hashes raw disk bytes, so materialization must write
        // raw blob bytes too. A smudge filter (core.autocrlf on Windows)
        // would make every restored file differ from its blob and fail the
        // verification below — and silently corrupt byte-exact restoration
        // for real users.
        repo.checkout_tree(
            &object,
            Some(
                git2::build::CheckoutBuilder::new()
                    .force()
                    .disable_filters(true),
            ),
        )?;
        Ok(())
    };
    let base_tree_oid = base_tree.id();
    let merge_tree_oid = current_tree.id();
    checkout(merge_tree_oid)?;
    let verified = tree_materialized(&repo, project_dir, &base_tree, &current_tree);
    if !verified {
        let _ = checkout(base_tree_oid);
        return Err(RootsError::Merge(
            "materialization verification failed — the workspace was restored to the pre-merge root; nothing was published"
                .into(),
        ));
    }

    /// Materialization verification: every merge-tree blob (outside
    /// `.stateroot/` bookkeeping and ignored paths) must exist on disk with
    /// identical bytes, and every base path the merge deleted must be gone.
    /// Direct content comparison — index-free and free of the mtime/stat-
    /// cache semantics that make a rebuild-and-diff check flaky on Windows.
    /// Pure untracked additions on disk are free to exist — "the next
    /// checkpoint is a no-op UNLESS new edits exist", and untracked new work
    /// is exactly that, not a merge lie.
    fn tree_materialized(
        repo: &Repository,
        project_dir: &Path,
        base_tree: &git2::Tree,
        merge_tree: &git2::Tree,
    ) -> bool {
        let ignore = crate::sync_engine::ignore::IgnoreRules::load(project_dir);
        let mut ok = true;
        let mut merge_paths = std::collections::HashSet::new();
        merge_tree
            .walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
                if entry.kind() != Some(git2::ObjectType::Blob) {
                    return git2::TreeWalkResult::Ok;
                }
                let rel = format!("{}{}", dir, entry.name().unwrap_or(""));
                if rel.starts_with(".stateroot") || ignore.is_ignored(&rel, false) {
                    return git2::TreeWalkResult::Ok;
                }
                merge_paths.insert(rel.clone());
                let present = entry
                    .to_object(repo)
                    .and_then(|o| o.peel_to_blob())
                    .map(|blob| {
                        std::fs::read(project_dir.join(&rel))
                            .map(|bytes| bytes == blob.content())
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if !present {
                    ok = false;
                }
                git2::TreeWalkResult::Ok
            })
            .expect("tree walk");
        base_tree
            .walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
                if entry.kind() != Some(git2::ObjectType::Blob) {
                    return git2::TreeWalkResult::Ok;
                }
                let rel = format!("{}{}", dir, entry.name().unwrap_or(""));
                if rel.starts_with(".stateroot") || ignore.is_ignored(&rel, false) {
                    return git2::TreeWalkResult::Ok;
                }
                if !merge_paths.contains(&rel) && project_dir.join(&rel).exists() {
                    ok = false;
                }
                git2::TreeWalkResult::Ok
            })
            .expect("tree walk");
        ok
    }

    /// Base-tracked paths whose on-disk content differs from the BASE blob —
    /// uncommitted work a force checkout would silently overwrite. Content-
    /// based and index-free; covers every path in the base tree (the checkout
    /// overwrites all of them, not just the merge's changed paths). Ignored
    /// and `.stateroot/` bookkeeping paths never block.
    fn workspace_conflicts(
        repo: &Repository,
        project_dir: &Path,
        base_tree: &git2::Tree,
        _merge_tree: &git2::Tree,
    ) -> Result<Vec<String>, RootsError> {
        let ignore = crate::sync_engine::ignore::IgnoreRules::load(project_dir);
        let mut conflicts = Vec::new();
        base_tree
            .walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
                if entry.kind() != Some(git2::ObjectType::Blob) {
                    return git2::TreeWalkResult::Ok;
                }
                let name = entry.name().unwrap_or("");
                let rel = format!("{dir}{name}");
                if rel.starts_with(".stateroot") || ignore.is_ignored(&rel, false) {
                    return git2::TreeWalkResult::Ok;
                }
                let Ok(blob) = entry.to_object(repo).and_then(|o| o.peel_to_blob()) else {
                    return git2::TreeWalkResult::Ok;
                };
                let disk_path = project_dir.join(&rel);
                match std::fs::read(&disk_path) {
                    Ok(bytes) if bytes == blob.content() => {}
                    Ok(_) => conflicts.push(rel.clone()),
                    Err(_) => conflicts.push(rel.clone()),
                }
                git2::TreeWalkResult::Ok
            })
            .expect("tree walk");
        Ok(conflicts)
    }

    let reason = format!(
        "merge {}{}",
        merged
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        if skipped.is_empty() {
            String::new()
        } else {
            format!(" (skipped contained: {})", skipped.join(", "))
        }
    );
    let message = format!("{reason} (by {harness})");
    let oid = commit_root(&repo, merge_tree_oid, &parents, &message)?;
    let parent_hashes: Vec<String> = parents.iter().map(|o| o.to_string()).collect();
    let (files_pinned, tree_bytes) = tree_stats(&repo, merge_tree_oid);
    let evidence = json!({
        "kind_detail": "fork merge",
        "merged_forks": merged.iter().map(|f| json!({"name": f.name, "tip": f.tip})).collect::<Vec<_>>(),
        "skipped_contained": skipped,
        "base": base_oid.to_string(),
        "harness": harness,
    });
    let (manifest, transition) = persist_root(
        &repo,
        project_dir,
        oid,
        parent_hashes,
        harness,
        &reason,
        files_pinned,
        tree_bytes,
        "merge",
        evidence,
    )?;

    // Automatic cleanup (6C): merged fork worktrees go away; the fork ref
    // and record stay as history. A cleanup failure becomes visible
    // retryable state on the fork record, not a printed shell instruction.
    for fork in &merged {
        if let Some(path) = registered_worktree_path(project_dir, &fork.name) {
            let cleanup = (|| -> Result<(), RootsError> {
                std::fs::remove_dir_all(&path)?;
                if let Ok(wt) = repo.find_worktree(&fork.name) {
                    wt.prune(None)?;
                }
                unregister_worktree(project_dir, &fork.name)
            })();
            if let Err(err) = cleanup {
                let record_path = local_store::root(project_dir)
                    .join(FORKS_DIR)
                    .join(format!("{}.json", fork.name));
                if let Ok(text) = std::fs::read_to_string(&record_path) {
                    if let Ok(mut record) = serde_json::from_str::<Value>(&text) {
                        record["cleanup_pending"] = json!(format!("{err}"));
                        let _ = write_json(&record_path, &record);
                    }
                }
            }
        }
    }

    Ok((manifest, transition, merged))
}

/// Blob count + total bytes of a tree (metadata-only walk).
fn tree_stats(repo: &Repository, tree_oid: git2::Oid) -> (i64, u64) {
    let mut files: i64 = 0;
    let mut bytes: u64 = 0;
    if let Ok(tree) = repo.find_tree(tree_oid) {
        let _ = tree.walk(git2::TreeWalkMode::PreOrder, |_, entry| {
            if entry.kind() == Some(git2::ObjectType::Blob) {
                files += 1;
                if let Ok(blob) = entry.to_object(repo).and_then(|o| o.peel_to_blob()) {
                    bytes += blob.size() as u64;
                }
            }
            git2::TreeWalkResult::Ok
        });
    }
    (files, bytes)
}

/// Load a transition by id (prefix match allowed).
pub fn get_transition(project_dir: &Path, id_prefix: &str) -> Result<Transition, RootsError> {
    let dir = local_store::root(project_dir).join(TRANSITIONS_DIR);
    let entries: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    e.file_name()
                        .to_str()
                        .and_then(|n| n.strip_suffix(".json").map(str::to_string))
                })
                .collect()
        })
        .unwrap_or_default();
    let matches: Vec<&String> = entries
        .iter()
        .filter(|id| id.starts_with(id_prefix))
        .collect();
    match matches.len() {
        0 => Err(RootsError::NotFound(format!(
            "no transition matching '{id_prefix}'"
        ))),
        1 => {
            let text = std::fs::read_to_string(dir.join(format!("{}.json", matches[0])))?;
            Ok(serde_json::from_str(&text)?)
        }
        _ => Err(RootsError::NotFound(format!(
            "ambiguous transition prefix '{id_prefix}'"
        ))),
    }
}

/// `receipt <transition>`: markdown from the transition + the git delta
/// (verified tier = `git diff from to`).
pub fn render_receipt(project_dir: &Path, id_prefix: &str) -> Result<String, RootsError> {
    let transition = get_transition(project_dir, id_prefix)?;
    let mut out = String::new();
    out.push_str(&format!("# Transition receipt — {}\n\n", transition.id));
    out.push_str(&format!("kind: {}\n", transition.kind));
    out.push_str(&format!(
        "roots: {} -> {}\n",
        short(&transition.from_root),
        short(&transition.to_root)
    ));
    out.push_str(&format!("harness: {}\n", transition.harness));
    if !transition.objective.is_empty() {
        out.push_str(&format!("objective: {}\n", transition.objective));
    }
    out.push_str(&format!("created_at: {}\n", transition.created_at));
    if let Some(revert_to) = transition
        .evidence
        .get("revert_to")
        .and_then(|v| v.as_str())
    {
        out.push_str(&format!("revert_to: {}\n", revert_to));
    }
    if let Some(reason) = transition.evidence.get("reason").and_then(|v| v.as_str()) {
        if !reason.is_empty() {
            out.push_str(&format!("reason: {reason}\n"));
        }
    }
    if let Some(seq) = transition
        .evidence
        .get("handoff_seq")
        .and_then(Value::as_u64)
    {
        out.push_str(&format!("handoff_seq: {seq}\n"));
    }

    if let Some(context) = transition.evidence.get("context") {
        out.push_str("\n## Context supplied (observed)\n");
        let learning_ids = context
            .get("learning_ids")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let skill_slugs = context
            .get("skill_slugs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if learning_ids.is_empty() && skill_slugs.is_empty() {
            out.push_str("_none recorded_\n");
        } else {
            if !learning_ids.is_empty() {
                out.push_str(&format!("learnings: {}\n", learning_ids.len()));
                for id in learning_ids.iter().take(20) {
                    out.push_str(&format!("  - {}\n", id.as_str().unwrap_or("?")));
                }
            }
            if !skill_slugs.is_empty() {
                out.push_str(&format!("skills: {}\n", skill_slugs.len()));
                for slug in skill_slugs.iter().take(20) {
                    out.push_str(&format!("  - {}\n", slug.as_str().unwrap_or("?")));
                }
            }
        }
    }

    if let Some(activity) = transition.evidence.get("activity") {
        out.push_str("\n## Activity (observed)\n");
        if let Some(reference) = activity.get("transcript_ref").and_then(Value::as_str) {
            out.push_str(&format!("transcript_ref: {reference}\n"));
        }
        if let Some(outcome) = activity.get("outcome").and_then(Value::as_str) {
            out.push_str(&format!("outcome: {outcome}\n"));
        }
        if let Some(count) = activity.get("tool_events").and_then(Value::as_u64) {
            out.push_str(&format!("tool_events: {count}\n"));
        }
        if let Some(files) = activity.get("files_touched").and_then(Value::as_array) {
            if !files.is_empty() {
                out.push_str(&format!("files_touched: {}\n", files.len()));
            }
        }
        if let Some(failures) = activity.get("failed_approaches").and_then(Value::as_array) {
            if !failures.is_empty() {
                out.push_str(&format!("failed_approaches: {}\n", failures.len()));
            }
        }
    }

    if let Some(verified) = transition.evidence.get("verified") {
        if let Some(count) = verified.get("files_changed").and_then(Value::as_u64) {
            out.push_str(&format!("\nverified.files_changed: {count}\n"));
        }
    }

    if !transition.from_root.is_empty() {
        let delta = diff_roots(
            project_dir,
            &transition.from_root,
            &transition.to_root,
            false,
            0,
            0,
        )?;
        out.push_str("\n## Verified (git diff)\n");
        for section in ["files", "state"] {
            let items = delta[section].as_array().cloned().unwrap_or_default();
            if items.is_empty() {
                continue;
            }
            let title = if section == "files" {
                "files"
            } else {
                "state (.stateroot/)"
            };
            out.push_str(&format!("\n### {title} ({})\n", items.len()));
            for item in items.iter().take(40) {
                let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                let path = item.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                out.push_str(&format!("  {status} {path}\n"));
            }
            if items.len() > 40 {
                out.push_str(&format!("  … {} more\n", items.len() - 40));
            }
        }
    } else {
        out.push_str("\n## Verified (git diff)\n\n_(genesis root — no predecessor)_\n");
    }
    Ok(out)
}

fn transition_into_root(project_dir: &Path, root_hash: &str) -> Option<Transition> {
    let dir = local_store::root(project_dir).join(TRANSITIONS_DIR);
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut matches = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(transition) = serde_json::from_str::<Transition>(&text) else {
            continue;
        };
        if transition.to_root == root_hash || transition.to_root.starts_with(root_hash) {
            matches.push(transition);
        }
    }
    matches
        .into_iter()
        .max_by(|left, right| left.created_at.cmp(&right.created_at))
}

/// Markdown lineage block for resume/handoff digests (verified facts only).
pub fn compose_digest_section(project_dir: &Path) -> String {
    let Ok(Some(latest)) = latest_root(project_dir) else {
        return String::new();
    };
    let mut out = String::from("## Work State Lineage\n\n");
    out.push_str(&format!("Current root: `{}`\n", short(&latest)));

    if let Ok(manifest) = get_root(project_dir, &latest) {
        if !manifest.created_by_harness.is_empty() {
            let reason = if manifest.created_reason.is_empty() {
                "snap".to_string()
            } else {
                manifest.created_reason.clone()
            };
            out.push_str(&format!(
                "Last actor: {} ({reason})\n",
                manifest.created_by_harness
            ));
        }
        if !manifest.coverage.is_empty() && manifest.coverage != "unknown" {
            out.push_str(&format!("Coverage: {}\n", manifest.coverage));
        }
    }

    if let Some(transition) = transition_into_root(project_dir, &latest) {
        if !transition.from_root.is_empty() {
            out.push_str(&format!(
                "Prior transition: `{}` → `{}` ({}) by {}\n",
                short(&transition.from_root),
                short(&transition.to_root),
                transition.kind,
                transition.harness
            ));
        }
        if let Some(count) = transition
            .evidence
            .get("verified")
            .and_then(|v| v.get("files_changed"))
            .and_then(|v| v.as_u64())
        {
            out.push_str(&format!("Verified tree delta at snap: {count} file(s)\n"));
        }
    }

    out.push_str(
        "\nLineage is automatic: checkpoints and finished turns snap the working tree whenever project files actually changed (bookkeeping never does). `stateroot snap` remains for explicit milestones. Use `stateroot revert` for verified restoration and `stateroot fork` for divergent work. Handoff carries continuity — it does not replace lineage.\n\n",
    );
    out
}

/// Compare two roots for experiment semantics (markdown report).
pub fn compare_roots(project_dir: &Path, a: &str, b: &str) -> Result<String, RootsError> {
    let manifest_a = get_root(project_dir, a)?;
    let manifest_b = get_root(project_dir, b)?;
    let delta = diff_roots(project_dir, &manifest_a.id, &manifest_b.id, false, 0, 0)?;

    let mut out = String::new();
    out.push_str("# Root compare\n\n");
    out.push_str(&format!(
        "A: {} (harness: {}; coverage: {})\n",
        short(&manifest_a.id),
        manifest_a.created_by_harness,
        manifest_a.coverage
    ));
    out.push_str(&format!(
        "B: {} (harness: {}; coverage: {})\n",
        short(&manifest_b.id),
        manifest_b.created_by_harness,
        manifest_b.coverage
    ));

    for (label, manifest_id) in [("A", manifest_a.id.as_str()), ("B", manifest_b.id.as_str())] {
        if let Some(transition) = transition_into_root(project_dir, manifest_id) {
            out.push_str(&format!(
                "\n## Transition into {label} (observed)\n\nharness: {}\nobjective: {}\n",
                transition.harness,
                if transition.objective.is_empty() {
                    "_(empty)_"
                } else {
                    transition.objective.as_str()
                }
            ));
            if let Some(activity) = transition.evidence.get("activity") {
                if let Some(reference) = activity.get("transcript_ref").and_then(Value::as_str) {
                    out.push_str(&format!("transcript_ref: {reference}\n"));
                }
                if let Some(outcome) = activity.get("outcome").and_then(Value::as_str) {
                    out.push_str(&format!("outcome: {outcome}\n"));
                }
            }
            if let Some(context) = transition.evidence.get("context") {
                let learning_count = context
                    .get("learning_ids")
                    .and_then(Value::as_array)
                    .map(|items| items.len())
                    .unwrap_or(0);
                let skill_count = context
                    .get("skill_slugs")
                    .and_then(Value::as_array)
                    .map(|items| items.len())
                    .unwrap_or(0);
                if learning_count > 0 || skill_count > 0 {
                    out.push_str(&format!(
                        "context: {learning_count} learning(s), {skill_count} skill(s)\n"
                    ));
                }
            }
        }
    }

    out.push_str("\n## Verified diff (files)\n\n");
    let files = delta["files"].as_array().cloned().unwrap_or_default();
    if files.is_empty() {
        out.push_str("_no project file changes_\n");
    } else {
        for item in files.iter().take(40) {
            out.push_str(&format!(
                "  {} {}\n",
                item.get("status").and_then(Value::as_str).unwrap_or("?"),
                item.get("path").and_then(Value::as_str).unwrap_or("?")
            ));
        }
        if files.len() > 40 {
            out.push_str(&format!("  … {} more\n", files.len() - 40));
        }
    }

    out.push_str("\n## Verified diff (state / .stateroot)\n\n");
    let state = delta["state"].as_array().cloned().unwrap_or_default();
    if state.is_empty() {
        out.push_str("_no state changes_\n");
    } else {
        for item in state.iter().take(40) {
            let path = item.get("path").and_then(Value::as_str).unwrap_or("?");
            out.push_str(&format!(
                "  {} {}\n",
                item.get("status").and_then(Value::as_str).unwrap_or("?"),
                path
            ));
        }
        if state.len() > 40 {
            out.push_str(&format!("  … {} more\n", state.len() - 40));
        }
    }

    Ok(out)
}

fn short(hash: &str) -> String {
    if hash.is_empty() {
        return "∅".into();
    }
    hash.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(dir.join(".stateroot")).expect("stateroot");
        (tmp, dir)
    }

    fn write(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn root_creation_non_git_auto_init_and_coverage() {
        let (_tmp, dir) = project();
        let (manifest, transition) = create_root(&dir, "cli", "first", None).expect("snap");
        assert!(dir.join(".git").is_dir(), "auto git init");
        assert_eq!(manifest.coverage, "state_only");
        assert_eq!(manifest.files_pinned, 0);
        assert!(manifest.parents.is_empty());
        assert!(transition.from_root.is_empty());
        assert_eq!(transition.kind, "snapshot");
        // Refs written.
        let repo = git2::Repository::open(&dir).unwrap();
        assert!(repo.refname_to_id(LATEST_REF).is_ok());
        assert!(repo
            .refname_to_id(&format!("{ROOTS_REF_PREFIX}{}", manifest.id))
            .is_ok());

        // Files change coverage. Root `.gitignore` and `.staterootignore`
        // are both honored (+ hardcoded `.git/` / `.stateroot/local/`).
        write(&dir, "src/main.rs", "fn main() {}\n");
        write(&dir, "node_modules/junk/index.js", "junk");
        write(&dir, ".venv/lib/foo.py", "venv");
        write(&dir, ".gitignore", ".venv/\n");
        write(&dir, ".staterootignore", "node_modules/\nsecret.txt\n");
        write(&dir, "secret.txt", "nope");
        let (m2, t2) = create_root(&dir, "cli", "second", None).expect("snap2");
        assert_eq!(m2.coverage, "full");
        assert_eq!(
            m2.files_pinned, 3,
            "src/main.rs + .gitignore + .staterootignore pinned; ignored files are not"
        );
        assert_eq!(m2.parents, vec![manifest.id.clone()]);
        assert_eq!(t2.from_root, manifest.id);
        // Ignored content is not in the tree.
        let commit = repo
            .find_commit(git2::Oid::from_str(&m2.id).unwrap())
            .unwrap();
        let tree = commit.tree().unwrap();
        assert!(tree.get_path(Path::new("secret.txt")).is_err());
        assert!(tree.get_path(Path::new("node_modules")).is_err());
        assert!(tree.get_path(Path::new(".venv")).is_err());
        assert!(tree.get_path(Path::new("src/main.rs")).is_ok());
        assert!(tree.get_path(Path::new(".stateroot")).is_ok());
    }

    #[test]
    fn revert_is_append_only_and_history_untouched() {
        let (_tmp, dir) = project();
        write(&dir, "a.txt", "v1");
        let (a, _) = create_root(&dir, "cli", "v1", None).expect("a");
        write(&dir, "a.txt", "v2");
        let (b, _) = create_root(&dir, "cli", "v2", None).expect("b");
        let (c, tc) = revert_to_root(&dir, &a.id[..12], "cli").expect("revert");
        assert_eq!(tc.kind, "revert");
        assert_eq!(tc.evidence["revert_to"], json!(a.id));
        assert_eq!(c.parents, vec![b.id.clone()]);
        // Tree equality with the target; both originals still exist.
        let repo = git2::Repository::open(&dir).unwrap();
        let tree_a = repo
            .find_commit(git2::Oid::from_str(&a.id).unwrap())
            .unwrap()
            .tree()
            .unwrap();
        let tree_c = repo
            .find_commit(git2::Oid::from_str(&c.id).unwrap())
            .unwrap()
            .tree()
            .unwrap();
        assert_eq!(tree_a.id(), tree_c.id(), "revert tree == target tree");
        assert!(repo
            .refname_to_id(&format!("{ROOTS_REF_PREFIX}{}", b.id))
            .is_ok());
        assert_eq!(latest_root(&dir).unwrap(), Some(c.id));
    }

    #[test]
    fn fork_creates_branch_ref_and_record() {
        let (_tmp, dir) = project();
        write(&dir, "a.txt", "v1");
        let (a, _) = create_root(&dir, "cli", "v1", None).expect("a");
        let (name, refname) = fork_root(&dir, &a.id, Some("claude-line"), "cli").expect("fork");
        assert_eq!(name, "claude-line");
        let repo = git2::Repository::open(&dir).unwrap();
        let oid = repo.refname_to_id(&refname).expect("fork ref");
        assert_eq!(oid.to_string(), a.id);
        assert!(dir.join(".stateroot/forks/claude-line.json").is_file());
    }

    #[test]
    fn receipt_renders_verified_git_delta() {
        let (_tmp, dir) = project();
        write(&dir, "a.txt", "v1");
        let (a, _) = create_root(&dir, "cli", "v1", None).expect("a");
        write(&dir, "a.txt", "v2");
        write(&dir, "b.txt", "new");
        let (_b, t2) = create_root(&dir, "codex", "v2", None).expect("b");
        let receipt = render_receipt(&dir, &t2.id).expect("receipt");
        assert!(receipt.contains("# Transition receipt"), "{receipt}");
        assert!(receipt.contains("harness: codex"), "{receipt}");
        assert!(receipt.contains("## Verified (git diff)"), "{receipt}");
        assert!(receipt.contains("M a.txt"), "{receipt}");
        assert!(receipt.contains("A b.txt"), "{receipt}");
        assert!(
            receipt.contains(&format!("{} -> {}", &a.id[..12], &_b.id[..12])),
            "{receipt}"
        );
    }

    #[test]
    fn diff_names_status_and_content_with_caps() {
        let (_tmp, dir) = project();
        write(&dir, "a.txt", "one\ntwo\nthree\n");
        let (a, _) = create_root(&dir, "cli", "v1", None).expect("a");
        write(&dir, "a.txt", "one\nTWO\nthree\nfour\nfive\nsix\n");
        let (b, _) = create_root(&dir, "cli", "v2", None).expect("b");
        let names = diff_roots(&dir, &a.id, &b.id, false, 20, 200).expect("diff");
        assert_eq!(names["files"][0]["path"], json!("a.txt"));
        assert_eq!(names["files"][0]["status"], json!("M"));
        let content = diff_roots(&dir, &a.id, &b.id, true, 20, 7).expect("content");
        let first = &content["contents"][0];
        assert_eq!(first["path"], json!("a.txt"));
        assert!(first["diff"].as_str().unwrap().contains("-two"));
        assert_eq!(first["truncated"], json!(true));
    }

    #[test]
    fn snap_if_changed_first_call_behaves_like_snap() {
        let (_tmp, dir) = project();
        write(&dir, "a.txt", "v1");
        let outcome = snap_if_changed(&dir, "cli", "auto: checkpoint", None).expect("auto");
        match outcome {
            SnapOutcome::Created(m, _) => assert!(m.parents.is_empty()),
            SnapOutcome::Unchanged { .. } => panic!("first snap must create a root"),
        }
    }

    #[test]
    fn snap_if_changed_skips_unchanged_and_bookkeeping() {
        let (_tmp, dir) = project();
        write(&dir, "src/main.rs", "fn main() {}\n");
        let (first, _) = create_root(&dir, "cli", "first", None).expect("snap");

        // Nothing moved -> unchanged, same root, no new manifest.
        let outcome = snap_if_changed(&dir, "cli", "auto: checkpoint", None).expect("auto");
        match outcome {
            SnapOutcome::Unchanged { root } => assert_eq!(root, first.id),
            SnapOutcome::Created(m, _) => panic!("expected unchanged, created {}", m.id),
        }

        // `.stateroot/` bookkeeping (outside local/) is pinned into trees but
        // is NOT work — it must not fabricate a root.
        write(&dir, ".stateroot/plans/plan-x.md", "# plan\n");
        let outcome = snap_if_changed(&dir, "cli", "auto: stop", None).expect("auto2");
        assert!(
            matches!(outcome, SnapOutcome::Unchanged { .. }),
            "store bookkeeping created a root"
        );

        // Real work moves the tree -> new root chained on the first.
        write(&dir, "src/main.rs", "fn main() { println!(\"hi\"); }\n");
        let outcome = snap_if_changed(&dir, "cli", "auto: checkpoint", None).expect("auto3");
        match outcome {
            SnapOutcome::Created(m, t) => {
                assert_eq!(m.parents, vec![first.id.clone()]);
                assert_eq!(t.from_root, first.id);
                assert_eq!(latest_root(&dir).unwrap(), Some(m.id));
            }
            SnapOutcome::Unchanged { .. } => panic!("expected a new root for real work"),
        }
    }

    #[test]
    fn stat_index_reuses_unchanged_blobs_and_rehashes_on_change() {
        let (_tmp, dir) = project();
        write(&dir, "a.txt", "alpha");
        write(&dir, "b.txt", "beta");
        write(&dir, "c.txt", "gamma");
        let repo = ensure_repo(&dir).expect("repo");

        let first = build_tree(&repo, &dir).expect("first build");
        assert_eq!(first.index_hits, 0, "first build hashes everything");
        assert_eq!(first.index_misses, 3);

        let second = build_tree(&repo, &dir).expect("second build");
        assert_eq!(second.index_hits, 3, "unchanged files reuse the index");
        assert_eq!(second.index_misses, 0);
        assert_eq!(first.tree, second.tree, "same content, same tree");

        // One changed file: only that file is re-hashed.
        write(&dir, "a.txt", "alpha v2");
        let third = build_tree(&repo, &dir).expect("third build");
        assert_eq!(third.index_hits, 2, "b and c reuse");
        assert_eq!(third.index_misses, 1, "only a.txt re-hashed");
        assert_ne!(first.tree, third.tree);
    }

    #[test]
    fn stat_index_never_reuses_entries_newer_than_the_index_write() {
        let (_tmp, dir) = project();
        write(&dir, "a.txt", "alpha");
        write(&dir, "b.txt", "beta");
        let repo = ensure_repo(&dir).expect("repo");
        build_tree(&repo, &dir).expect("first build");

        // Poison the index's write time into the past: every stored entry is
        // now "newer-or-equal" to it, so the racy-clean rule must re-hash
        // everything instead of trusting possibly-stale stats.
        let index_path = blob_index_path(&dir);
        let mut index: BlobIndex =
            serde_json::from_str(&std::fs::read_to_string(&index_path).unwrap()).unwrap();
        index.written_at = (0, 0);
        std::fs::write(&index_path, serde_json::to_string(&index).unwrap()).unwrap();

        let build = build_tree(&repo, &dir).expect("second build");
        assert_eq!(build.index_hits, 0, "racy entries are never reused");
        assert_eq!(build.index_misses, 2);
    }

    #[test]
    fn storage_amplification_is_bounded_by_changes_not_by_snaps() {
        let (_tmp, dir) = project();
        for i in 0..20 {
            write(&dir, &format!("file-{i}.txt"), &format!("content {i}"));
        }
        let repo = ensure_repo(&dir).expect("repo");

        let mut misses_total = 0u64;
        let mut hits_total = 0u64;
        build_tree(&repo, &dir).expect("initial");
        for i in 0..100 {
            // One changed file per snap; the other 19 must come from the index.
            write(
                &dir,
                &format!("file-{}.txt", i % 20),
                &format!("content {i} v{i}"),
            );
            let build = build_tree(&repo, &dir).expect("snap");
            misses_total += build.index_misses;
            hits_total += build.index_hits;
        }
        assert!(
            misses_total <= 100 + 20,
            "misses must track changes, not files x snaps: {misses_total}"
        );
        assert!(
            hits_total >= 99 * 19,
            "the index must dominate at steady state: {hits_total}"
        );

        // The index itself stays machine-local and small — never a
        // per-snap copy, never inside roots.
        let index_size = std::fs::metadata(blob_index_path(&dir))
            .expect("index")
            .len();
        assert!(
            index_size < 20 * 1024,
            "index is a few KB for 20 files: {index_size}"
        );
        let (m, _) = create_root(&dir, "cli", "final", None).expect("final root");
        let repo_read = git2::Repository::open(&dir).expect("open");
        let tip = repo_read
            .find_commit(git2::Oid::from_str(&m.id).unwrap())
            .unwrap()
            .tree()
            .unwrap();
        assert!(
            tip.get_path(std::path::Path::new(".stateroot/local"))
                .is_err(),
            "local/ must never enter roots"
        );
    }

    #[test]
    fn concurrent_snaps_never_fork_the_lineage_silently() {
        // Phase 1 CAS contract: racing same-ref snaps either chain (the
        // second reads the new tip) or one wins and the loser gets a
        // retryable Moved conflict. Whatever the interleave, every created
        // root stays reachable from latest — no silent siblings.
        let (_tmp, dir) = project();
        write(&dir, "base.txt", "base");
        create_root(&dir, "cli", "base", None).expect("base");
        let dir_one = dir.clone();
        let dir_two = dir.clone();
        let t1 = std::thread::spawn(move || {
            write(&dir_one, "t1.txt", "one");
            create_root(&dir_one, "cli", "t1", None)
        });
        let t2 = std::thread::spawn(move || {
            write(&dir_two, "t2.txt", "two");
            create_root(&dir_two, "cli", "t2", None)
        });
        let results = vec![t1.join().expect("join1"), t2.join().expect("join2")];
        let repo = git2::Repository::open(&dir).unwrap();
        let latest = repo.refname_to_id(LATEST_REF).expect("latest").to_string();
        let mut reachable = std::collections::BTreeSet::new();
        let mut current: Option<git2::Oid> = Some(latest.parse().expect("oid"));
        while let Some(oid) = current {
            reachable.insert(oid.to_string());
            let commit = repo.find_commit(oid).expect("commit");
            current = (0..commit.parent_count()).find_map(|i| commit.parent_id(i).ok());
        }
        let mut wins = 0;
        let mut conflicts = 0;
        for result in results {
            match result {
                Ok((manifest, _)) => {
                    wins += 1;
                    assert!(
                        reachable.contains(&manifest.id),
                        "root {} created but unreachable from latest — a silent sibling",
                        manifest.id
                    );
                }
                Err(RootsError::RefCas(crate::safe_io::RefCasError::Moved { .. })) => {
                    conflicts += 1
                }
                Err(other) => panic!("unexpected snap error: {other}"),
            }
        }
        assert!(wins >= 1, "at least one snap must succeed");
        assert!(wins + conflicts == 2);
    }

    #[test]
    fn blob_index_survives_a_partial_tmp_write() {
        let (_tmp, dir) = project();
        write(&dir, "a.txt", "alpha");
        let repo = ensure_repo(&dir).expect("repo");
        build_tree(&repo, &dir).expect("first");
        let path = blob_index_path(&dir);
        let before = std::fs::read_to_string(&path).expect("before");
        std::fs::write(path.with_extension("json.tmp"), "{").expect("torn tmp");
        let after = std::fs::read_to_string(&path).expect("after");
        assert_eq!(before, after);
        let _: BlobIndex = serde_json::from_str(&after).expect("valid json");
    }

    #[test]
    fn write_audit_names_an_unreported_stateroot_write() {
        let (_tmp, dir) = project();
        write(&dir, "src/main.rs", "fn main() {}\n");
        create_root(&dir, "cli", "first", None).expect("snap");
        // Bypass the funnel with a bare write (what the audit must catch),
        // then move a project file so a root is created.
        write(&dir, ".stateroot/custom.md", "unreported\n");
        write(&dir, "src/main.rs", "fn main() { println!(\"hi\"); }\n");
        let outcome = snap_if_changed(&dir, "cli", "auto: checkpoint", None).expect("snap2");
        let SnapOutcome::Created(_, transition) = outcome else {
            panic!("expected a new root");
        };
        let audit = &transition.evidence["write_audit"];
        let unreported = audit["unreported_writes"]
            .as_array()
            .expect("unreported_writes array");
        assert!(
            unreported
                .iter()
                .any(|p| p.as_str() == Some(".stateroot/custom.md")),
            "audit must name the bypassed write: {audit}"
        );
    }

    #[test]
    fn write_audit_lets_funnel_reported_writes_pass() {
        let (_tmp, dir) = project();
        write(&dir, "src/main.rs", "fn main() {}\n");
        create_root(&dir, "cli", "first", None).expect("snap");
        // Routed write (append_episodic reports to the ledger) + real work.
        local_store::append_episodic(
            &dir,
            &serde_json::json!({"ts": local_store::now_rfc3339(), "harness": "cli", "note": "n", "files": []}),
        )
        .expect("episodic");
        write(&dir, "src/main.rs", "fn main() { println!(\"hi\"); }\n");
        let outcome = snap_if_changed(&dir, "cli", "auto: checkpoint", None).expect("snap2");
        let SnapOutcome::Created(_, transition) = outcome else {
            panic!("expected a new root");
        };
        let unreported = transition.evidence["write_audit"]["unreported_writes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            !unreported
                .iter()
                .any(|p| p.as_str() == Some(".stateroot/memories/episodic.jsonl")),
            "funnel-reported write flagged: {:?}",
            transition.evidence["write_audit"]
        );
    }

    #[test]
    fn a_fully_routed_flow_audits_completely_clean() {
        // Phase 4 clean-audit contract: episodic + handoff + plan + root
        // manifest/transition all flow through the funnel, so the next
        // snap's audit is entirely empty.
        let (_tmp, dir) = project();
        write(&dir, "src/main.rs", "fn main() {}\n");
        create_root(&dir, "cli", "first", None).expect("snap");
        local_store::append_episodic(
            &dir,
            &serde_json::json!({"ts": local_store::now_rfc3339(), "harness": "cli", "note": "n", "files": []}),
        )
        .expect("episodic");
        crate::plans::record(&dir, "demo plan", "cli", None, "# body\n").expect("plan");
        write(&dir, "src/main.rs", "fn main() { println!(\"hi\"); }\n");
        let outcome = snap_if_changed(&dir, "cli", "auto: checkpoint", None).expect("snap2");
        let SnapOutcome::Created(_, transition) = outcome else {
            panic!("expected a new root");
        };
        assert!(
            transition.evidence.get("write_audit").is_none(),
            "a fully routed flow must audit clean: {:?}",
            transition.evidence["write_audit"]
        );
    }

    #[test]
    fn tree_violations_flags_a_path_the_rules_forbid() {
        let (_tmp, dir) = project();
        let repo = ensure_repo(&dir).expect("repo");
        // Hand-build a tree the walk would never produce: `.stateroot/local/`
        // is hardcoded-ignored and must never appear in any root.
        let blob = repo.blob(b"nope").expect("blob");
        let mut builder = repo.treebuilder(None).expect("treebuilder");
        let mut stateroot = repo.treebuilder(None).expect("treebuilder");
        let mut local = repo.treebuilder(None).expect("treebuilder");
        local
            .insert("secret.txt", blob, git2::FileMode::Blob.into())
            .expect("insert");
        let local_oid = repo
            .find_tree(local.write().expect("local tree"))
            .expect("t")
            .id();
        stateroot
            .insert("local", local_oid, git2::FileMode::Tree.into())
            .expect("insert");
        let stateroot_oid = repo
            .find_tree(stateroot.write().expect("stateroot tree"))
            .expect("t")
            .id();
        builder
            .insert(".stateroot", stateroot_oid, git2::FileMode::Tree.into())
            .expect("insert");
        let tree_oid = builder.write().expect("tree");
        let violations = tree_violations(&repo, &dir, tree_oid);
        assert!(
            violations
                .iter()
                .any(|p| p == ".stateroot/local/secret.txt"),
            "violation not flagged: {violations:?}"
        );
    }

    #[test]
    fn written_log_rotation_bounds_its_size() {
        let (_tmp, dir) = project();
        for i in 0..3000 {
            local_store::report_written(&dir, &format!("memories/fill-{i}.md"));
        }
        let len = std::fs::metadata(dir.join(".stateroot/local/written-log.jsonl"))
            .expect("log")
            .len();
        assert!(len <= 128 * 1024 + 4096, "log grew unbounded: {len}");
    }

    // -- WS5: fork worktrees + per-fork lineage ------------------------------

    /// Materialize a fork with a claimed plan into a detached worktree.
    fn forked_worktree(dir: &Path) -> (tempfile::TempDir, PathBuf, String, RootManifest, String) {
        write(dir, "src/main.rs", "fn main() {}\n");
        let (first, _) = create_root(dir, "cli", "first", None).expect("snap");
        // Deliberately record after the root: this is the normal plan-first,
        // parallel-execution flow that used to leave the new fork planless.
        let plan = crate::plans::record(dir, "plan x", "cli", None, "# plan x\n").expect("plan");
        let plan_id = plan.id.clone();
        let (name, _) = fork_root(dir, &first.id, Some("fork-x"), "cli").expect("fork");
        let wt_tmp = tempfile::tempdir().expect("wt tmp");
        let wt = wt_tmp.path().join("checkout");
        fork_materialize(dir, &name, &wt, Some(&plan_id)).expect("materialize");
        (wt_tmp, wt, name, first, plan_id)
    }

    #[test]
    fn fork_materialize_detaches_head_and_carries_the_plan() {
        let (_tmp, dir) = project();
        let (_wt_tmp, wt, name, first, plan_id) = forked_worktree(&dir);
        // The worktree physically carries the snapshot's .stateroot state.
        let carried = std::fs::read_dir(wt.join(".stateroot/plans"))
            .expect("plans dir")
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .count();
        assert!(carried >= 1, "the claimed plan must travel with the fork");
        assert!(wt.join("src/main.rs").is_file());
        // HEAD detached at the fork root commit (user branches untouched).
        let wt_repo = git2::Repository::open(&wt).expect("wt repo");
        assert!(wt_repo.head_detached().expect("detached"));
        assert_eq!(
            wt_repo
                .head()
                .expect("head")
                .target()
                .expect("oid")
                .to_string(),
            first.id
        );
        // Machine-local fork context stamped with the claimed plan.
        let ctx = local_store::fork_context(&wt).expect("fork context");
        assert_eq!(ctx.fork, name);
        assert_eq!(ctx.plan.as_deref(), Some(plan_id.as_str()));
        assert_eq!(ctx.parent_root, first.id);
        // The fork record carries the plan id (absolute paths stay in
        // the machine-local registry, never in shared state).
        let record =
            std::fs::read_to_string(dir.join(".stateroot/forks").join(format!("{name}.json")))
                .expect("fork record");
        assert!(record.contains(&plan_id), "{record}");
        assert!(
            !record.contains("\"worktree\""),
            "paths stay local: {record}"
        );
        // The plan meta records the claim.
        let meta_text =
            std::fs::read_to_string(dir.join(".stateroot/plans").join(format!("{plan_id}.json")))
                .expect("plan meta");
        assert!(meta_text.contains("claimed_by"), "{meta_text}");
        assert!(meta_text.contains(&name), "{meta_text}");
        // Regression: the plan can be recorded after the fork root. It must
        // still be readable and independently activatable in the worktree.
        let (carried_plan, carried_body) = crate::plans::load(&wt, &plan_id).expect("carried plan");
        assert_eq!(carried_plan.status(), crate::plans::PlanStatus::Draft);
        assert!(std::fs::read_to_string(carried_body)
            .expect("carried body")
            .contains("# plan x"));
        crate::plans::transition(&wt, &plan_id, crate::plans::PlanStatus::Approved)
            .expect("approve inside fork");
        let (active, _) = crate::plans::transition(&wt, &plan_id, crate::plans::PlanStatus::Active)
            .expect("activate inside fork");
        assert_eq!(active.status(), crate::plans::PlanStatus::Active);
    }

    #[test]
    fn fork_materialize_validates_before_mutating_and_rolls_back() {
        let (_tmp, dir) = project();
        let (_wt_tmp, _wt, name, first, plan_id) = forked_worktree(&dir);

        // Existing destination: refused.
        let occupied = tempfile::tempdir().expect("occupied");
        let (name2, _) = fork_root(&dir, &first.id, Some("fork-y"), "cli").expect("fork2");
        let err = fork_materialize(&dir, &name2, occupied.path(), None)
            .expect_err("existing destination must refuse");
        assert!(err.to_string().contains("already exists"), "{err}");

        // Destination inside the project tree: refused.
        let inside = dir.join("nested-checkout");
        let err = fork_materialize(&dir, &name2, &inside, None)
            .expect_err("nested destination must refuse");
        assert!(err.to_string().contains("inside the project tree"), "{err}");

        // A plan already claimed elsewhere: refused, and the claim stays.
        let err = fork_materialize(&dir, &name2, &occupied.path().join("new"), Some(&plan_id))
            .expect_err("duplicate claim must refuse");
        assert!(err.to_string().contains("already claimed"), "{err}");
        let meta_text =
            std::fs::read_to_string(dir.join(".stateroot/plans").join(format!("{plan_id}.json")))
                .expect("meta");
        assert!(
            meta_text.contains(&name),
            "the original claim must survive the rejected attempt"
        );

        // A nonexistent plan: refused before any mutation.
        let err = fork_materialize(
            &dir,
            &name2,
            &occupied.path().join("new"),
            Some("plan-ghost"),
        )
        .expect_err("ghost plan must refuse");
        assert!(err.to_string().contains("no plan named"), "{err}");
    }

    #[test]
    fn fork_worktree_snaps_chain_on_the_fork_ref_not_latest() {
        let (_tmp, dir) = project();
        let (_wt_tmp, wt, name, first, _plan) = forked_worktree(&dir);
        write(&wt, "src/lib.rs", "pub fn work() {}\n");
        let outcome = snap_if_changed(&wt, "codex", "auto: fork work", None).expect("fork snap");
        let SnapOutcome::Created(m, t) = outcome else {
            panic!("expected a root in the fork worktree");
        };
        // Parent is the fork tip (the original root), lineage is on the fork.
        assert_eq!(m.parents, vec![first.id.clone()]);
        assert_eq!(t.from_root, first.id);
        let repo = git2::Repository::open(&dir).expect("repo");
        let fork_tip = repo
            .refname_to_id(&format!("{FORKS_REF_PREFIX}{name}"))
            .expect("fork ref")
            .to_string();
        assert_eq!(fork_tip, m.id, "fork ref advanced to the fork snap");
        let latest = repo.refname_to_id(LATEST_REF).expect("latest").to_string();
        assert_eq!(latest, first.id, "trunk latest untouched by fork work");
        // And the trunk's next snap still chains on the trunk's latest.
        write(&dir, "src/other.rs", "fn other() {}\n");
        let outcome = snap_if_changed(&dir, "kimi", "auto: trunk work", None).expect("trunk snap");
        let SnapOutcome::Created(tm, _) = outcome else {
            panic!("expected a root on the trunk");
        };
        assert_eq!(tm.parents, vec![first.id.clone()]);
    }

    #[test]
    fn stateroot_worktrees_dir_never_enters_a_tree() {
        let (_tmp, dir) = project();
        write(&dir, "src/main.rs", "fn main() {}\n");
        create_root(&dir, "cli", "first", None).expect("snap");
        write(&dir, ".stateroot/worktrees/nested-checkout.txt", "bloat\n");
        write(&dir, "src/main.rs", "fn main() { println!(\"hi\"); }\n");
        let outcome = snap_if_changed(&dir, "cli", "auto", None).expect("snap2");
        let SnapOutcome::Created(m, _) = outcome else {
            panic!("expected a new root");
        };
        let repo = git2::Repository::open(&dir).expect("repo");
        let tree = repo
            .find_commit(m.id.parse().expect("oid"))
            .expect("commit")
            .tree()
            .expect("tree");
        assert!(
            tree.get_path(Path::new(".stateroot/worktrees")).is_err(),
            "nested worktree content entered the tree"
        );
    }

    // -- WS5 batch B: merge -------------------------------------------------

    /// Fork from `root_id`, materialize, write `file`, snap, return the fork tip.
    fn fork_with_change(
        dir: &Path,
        root_id: &str,
        name: &str,
        file: &str,
        content: &str,
    ) -> (tempfile::TempDir, String) {
        let (fork_name, _) = fork_root(dir, root_id, Some(name), "cli").expect("fork");
        let wt_tmp = tempfile::tempdir().expect("wt tmp");
        let wt = wt_tmp.path().join("checkout");
        fork_materialize(dir, &fork_name, &wt, None).expect("materialize");
        write(&wt, file, content);
        let outcome = snap_if_changed(&wt, "codex", "auto: fork work", None).expect("fork snap");
        let SnapOutcome::Created(m, _) = outcome else {
            panic!("expected a root in fork {name}");
        };
        (wt_tmp, m.id)
    }

    #[test]
    fn merge_folds_two_forks_into_one_multi_parent_root() {
        let (_tmp, dir) = project();
        write(&dir, "src/main.rs", "fn main() {}\n");
        let (first, _) = create_root(&dir, "cli", "first", None).expect("snap");
        let (_wa, tip_a) =
            fork_with_change(&dir, &first.id, "fork-a", "src/lib_a.rs", "pub fn a() {}\n");
        let (_wb, tip_b) =
            fork_with_change(&dir, &first.id, "fork-b", "src/lib_b.rs", "pub fn b() {}\n");

        let (manifest, transition, merged) =
            merge_forks(&dir, &["fork-a".to_string(), "fork-b".to_string()], "kimi")
                .expect("merge");
        assert_eq!(merged.len(), 2);
        assert_eq!(
            manifest.parents,
            vec![first.id.clone(), tip_a.clone(), tip_b.clone()],
            "one merge root with trunk + both fork tips as parents"
        );
        assert_eq!(transition.kind, "merge");
        let repo = git2::Repository::open(&dir).expect("repo");
        let tree = repo
            .find_commit(manifest.id.parse().expect("oid"))
            .expect("commit")
            .tree()
            .expect("tree");
        assert!(
            tree.get_path(Path::new("src/lib_a.rs")).is_ok(),
            "fork-a work missing"
        );
        assert!(
            tree.get_path(Path::new("src/lib_b.rs")).is_ok(),
            "fork-b work missing"
        );
        assert!(
            tree.get_path(Path::new("src/main.rs")).is_ok(),
            "trunk file lost"
        );
        let latest = repo.refname_to_id(LATEST_REF).expect("latest").to_string();
        assert_eq!(latest, manifest.id, "trunk advanced to the merge root");
        // Fork refs stay as history.
        assert_eq!(
            repo.refname_to_id(&format!("{FORKS_REF_PREFIX}fork-a"))
                .expect("fork-a ref")
                .to_string(),
            tip_a
        );
    }

    #[test]
    fn merge_conflict_reports_paths_and_creates_no_root() {
        let (_tmp, dir) = project();
        write(&dir, "src/main.rs", "fn main() {}\n");
        let (first, _) = create_root(&dir, "cli", "first", None).expect("snap");
        let (_wa, _ta) = fork_with_change(
            &dir,
            &first.id,
            "fork-a",
            "src/main.rs",
            "fn main() { println!(\"a\"); }\n",
        );
        let (_wb, _tb) = fork_with_change(
            &dir,
            &first.id,
            "fork-b",
            "src/main.rs",
            "fn main() { println!(\"b\"); }\n",
        );

        let err = merge_forks(&dir, &["fork-a".to_string(), "fork-b".to_string()], "kimi")
            .expect_err("conflicting forks must not merge");
        let text = err.to_string();
        assert!(
            text.contains("src/main.rs"),
            "conflict path not reported: {text}"
        );
        assert!(text.contains("no merge root created"), "{text}");
        let repo = git2::Repository::open(&dir).expect("repo");
        assert_eq!(
            repo.refname_to_id(LATEST_REF).expect("latest").to_string(),
            first.id,
            "trunk moved despite the conflict"
        );
    }

    #[test]
    fn merge_reports_nothing_when_forks_are_contained() {
        let (_tmp, dir) = project();
        write(&dir, "src/main.rs", "fn main() {}\n");
        let (first, _) = create_root(&dir, "cli", "first", None).expect("snap");
        let (_name, _) = fork_root(&dir, &first.id, Some("fork-still"), "cli").expect("fork");
        let err = merge_forks(&dir, &["fork-still".to_string()], "kimi")
            .expect_err("a fork that never moved contributes nothing");
        assert!(err.to_string().contains("nothing to merge"), "{err}");
    }

    // -- Repair-plan failing fixtures (Phase 0): red until their phase lands --

    #[test]
    fn merge_materializes_the_trunk_working_tree() {
        // Audit F2: a successful merge must leave the trunk filesystem equal
        // to the merge root — advancing the ref alone is a false success.
        let (_tmp, dir) = project();
        write(&dir, "src/main.rs", "fn main() {}\n");
        let (first, _) = create_root(&dir, "cli", "first", None).expect("snap");
        let (_wt, _tip) =
            fork_with_change(&dir, &first.id, "fork-a", "src/lib_a.rs", "pub fn a() {}\n");
        let (manifest, _t, _merged) =
            merge_forks(&dir, &["fork-a".to_string()], "kimi").expect("merge");
        let _ = manifest;
        assert!(
            dir.join("src/lib_a.rs").is_file(),
            "merge advanced the ref but left the trunk working tree without the merged file"
        );
    }

    #[test]
    fn a_dirty_trunk_workspace_refuses_and_nothing_changes() {
        // 6C: uncommitted trunk work is never overwritten by a merge.
        let (_tmp, dir) = project();
        write(&dir, "src/main.rs", "fn main() {}\n");
        let (first, _) = create_root(&dir, "cli", "first", None).expect("snap");
        let (_wt, _tip) =
            fork_with_change(&dir, &first.id, "fork-a", "src/lib_a.rs", "pub fn a() {}\n");
        write(&dir, "src/main.rs", "fn main() { println!(\"dirty\"); }\n");
        let err = merge_forks(&dir, &["fork-a".to_string()], "kimi")
            .expect_err("dirty workspace must refuse");
        assert!(err.to_string().contains("uncommitted"), "{err}");
        assert!(
            !dir.join("src/lib_a.rs").exists(),
            "the merged file must NOT appear after a refusal"
        );
        let repo = git2::Repository::open(&dir).expect("repo");
        assert_eq!(
            repo.refname_to_id(LATEST_REF).expect("latest").to_string(),
            first.id
        );
    }

    #[test]
    fn next_checkpoint_after_a_merge_is_a_noop() {
        // 6C exit invariant: the trunk filesystem equals the merge root, so
        // the next automatic snap is Unchanged.
        let (_tmp, dir) = project();
        write(&dir, "src/main.rs", "fn main() {}\n");
        let (first, _) = create_root(&dir, "cli", "first", None).expect("snap");
        let (_wt, _tip) =
            fork_with_change(&dir, &first.id, "fork-a", "src/lib_a.rs", "pub fn a() {}\n");
        merge_forks(&dir, &["fork-a".to_string()], "kimi").expect("merge");
        match snap_if_changed(&dir, "cli", "auto: checkpoint", None).expect("snap") {
            SnapOutcome::Unchanged { .. } => {}
            SnapOutcome::Created(m, _) => panic!("merge left a dirty workspace; new root {}", m.id),
        }
    }

    #[test]
    fn merge_fails_closed_from_a_fork_worktree() {
        let (_tmp, dir) = project();
        write(&dir, "src/main.rs", "fn main() {}\n");
        let (first, _) = create_root(&dir, "cli", "first", None).expect("snap");
        let (_wt, _tip) =
            fork_with_change(&dir, &first.id, "fork-a", "src/lib_a.rs", "pub fn a() {}\n");
        let (_wt2, _tip2) =
            fork_with_change(&dir, &first.id, "fork-b", "src/lib_b.rs", "pub fn b() {}\n");
        // Run the merge FROM fork-a's worktree: must refuse with the exact
        // recovery, never redirect the trunk merge into the fork ref.
        let wt_path = registered_worktree_path(&dir, "fork-a").expect("registered");
        let err = merge_forks(&wt_path, &["fork-b".to_string()], "kimi")
            .expect_err("merge from a fork worktree must fail closed");
        assert!(err.to_string().contains("trunk checkout"), "{err}");
    }

    #[test]
    fn a_merged_fork_worktree_is_cleaned_up_but_history_stays() {
        let (_tmp, dir) = project();
        write(&dir, "src/main.rs", "fn main() {}\n");
        let (first, _) = create_root(&dir, "cli", "first", None).expect("snap");
        let (_wt, _tip) =
            fork_with_change(&dir, &first.id, "fork-a", "src/lib_a.rs", "pub fn a() {}\n");
        let wt_path = registered_worktree_path(&dir, "fork-a").expect("registered");
        merge_forks(&dir, &["fork-a".to_string()], "kimi").expect("merge");
        assert!(
            !wt_path.exists(),
            "merged fork worktree must be removed: {}",
            wt_path.display()
        );
        assert!(
            registered_worktree_path(&dir, "fork-a").is_none(),
            "registry entry must go"
        );
        // History stays: the fork ref and record survive the cleanup.
        let repo = git2::Repository::open(&dir).expect("repo");
        assert!(
            repo.refname_to_id(&format!("{FORKS_REF_PREFIX}fork-a"))
                .is_ok(),
            "fork ref must stay as history"
        );
        assert!(
            dir.join(".stateroot/forks/fork-a.json").is_file(),
            "fork record must stay as history"
        );
    }

    #[test]
    fn concurrent_trunk_snaps_form_one_chain_not_orphaned_siblings() {
        // Audit F6 (Phase 4): concurrent same-ref snaps must serialize into
        // ONE causal chain. Note the invariant is NOT "every racer produces
        // a root" — a snap may legitimately capture several racers' files,
        // leaving later racers Unchanged. The invariants are: every created
        // root is reachable from latest, the chain is unbroken, and the
        // final tree carries every racer's file.
        let (_tmp, dir) = project();
        write(&dir, "src/main.rs", "fn main() {}\n");
        let (first, _) = create_root(&dir, "cli", "first", None).expect("snap");
        let mut handles = Vec::new();
        for i in 0..8 {
            let d = dir.clone();
            handles.push(std::thread::spawn(move || {
                write(
                    &d,
                    &format!("src/f{i}.rs"),
                    &format!("pub fn f{i}() {{}}\n"),
                );
                match snap_if_changed(&d, "cli", "race", None) {
                    Ok(outcome) => Some(outcome),
                    // Losing the CAS race after every retry is legitimate
                    // under real contention (slow runners) — the invariant
                    // is the unbroken chain below, not that THIS racer
                    // landed a root.
                    Err(RootsError::RefCas(crate::safe_io::RefCasError::Moved { .. })) => None,
                    Err(e) => panic!("snap: {e}"),
                }
            }));
        }
        let mut created = std::collections::BTreeSet::new();
        for h in handles {
            if let Some(SnapOutcome::Created(m, _)) = h.join().expect("join") {
                created.insert(m.id);
            }
        }
        assert!(!created.is_empty(), "at least one snap must land");
        let repo = ensure_repo(&dir).expect("repo");
        let mut chain = Vec::new();
        let mut current = repo.refname_to_id(LATEST_REF).ok();
        while let Some(oid) = current {
            chain.push(oid.to_string());
            let commit = repo.find_commit(oid).expect("commit");
            // Single-parent line only: any merge-shaped commit here means a
            // broken chain in this scenario.
            assert!(
                commit.parent_count() <= 1,
                "chain broke into a merge shape: {oid}"
            );
            current = commit.parent_id(0).ok();
        }
        assert_eq!(
            chain.last().map(String::as_str),
            Some(first.id.as_str()),
            "the chain must reach back to the first root"
        );
        let reachable: std::collections::BTreeSet<_> = chain.iter().cloned().collect();
        let orphaned: Vec<_> = created.difference(&reachable).collect();
        assert!(
            orphaned.is_empty(),
            "{} roots were created but are unreachable from latest — same-ref writes formed siblings, not a chain",
            orphaned.len()
        );
        let tip_tree = repo
            .find_commit(repo.refname_to_id(LATEST_REF).expect("latest"))
            .expect("commit")
            .tree()
            .expect("tree");
        for i in 0..8 {
            assert!(
                tip_tree
                    .get_path(Path::new(&format!("src/f{i}.rs")))
                    .is_ok(),
                "racer {i}'s file lost despite the chain"
            );
        }
    }
}
