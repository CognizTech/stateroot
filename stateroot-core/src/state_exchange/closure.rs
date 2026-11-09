use super::format::{
    artifact_path, invalid, parse_oid, read_artifact, AuxiliaryRef, OmittedRef, RootMetadata,
    ARTIFACT_COUNT, OBJECT_COUNT, ROOT_COUNT,
};
use super::ExchangeError;
use crate::{local_store, roots, sync_engine::ignore::IgnoreRules};
use git2::{ObjectType, Oid, Repository};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Closure {
    pub commits: BTreeSet<Oid>,
    pub objects: BTreeMap<Oid, ObjectType>,
}

#[derive(Debug)]
pub struct ArtifactSource {
    pub name: String,
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug)]
pub struct ExportClosure {
    pub manifest_roots: Vec<RootMetadata>,
    pub auxiliary_refs: Vec<AuxiliaryRef>,
    pub omissions: Vec<OmittedRef>,
    pub artifacts: Vec<ArtifactSource>,
    pub closure: Closure,
}

fn insert_object(closure: &mut Closure, oid: Oid, kind: ObjectType) -> Result<(), ExchangeError> {
    closure.objects.insert(oid, kind);
    if closure.objects.len() > OBJECT_COUNT {
        return Err(invalid("Git closure exceeds the v2 object bound"));
    }
    Ok(())
}

