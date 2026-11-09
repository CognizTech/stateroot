//! Retained fork closure. Refs remain authoritative; a checkout is a local
//! locator. Private evidence is moved outside it, never put in root trees.
use super::*;
use crate::safe_io::ResourceLock;

const SCHEMA: &str = "stateroot.fork-retention.v1";

pub(super) fn operation_guard(repo: &Repository, name: &str) -> Result<ResourceLock, RootsError> {
    validate_fork_name(name)?;
    let common = Repository::open(repo.commondir())?;
    let owner = common
        .workdir()
        .ok_or_else(|| RootsError::Merge("fork operation owner unavailable".into()))?;
    ResourceLock::acquire(
        local_store::root(owner)
            .join("local/locks")
            .join(format!("fork-operation-{name}.lock")),
    )
    .map_err(|error| crate::safe_io::RefCasError::from(error).into())
}

fn record(owner: &Path, name: &str) -> Result<Value, RootsError> {
    validate_fork_name(name)?;
    let value: Value = serde_json::from_slice(&std::fs::read(fork_record_path(owner, name))?)?;
    if value
        .get("lifecycle")
        .is_some_and(|lifecycle| !lifecycle.is_null() && !lifecycle.is_object())
        || value["schema_version"] != "stateroot.fork.local.v1"
        || value["name"] != name
        || value["ref"] != format!("{FORKS_REF_PREFIX}{name}")
    {
        return Err(RootsError::Merge(
            "fork identity unavailable; checkout retained".into(),
        ));
    }
    Ok(value)
}

pub(super) fn validate_admission(
    owner: &Path,
    fork: &str,
    checkout: &Path,
) -> Result<(), RootsError> {
    let value = record(owner, fork)?;
    let actual = retained_storage_owner(checkout)?;
    let context = local_store::fork_context(checkout).ok_or_else(|| {
        RootsError::Merge("execution checkout has no supported fork context".into())
    })?;
    if !crate::path_identity::same_directory(&actual, owner)? || context.fork != fork {
        return Err(RootsError::Merge(
            "execution fork/locator no longer matches its owner registration".into(),
        ));
    }
    // Registration/Git/project identity above establishes a resident checkout.
    // Legacy or partial lifecycle annotations may have no state, just as the
    // lifecycle reader infers active from registration. An explicit state is
    // authoritative and must be the supported active string.
    let active = match value.get("lifecycle") {
        None | Some(Value::Null) => true,
        Some(Value::Object(fields)) => fields.get("state").is_none_or(|state| state == "active"),
        Some(_) => false,
    };
    if !active {
        return Err(RootsError::Merge(format!(
            "fork {fork} is not active for new execution admission; lifecycle={}; finish or recover its transition before launching work",
            value["lifecycle"]
        )));
    }
    Ok(())
}

fn retention_dir(owner: &Path, name: &str, id: &str) -> Result<PathBuf, RootsError> {
    validate_fork_name(name)?;
    uuid::Uuid::parse_str(id)
        .map_err(|_| RootsError::Merge("invalid fork closure identity".into()))?;
    Ok(local_store::root(owner)
        .join("local/retained-forks")
        .join(name)
        .join(id))
}

// A snapshot cannot contain its own generated manifest/transition. Retain
// those immutable facts with the common Git objects before retiring its cwd.
fn retain_root_fact(owner: &Path, relative: &str, value: &Value) -> Result<(), RootsError> {
    use std::io::Write;
    let path = local_store::root(owner).join(relative);
    if path.exists() {
        let existing: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        if existing != *value {
            return Err(RootsError::Merge(
                "retained root metadata conflict; checkout retained".into(),
            ));
        }
        return Ok(());
    }
    let parent = path.parent().unwrap();
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(format!("{}\n", serde_json::to_string_pretty(value)?).as_bytes())?;
    file.as_file().sync_all()?;
    file.persist_noclobber(&path).map_err(|error| error.error)?;
    local_store::report_written(owner, relative);
    Ok(())
}

fn closure(
    owner: &Path,
    name: &str,
    value: &Value,
    verify_local: bool,
) -> Result<(PathBuf, Value), RootsError> {
    let id = value["lifecycle"]["closure"]
        .as_str()
        .ok_or_else(|| RootsError::Merge("fork has no retained closure".into()))?;
    let dir = retention_dir(owner, name, id)?;
    let manifest: Value = serde_json::from_slice(&std::fs::read(dir.join("closure.json"))?)?;
    if manifest["schema"] != SCHEMA
        || manifest["fork"] != name
        || manifest["id"] != id
        || manifest["project_id"]
            != local_store::read_manifest(owner)?.unwrap_or_default()["project_id"]
    {
        return Err(RootsError::Merge(
            "retained closure project/fork identity mismatch".into(),
        ));
    }
    let root = manifest["root"]
        .as_str()
        .ok_or_else(|| RootsError::Merge("closure root unavailable".into()))?;
    get_root(owner, root)?;
    if verify_local && manifest["phase"] == "retained" {
        for entry in manifest["local_files"]
            .as_array()
            .ok_or_else(|| RootsError::Merge("retained local inventory unavailable".into()))?
        {
            let relative = Path::new(
                entry["path"]
                    .as_str()
                    .ok_or_else(|| RootsError::Merge("retained local path unavailable".into()))?,
            );
            if relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err(RootsError::Merge(
                    "retained local path is not relative".into(),
                ));
            }
            let path = dir.join("local").join(relative);
            if let Some(expected) = entry["sha256"].as_str() {
                if crate::verification::output_digest(&path).map_err(RootsError::Merge)? != expected
                {
                    return Err(RootsError::Merge(
                        "retained local evidence digest mismatch".into(),
                    ));
                }
            } else if std::fs::read_link(path)?.to_string_lossy()
                != entry["symlink"].as_str().unwrap_or_default()
            {
                return Err(RootsError::Merge("retained local link changed".into()));
            }
        }
    }
    Ok((dir, manifest))
}

pub fn fork_lifecycle(owner: &Path, name: &str) -> Result<Value, RootsError> {
    let value = record(owner, name)?;
    let root = ensure_repo(owner)?
        .refname_to_id(&format!("{FORKS_REF_PREFIX}{name}"))?
        .to_string();
    let retained = if value["lifecycle"]["closure"].is_string() {
        let (dir, manifest) = closure(owner, name, &value, false)?;
        json!({"directory":dir,"manifest":manifest,"payload_status":"not revalidated by read-only inspection"})
    } else {
        Value::Null
    };
    Ok(
        json!({"schema_version":"stateroot.fork-lifecycle.v1","fork":name,"root":root,
        "state":value["lifecycle"]["state"].as_str().unwrap_or(if registered_worktree_path(owner,name).is_some(){"active"}else{"retained"}),
        "plan":value["plan"],"worktree":registered_worktree_path(owner,name),"retention":retained}),
    )
}

