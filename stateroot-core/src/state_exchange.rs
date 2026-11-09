mod closure;
mod destination;
mod format;
mod stage;

use format::{ArtifactDescriptor, BundleLimits, BundleManifest, BundleMode};
use serde::Serialize;
use std::path::Path;

pub const BUNDLE_MAGIC: &[u8; 8] = format::BUNDLE_MAGIC;
pub const BUNDLE_SCHEMA: &str = format::BUNDLE_SCHEMA;

#[derive(Debug, thiserror::Error)]
pub enum ExchangeError {
    #[error(transparent)]
    Roots(#[from] roots::RootsError),
    #[error(transparent)]
    Git(#[from] git2::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Store(#[from] local_store::LocalStoreError),
    #[error(transparent)]
    RefCas(#[from] safe_io::RefCasError),
    #[error("invalid portable exchange: {0}")]
    Invalid(String),
    #[error("portable exchange refused: {0}")]
    Refused(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct ExchangeReport {
    pub schema_version: String,
    pub operation: String,
    pub schema: String,
    pub project_id: String,
    pub root_oid: String,
    pub previous_root: Option<String>,
    pub fresh: Option<bool>,
    pub destination: String,
    pub lineage_ref: Option<String>,
    pub object_count: usize,
    pub artifact_count: usize,
    pub auxiliary_count: usize,
    pub omission_count: usize,
    pub full_evidence: bool,
    pub bundle_bytes: u64,
    pub native_history: String,
    pub environment_reproduction: String,
}

impl ExchangeReport {
    fn new(
        operation: &str,
        staged: &stage::StagedBundle,
        destination: &Path,
        bundle_bytes: u64,
    ) -> Self {
        Self {
            schema_version: "stateroot.state-exchange.report.v1".into(),
            operation: operation.into(),
            schema: BUNDLE_SCHEMA.into(),
            project_id: staged.manifest.project_id.clone(),
            root_oid: staged.manifest.root_oid.clone(),
            previous_root: None,
            fresh: None,
            destination: destination.display().to_string(),
            lineage_ref: None,
            object_count: staged.received.len(),
            artifact_count: staged.manifest.artifacts.len(),
            auxiliary_count: staged.manifest.auxiliary_refs.len(),
            omission_count: staged.manifest.omissions.len(),
            full_evidence: staged.full_evidence(),
            bundle_bytes,
            native_history: "not pinned; local native sessions are not exchanged".into(),
            environment_reproduction: "not guaranteed".into(),
        }
    }
}

pub fn export(
    project: &Path,
    root_prefix: &str,
    output: &Path,
) -> Result<ExchangeReport, ExchangeError> {
    let (project_id, exported) = closure::export_closure(project, root_prefix)?;
    let manifest = BundleManifest {
        schema: BUNDLE_SCHEMA.into(),
        mode: BundleMode::Download,
        project_id: project_id.clone(),
        root_oid: roots::resolve_hash(project, root_prefix)?,
        roots: exported.manifest_roots,
        auxiliary_refs: exported.auxiliary_refs,
        omissions: exported.omissions,
        artifacts: exported
            .artifacts
            .iter()
            .map(|artifact| ArtifactDescriptor {
                name: artifact.name.clone(),
                size: artifact.size,
                sha256: artifact.sha256.clone(),
            })
            .collect(),
        object_count: exported.closure.objects.len(),
        limits: BundleLimits::default(),
    };
    let selected = manifest.root_oid.clone();
    let repo = roots::ensure_repo(project)?;
    let mut file = std::fs::File::create_new(output)?;
    let result = (|| {
        format::write_manifest(&mut file, &manifest)?;
        for (oid, kind) in &exported.closure.objects {
            format::write_object(&mut file, &repo, *oid, *kind)?;
        }
        for artifact in &exported.artifacts {
            format::write_frame_header(&mut file, 4, &artifact.name, artifact.size)?;
            let mut source = std::fs::File::open(&artifact.path)?;
            format::copy_exact(&mut source, &mut file, artifact.size)?;
        }
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = result {
        drop(file);
        let _ = std::fs::remove_file(output);
        return Err(error);
    }
    let selected_oid = format::parse_oid(&selected)?;
    let staged = stage::stage(output)?;
    let mut report =
        ExchangeReport::new("export", &staged, output, std::fs::metadata(output)?.len());
    report.project_id = project_id;
    report.root_oid = selected_oid.to_string();
    Ok(report)
}

pub fn inspect(bundle: &Path) -> Result<ExchangeReport, ExchangeError> {
    let staged = stage::stage(bundle)?;
    Ok(ExchangeReport::new(
        "inspect",
        &staged,
        bundle,
        std::fs::metadata(bundle)?.len(),
    ))
}

pub fn import(project: &Path, bundle: &Path) -> Result<ExchangeReport, ExchangeError> {
    let staged = stage::stage(bundle)?;
    let outcome = destination::import(&staged, project)?;
    let mut report =
        ExchangeReport::new("import", &staged, project, std::fs::metadata(bundle)?.len());
    report.previous_root = outcome.previous_root;
    report.fresh = Some(outcome.fresh);
    report.lineage_ref = Some(outcome.lineage_ref);
    Ok(report)
}

pub fn materialize(project: &Path, bundle: &Path) -> Result<ExchangeReport, ExchangeError> {
    let staged = stage::stage(bundle)?;
    let outcome = destination::materialize(&staged, project)?;
    let mut report = ExchangeReport::new(
        "materialize",
        &staged,
        project,
        std::fs::metadata(bundle)?.len(),
    );
    report.previous_root = outcome.previous_root;
    report.fresh = Some(outcome.fresh);
    report.lineage_ref = Some(outcome.lineage_ref);
    Ok(report)
}

use crate::{local_store, roots, safe_io};

#[cfg(test)]
mod tests;