pub(crate) fn valid_component(name: &str) -> bool {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.eq_ignore_ascii_case(".git")
        || name.contains(['/', '\\', ':'])
        || name.chars().any(|ch| ch.is_control())
        || name.ends_with(['.', ' '])
    {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

pub fn enumerate_tree(
    repo: &Repository,
    oid: Oid,
    closure: &mut Closure,
) -> Result<(), ExchangeError> {
    if closure.objects.contains_key(&oid) {
        return Ok(());
    }
    insert_object(closure, oid, ObjectType::Tree)?;
    let tree = repo.find_tree(oid)?;
    let mut portable_names = BTreeSet::new();
    for entry in tree.iter() {
        let name = entry.name().unwrap_or_default();
        if !valid_component(name) || !portable_names.insert(name.to_lowercase()) {
            return Err(invalid(
                "tree contains an invalid or colliding portable path component",
            ));
        }
        match entry.kind() {
            Some(ObjectType::Tree) => enumerate_tree(repo, entry.id(), closure)?,
            Some(ObjectType::Blob) if matches!(entry.filemode(), 0o100644 | 0o100755) => {
                insert_object(closure, entry.id(), ObjectType::Blob)?
            }
            _ => return Err(invalid("root tree contains a non-file Git entry")),
        }
    }
    Ok(())
}

pub fn enumerate_commit(
    repo: &Repository,
    oid: Oid,
    closure: &mut Closure,
) -> Result<(), ExchangeError> {
    if closure.commits.contains(&oid) {
        return Ok(());
    }
    if closure.commits.len() >= ROOT_COUNT {
        return Err(invalid("commit ancestry exceeds the v2 bound"));
    }
    let commit = repo.find_commit(oid)?;
    closure.commits.insert(oid);
    insert_object(closure, oid, ObjectType::Commit)?;
    for parent in commit.parent_ids() {
        enumerate_commit(repo, parent, closure)?;
    }
    enumerate_tree(repo, commit.tree_id(), closure)
}

pub fn enumerate(
    repo: &Repository,
    selected: Oid,
    auxiliary: &[AuxiliaryRef],
) -> Result<Closure, ExchangeError> {
    let mut closure = Closure::default();
    enumerate_commit(repo, selected, &mut closure)?;
    for reference in auxiliary {
        let oid = parse_oid(&reference.oid)?;
        match reference.kind.as_str() {
            "commit" => enumerate_commit(repo, oid, &mut closure)?,
            "tree" => enumerate_tree(repo, oid, &mut closure)?,
            _ => return Err(invalid("unsupported auxiliary Git kind")),
        }
    }
    Ok(closure)
}

pub fn validate_tree_paths(
    repo: &Repository,
    commits: &BTreeSet<Oid>,
) -> Result<(), ExchangeError> {
    let hardcoded = IgnoreRules::none();
    for oid in commits {
        let tree = repo.find_commit(*oid)?.tree()?;
        let mut invalid_path = None;
        tree.walk(git2::TreeWalkMode::PreOrder, |directory, entry| {
            let relative = format!("{directory}{}", entry.name().unwrap_or_default());
            if hardcoded.is_ignored(
                &relative.to_ascii_lowercase(),
                entry.kind() == Some(ObjectType::Tree),
            ) {
                invalid_path = Some(relative);
                return git2::TreeWalkResult::Abort;
            }
            git2::TreeWalkResult::Ok
        })?;
        if let Some(path) = invalid_path {
            return Err(invalid(format!(
                "root tree contains machine-local path {path}"
            )));
        }
    }
    Ok(())
}

pub fn project_id_at_root(repo: &Repository, root: Oid) -> Result<String, ExchangeError> {
    let tree = repo.find_commit(root)?.tree()?;
    let entry = tree
        .get_path(Path::new(".stateroot/manifest.json"))
        .map_err(|_| invalid("selected root has no .stateroot/manifest.json"))?;
    let manifest: Value = serde_json::from_slice(repo.find_blob(entry.id())?.content())?;
    if manifest["schema_version"] != local_store::SCHEMA_MANIFEST_V1 {
        return Err(invalid("unsupported project manifest schema"));
    }
    manifest["project_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid("root manifest has no project identity"))
}

fn verification_records(
    repo: &Repository,
    selected: Oid,
) -> Result<Vec<(String, Value)>, ExchangeError> {
    let tree = repo.find_commit(selected)?.tree()?;
    let mut records: BTreeMap<String, (bool, Value)> = BTreeMap::new();
    let Ok(directory) = tree.get_path(Path::new(".stateroot/plans/verification")) else {
        return Ok(Vec::new());
    };
    let Some(ObjectType::Tree) = directory.kind() else {
        return Err(invalid("verification metadata path is not a tree"));
    };
    let directory = repo.find_tree(directory.id())?;
    for entry in directory.iter() {
        let name = entry.name().unwrap_or_default();
        let finished = name.ends_with(".finished.json");
        let started = name.ends_with(".started.json");
        if !finished && !started {
            continue;
        }
        let id = name
            .strip_suffix(if finished {
                ".finished.json"
            } else {
                ".started.json"
            })
            .unwrap_or_default()
            .to_string();
        let value: Value = serde_json::from_slice(repo.find_blob(entry.id())?.content())?;
        let replace = records
            .get(&id)
            .is_none_or(|(current_finished, _)| !*current_finished && finished);
        if replace {
            records.insert(id, (finished, value));
        }
    }
    Ok(records
        .into_iter()
        .map(|(id, (_, value))| (id, value))
        .collect())
}

pub(super) fn collect_verification_refs(
    repo: &Repository,
    selected: Oid,
) -> Result<(Vec<AuxiliaryRef>, Vec<OmittedRef>), ExchangeError> {
    let mut auxiliary = BTreeMap::<String, AuxiliaryRef>::new();
    let mut omissions = BTreeMap::<String, OmittedRef>::new();
    for (filename_id, record) in verification_records(repo, selected)? {
        let id = record["id"].as_str().unwrap_or(&filename_id);
        let reference = format!("refs/stateroot/verification/{id}");
        let product_tree = record["product_tree"].as_str().unwrap_or_default();
        let valid = id == filename_id
            && id
                .strip_prefix("v_")
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
            && record["schema_version"] == crate::verification::SCHEMA
            && parse_oid(product_tree).is_ok();
        if !valid {
            omissions.insert(
                reference.clone(),
                OmittedRef {
                    reference,
                    oid: None,
                    kind: "tree".into(),
                    status: "invalid verification metadata".into(),
                },
            );
            continue;
        }
        if repo
            .refname_to_id(&reference)
            .ok()
            .is_some_and(|oid| oid.to_string() != product_tree)
        {
            return Err(invalid(format!(
                "verification ref disagrees with its record: {reference}"
            )));
        }
        if repo.find_tree(parse_oid(product_tree)?).is_ok() {
            auxiliary.insert(
                reference.clone(),
                AuxiliaryRef {
                    reference,
                    oid: product_tree.into(),
                    kind: "tree".into(),
                },
            );
        } else {
            omissions.insert(
                reference.clone(),
                OmittedRef {
                    reference,
                    oid: Some(product_tree.into()),
                    kind: "tree".into(),
                    status: "source object unavailable".into(),
                },
            );
        }
        if let Some(source_root) = record["source_root"].as_str().filter(|id| !id.is_empty()) {
            let source_ref = format!("refs/stateroot/roots/{source_root}");
            if repo.find_commit(parse_oid(source_root)?).is_ok() {
                auxiliary.insert(
                    source_ref.clone(),
                    AuxiliaryRef {
                        reference: source_ref,
                        oid: source_root.into(),
                        kind: "commit".into(),
                    },
                );
            } else {
                omissions.insert(
                    source_ref.clone(),
                    OmittedRef {
                        reference: source_ref,
                        oid: Some(source_root.into()),
                        kind: "commit".into(),
                        status: "source root unavailable".into(),
                    },
                );
            }
        }
    }
    Ok((
        auxiliary.into_values().collect(),
        omissions.into_values().collect(),
    ))
}

fn artifact_source(name: String, path: PathBuf) -> Result<ArtifactSource, ExchangeError> {
    let bytes = read_artifact(&path)?;
    Ok(ArtifactSource {
        name,
        path,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        size: bytes.len() as u64,
    })
}

fn export_artifacts(
    project: &Path,
    commits: &BTreeSet<Oid>,
) -> Result<(Vec<RootMetadata>, Vec<ArtifactSource>), ExchangeError> {
    let store = local_store::root(project);
    let mut by_root: BTreeMap<Oid, RootMetadata> = commits
        .iter()
        .map(|oid| {
            let id = oid.to_string();
            let name = format!("root/{id}.json");
            let manifest = store
                .join("roots")
                .join(format!("{id}.json"))
                .is_file()
                .then_some(name);
            (
                *oid,
                RootMetadata {
                    oid: id,
                    manifest,
                    transitions: Vec::new(),
                },
            )
        })
        .collect();
    let transition_dir = store.join("transitions");
    if transition_dir.exists() {
        for entry in std::fs::read_dir(transition_dir)? {
            let entry = entry?;
            let filename = entry.file_name().to_string_lossy().into_owned();
            if !filename.ends_with(".json") {
                continue;
            }
            let name = format!("transition/{filename}");
            let path = entry.path();
            let bytes = read_artifact(&path)?;
            let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            let Some(to_root) = value["to_root"].as_str() else {
                continue;
            };
            let Ok(oid) = parse_oid(to_root) else {
                continue;
            };
            if let Some(metadata) = by_root.get_mut(&oid) {
                metadata.transitions.push(name);
            }
        }
    }
    let mut artifacts = Vec::new();
    for metadata in by_root.values_mut() {
        metadata.transitions.sort();
        if metadata.transitions.len() > 16 {
            return Err(invalid("root has more than 16 transition artifacts"));
        }
        if let Some(name) = &metadata.manifest {
            artifacts.push(artifact_source(name.clone(), artifact_path(&store, name)?)?);
        }
        for name in &metadata.transitions {
            artifacts.push(artifact_source(name.clone(), artifact_path(&store, name)?)?);
        }
    }
    artifacts.sort_by(|left, right| left.name.cmp(&right.name));
    if artifacts.len() > ARTIFACT_COUNT {
        return Err(invalid("artifact closure exceeds the v2 bound"));
    }
    Ok((by_root.into_values().collect(), artifacts))
}

pub fn export_closure(
    project: &Path,
    root_prefix: &str,
) -> Result<(String, ExportClosure), ExchangeError> {
    let selected = parse_oid(&roots::resolve_hash(project, root_prefix)?)?;
    let repo = roots::ensure_repo(project)?;
    let (mut auxiliary_refs, omissions) = collect_verification_refs(&repo, selected)?;
    auxiliary_refs.sort_by(|left, right| left.reference.cmp(&right.reference));
    let closure = enumerate(&repo, selected, &auxiliary_refs)?;
    validate_tree_paths(&repo, &closure.commits)?;
    let project_id = project_id_at_root(&repo, selected)?;
    let (manifest_roots, artifacts) = export_artifacts(project, &closure.commits)?;
    Ok((
        project_id,
        ExportClosure {
            manifest_roots,
            auxiliary_refs,
            omissions,
            artifacts,
            closure,
        },
    ))
}
