use super::closure::{enumerate, project_id_at_root, validate_tree_paths, Closure};
use super::format::{
    artifact_path, invalid, object_kind, parse_oid, read_artifact, read_frame_header,
    read_manifest, write_artifact, BundleManifest, ARTIFACT_BYTES, BUNDLE_BYTES,
};
use super::ExchangeError;
use git2::{ObjectType, Oid, Repository};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

pub struct StagedBundle {
    _temp: tempfile::TempDir,
    pub repo: Repository,
    pub manifest: BundleManifest,
    pub received: BTreeMap<Oid, ObjectType>,
    pub artifact_root: PathBuf,
    pub closure: Closure,
}

impl StagedBundle {
    pub fn selected(&self) -> Oid {
        parse_oid(&self.manifest.root_oid).expect("validated selected root")
    }

    pub fn full_evidence(&self) -> bool {
        self.manifest.omissions.is_empty()
    }

    pub fn artifact_bytes(&self, name: &str) -> Result<Vec<u8>, ExchangeError> {
        read_artifact(&artifact_path(&self.artifact_root, name)?)
    }
}

fn validate_artifacts(staged: &StagedBundle) -> Result<(), ExchangeError> {
    let mut referenced = BTreeSet::new();
    for metadata in &staged.manifest.roots {
        let oid = parse_oid(&metadata.oid)?;
        let commit = staged.repo.find_commit(oid)?;
        let parents: Vec<String> = commit.parent_ids().map(|oid| oid.to_string()).collect();
        if let Some(name) = &metadata.manifest {
            let value: Value = serde_json::from_slice(&staged.artifact_bytes(name)?)?;
            if value["id"].as_str() != Some(metadata.oid.as_str())
                || value["parents"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    != parents
            {
                return Err(invalid("root artifact does not match Git identity"));
            }
            referenced.insert(name.clone());
        }
        for name in &metadata.transitions {
            let value: Value = serde_json::from_slice(&staged.artifact_bytes(name)?)?;
            if value["to_root"].as_str() != Some(metadata.oid.as_str()) {
                return Err(invalid("transition artifact does not match root"));
            }
            referenced.insert(name.clone());
        }
    }
    let descriptors: BTreeSet<String> = staged
        .manifest
        .artifacts
        .iter()
        .map(|a| a.name.clone())
        .collect();
    if descriptors != referenced {
        return Err(invalid("root metadata and artifact descriptors disagree"));
    }
    Ok(())
}

pub fn stage(path: &Path) -> Result<StagedBundle, ExchangeError> {
    let temp = tempfile::tempdir()?;
    let repo = Repository::init_bare(temp.path().join("repo.git"))?;
    let artifact_root = temp.path().join("artifacts");
    let mut reader = BufReader::new(std::fs::File::open(path)?);
    let manifest = read_manifest(&mut reader)?;
    let mut received = BTreeMap::new();
    let mut artifact_names = BTreeSet::new();
    let mut total_bytes = 0u64;
    let odb = repo.odb()?;
    for _ in 0..manifest.object_count + manifest.artifacts.len() {
        let (frame, name, payload) = read_frame_header(&mut reader)?;
        total_bytes = total_bytes.saturating_add(16 + name.len() as u64 + payload);
        if total_bytes > BUNDLE_BYTES {
            return Err(invalid("bundle exceeds v2 byte bound"));
        }
        if frame == 4 {
            if payload > ARTIFACT_BYTES || !artifact_names.insert(name.clone()) {
                return Err(invalid("duplicate or oversized artifact frame"));
            }
            let descriptor = manifest
                .artifacts
                .iter()
                .find(|artifact| artifact.name == name)
                .ok_or_else(|| invalid("undeclared artifact frame"))?;
            if descriptor.size != payload {
                return Err(invalid("artifact frame size does not match descriptor"));
            }
            let destination = artifact_path(&artifact_root, &name)?;
            std::fs::create_dir_all(destination.parent().unwrap())?;
            let mut file = std::fs::File::create_new(&destination)?;
            let digest = write_artifact(&mut reader, &mut file, payload)?;
            file.sync_all()?;
            if digest != descriptor.sha256 {
                return Err(invalid("artifact SHA-256 mismatch"));
            }
            continue;
        }
        let kind = object_kind(frame)?;
        let oid = parse_oid(&name)?;
        if received.contains_key(&oid) {
            return Err(invalid("duplicate Git object frame"));
        }
        super::format::read_object(&mut reader, &odb, kind, &name, payload)?;
        received.insert(oid, kind);
    }
    let mut trailing = [0u8; 1];
    if reader.read(&mut trailing)? != 0 {
        return Err(invalid("trailing bytes after declared frames"));
    }
    let selected = parse_oid(&manifest.root_oid)?;
    let closure = enumerate(&repo, selected, &manifest.auxiliary_refs)?;
    if closure.objects.len() != received.len()
        || closure
            .objects
            .iter()
            .any(|(oid, kind)| received.get(oid) != Some(kind))
    {
        return Err(invalid(
            "received objects do not equal the selected and auxiliary closure",
        ));
    }
    let metadata_oids: BTreeSet<Oid> = manifest
        .roots
        .iter()
        .map(|root| parse_oid(&root.oid))
        .collect::<Result<_, _>>()?;
    if metadata_oids != closure.commits {
        return Err(invalid(
            "root metadata does not equal the complete commit closure",
        ));
    }
    validate_tree_paths(&repo, &closure.commits)?;
    let project = project_id_at_root(&repo, selected)?;
    if project != manifest.project_id {
        return Err(invalid("bundle and Git project identities differ"));
    }
    // Derive the required evidence from the immutable selected tree. A sender
    // cannot obtain a full-evidence claim by dropping its auxiliary descriptors
    // or hiding missing evidence by deleting an omission.
    let (required, omitted) = super::closure::collect_verification_refs(&repo, selected)?;
    let auxiliary = |entries: &[super::format::AuxiliaryRef]| {
        entries
            .iter()
            .map(|entry| {
                (
                    entry.reference.clone(),
                    entry.oid.clone(),
                    entry.kind.clone(),
                )
            })
            .collect::<BTreeSet<_>>()
    };
    let omissions = |entries: &[super::format::OmittedRef]| {
        entries
            .iter()
            .map(|entry| {
                (
                    entry.reference.clone(),
                    entry.oid.clone(),
                    entry.kind.clone(),
                )
            })
            .collect::<BTreeSet<_>>()
    };
    if auxiliary(&required) != auxiliary(&manifest.auxiliary_refs)
        || omissions(&omitted) != omissions(&manifest.omissions)
    {
        return Err(invalid(
            "declared evidence does not match selected-root verification records",
        ));
    }
    drop(odb);
    let staged = StagedBundle {
        _temp: temp,
        repo,
        manifest,
        received,
        artifact_root,
        closure,
    };
    validate_artifacts(&staged)?;
    Ok(staged)
}