fn worker_idle(
    owner: &Path,
    checkout: &Path,
    name: &str,
    locator: &Path,
) -> Result<(), RootsError> {
    for directory in [owner, checkout] {
        for entry in crate::delegation_status::read_entries(directory) {
            match entry {
                crate::delegation_status::DelegationEntry::Degraded { reason, .. } => {
                    return Err(RootsError::Merge(format!(
                        "worker ownership unavailable ({reason}); checkout retained"
                    )))
                }
                crate::delegation_status::DelegationEntry::Record(_, value) => {
                    let matches = value["fork_id"] == name
                        || value["worktree"].as_str().is_some_and(|path| {
                            let a = crate::path_identity::normalize_host_path(path);
                            let b = crate::path_identity::normalize_host_path(
                                &locator.to_string_lossy(),
                            );
                            if a.as_bytes().get(1) == Some(&b':') {
                                a.eq_ignore_ascii_case(&b)
                            } else {
                                a == b
                            }
                        });
                    if !matches {
                        continue;
                    }
                    let status = crate::delegation_status::interpret(&value);
                    if status.is_running()
                        || status.is_unknown()
                        || status.as_str() == "cancelling"
                        || (status.as_str() == "cancelled_with_root"
                            && value["cancel_confirmed"] != true)
                    {
                        return Err(RootsError::Merge(format!(
                            "fork worker is {}; checkout retained",
                            status.as_str()
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

fn idle(owner: &Path, checkout: &Path, name: &str, locator: &Path) -> Result<(), RootsError> {
    worker_idle(owner, checkout, name, locator)?;
    crate::build_storage::ensure_native_checkout_idle(checkout, locator)
        .map_err(RootsError::Merge)?;
    crate::verification::ensure_no_active_commands(checkout).map_err(RootsError::Merge)?;
    if !crate::finalize_journal::invalid_records(checkout).is_empty()
        || crate::finalize_journal::load_all(checkout)
            .iter()
            .any(|job| job.state != "terminal")
    {
        return Err(RootsError::Merge(
            "durable finalization remains unresolved; checkout retained".into(),
        ));
    }
    let health = crate::observations::health(checkout);
    if !health.pending_seal.is_empty()
        || !health.corrupt.is_empty()
        || !health.read_failures.is_empty()
        || !health.missing_frontier.is_empty()
    {
        return Err(RootsError::Merge(
            "capture frontier is unsealed or unavailable; checkout retained".into(),
        ));
    }
    Ok(())
}

fn local_inventory(dir: &Path) -> Result<Vec<Value>, RootsError> {
    fn walk(root: &Path, directory: &Path, output: &mut Vec<Value>) -> Result<(), RootsError> {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            let kind = std::fs::symlink_metadata(&path)?.file_type();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| RootsError::Merge("local evidence escaped its directory".into()))?;
            if kind.is_dir() {
                walk(root, &path, output)?;
            } else if kind.is_file() {
                output.push(json!({"path":relative.to_string_lossy().replace('\\',"/"),"sha256":crate::verification::output_digest(&path).map_err(RootsError::Merge)?,"bytes":path.metadata()?.len()}));
            } else if kind.is_symlink() {
                output.push(json!({"path":relative.to_string_lossy().replace('\\',"/"),"symlink":std::fs::read_link(&path)?}));
            } else {
                return Err(RootsError::Merge(format!(
                    "local evidence {} has unknown live ownership; checkout retained",
                    relative.display()
                )));
            }
        }
        Ok(())
    }
    let mut output = Vec::new();
    if dir.exists() {
        walk(dir, dir, &mut output)?;
    }
    output.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok(output)
}

fn state_fingerprint(repo: &Repository, checkout: &Path) -> Result<git2::Oid, RootsError> {
    let tree = repo.find_tree(build_tree(repo, checkout)?.tree)?;
    retained_fingerprint(repo, &tree)
}

fn retained_fingerprint(repo: &Repository, tree: &git2::Tree<'_>) -> Result<git2::Oid, RootsError> {
    let mut index = git2::Index::new()?;
    index.read_tree(tree)?;
    let mut generated = Vec::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |directory, entry| {
        if entry.kind() == Some(git2::ObjectType::Blob) {
            let path = format!("{directory}{}", entry.name().unwrap_or_default());
            if path.starts_with(".stateroot/roots/")
                || path.starts_with(".stateroot/transitions/")
                || (path == crate::fidelity::MANIFEST_PATH
                    && crate::fidelity::generated_declaration(repo, entry.id()))
            {
                generated.push(path);
            }
        }
        git2::TreeWalkResult::Ok
    })?;
    for path in generated {
        index.remove_path(Path::new(&path))?;
    }
    Ok(index.write_tree_to(repo)?)
}

type CaptureLocks = std::collections::BTreeMap<String, String>;

fn held_capture_file(checkout: &Path, relative: &str, locks: &CaptureLocks) -> bool {
    locks.get(relative).is_some_and(|expected| {
        let path = checkout.join(relative);
        std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_file())
            && crate::verification::output_digest(&path).ok().as_ref() == Some(expected)
    })
}

fn no_unretained_project_bytes(checkout: &Path, locks: &CaptureLocks) -> Result<(), RootsError> {
    let repo = Repository::open(checkout)?;
    let rules = IgnoreRules::load(checkout);
    let mut options = git2::StatusOptions::new();
    options
        .include_ignored(true)
        .include_untracked(true)
        .recurse_ignored_dirs(true)
        .recurse_untracked_dirs(true);
    for status in repo.statuses(Some(&mut options))?.iter() {
        let path = status.path().ok_or_else(|| {
            RootsError::Merge("unreadable working-tree status; checkout retained".into())
        })?;
        if status.status().intersects(
            git2::Status::INDEX_NEW
                | git2::Status::INDEX_MODIFIED
                | git2::Status::INDEX_DELETED
                | git2::Status::INDEX_RENAMED
                | git2::Status::INDEX_TYPECHANGE,
        ) {
            return Err(RootsError::Merge(
                "fork index has independently staged work; checkout retained".into(),
            ));
        }
        if status.status().is_ignored()
            && rules.is_ignored(path, false)
            && !path.starts_with(".stateroot/local/")
            && path != ".stateroot/local"
            && !held_capture_file(checkout, path, locks)
        {
            return Err(RootsError::Merge(format!(
                "ignored output {path} has no disposable-owner declaration; checkout retained"
            )));
        }
    }
    // Git UNTRACKED and Git IGNORED are different from StateRoot's own
    // exclusions. Check the same physical boundary used after staging before
    // publishing a closing transition, including unchanged tracked exclusions.
    no_unretained_project_bytes_at(&repo, checkout, locks)
}

fn acquire_capture_guard(
    checkout: &Path,
    path: PathBuf,
    locks: &mut CaptureLocks,
) -> Result<ResourceLock, RootsError> {
    use sha2::Digest;
    let guard = ResourceLock::acquire(&path).map_err(crate::safe_io::RefCasError::from)?;
    let relative = path
        .strip_prefix(checkout)
        .map_err(|_| RootsError::Merge("capture lock escaped checkout".into()))?
        .to_string_lossy()
        .replace('\\', "/");
    locks.insert(
        format!("{relative}.guard"),
        format!("{:x}", sha2::Sha256::digest(b"")),
    );
    locks.insert(
        relative,
        crate::verification::output_digest(&path).map_err(RootsError::Merge)?,
    );
    Ok(guard)
}

fn close_guards(checkout: &Path) -> Result<(Vec<ResourceLock>, CaptureLocks), RootsError> {
    let mut capture = CaptureLocks::new();
    let mut guards = vec![
        ResourceLock::acquire(Repository::open(checkout)?.path().join("index.lock"))
            .map_err(crate::safe_io::RefCasError::from)?,
        ResourceLock::acquire(Repository::open(checkout)?.path().join("HEAD.lock"))
            .map_err(crate::safe_io::RefCasError::from)?,
        ResourceLock::acquire(local_store::root(checkout).join(local_store::FINALIZE_LOCK_PATH))
            .map_err(crate::safe_io::RefCasError::from)?,
        ResourceLock::acquire(
            local_store::root(checkout)
                .join(crate::finalize_journal::JOURNAL_DIR)
                .join("enqueue.lock"),
        )
        .map_err(crate::safe_io::RefCasError::from)?,
        crate::learnings::write_guard(checkout, checkout, "project")?,
        crate::plans::write_guard(checkout).map_err(RootsError::Merge)?,
        crate::hot_apex::write_guard(checkout)?,
        crate::wiki::write_guard(checkout)?,
        local_store::episodic_guard(checkout)?,
    ];
    for name in ["handoff-write.lock", "handoff-current.lock"] {
        guards.push(
            ResourceLock::acquire(local_store::root(checkout).join("local/locks").join(name))
                .map_err(crate::safe_io::RefCasError::from)?,
        );
    }
    for segment in crate::observations::list_segments(checkout) {
        guards.push(acquire_capture_guard(
            checkout,
            segment.path.with_extension("lock"),
            &mut capture,
        )?);
    }
    guards.push(acquire_capture_guard(
        checkout,
        local_store::root(checkout).join("spool/current.lock"),
        &mut capture,
    )?);
    guards.push(crate::rules::write_guard(checkout)?);
    guards.push(crate::skill_federation::write_guard(checkout).map_err(RootsError::Merge)?);
    guards.push(crate::mcp_federation::write_guard(checkout).map_err(RootsError::Merge)?);
    Ok((guards, capture))
}

fn finalize_retirement(
    repo: &Repository,
    owner: &Path,
    name: &str,
    entry: &Value,
    dir: &Path,
    manifest: &Value,
    guard: ResourceLock,
) -> Result<ForkCleanup, RootsError> {
    let root = manifest["root"]
        .as_str()
        .ok_or_else(|| RootsError::Merge("closure root unavailable".into()))?;
    if repo
        .refname_to_id(&format!("{FORKS_REF_PREFIX}{name}"))?
        .to_string()
        != root
    {
        return Err(RootsError::Merge(
            "fork changed after closure; retained data requires reconciliation".into(),
        ));
    }
    if let Some(path) = entry["path"]
        .as_str()
        .map(PathBuf::from)
        .filter(|path| path.exists())
    {
        let current =
            Repository::open(&path).and_then(|repo| git2::Worktree::open_from_repository(&repo));
        if current.as_ref().ok().and_then(|worktree| worktree.name()) == Some(name)
            || current.is_err()
        {
            return Err(RootsError::Merge(
                "old checkout locator has new or unresolved writes; retained without deletion"
                    .into(),
            ));
        }
    }
    let _guard = remove_worktree_bounded(&dir.join("checkout"), guard)?;
    if let Ok(worktree) = repo.find_worktree(name) {
        let mut options = git2::WorktreePruneOptions::new();
        options.valid(true);
        worktree.prune(Some(&mut options))?;
    }
    if entry.is_null() {
        if read_registry(owner)["entries"].get(name).is_some() {
            return Err(RootsError::Merge(
                "fork acquired a new locator while finishing closure; retained".into(),
            ));
        }
    } else {
        unregister_worktree(owner, name, Some(entry))?;
    }
    update_fork_record(owner, name, |value| {
        value["lifecycle"] = json!({"state":manifest["kind"],"closure":manifest["id"],"root":root,"merged_tip":manifest["merged_tip"]});
        if let Some(object) = value.as_object_mut() {
            object.remove("cleanup_pending");
        }
    })?;
    Ok(ForkCleanup {
        name: name.into(),
        cleaned: true,
        pending: None,
    })
}

fn retire(owner: &Path, name: &str, park: bool) -> Result<ForkCleanup, RootsError> {
    lineage_refname_checked(owner)?;
    let _operation = operation_guard(&ensure_repo(owner)?, name)?;
    retire_locked(owner, name, park, None)
}

fn retire_locked(
    owner: &Path,
    name: &str,
    park: bool,
    terminal_tip: Option<git2::Oid>,
) -> Result<ForkCleanup, RootsError> {
    lineage_refname_checked(owner)?;
    if local_store::fork_context(owner).is_some() {
        return Err(RootsError::Merge(
            "retire a fork from its owning project, not a disposable checkout".into(),
        ));
    }
    let repo = ensure_repo(owner)?;
    let previous = record(owner, name)?;
    let entry = read_registry(owner)["entries"][name].clone();
    let Some(raw_path) = entry["path"].as_str().map(PathBuf::from) else {
        if previous["lifecycle"]["state"] == "closing" {
            let (dir, manifest) = closure(owner, name, &previous, true)?;
            if manifest["phase"] == "retained" {
                let path =
                    locator(Path::new(manifest["original_path"].as_str().ok_or_else(
                        || RootsError::Merge("closure locator unavailable".into()),
                    )?))?;
                let guard = fork_worktree_path_guard(&repo, &path)?;
                return finalize_retirement(
                    &repo,
                    owner,
                    name,
                    &Value::Null,
                    &dir,
                    &manifest,
                    guard,
                );
            }
            return Err(RootsError::Merge(format!(
                "incomplete closure locator remains retained at {}",
                dir.display()
            )));
        }
        if repo.find_worktree(name).is_ok() {
            return Err(RootsError::Merge(
                "Git worktree exists without a verified local locator; retained".into(),
            ));
        }
        let tip = repo.refname_to_id(&format!("{FORKS_REF_PREFIX}{name}"))?;
        if !park {
            let integrated = previous["lifecycle"]["merged_tip"]
                .as_str()
                .filter(|_| previous["lifecycle"]["root"] == tip.to_string())
                .and_then(|id| git2::Oid::from_str(id).ok())
                .unwrap_or(tip);
            let main = repo.refname_to_id(LATEST_REF)?;
            if main != integrated && !repo.graph_descendant_of(main, integrated)? {
                return Err(RootsError::Merge(
                    "retained fork is unmerged; use explicit park for unfinished work".into(),
                ));
            }
            update_fork_record(owner, name, |value| {
                value["lifecycle"]["state"] = json!("retired");
                value["lifecycle"]["root"] = json!(tip.to_string());
                value["lifecycle"]["merged_tip"] = json!(integrated.to_string());
            })?;
        }
        clear_cleanup_pending(owner, name)?;
        return Ok(ForkCleanup {
            name: name.into(),
            cleaned: true,
            pending: None,
        });
    };
    let path = locator(&raw_path)?;
    #[cfg(test)]
    if let Some(observer) = TEST_BEFORE_CLEANUP_PATH_LOCK.with(|slot| slot.borrow_mut().take()) {
        observer(owner);
    }
    let guard = fork_worktree_path_guard(&repo, &path)?;
    if read_registry(owner)["entries"].get(name) != Some(&entry) {
        return Err(RootsError::Merge(
            "fork worktree registration changed before closure; retained".into(),
        ));
    }
    if previous["lifecycle"]["state"] == "closing" {
        let (dir, manifest) = closure(owner, name, &previous, true)?;
        if manifest["phase"] == "retained" {
            return finalize_retirement(&repo, owner, name, &entry, &dir, &manifest, guard);
        }
        if !path.exists() && dir.join("checkout").exists() {
            if dir.join("local").exists() {
                std::fs::rename(dir.join("local"), dir.join("checkout/.stateroot/local"))?;
            }
            std::fs::rename(dir.join("checkout"), &path)?;
        }
    }
    if !path.exists() {
        worker_idle(owner, &path, name, &path)?;
        let tip = repo.refname_to_id(&format!("{FORKS_REF_PREFIX}{name}"))?;
        let main = repo.refname_to_id(LATEST_REF)?;
        if !park && main != tip && !repo.graph_descendant_of(main, tip)? {
            return Err(RootsError::Merge(
                "missing fork is unmerged; retained source remains available".into(),
            ));
        }
        let administration = repo.commondir().join("worktrees").join(name);
        if administration.exists() {
            let expected = previous["lifecycle"]["reopened_from"]
                .as_str()
                .or(previous["root"].as_str())
                .ok_or_else(|| RootsError::Merge("fork materialization root unavailable".into()))?;
            let expected = git2::Oid::from_str(expected)?;
            let head = std::fs::read_to_string(administration.join("HEAD"))?;
            if git2::Oid::from_str(head.trim()).ok() != Some(expected) {
                return Err(RootsError::Merge("missing checkout has independently moved Git HEAD; registration/evidence retained".into()));
            }
            let mut index = git2::Index::open(&administration.join("index"))?;
            let origin = repo.find_commit(expected)?;
            if index.write_tree_to(&repo)? != origin.tree_id() {
                return Err(RootsError::Merge(
                    "missing checkout has independently staged Git evidence; registration retained"
                        .into(),
                ));
            }
        }
        let id = uuid::Uuid::now_v7().to_string();
        let dir = retention_dir(owner, name, &id)?;
        std::fs::create_dir_all(&dir)?;
        let manifest = json!({"schema":SCHEMA,"id":id,"fork":name,"root":tip.to_string(),"kind":if park{"parked"}else{"retired"},"merged_tip":if park{Value::Null}else{json!(tip.to_string())},
            "project_id":local_store::read_manifest(owner)?.unwrap_or_default()["project_id"],"original_path":path,"plan":previous["plan"],"phase":"retained","local_files":[],"coverage":"source_only",
            "fidelity":"checkout was already missing; retained root remains inspectable, unsnapped/private evidence availability is unknown", "previous_closure":previous["lifecycle"]["closure"]});
        write_json(&dir.join("closure.json"), &manifest)?;
        return finalize_retirement(&repo, owner, name, &entry, &dir, &manifest, guard);
    }
    if !crate::path_identity::same_directory(&retained_storage_owner(&path)?, owner)? {
        return Err(RootsError::Merge(
            "fork storage ownership changed; retained".into(),
        ));
    }
    let outputs =
        crate::build_storage::checkout_disposition(owner, &path).map_err(RootsError::Merge)?;
    if outputs.blocked {
        return Err(RootsError::Merge("execution output producer or consumer ownership is active/unverifiable; checkout retained".into()));
    }
    let context = local_store::fork_context(&path)
        .ok_or_else(|| RootsError::Merge("fork context unavailable".into()))?;
    let checkout = Repository::open(&path)?;
    if !checkout.head_detached()?
        || checkout.head()?.target().map(|id| id.to_string()) != Some(context.parent_root.clone())
    {
        return Err(RootsError::Merge(
            "fork HEAD has independently checked-out work; retained".into(),
        ));
    }
    for other in repo.worktrees()?.iter().flatten() {
        if other != name && repo.find_worktree(other)?.path().starts_with(&path) {
            return Err(RootsError::Merge(
                "fork contains another worktree; retain it before retirement".into(),
            ));
        }
    }
    idle(owner, &path, name, &path)?;
    #[cfg(test)]
    if let Some(observer) = TEST_BEFORE_FORK_CLOSE_GUARDS.with(|slot| slot.borrow_mut().take()) {
        observer(&path);
    }
    let (_writers, capture_locks) = close_guards(&path)?;
    if !checkout.head_detached()?
        || checkout.head()?.target().map(|id| id.to_string()) != Some(context.parent_root)
    {
        return Err(RootsError::Merge(
            "fork HEAD changed before close ownership; checkout retained".into(),
        ));
    }
    no_unretained_project_bytes(&path, &capture_locks)?;
    idle(owner, &path, name, &path)?;
    let tip = repo.refname_to_id(&format!("{FORKS_REF_PREFIX}{name}"))?;
    // Automatic terminal closure must still match its captured source after
    // all cooperating writers have released ownership. Manual parking has no
    // expected terminal tip and intentionally seals unfinished current work.
    if let Some(expected) = terminal_tip {
        if tip != expected
            || state_fingerprint(&repo, &path)?
                != retained_fingerprint(&repo, &repo.find_commit(expected)?.tree()?)?
        {
            return Err(RootsError::Merge(
                "source or intelligence changed after terminal capture; checkout retained".into(),
            ));
        }
    }
    let integrated = previous["lifecycle"]["merged_tip"]
        .as_str()
        .filter(|_| previous["lifecycle"]["root"] == tip.to_string())
        .and_then(|id| git2::Oid::from_str(id).ok())
        .unwrap_or(tip);
    if !park {
        let main = repo.refname_to_id(LATEST_REF)?;
        if main != integrated && !repo.graph_descendant_of(main, integrated)? {
            return Err(RootsError::Merge(
                "fork is not merged into the current trunk; use explicit park for unfinished work"
                    .into(),
            ));
        }
        if project_files_changed(&repo, tip, build_tree(&repo, &path)?.tree)? {
            return Err(RootsError::Merge(
                "fork has newer unmerged project bytes; checkout retained".into(),
            ));
        }
    }
    let baseline = repo
        .find_commit(git2::Oid::from_str(previous["root"].as_str().ok_or_else(
            || RootsError::Merge("fork origin unavailable".into()),
        )?)?)?
        .tree()?;
    let verification =
        crate::verification::retain_fork_outputs(&path, &baseline).map_err(RootsError::Merge)?;
    let before = state_fingerprint(&repo, &path)?;
    let (sealed, transition) = create_root(&path, "cli", "seal retained fork closure", None)?;
    retain_root_fact(
        owner,
        &format!("roots/{}.json", sealed.id),
        &serde_json::to_value(&sealed)?,
    )?;
    retain_root_fact(
        owner,
        &format!("transitions/{}.json", transition.id),
        &serde_json::to_value(&transition)?,
    )?;
    if state_fingerprint(&repo, &path)? != before {
        return Err(RootsError::Merge(
            "fork changed during sealing; checkout retained".into(),
        ));
    }
    let _lineage = ResourceLock::acquire(crate::safe_io::ref_lock_path(
        &local_store::root(&path).join("local/locks"),
        &format!("{FORKS_REF_PREFIX}{name}"),
    ))
    .map_err(crate::safe_io::RefCasError::from)?;
    if repo
        .refname_to_id(&format!("{FORKS_REF_PREFIX}{name}"))?
        .to_string()
        != sealed.id
    {
        return Err(RootsError::Merge(
            "fork tip changed after seal; checkout retained".into(),
        ));
    }
    let id = uuid::Uuid::now_v7().to_string();
    let dir = retention_dir(owner, name, &id)?;
    std::fs::create_dir_all(&dir)?;
    let mut manifest = json!({"schema":SCHEMA,"id":id,"fork":name,"root":sealed.id,"kind":if park {"parked"}else{"retired"},"merged_tip":if park{Value::Null}else{json!(integrated.to_string())},
        "project_id":local_store::read_manifest(owner)?.unwrap_or_default()["project_id"],"original_path":path,"plan":previous["plan"],"phase":"sealed","verification":verification,
        "fidelity":"eligible source and shared intelligence rooted; private local bytes retained; native cwd/provenance unchanged; unknown local runtime state not automatically replayed"});
    manifest["native_archives"] = json!(crate::sessions::codex_native::inventory(&path));
    manifest["transition"] = json!(transition.id);
    manifest["previous_closure"] = previous["lifecycle"]["closure"].clone();
    write_json(&dir.join("closure.json"), &manifest)?;
    update_fork_record(
        owner,
        name,
        |value| value["lifecycle"] = json!({"state":"closing","closure":id,"root":sealed.id,"merged_tip":manifest["merged_tip"]}),
    )?;
    let staged = dir.join("checkout");
    if let Err(error) = std::fs::rename(&path, &staged) {
        update_fork_record(owner, name, |value| {
            value["lifecycle"]["state"] = json!("active")
        })?;
        return Err(RootsError::Merge(format!(
            "cannot atomically park this locator ({error}); current checkout/evidence retained"
        )));
    }
    let retained = (|| -> Result<(), RootsError> {
        #[cfg(test)]
        if let Some(observer) = TEST_BEFORE_FORK_RETIREMENT.with(|slot| slot.borrow_mut().take()) {
            observer(&staged);
        }
        idle(owner, &staged, name, &path)?;
        if state_fingerprint(&repo, &staged)? != before || path.exists() {
            return Err(RootsError::Merge(
                "late write during retirement; checkout and evidence retained".into(),
            ));
        }
        no_unretained_project_bytes_at(&repo, &staged, &capture_locks)?;
        let local = local_store::root(&staged).join("local");
        if local.exists() {
            std::fs::rename(local, dir.join("local"))?;
        }
        manifest["local_files"] = json!(local_inventory(&dir.join("local"))?);
        manifest["phase"] = json!("retained");
        write_json(&dir.join("closure.json"), &manifest)?;
        Ok(())
    })();
    if let Err(error) = retained {
        if !path.exists() {
            if dir.join("local").exists() {
                std::fs::rename(dir.join("local"), staged.join(".stateroot/local"))?;
            }
            std::fs::rename(&staged, &path)?;
            // The original locator and local lane are fully restored. Restore
            // the exact prior lifecycle annotation; if any rollback step fails
            // above, keep closing/closure for explicit recovery instead.
            update_fork_record(owner, name, |value| {
                if let Some(lifecycle) = previous.get("lifecycle") {
                    value["lifecycle"] = lifecycle.clone();
                } else if let Some(fields) = value.as_object_mut() {
                    fields.remove("lifecycle");
                }
            })?;
        }
        return Err(error);
    }
    finalize_retirement(&repo, owner, name, &entry, &dir, &manifest, guard).map_err(|error| {
        RootsError::Merge(format!(
            "{error}; sealed checkout/evidence retained at {}",
            dir.display()
        ))
    })
}

// The staged directory is no longer its Git administrative locator. Inspect
// ignored bytes with the same exclusion declarations without opening it as a
// new project or changing Git authority.
fn no_unretained_project_bytes_at(
    _repo: &Repository,
    checkout: &Path,
    locks: &CaptureLocks,
) -> Result<(), RootsError> {
    fn visit(
        root: &Path,
        dir: &Path,
        rules: &IgnoreRules,
        locks: &CaptureLocks,
    ) -> Result<(), RootsError> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let rel = path.strip_prefix(root).unwrap();
            if rel == Path::new(".git") || rel.starts_with(".stateroot/local") {
                continue;
            }
            let kind = std::fs::symlink_metadata(&path)?.file_type();
            let text = rel.to_string_lossy().replace('\\', "/");
            if rules.is_ignored(&text, kind.is_dir()) && !held_capture_file(root, &text, locks) {
                return Err(RootsError::Merge(format!(
                    "unowned ignored bytes {text}; checkout retained"
                )));
            }
            if kind.is_dir() {
                visit(root, &path, rules, locks)?;
            } else if !kind.is_file() {
                return Err(RootsError::Merge(
                    "unowned filesystem entry; checkout retained".into(),
                ));
            }
        }
        Ok(())
    }
    visit(checkout, checkout, &IgnoreRules::load(checkout), locks)
}

pub fn park_fork(owner: &Path, name: &str) -> Result<Value, RootsError> {
    retire(owner, name, true)?;
    fork_lifecycle(owner, name)
}

/// A retained closing operation has moved its locator; never drain another
/// checkout that may have reused that path.
pub fn retirement_checkout(owner: &Path, name: &str) -> Result<Option<PathBuf>, RootsError> {
    let value = record(owner, name)?;
    if value["lifecycle"]["state"] == "closing" {
        return Ok(None);
    }
    let Some(path) = registered_worktree_path(owner, name) else {
        return Ok(None);
    };
    let path = locator(&path)?;
    if !path.exists() {
        return Err(RootsError::Merge(
            "registered fork is missing without a retained closure".into(),
        ));
    }
    retained_storage_owner(&path)?;
    worker_idle(owner, &path, name, &path)?;
    crate::verification::ensure_no_active_commands(&path).map_err(RootsError::Merge)?;
    Ok(Some(path))
}

pub(super) fn cleanup(owner: &Path, name: &str) -> Result<ForkCleanup, RootsError> {
    let guard = operation_guard(&ensure_repo(owner)?, name);
    let _operation = match guard {
        Ok(guard) => guard,
        Err(error) => {
            return Ok(ForkCleanup {
                name: name.into(),
                cleaned: false,
                pending: Some(error.to_string()),
            })
        }
    };
    match retire_locked(owner, name, false, None) {
        Ok(result) => Ok(result),
        Err(error) => {
            let message = error.to_string();
            let _ = record_cleanup_pending(owner, name, &message);
            Ok(ForkCleanup {
                name: name.into(),
                cleaned: false,
                pending: Some(message),
            })
        }
    }
}

/// Bounded automatic closure uses the existing fork records and lifecycle.
/// Residency is never completion. A terminal worker must have published the
/// exact current tip, or the fork must already be contained in the trunk.
pub fn retirement_candidates(owner: &Path) -> Result<Vec<(String, bool)>, RootsError> {
    if local_store::fork_context(owner).is_some() || reconciliation_owner(owner)?.is_some() {
        return Ok(Vec::new());
    }
    let repo = ensure_repo(owner)?;
    let Some(main) = latest_oid_for(&repo, owner)? else {
        return Ok(Vec::new());
    };
    let workers = crate::delegation_status::read_entries(owner);
    let mut candidates = Vec::new();
    for reference in repo
        .references_glob(&format!("{FORKS_REF_PREFIX}*"))?
        .flatten()
    {
        let Some(name) = reference
            .name()
            .and_then(|name| name.strip_prefix(FORKS_REF_PREFIX))
        else {
            continue;
        };
        let value = match record(owner, name) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if registered_worktree_path(owner, name).is_none()
            && value["lifecycle"]["state"] != "closing"
        {
            continue;
        }
        let Some(tip) = reference.target() else {
            continue;
        };
        let integrated = value["lifecycle"]["merged_tip"]
            .as_str()
            .filter(|_| value["lifecycle"]["root"] == tip.to_string())
            .and_then(|id| git2::Oid::from_str(id).ok())
            .unwrap_or(tip);
        let merged = (main == integrated || repo.graph_descendant_of(main, integrated)?)
            && value["cleanup_pending"].is_string();
        let terminal = workers.iter().any(|entry| match entry {
            crate::delegation_status::DelegationEntry::Record(_, worker) => {
                worker["fork_id"] == name
                    && worker["outcome_root"] == tip.to_string()
                    && matches!(
                        worker["outcome"].as_str(),
                        Some("completed" | "failed" | "cancelled_with_root")
                    )
                    && (worker["outcome"] != "cancelled_with_root"
                        || worker["cancel_confirmed"] == true)
            }
            _ => false,
        });
        if merged || terminal || value["lifecycle"]["state"] == "closing" {
            candidates.push((
                name.to_owned(),
                !merged,
                value["lifecycle"]["last_attempt"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned(),
            ));
        }
    }
    candidates.sort_by(|a, b| (&a.2, &a.0).cmp(&(&b.2, &b.0)));
    Ok(candidates
        .into_iter()
        .map(|(name, park, _)| (name, park))
        .collect())
}

/// Revalidate under the admission guard after any boundary drain. No late
/// product or intelligence edit may be silently parked by a terminal event.
pub fn retire_eligible_fork(owner: &Path, name: &str) -> Result<ForkCleanup, RootsError> {
    let repo = ensure_repo(owner)?;
    let _operation = operation_guard(&repo, name)?;
    record(owner, name)?;
    let result = (|| {
        let park = retirement_candidates(owner)?
            .into_iter()
            .find(|(fork, _)| fork == name)
            .map(|(_, park)| park)
            .ok_or_else(|| {
                RootsError::Merge(
                    "fork has no terminal capture or merged source; checkout retained".into(),
                )
            })?;
        let mut terminal_tip = None;
        if let Some(path) = registered_worktree_path(owner, name).filter(|path| path.exists()) {
            // Terminal outcomes precede the worker's final log/receipt writes.
            // A live or foreign process is not released by that outcome.
            for entry in crate::delegation_status::read_entries(owner) {
                if let crate::delegation_status::DelegationEntry::Record(_, worker) = entry {
                    if worker["fork_id"] == name {
                        let pid = worker["pid"]
                            .as_u64()
                            .filter(|pid| *pid > 0 && *pid <= u32::MAX as u64);
                        if pid.is_none()
                            || !crate::delegation_status::owns_namespace(&worker)
                            || pid.is_some_and(|pid| crate::safe_io::pid_alive(pid as u32))
                        {
                            return Err(RootsError::Merge(
                                "worker process release is active or unverified; checkout retained"
                                    .into(),
                            ));
                        }
                    }
                }
            }
            let tip = repo.refname_to_id(&format!("{FORKS_REF_PREFIX}{name}"))?;
            if park
                && state_fingerprint(&repo, &path)?
                    != retained_fingerprint(&repo, &repo.find_commit(tip)?.tree()?)?
            {
                return Err(RootsError::Merge(
                    "source or intelligence changed after terminal capture; checkout retained"
                        .into(),
                ));
            }
            if park {
                terminal_tip = Some(tip);
            }
        }
        retire_locked(owner, name, park, terminal_tip)
    })();
    let pending = result.as_ref().err().map(ToString::to_string);
    update_fork_record(owner, name, |value| {
        value["lifecycle"]["last_attempt"] = json!(local_store::now_rfc3339());
        if let Some(reason) = &pending {
            value["cleanup_pending"] = json!(reason);
        }
    })?;
    match result {
        Ok(result) => Ok(result),
        Err(_) => Ok(ForkCleanup {
            name: name.into(),
            cleaned: false,
            pending,
        }),
    }
}

/// One bounded maintenance pass; failure preserves the reason on its existing
/// fork record and never turns an originating mutation into a failed command.
pub fn reconcile_retirements(owner: &Path, limit: usize) -> Result<Vec<ForkCleanup>, RootsError> {
    retirement_candidates(owner)?
        .into_iter()
        .take(limit.min(4))
        .map(|(name, _)| retire_eligible_fork(owner, &name))
        .collect()
}

/// Existing status/doctor consumers share these read-only facts. This does
/// not materialize historical work or revalidate every retained evidence byte.
pub fn storage_lifecycle_report(project: &Path) -> Result<Value, RootsError> {
    let owner = retained_storage_owner(project)?;
    let repo = ensure_repo(&owner)?;
    let mut forks = Vec::new();
    for reference in repo
        .references_glob(&format!("{FORKS_REF_PREFIX}*"))?
        .flatten()
    {
        if forks.len() >= 4096 {
            return Err(RootsError::Merge(
                "fork diagnostic budget exceeded; history retained".into(),
            ));
        }
        let Some(name) = reference
            .name()
            .and_then(|name| name.strip_prefix(FORKS_REF_PREFIX))
        else {
            continue;
        };
        let detail = match fork_lifecycle(&owner, name) {
            Ok(mut detail) => {
                detail["retained_reason"] = record(&owner, name)?["cleanup_pending"].clone();
                detail["reopen"] = json!(format!("stateroot fork --reopen {name}"));
                detail["history"] = json!(format!(
                    "stateroot show {}",
                    detail["root"].as_str().unwrap_or_default()
                ));
                if let Some(path) = detail["worktree"].as_str().map(PathBuf::from) {
                    detail["resident"] = json!(path.is_dir());
                    if path.join("target").exists() {
                        detail["legacy_outputs"] = json!({"path":path.join("target"),"state":"unmanaged", "reason":"legacy Cargo output requires inspected inventory and disposal judgment before migration; required or unknown bytes stay resident"});
                    }
                    detail["outputs"] =
                        match crate::build_storage::checkout_disposition(&owner, &path) {
                            Ok(outputs) => serde_json::to_value(outputs)?,
                            Err(reason) => json!({"blocked":true,"reason":reason}),
                        };
                } else {
                    detail["resident"] = json!(false);
                }
                detail
            }
            Err(error) => {
                json!({"fork":name,"state":"unknown","retained_reason":error.to_string()})
            }
        };
        forks.push(detail);
    }
    let storage = match crate::build_storage::inspect(&owner) {
        Ok(report) => serde_json::to_value(report)?,
        Err(reason) => json!({"state":"unknown","reason":reason}),
    };
    Ok(
        json!({"forks":forks,"rebuildable_outputs":storage,"owner":owner,
        "limits":"original native cwd remains historical; explicit native configuration and unknown ignored files are preserved; Linux supplemental process scans cover current-user readable native processes, while durable ownership records retain foreign/unknown owners; reopen validates the stable fork identity"}),
    )
}

pub fn reopen_fork(
    owner: &Path,
    name: &str,
    destination: Option<&Path>,
    plan: Option<&str>,
) -> Result<Value, RootsError> {
    lineage_refname_checked(owner)?;
    let _operation = operation_guard(&ensure_repo(owner)?, name)?;
    if local_store::fork_context(owner).is_some() {
        return Err(RootsError::Merge(
            "reopen from the owning project, not a disposable checkout".into(),
        ));
    }
    let value = record(owner, name)?;
    let original = value["plan"].as_str();
    if plan.is_some() && plan != original {
        return Err(RootsError::Merge(
            "reopen preserves the original plan binding; use a new fork for a new assignment"
                .into(),
        ));
    }
    let mut existing = None;
    if let Some(path) = registered_worktree_path(owner, name) {
        let path = locator(&path)?;
        if path.exists() {
            retained_storage_owner(&path)?;
            if value["lifecycle"]["state"] == "opening" {
                existing = Some(path);
            } else if value["lifecycle"]["state"] == "closing" {
                let (_, manifest) = closure(owner, name, &value, true)?;
                if manifest["phase"] != "sealed" {
                    return Err(RootsError::Merge(
                        "closing fork has unresolved live ownership; checkout retained".into(),
                    ));
                }
                update_fork_record(owner, name, |value| {
                    value["lifecycle"]["state"] = json!("active")
                })?;
                return fork_lifecycle(owner, name);
            } else {
                if destination
                    .is_some_and(|requested| locator(requested).ok().as_ref() != Some(&path))
                {
                    return Err(RootsError::Merge(
                        "fork is already active at another locator; park it before relocation"
                            .into(),
                    ));
                }
                return fork_lifecycle(owner, name);
            }
        } else {
            retire_locked(
                owner,
                name,
                value["lifecycle"]["merged_tip"].is_null(),
                None,
            )?;
        }
    }
    let current = record(owner, name)?;
    let retained = if current["lifecycle"]["closure"].is_string() {
        Some(closure(owner, name, &current, true)?)
    } else {
        None
    };
    let target = match destination {
        Some(path) => path.to_path_buf(),
        None => retained
            .as_ref()
            .and_then(|(_, manifest)| manifest["original_path"].as_str())
            .map(PathBuf::from)
            .unwrap_or(managed_worktree_path(owner, name)?),
    };
    let path = match existing {
        Some(path) => {
            if destination.is_some_and(|requested| locator(requested).ok().as_ref() != Some(&path))
            {
                return Err(RootsError::Merge(
                    "partial reopen has a different locator; finish recovery before relocation"
                        .into(),
                ));
            }
            if let Some(plan) = original {
                claim_plan(owner, plan, name, &uuid::Uuid::now_v7().to_string(), true)?;
                copy_plan_to_worktree(owner, &path, plan, true)?;
            }
            path
        }
        None => {
            let path = locator(&target)?;
            update_fork_record(owner, name, |value| {
                value["lifecycle"]["state"] = json!("opening")
            })?;
            materialize_fork(owner, name, &path, original, true)?;
            path
        }
    };
    if let Some((dir, manifest)) = retained {
        for subtree in ["sessions", "captured-tool-output"] {
            let source = dir.join("local").join(subtree);
            if source.exists() {
                copy_evidence_tree(
                    &source,
                    &local_store::root(&path).join("local").join(subtree),
                )?;
            }
        }
        let repo = ensure_repo(owner)?;
        let before = repo.refname_to_id(&format!("{FORKS_REF_PREFIX}{name}"))?;
        if project_files_changed(&repo, before, build_tree(&repo, &path)?.tree)? {
            create_root(&path, "cli", "reopen with current exclusions", None)?;
        }
        update_fork_record(owner, name, |value| {
            value["lifecycle"]["state"] = json!("active");
            value["lifecycle"]["reopened_from"] = manifest["root"].clone();
        })?;
    } else {
        update_fork_record(owner, name, |value| {
            value["lifecycle"]["state"] = json!("active")
        })?;
    }
    fork_lifecycle(owner, name)
}

fn locator(raw: &Path) -> Result<PathBuf, RootsError> {
    if let Some(path) = crate::path_identity::resolve_existing_dir(raw) {
        return Ok(path);
    }
    let normalized = crate::path_identity::normalize_host_path(&raw.to_string_lossy());
    for path in [raw.to_path_buf(), PathBuf::from(&normalized)] {
        if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
            if let Some(parent) = crate::path_identity::resolve_existing_dir(parent) {
                return Ok(parent.join(name));
            }
        }
    }
    #[cfg(not(windows))]
    if normalized.as_bytes().get(1) == Some(&b':') {
        return resolved_worktree_path(&PathBuf::from(format!(
            "/mnt/{}{}",
            &normalized[..1],
            &normalized[2..]
        )));
    }
    resolved_worktree_path(raw)
}

fn copy_evidence_tree(source: &Path, target: &Path) -> Result<(), RootsError> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let path = entry?.path();
        let destination = target.join(path.file_name().unwrap());
        let kind = std::fs::symlink_metadata(&path)?.file_type();
        if kind.is_dir() {
            copy_evidence_tree(&path, &destination)?;
        } else if kind.is_file() {
            if destination.exists() {
                if crate::verification::output_digest(&path).map_err(RootsError::Merge)?
                    != crate::verification::output_digest(&destination)
                        .map_err(RootsError::Merge)?
                {
                    return Err(RootsError::Merge(
                        "retained local evidence conflict; original remains retained".into(),
                    ));
                }
            } else {
                std::fs::copy(path, destination)?;
            }
        } else {
            return Err(RootsError::Merge(
                "retained evidence has unsupported filesystem ownership; original retained".into(),
            ));
        }
    }
    Ok(())
}

pub(super) fn apply_current_exclusions(
    owner: &Path,
    checkout: &Path,
    repo: &Repository,
    tip: git2::Oid,
) -> Result<(), RootsError> {
    let rules = IgnoreRules::load(owner);
    let tree = repo.find_commit(tip)?.tree()?;
    let mut excluded = Vec::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |directory, entry| {
        if entry.kind() == Some(git2::ObjectType::Blob) {
            let path = format!("{directory}{}", entry.name().unwrap_or_default());
            if !path.starts_with(".stateroot/") && rules.is_ignored(&path, false) {
                excluded.push(path);
            }
        }
        git2::TreeWalkResult::Ok
    })?;
    for path in excluded {
        let path = checkout.join(path);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    for declaration in [".gitignore", ".staterootignore", ".staterootcaptureignore"] {
        match std::fs::read(owner.join(declaration)) {
            Ok(bytes) => crate::safe_io::atomic_replace(&checkout.join(declaration), &bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if checkout.join(declaration).exists() {
                    std::fs::remove_file(checkout.join(declaration))?;
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let owner = temp.path().join("project");
        std::fs::create_dir_all(&owner).unwrap();
        local_store::init_skeleton(&owner, "retirement-fixture", "Fixture", "default").unwrap();
        std::fs::write(owner.join("work.txt"), "base").unwrap();
        let (base, _) = create_root(&owner, "cli", "base", None).unwrap();
        fork_root(&owner, &base.id, Some("worker"), "cli").unwrap();
        let path = managed_worktree_path(&owner, "worker").unwrap();
        fork_materialize(&owner, "worker", &path, None).unwrap();
        (temp, owner, path)
    }
    fn terminal(owner: &Path, path: &Path) -> String {
        std::fs::write(path.join("work.txt"), "completed output").unwrap();
        let (root, _) = create_root(path, "cli", "outcome", None).unwrap();
        // 1_000_000_000u32 cannot be assigned by either qualified native runtime.
        write_json(&local_store::root(owner).join("delegations/worker.json"),&json!({
            "schema_version":crate::delegation_status::SCHEMA_V2,"id":"worker","fork_id":"worker",
            "outcome":"completed","outcome_root":root.id,"attempt":1,"pid":1_000_000_000u32,
            "process_namespace":crate::delegation_status::process_namespace()
        })).unwrap();
        root.id
    }
    #[test]
    fn automatic_retirement_preserves_unfinished_residency_and_human_worktrees() {
        let (_temp, owner, path) = fixture();
        assert!(
            retirement_candidates(&owner).unwrap().is_empty(),
            "fork starts at contained base, not a completed task"
        );
        assert!(
            retirement_candidates(&path).unwrap().is_empty(),
            "disposable checkout never maintains the owner"
        );
        let repo = ensure_repo(&owner).unwrap();
        repo.branch(
            "human-cloud",
            &repo
                .find_commit(repo.refname_to_id(LATEST_REF).unwrap())
                .unwrap(),
            false,
        )
        .unwrap();
        let human = owner.parent().unwrap().join("human-cloud");
        let reference = repo.find_reference("refs/heads/human-cloud").unwrap();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(&reference));
        repo.worktree("human-cloud", &human, Some(&options))
            .unwrap();
        assert!(retirement_candidates(&owner).unwrap().is_empty());
        assert!(human.exists());
        assert!(path.exists());
    }
    #[test]
    fn automatic_terminal_retirement_reopens_exact_identity_and_retains_late_edits() {
        let (_temp, owner, path) = fixture();
        let outcome = terminal(&owner, &path);
        std::fs::write(path.join("work.txt"), "late work").unwrap();
        let result = retire_eligible_fork(&owner, "worker").unwrap();
        assert!(!result.cleaned, "{result:?}");
        assert!(
            result
                .pending
                .as_deref()
                .unwrap()
                .contains("changed after terminal capture"),
            "{result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(path.join("work.txt")).unwrap(),
            "late work"
        );
        std::fs::write(path.join("work.txt"), "completed output").unwrap();
        for relative in ["work.txt", ".stateroot/memories/MEMORY.md"] {
            let original = std::fs::read(path.join(relative)).ok();
            TEST_BEFORE_FORK_CLOSE_GUARDS.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |checkout| {
                    let changed = checkout.join(relative);
                    std::fs::create_dir_all(changed.parent().unwrap()).unwrap();
                    std::fs::write(changed, "late bytes before close guards").unwrap();
                }));
            });
            let raced = retire_eligible_fork(&owner, "worker").unwrap();
            assert!(!raced.cleaned, "{relative}: {raced:?}");
            assert!(raced
                .pending
                .as_deref()
                .unwrap()
                .contains("changed after terminal capture"));
            assert_eq!(
                std::fs::read_to_string(path.join(relative)).unwrap(),
                "late bytes before close guards"
            );
            let detail = fork_lifecycle(&owner, "worker").unwrap();
            assert_eq!(detail["root"], outcome, "ref stays on the terminal capture");
            assert_eq!(detail["state"], "active");
            assert!(record(&owner, "worker").unwrap()["cleanup_pending"]
                .as_str()
                .unwrap()
                .contains("changed after terminal capture"));
            match original {
                Some(bytes) => std::fs::write(path.join(relative), bytes).unwrap(),
                None => std::fs::remove_file(path.join(relative)).unwrap(),
            }
        }
        std::fs::write(
            local_store::root(&path).join("local/evidence.txt"),
            "local-only proof",
        )
        .unwrap();
        assert!(retire_eligible_fork(&owner, "worker").unwrap().cleaned);
        assert!(!path.exists());
        let detail = fork_lifecycle(&owner, "worker").unwrap();
        assert_eq!(detail["state"], "parked");
        assert!(
            get_root(&owner, &outcome).is_ok(),
            "exact source dependency survives"
        );
        assert!(
            reconcile_retirements(&owner, 2).unwrap().is_empty(),
            "idempotent terminal event"
        );
        let reopened = reopen_fork(&owner, "worker", None, None).unwrap();
        let current = PathBuf::from(reopened["worktree"].as_str().unwrap());
        assert_eq!(local_store::fork_context(&current).unwrap().fork, "worker");
        assert_eq!(
            std::fs::read_to_string(current.join("work.txt")).unwrap(),
            "completed output"
        );
        assert_eq!(
            std::fs::read_to_string(
                PathBuf::from(detail["retention"]["directory"].as_str().unwrap())
                    .join("local/evidence.txt")
            )
            .unwrap(),
            "local-only proof"
        );
        assert!(
            retirement_candidates(&owner).unwrap().is_empty(),
            "old terminal record cannot retire a reopened fork"
        );
    }
    #[test]
    fn automatic_retirement_keeps_live_unmanaged_commands_and_unknown_workers() {
        let (_temp, owner, path) = fixture();
        terminal(&owner, &path);
        let (_, command) = crate::verification::reserve_execution(&path).unwrap();
        assert!(!retire_eligible_fork(&owner, "worker").unwrap().cleaned);
        assert!(path.exists());
        drop(command);
        #[cfg(target_os = "linux")]
        {
            let mut consumer = std::process::Command::new("sleep")
                .arg("30")
                .current_dir(&path)
                .spawn()
                .unwrap();
            assert!(
                !retire_eligible_fork(&owner, "worker").unwrap().cleaned,
                "surviving unmanaged consumer retains its checkout"
            );
            consumer.kill().unwrap();
            consumer.wait().unwrap();
        }
        let record_path = local_store::root(&owner).join("delegations/worker.json");
        let mut worker: Value =
            serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
        worker["pid"] = json!(std::process::id());
        write_json(&record_path, &worker).unwrap();
        assert!(!retire_eligible_fork(&owner, "worker").unwrap().cleaned);
        worker["pid"] = json!(1_000_000_000u32);
        worker["process_namespace"] = json!("foreign");
        write_json(&record_path, &worker).unwrap();
        assert!(!retire_eligible_fork(&owner, "worker").unwrap().cleaned);
        assert!(path.exists());
    }
    #[test]
    fn automatic_merged_retirement_is_bounded_and_reopened_history_stays_active() {
        let (_temp, owner, path) = fixture();
        std::fs::write(path.join("work.txt"), "merged output").unwrap();
        create_root(&path, "test", "outcome", None).unwrap();
        merge_forks(&owner, &["worker".into()], "test").unwrap();
        assert_eq!(retirement_candidates(&owner).unwrap().len(), 1);
        assert!(reconcile_retirements(&owner, 0).unwrap().is_empty());
        assert!(path.exists());
        assert!(reconcile_retirements(&owner, 1).unwrap()[0].cleaned);
        assert_eq!(
            fork_lifecycle(&owner, "worker").unwrap()["state"],
            "retired"
        );
        assert!(get_root(&owner, &latest_root(&owner).unwrap().unwrap()).is_ok());
        let reopened = reopen_fork(&owner, "worker", None, None).unwrap();
        assert_eq!(reopened["state"], "active");
        assert!(
            retirement_candidates(&owner).unwrap().is_empty(),
            "a historical reopen is active work, not a new completion event"
        );
    }
    #[test]
    fn malformed_lifecycle_is_retained_without_retry_rewrite() {
        let (_temp, owner, path) = fixture();
        terminal(&owner, &path);
        update_fork_record(&owner, "worker", |record| {
            record["lifecycle"] = json!("future-shape")
        })
        .unwrap();
        assert!(retirement_candidates(&owner).unwrap().is_empty());
        assert!(retire_eligible_fork(&owner, "worker").is_err());
        assert!(path.join("work.txt").exists());
    }
}
