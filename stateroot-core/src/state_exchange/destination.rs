use super::format::{artifact_path, copy_exact, invalid, parse_oid, read_artifact};
use super::stage::StagedBundle;
use super::ExchangeError;
use crate::{local_store, roots, safe_io};
use git2::{ObjectType, Oid, Repository};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[cfg(test)]
type AfterCheckout = Box<dyn FnOnce(&Repository, &Path)>;

#[cfg(test)]
thread_local! {
    static AFTER_CHECKOUT: std::cell::RefCell<Option<AfterCheckout>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn after_checkout_for_test(hook: impl FnOnce(&Repository, &Path) + 'static) {
    AFTER_CHECKOUT.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

const LATEST_REF: &str = "refs/stateroot/latest";

#[derive(Debug)]
pub struct DestinationOutcome {
    pub previous_root: Option<String>,
    pub fresh: bool,
    pub lineage_ref: String,
}

#[derive(Debug)]
struct Backup {
    _temp: tempfile::TempDir,
    entries: Vec<(PathBuf, Option<PathBuf>)>,
}

#[derive(Debug, Default)]
struct Transaction {
    lineage_before: Option<Oid>,
    lineage_ref: Option<String>,
    lineage_ref_target: Option<Oid>,
}

fn copy_object(
    source: &Repository,
    destination: &Repository,
    oid: Oid,
    kind: ObjectType,
) -> Result<(), ExchangeError> {
    let source_odb = source.odb()?;
    let (mut reader, size, actual_kind) = source_odb.reader(oid)?;
    if actual_kind != kind {
        return Err(invalid("Git kind changed during object copy"));
    }
    let destination_odb = destination.odb()?;
    let mut writer = destination_odb.writer(size, kind)?;
    copy_exact(&mut reader, &mut writer, size as u64)?;
    if writer.finalize()? != oid {
        return Err(invalid(format!("Git object changed during copy: {oid}")));
    }
    Ok(())
}

fn copy_objects(staged: &StagedBundle, destination: &Repository) -> Result<(), ExchangeError> {
    for (oid, kind) in &staged.received {
        copy_object(&staged.repo, destination, *oid, *kind)?;
    }
    Ok(())
}

fn current_identity(destination: &Path) -> Result<Option<String>, ExchangeError> {
    let Some(manifest) = local_store::read_manifest(destination)? else {
        return Ok(None);
    };
    if manifest["schema_version"] != local_store::SCHEMA_MANIFEST_V1 {
        return Err(ExchangeError::Refused(
            "unsupported destination project manifest".into(),
        ));
    }
    manifest["project_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .map(Some)
        .ok_or_else(|| ExchangeError::Refused("destination project identity unavailable".into()))
}

fn validate_fresh_directory(destination: &Path) -> Result<(), ExchangeError> {
    if !destination.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(destination)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        if name == ".stateroot" {
            for state in std::fs::read_dir(entry.path())? {
                let state = state?;
                let state_name = state.file_name();
                if !matches!(
                    state_name.to_str(),
                    Some(".gitignore" | ".gitattributes" | "local" | "roots" | "transitions")
                ) {
                    return Err(ExchangeError::Refused(
                        "fresh destination contains unrelated StateRoot state".into(),
                    ));
                }
            }
            continue;
        }
        return Err(ExchangeError::Refused(
            "fresh destination contains unrelated work".into(),
        ));
    }
    Ok(())
}

fn preflight_refs(staged: &StagedBundle, repo: &Repository) -> Result<(), ExchangeError> {
    for oid in &staged.closure.commits {
        let reference = format!("refs/stateroot/roots/{oid}");
        if repo
            .refname_to_id(&reference)
            .ok()
            .is_some_and(|current| current != *oid)
        {
            return Err(ExchangeError::Refused(format!(
                "retained root ref conflicts: {reference}"
            )));
        }
    }
    for auxiliary in &staged.manifest.auxiliary_refs {
        let oid = parse_oid(&auxiliary.oid)?;
        if repo
            .refname_to_id(&auxiliary.reference)
            .ok()
            .is_some_and(|current| current != oid)
        {
            return Err(ExchangeError::Refused(format!(
                "auxiliary ref conflicts: {}",
                auxiliary.reference
            )));
        }
    }
    for artifact in &staged.manifest.artifacts {
        let destination =
            artifact_path(&local_store::root(repo.workdir().unwrap()), &artifact.name)?;
        if destination.exists()
            && read_artifact(&destination)? != staged.artifact_bytes(&artifact.name)?
        {
            return Err(ExchangeError::Refused(format!(
                "artifact conflicts with retained bytes: {}",
                artifact.name
            )));
        }
    }
    Ok(())
}

fn changed_paths(
    repo: &Repository,
    before: Oid,
    target: Oid,
    retain_controls: bool,
) -> Result<Vec<PathBuf>, ExchangeError> {
    let before = repo.find_tree(before)?;
    let target = repo.find_tree(target)?;
    let delta = repo.diff_tree_to_tree(Some(&before), Some(&target), None)?;
    let mut paths = BTreeSet::new();
    for change in delta.deltas() {
        let Some(path) = change
            .new_file()
            .path()
            .or_else(|| change.old_file().path())
        else {
            continue;
        };
        let normalized = path.to_string_lossy().replace('\\', "/");
        if roots::receiver_owned_path(&normalized, retain_controls) {
            continue;
        }
        paths.insert(path.to_path_buf());
    }
    Ok(paths.into_iter().collect())
}

fn live_matches_before(
    repo: &Repository,
    before: &git2::Tree,
    destination: &Path,
    path: &Path,
) -> Result<bool, ExchangeError> {
    let metadata = std::fs::symlink_metadata(destination.join(path)).ok();
    let entry = before.get_path(path).ok();
    match entry {
        Some(entry) => {
            let Some(metadata) = metadata else {
                return Ok(false);
            };
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                return Ok(false);
            }
            Ok(std::fs::read(destination.join(path))? == repo.find_blob(entry.id())?.content())
        }
        None => Ok(metadata.is_none()),
    }
}

fn backup_changes(
    repo: &Repository,
    before: Oid,
    destination: &Path,
    paths: &[PathBuf],
) -> Result<Backup, ExchangeError> {
    let before_tree = repo.find_tree(before)?;
    let temp = tempfile::tempdir()?;
    let mut entries = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        let normalized = path.to_string_lossy().replace('\\', "/");
        let sidecar = normalized.starts_with(".stateroot/roots/")
            || normalized.starts_with(".stateroot/transitions/");
        if !sidecar && !live_matches_before(repo, &before_tree, destination, path)? {
            return Err(ExchangeError::Refused(format!(
                "destination work conflicts at {}",
                path.display()
            )));
        }
        let live = destination.join(path);
        let backup = std::fs::symlink_metadata(&live)
            .ok()
            .filter(|metadata| metadata.file_type().is_file() && !metadata.file_type().is_symlink())
            .map(|_| temp.path().join(index.to_string()));
        if let Some(backup) = &backup {
            std::fs::copy(&live, backup)?;
        }
        entries.push((path.clone(), backup));
    }
    Ok(Backup {
        _temp: temp,
        entries,
    })
}

