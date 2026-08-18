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

/// Build the working tree; returns (tree oid, files pinned, total bytes).
fn build_tree(repo: &Repository, dir: &Path) -> Result<(git2::Oid, i64, u64), RootsError> {
    let rules = IgnoreRules::load(dir);
    fn walk(
        repo: &Repository,
        root: &Path,
        dir: &Path,
        rules: &IgnoreRules,
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
                if let Some(sub) = walk(repo, root, &path, rules, pinned, total_bytes)? {
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
                let bytes = std::fs::read(&path)?;
                *total_bytes += bytes.len() as u64;
                let blob = repo.blob(&bytes)?;
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
    let tree = walk(repo, dir, dir, &rules, &mut pinned, &mut total_bytes)?;
    // An empty tree is legal (state_only roots before any project file).
    let tree = match tree {
        Some(oid) => oid,
        None => repo.treebuilder(None)?.write()?,
    };
    Ok((tree, pinned, total_bytes))
}

fn latest_oid(repo: &Repository) -> Option<git2::Oid> {
    repo.refname_to_id(LATEST_REF).ok()
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
    let hash = oid.to_string();
    repo.reference(&format!("{ROOTS_REF_PREFIX}{hash}"), oid, true, "root")?;
    repo.reference(LATEST_REF, oid, true, "latest root")?;

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
    write_json(
        &root
            .join(TRANSITIONS_DIR)
            .join(format!("{}.json", transition.id)),
        &transition,
    )?;

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
    write_json(
        &root.join(ROOTS_DIR).join(format!("{}.json", manifest.id)),
        &manifest,
    )?;
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
    let (tree, pinned, tree_bytes) = build_tree(&repo, project_dir)?;
    let parent = latest_oid(&repo);
    let parents: Vec<git2::Oid> = parent.into_iter().collect();
    let parent_hashes: Vec<String> = parents.iter().map(|o| o.to_string()).collect();
    let from_root = parent_hashes.first().cloned().unwrap_or_default();
    let message = match reason {
        "" => format!("root by {harness}"),
        r => format!("root: {r} (by {harness})"),
    };
    let oid = commit_root(&repo, tree, &parents, &message)?;
    let to_root = oid.to_string();
    let evidence = crate::snap_context::build_snap_evidence(
        project_dir,
        harness,
        reason,
        &from_root,
        &to_root,
        snap_ctx,
    );
    persist_root(
        &repo,
        project_dir,
        oid,
        parent_hashes,
        harness,
        reason,
        pinned,
        tree_bytes,
        "snapshot",
        evidence,
    )
}

/// The latest root hash, if any.
pub fn latest_root(project_dir: &Path) -> Result<Option<String>, RootsError> {
    let repo = ensure_repo(project_dir)?;
    Ok(latest_oid(&repo).map(|oid| oid.to_string()))
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
    if let Some(tip) = latest_oid(&repo) {
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
    let parent = latest_oid(&repo);
    let parents: Vec<git2::Oid> = parent.into_iter().collect();
    let parent_hashes: Vec<String> = parents.iter().map(|o| o.to_string()).collect();
    let message = format!("revert to {} (by {harness})", &target_id[..12]);
    let oid = commit_root(&repo, target.tree()?.id(), &parents, &message)?;
    let manifest = get_root(project_dir, &target_id).unwrap_or_default();
    persist_root(
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
    )
}

/// `fork <hash> --branch <name>`: branch ref at the root commit (no
/// worktree checkout in M2 — the report prints the materialization command).
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
        .unwrap_or_else(|| format!("fork-{}", &commit.id().to_string()[..8]));
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
        "\nRun `stateroot snap` after meaningful real-tree changes. Use `stateroot revert` for verified restoration and `stateroot fork` for divergent work. Handoff carries continuity — it does not replace lineage.\n\n",
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
}