fn restore_backup(
    repo: &Repository,
    destination: &Path,
    backup: &Backup,
    written: Oid,
) -> Result<(), ExchangeError> {
    let written = repo.find_commit(written)?.tree()?;
    for (path, saved) in &backup.entries {
        // Restore only bytes this attempt still owns. A late external writer
        // may bypass StateRoot's lineage lock; never erase its changed bytes.
        if !live_matches_before(repo, &written, destination, path)? {
            continue;
        }
        let live = destination.join(path);
        if let Some(saved) = saved {
            if let Some(parent) = live.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(saved, &live)?;
        } else {
            match std::fs::remove_file(&live) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

fn verify_materialized(
    repo: &Repository,
    destination: &Path,
    target: Oid,
    retain_controls: bool,
) -> Result<(), ExchangeError> {
    let tree = repo.find_commit(target)?.tree()?;
    let mut failure = None;
    tree.walk(git2::TreeWalkMode::PreOrder, |directory, entry| {
        let relative = format!("{directory}{}", entry.name().unwrap_or_default());
        if roots::receiver_owned_path(&relative, retain_controls) {
            return git2::TreeWalkResult::Skip;
        }
        if entry.kind() == Some(ObjectType::Blob) {
            let matches = std::fs::symlink_metadata(destination.join(&relative))
                .ok()
                .filter(|metadata| {
                    metadata.file_type().is_file() && !metadata.file_type().is_symlink()
                })
                .and_then(|_| std::fs::read(destination.join(&relative)).ok())
                .is_some_and(|bytes| {
                    repo.find_blob(entry.id())
                        .ok()
                        .is_some_and(|blob| blob.content() == bytes)
                });
            if !matches {
                failure = Some(relative);
                return git2::TreeWalkResult::Abort;
            }
        }
        git2::TreeWalkResult::Ok
    })?;
    if let Some(path) = failure {
        return Err(invalid(format!("materialized bytes differ at {path}")));
    }
    Ok(())
}

fn checkout(
    repo: &Repository,
    destination: &Path,
    before: Oid,
    target: Oid,
    retain_controls: bool,
) -> Result<(), ExchangeError> {
    let target_tree = repo.find_commit(target)?.tree_id();
    let paths = changed_paths(repo, before, target_tree, retain_controls)?;
    let backup = backup_changes(repo, before, destination, &paths)?;
    let mut receiver_paths = BTreeSet::new();
    for tree in [repo.find_tree(before)?, repo.find_tree(target_tree)?] {
        tree.walk(git2::TreeWalkMode::PreOrder, |directory, entry| {
            let path = format!("{directory}{}", entry.name().unwrap_or_default());
            if entry.kind() == Some(ObjectType::Blob)
                && roots::receiver_owned_path(&path, retain_controls)
            {
                receiver_paths.insert(path);
            }
            git2::TreeWalkResult::Ok
        })?;
    }
    let mut kept = Vec::new();
    for path in receiver_paths {
        let bytes = match std::fs::read(destination.join(&path)) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let blob = bytes
            .map(|bytes| repo.blob(&bytes).map(|oid| oid.to_string()))
            .transpose()?;
        kept.push(roots::RetainedMainPath {
            path,
            mode: blob.as_ref().map(|_| 0o100644),
            blob,
        });
    }
    // One checkout owner preserves the human index, excluded tracked files,
    // root-only deletions, raw bytes and permissions for every materializer.
    let result = roots::checkout_root_tree_retaining_main(
        repo,
        destination,
        target_tree,
        Some(before),
        &kept,
    )
    .map_err(ExchangeError::from)
    .and_then(|()| verify_materialized(repo, destination, target, retain_controls));
    if result.is_err() {
        restore_backup(repo, destination, &backup, target)?;
    }
    result
}

fn publish_artifacts(staged: &StagedBundle, destination: &Path) -> Result<(), ExchangeError> {
    for artifact in &staged.manifest.artifacts {
        let path = artifact_path(&local_store::root(destination), &artifact.name)?;
        let existed = path.exists();
        let bytes = staged.artifact_bytes(&artifact.name)?;
        if existed && read_artifact(&path)? != bytes {
            return Err(ExchangeError::Refused(format!(
                "artifact changed before publication: {}",
                artifact.name
            )));
        }
        std::fs::create_dir_all(path.parent().unwrap())?;
        safe_io::atomic_replace(&path, &bytes)?;
    }
    Ok(())
}

fn publish_refs(
    staged: &StagedBundle,
    repo: &Repository,
    destination: &Path,
) -> Result<(), ExchangeError> {
    let lock_dir = local_store::root(destination).join("local/locks");
    let references = staged
        .closure
        .commits
        .iter()
        .map(|oid| (format!("refs/stateroot/roots/{oid}"), *oid))
        .chain(staged.manifest.auxiliary_refs.iter().map(|reference| {
            (
                reference.reference.clone(),
                parse_oid(&reference.oid).expect("validated auxiliary OID"),
            )
        }));
    for (reference, oid) in references {
        match repo.refname_to_id(&reference) {
            Ok(current) if current == oid => {}
            Ok(_) => {
                return Err(ExchangeError::Refused(format!(
                    "retained ref changed: {reference}"
                )))
            }
            Err(error) if error.code() == git2::ErrorCode::NotFound => {
                safe_io::update_ref_cas(
                    repo,
                    &lock_dir,
                    &reference,
                    None,
                    oid,
                    "portable exchange retained evidence",
                )?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

// Caller holds the same lineage lock as snap/revert/merge for the whole
// destination operation. Validated immutable evidence is never un-retained.
fn rollback(repo: &Repository, tx: &mut Transaction) -> Result<(), ExchangeError> {
    if let Some(lineage) = tx.lineage_ref.take() {
        if repo.refname_to_id(&lineage).ok() != tx.lineage_ref_target.take() {
            return Err(ExchangeError::Refused(
                "lineage advanced during rollback; current work and retained evidence preserved"
                    .into(),
            ));
        }
        if let Some(previous) = tx.lineage_before.take() {
            repo.reference(&lineage, previous, true, "portable exchange rollback")?;
        } else {
            repo.find_reference(&lineage)?.delete()?;
        }
    }
    Ok(())
}

fn destination_guard(
    destination: &Path,
    lineage: &str,
) -> Result<(Option<safe_io::ResourceLock>, safe_io::ResourceLock), ExchangeError> {
    let admission = if let Some(context) = local_store::fork_context(destination) {
        let owner = roots::retained_storage_owner(destination)?;
        let guard = roots::fork_operation_guard(&owner, &context.fork)?;
        roots::validate_fork_execution(&owner, &context.fork, destination)?;
        Some(guard)
    } else {
        None
    };
    let lineage = safe_io::ResourceLock::acquire(safe_io::ref_lock_path(
        &local_store::root(destination).join("local/locks"),
        lineage,
    ))
    .map_err(safe_io::RefCasError::from)?;
    Ok((admission, lineage))
}

fn check_current_exclusions(
    repo: &Repository,
    destination: &Path,
    selected: Oid,
    current: Option<Oid>,
) -> Result<(), ExchangeError> {
    let Some(current) = current else {
        return Ok(());
    };
    let rules = crate::sync_engine::ignore::IgnoreRules::load(destination);
    let target = repo.find_commit(selected)?.tree()?;
    for path in changed_paths(
        repo,
        repo.find_commit(current)?.tree_id(),
        target.id(),
        true,
    )? {
        if rules.is_ignored(&path.to_string_lossy().replace('\\', "/"), false) {
            return Err(ExchangeError::Refused(format!("current exclusions prevent changing {}; import retains the root without changing files", path.display())));
        }
    }
    let mut excluded = None;
    target.walk(git2::TreeWalkMode::PreOrder, |directory, entry| {
        let relative = format!("{directory}{}", entry.name().unwrap_or_default());
        if rules.is_ignored(&relative, entry.kind() == Some(ObjectType::Tree)) {
            excluded = Some(relative);
            return git2::TreeWalkResult::Abort;
        }
        git2::TreeWalkResult::Ok
    })?;
    if let Some(path) = excluded {
        return Err(ExchangeError::Refused(format!("current exclusions prevent exact materialization at {path}; import retains the root without changing files")));
    }
    for name in [".gitignore", ".staterootignore"] {
        let desired = target
            .get_path(Path::new(name))
            .ok()
            .map(|entry| {
                repo.find_blob(entry.id())
                    .map(|blob| blob.content().to_vec())
            })
            .transpose()?;
        let current = match std::fs::read(destination.join(name)) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if desired != current {
            return Err(ExchangeError::Refused(format!("materialization would change current {name}; import retains the root without changing exclusions")));
        }
    }
    Ok(())
}

fn prepare(
    staged: &StagedBundle,
    destination: &Path,
    materialize: bool,
) -> Result<(Repository, Option<Oid>, String), ExchangeError> {
    let identity = current_identity(destination)?;
    if identity
        .as_deref()
        .is_some_and(|identity| identity != staged.manifest.project_id)
    {
        return Err(ExchangeError::Refused(
            "conflicting destination project identity".into(),
        ));
    }
    if materialize && identity.is_none() {
        validate_fresh_directory(destination)?;
    }
    std::fs::create_dir_all(destination)?;
    let repo = roots::ensure_repo(destination)?;
    let lineage = if identity.is_some() {
        roots::lineage_refname_checked(destination)?
    } else {
        LATEST_REF.into()
    };
    let current = repo.refname_to_id(&lineage).ok();
    if materialize {
        if let Some(current) = current {
            if current != staged.selected()
                && !staged
                    .repo
                    .graph_descendant_of(staged.selected(), current)?
            {
                return Err(ExchangeError::Refused(
                    "destination lineage is divergent".into(),
                ));
            }
        } else {
            validate_fresh_directory(destination)?;
        }
    }
    preflight_refs(staged, &repo)?;
    Ok((repo, current, lineage))
}

pub fn import(
    staged: &StagedBundle,
    destination: &Path,
) -> Result<DestinationOutcome, ExchangeError> {
    let (_, _, expected_lineage) = prepare(staged, destination, false)?;
    let _guard = destination_guard(destination, &expected_lineage)?;
    let (repo, current, lineage) = prepare(staged, destination, false)?;
    if lineage != expected_lineage {
        return Err(ExchangeError::Refused(
            "destination identity changed".into(),
        ));
    }
    copy_objects(staged, &repo)?;
    publish_artifacts(staged, destination)?;
    publish_refs(staged, &repo, destination)?;
    Ok(DestinationOutcome {
        previous_root: current.map(|oid| oid.to_string()),
        fresh: current.is_none(),
        lineage_ref: lineage,
    })
}

pub fn materialize(
    staged: &StagedBundle,
    destination: &Path,
) -> Result<DestinationOutcome, ExchangeError> {
    let (_, _, expected_lineage) = prepare(staged, destination, true)?;
    let _guard = destination_guard(destination, &expected_lineage)?;
    let (repo, current, lineage) = prepare(staged, destination, true)?;
    if lineage != expected_lineage {
        return Err(ExchangeError::Refused(
            "destination identity changed".into(),
        ));
    }
    check_current_exclusions(&staged.repo, destination, staged.selected(), current)?;
    let before = match current {
        Some(current) => repo.find_commit(current)?.tree_id(),
        None => repo.treebuilder(None)?.write()?,
    };
    copy_objects(staged, &repo)
        .map_err(|error| invalid(format!("destination object copy failed: {error}")))?;
    let _store_guards = if current.is_some() {
        roots::materialization_guards(
            &repo,
            destination,
            before,
            staged.repo.find_commit(staged.selected())?.tree_id(),
            true,
        )?
    } else {
        Vec::new()
    };
    let paths = changed_paths(
        &repo,
        before,
        staged.repo.find_commit(staged.selected())?.tree_id(),
        current.is_some(),
    )?;
    let backup = backup_changes(&repo, before, destination, &paths)?;
    let selected = staged.selected();
    let mut tx = Transaction {
        lineage_before: Some(selected),
        ..Default::default()
    };
    let result = (|| {
        checkout(&repo, destination, before, selected, current.is_some())
            .map_err(|error| invalid(format!("destination checkout failed: {error}")))?;
        #[cfg(test)]
        if let Some(hook) = AFTER_CHECKOUT.with(|slot| slot.borrow_mut().take()) {
            hook(&repo, destination);
        }
        publish_artifacts(staged, destination)
            .map_err(|error| invalid(format!("artifact publication failed: {error}")))?;
        let manifest_path = local_store::root(destination).join(local_store::MANIFEST_PATH);
        if !manifest_path.exists() {
            let bytes = staged.repo.find_blob(
                staged
                    .repo
                    .find_commit(selected)?
                    .tree()?
                    .get_path(Path::new(".stateroot/manifest.json"))?
                    .id(),
            )?;
            std::fs::create_dir_all(manifest_path.parent().unwrap())?;
            safe_io::atomic_replace(&manifest_path, bytes.content())?;
        }
        publish_refs(staged, &repo, destination)
            .map_err(|error| invalid(format!("retained ref publication failed: {error}")))?;
        if current != Some(selected) {
            if repo.refname_to_id(&lineage).ok() != current {
                return Err(ExchangeError::Refused(
                    "destination lineage changed before publication".into(),
                ));
            }
            repo.reference(&lineage, selected, true, "materialize portable exchange")?;
            tx.lineage_ref = Some(lineage.clone());
            tx.lineage_ref_target = Some(selected);
            tx.lineage_before = current;
        }
        Ok(())
    })();
    if result.is_ok() {
        if let Err(error) = verify_materialized(&repo, destination, selected, current.is_some()) {
            rollback(&repo, &mut tx)?;
            return Err(invalid(format!(
                "late destination write detected; current bytes preserved: {error}"
            )));
        }
    }
    if result.is_err() {
        rollback(&repo, &mut tx)?;
        restore_backup(&repo, destination, &backup, selected)?;
    }
    result.map(|()| DestinationOutcome {
        previous_root: current.map(|oid| oid.to_string()),
        fresh: current.is_none(),
        lineage_ref: lineage,
    })
}
