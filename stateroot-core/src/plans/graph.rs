//! Dependency declarations live in plan sidecars. This module only derives
//! graph/readiness views; it owns neither workers nor a second task store.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA_GRAPH: &str = "stateroot.plan-graph.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Completion,
    Source,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequiredOutput {
    pub path: String,
    pub sha256: String,
}

/// A native coordinator's explicit compatibility judgment. The target source
/// identity makes this stale on edits; retained objects alone never create it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceAcceptance {
    pub source_root: String,
    pub target_root: String,
    pub target_product_tree: String,
    pub accepted_by: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanDependency {
    pub plan_id: String,
    pub body_digest: String,
    pub kind: DependencyKind,
    #[serde(default)]
    pub outputs: Vec<RequiredOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<SourceAcceptance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorizedDispatch {
    /// Stable owner-selected operation identity; observation times never key launches.
    pub operation_id: String,
    pub task: String,
    /// Existing owner/programme authorization, not another approval stage.
    pub authorization: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanAssignment {
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<AuthorizedDispatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExecutionDeclaration {
    pub dependencies: Vec<PlanDependency>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment: Option<PlanAssignment>,
    #[serde(default)]
    pub owned_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanExecution {
    pub revision: u64,
    pub edited_at: String,
    pub edited_by: String,
    #[serde(flatten)]
    pub declaration: ExecutionDeclaration,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadinessReason {
    pub code: String,
    pub detail: String,
    pub prerequisite: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyReadiness {
    pub dependency: PlanDependency,
    pub state: String,
    pub reason: String,
    pub evidence_basis: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerObservation {
    pub id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanReadiness {
    pub id: String,
    pub title: String,
    pub status: String,
    pub revision: String,
    pub body_digest: Option<String>,
    pub dependencies_declared: bool,
    pub dependencies: Vec<DependencyReadiness>,
    pub assignment: Option<PlanAssignment>,
    pub target_fork: Option<String>,
    pub target_root: Option<String>,
    pub worker_state: String,
    pub workers: Vec<WorkerObservation>,
    pub readiness: String,
    pub blockers: Vec<ReadinessReason>,
    pub owned_paths: Vec<String>,
    pub dispatch_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnershipOverlap {
    pub plans: Vec<String>,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanGraph {
    pub schema_version: String,
    /// Stable digest of declarations, target/source observations and worker state.
    pub observation_id: String,
    pub nodes: Vec<PlanReadiness>,
    pub ready: Vec<String>,
    pub ownership_overlaps: Vec<OwnershipOverlap>,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", sha2::Sha256::digest(bytes))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && id != "."
        && id != ".."
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':'])
        && !Path::new(path).is_absolute()
        && Path::new(path)
            .components()
            .all(|p| matches!(p, std::path::Component::Normal(_)))
        && !path.starts_with(".stateroot/")
        && path != ".stateroot"
        && !path.starts_with(".git/")
        && path != ".git"
}

/// CAS identity covers the complete sidecar (including preserved annotations)
/// and body, so same-second transitions and native edits cannot evade checks.
pub fn revision(project: &Path, id: &str) -> Result<String, String> {
    if !valid_id(id) {
        return Err("invalid plan id".into());
    }
    let mut bytes = std::fs::read(meta_path(project, id)).map_err(|e| e.to_string())?;
    bytes.push(0);
    bytes.extend(std::fs::read(body_path(project, id)).map_err(|e| e.to_string())?);
    Ok(digest(&bytes))
}

fn validate_declaration(id: &str, declaration: &ExecutionDeclaration) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for edge in &declaration.dependencies {
        if !valid_id(&edge.plan_id) || !valid_digest(&edge.body_digest) {
            return Err("dependency requires an exact plan id and SHA-256 body identity".into());
        }
        if edge.plan_id == id {
            return Err("self dependency is a cycle".into());
        }
        if !seen.insert(&edge.plan_id) {
            return Err("duplicate prerequisite".into());
        }
        if edge
            .outputs
            .iter()
            .any(|o| !valid_path(&o.path) || !valid_digest(&o.sha256))
        {
            return Err("output requires a relative product path and SHA-256 identity".into());
        }
        if edge.kind == DependencyKind::Completion
            && (!edge.outputs.is_empty() || edge.acceptance.is_some())
        {
            return Err("completion edges do not declare source outputs or acceptance".into());
        }
    }
    if declaration.owned_paths.iter().any(|p| !valid_path(p)) {
        return Err("owned paths must be relative product paths".into());
    }
    if declaration
        .superseded_by
        .as_ref()
        .is_some_and(|s| !valid_id(s) || s == id)
    {
        return Err("invalid superseding plan identity".into());
    }
    if let Some(assignment) = &declaration.assignment {
        if !valid_id(&assignment.harness) || assignment.fork.as_ref().is_some_and(|f| !valid_id(f))
        {
            return Err("invalid harness or fork assignment".into());
        }
        if assignment.dispatch.as_ref().is_some_and(|d| {
            d.operation_id.trim().is_empty()
                || d.task.trim().is_empty()
                || d.authorization.trim().is_empty()
        }) {
            return Err(
                "dispatch requires a stable operation, task and existing authorization".into(),
            );
        }
    }
    Ok(())
}

fn reaches(
    start: &str,
    target: &str,
    all: &BTreeMap<String, PlanMeta>,
    visited: &mut BTreeSet<String>,
) -> bool {
    if start == target {
        return true;
    }
    if !visited.insert(start.to_string()) {
        return false;
    }
    all.get(start)
        .and_then(|p| p.execution.as_ref())
        .is_some_and(|e| {
            e.declaration
                .dependencies
                .iter()
                .any(|d| reaches(&d.plan_id, target, all, visited))
        })
}

/// Explicit declaration edit, never a Markdown inference or implicit migration.
pub fn declare_execution(
    project: &Path,
    id: &str,
    expected_revision: &str,
    declaration: ExecutionDeclaration,
    actor: &str,
) -> Result<PlanMeta, String> {
    let _guard = write_guard(project)?;
    let (mut meta, _) = load(project, id)
        .filter(|(p, _)| p.id == id)
        .ok_or("exact supported plan id required")?;
    if revision(project, id)? != expected_revision {
        return Err("plan revision changed; reread before editing".into());
    }
    if meta.status().terminal() {
        return Err("completed or abandoned declarations are historical and immutable".into());
    }
    if actor.trim().is_empty() {
        return Err("declaration actor is empty".into());
    }
    validate_declaration(id, &declaration)?;
    let now = now_rfc3339();
    meta.execution = Some(PlanExecution {
        revision: meta.execution.as_ref().map_or(1, |e| e.revision + 1),
        edited_at: now.clone(),
        edited_by: actor.into(),
        declaration,
    });
    meta.schema_version = SCHEMA_PLAN_V2.into();
    let mut all: BTreeMap<_, _> = list(project)
        .into_iter()
        .map(|p| (p.id.clone(), p))
        .collect();
    all.insert(id.into(), meta.clone());
    for edge in &meta.execution.as_ref().unwrap().declaration.dependencies {
        if reaches(&edge.plan_id, id, &all, &mut BTreeSet::new()) {
            return Err("dependency edit would introduce a cycle".into());
        }
    }
    meta.updated_at = now;
    meta.root_ref = latest_root_ref(project);
    write_meta(project, &meta)?;
    Ok(meta)
}

fn claimed_fork(project: &Path, id: &str) -> Option<String> {
    let raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(meta_path(project, id)).ok()?).ok()?;
    raw["claimed_by"]["fork"].as_str().map(str::to_owned)
}

/// Automatic operations share the existing trunk delegation store. A Git
/// common directory alone is insufficient: only a registered StateRoot fork
/// with the same declared project identity can use that owner's authority.
/// Ordinary independent projects/worktrees retain their own caller scope.
pub fn automatic_dispatch_owner(project: &Path) -> Result<PathBuf, String> {
    let project = project.canonicalize().map_err(|e| e.to_string())?;
    let repository = git2::Repository::open(&project).map_err(|e| e.to_string())?;
    let common = git2::Repository::open(repository.commondir()).map_err(|e| e.to_string())?;
    let owner = common
        .workdir()
        .ok_or("automatic dispatch owner has no worktree")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let identity = |path: &Path| -> Result<String, String> {
        let manifest = local_store::read_manifest(path)
            .map_err(|e| e.to_string())?
            .ok_or("automatic dispatch project identity is unavailable")?;
        if manifest["schema_version"] != local_store::SCHEMA_MANIFEST_V1 {
            return Err("automatic dispatch project identity is unsupported".into());
        }
        manifest["project_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| "automatic dispatch project identity is unavailable".into())
    };
    let Some(context) = local_store::fork_context(&project) else {
        // A lost/corrupt fork-context must not turn an existing shared project
        // into a second private automatic authority. An independently identified
        // StateRoot project in an ordinary Git worktree keeps its own scope.
        if owner != project
            && local_store::is_stateroot_dir(&owner)
            && identity(&project)? == identity(&owner)?
        {
            return Err("shared project worktree has no supported StateRoot fork context".into());
        }
        return Ok(project);
    };
    if context.schema != "stateroot.fork-context.v1" {
        return Err("automatic dispatch fork context is unsupported".into());
    }
    let registered = crate::roots::registered_worktree_path(&owner, &context.fork)
        .ok_or("automatic dispatch fork is not registered with its owner")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if registered != project || owner == project {
        return Err("automatic dispatch fork registration does not match this checkout".into());
    }
    if identity(&project)? != identity(&owner)? {
        return Err("automatic dispatch fork belongs to a different StateRoot project".into());
    }
    Ok(owner)
}

fn validate_automatic_authority(
    project: &Path,
    owner: &Path,
    plan: &PlanMeta,
) -> Result<(), String> {
    let owner_plan = if owner == project {
        None
    } else {
        Some(
            load(owner, &plan.id)
                .filter(|(p, _)| p.id == plan.id)
                .ok_or("automatic dispatch owner has no matching supported plan")?
                .0,
        )
    };
    let authority = owner_plan.as_ref().unwrap_or(plan);
    if !matches!(
        authority.status(),
        PlanStatus::Approved | PlanStatus::Active
    ) {
        return Err(format!(
            "automatic dispatch owner plan is {}",
            authority.status
        ));
    }
    if authority.execution.as_ref().map(|e| &e.declaration)
        != plan.execution.as_ref().map(|e| &e.declaration)
        || body_digest(&body_path(owner, &plan.id)) != body_digest(&body_path(project, &plan.id))
        || drifted_since_approval(owner, authority)
    {
        return Err(
            "automatic dispatch declaration differs from current owner authorization".into(),
        );
    }
    Ok(())
}

/// Used again under the shared delegation key lock immediately before launch.
pub fn automatic_dispatch_authority(project: &Path, id: &str) -> Result<PathBuf, String> {
    let owner = automatic_dispatch_owner(project)?;
    let plan = load(project, id)
        .filter(|(p, _)| p.id == id)
        .ok_or("automatic dispatch plan is unavailable")?
        .0;
    validate_automatic_authority(project, &owner, &plan)?;
    Ok(owner)
}

pub fn execution_target(project: &Path, fork: Option<&str>) -> Option<PathBuf> {
    match fork {
        None => Some(project.to_path_buf()),
        Some(fork) if local_store::fork_context(project).is_some_and(|c| c.fork == fork) => {
            Some(project.to_path_buf())
        }
        Some(fork) => crate::roots::registered_worktree_path(project, fork)
            .or_else(|| {
                automatic_dispatch_owner(project)
                    .ok()
                    .and_then(|owner| crate::roots::registered_worktree_path(&owner, fork))
            })
            .filter(|p| p.is_dir()),
    }
}

fn repo(project: &Path) -> Result<git2::Repository, String> {
    git2::Repository::open(crate::roots::repository_owner(project).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

fn root_output(project: &Path, root: &str, path: &str) -> Option<String> {
    let repo = repo(project).ok()?;
    let tree = repo.find_commit(root.parse().ok()?).ok()?.tree().ok()?;
    let entry = tree.get_path(Path::new(path)).ok()?;
    if entry.filemode() != 0o100644 && entry.filemode() != 0o100755 {
        return None;
    }
    let blob = repo.find_blob(entry.id()).ok()?;
    Some(digest(blob.content()))
}

fn current_output(project: &Path, path: &str) -> Option<String> {
    // Do not follow symlinks out of the declared product path.
    let mut cursor = project.to_path_buf();
    for component in Path::new(path).components() {
        cursor.push(component);
        if std::fs::symlink_metadata(&cursor)
            .ok()?
            .file_type()
            .is_symlink()
        {
            return None;
        }
    }
    body_digest(&cursor)
}

fn published_tip(project: &Path) -> Option<String> {
    let repo = repo(project).ok()?;
    let reference = local_store::fork_context(project)
        .map(|c| format!("refs/stateroot/forks/{}", c.fork))
        .unwrap_or_else(|| "refs/stateroot/latest".into());
    repo.refname_to_id(&reference).ok().map(|id| id.to_string())
}

fn published_contains(project: &Path, root: &str) -> bool {
    let Some(tip) = published_tip(project) else {
        return false;
    };
    let Ok(repo) = repo(project) else {
        return false;
    };
    let (Ok(tip), Ok(root)) = (tip.parse::<git2::Oid>(), root.parse::<git2::Oid>()) else {
        return false;
    };
    tip == root || repo.graph_descendant_of(tip, root).unwrap_or(false)
}

fn dependency_state(
    project: &Path,
    target: Option<&Path>,
    edge: &PlanDependency,
    all: &BTreeMap<String, PlanMeta>,
) -> (String, String) {
    let Some(upstream) = all.get(&edge.plan_id) else {
        return if meta_path(project, &edge.plan_id).exists() {
            (
                "unsupported".into(),
                "prerequisite metadata is unsupported or malformed".into(),
            )
        } else {
            (
                "missing".into(),
                "exact prerequisite plan is missing".into(),
            )
        };
    };
    if upstream
        .execution
        .as_ref()
        .and_then(|e| e.declaration.superseded_by.as_ref())
        .is_some()
    {
        return (
            "superseded".into(),
            "prerequisite was superseded; no replacement is inferred".into(),
        );
    }
    if upstream.status() == PlanStatus::Abandoned {
        return ("abandoned".into(), "prerequisite was abandoned".into());
    }
    if body_digest(&body_path(project, &edge.plan_id)).as_deref() != Some(&edge.body_digest) {
        return (
            "body_mismatch".into(),
            "prerequisite body differs from the declared identity".into(),
        );
    }
    if upstream.status() != PlanStatus::Done {
        return (
            "incomplete".into(),
            "prerequisite has no completed lifecycle".into(),
        );
    }
    let Some(receipt) = &upstream.completion_receipt else {
        return (
            "receipt_missing".into(),
            "legacy done status has no completion receipt".into(),
        );
    };
    if receipt.body_digest != edge.body_digest || receipt.evidence.trim().is_empty() {
        return (
            "receipt_mismatch".into(),
            "completion receipt does not establish this plan body".into(),
        );
    }
    if edge.kind == DependencyKind::Completion {
        return ("satisfied".into(), "matching completion receipt".into());
    }
    let Some(target) = target else {
        return (
            "target_unavailable".into(),
            "assigned executor checkout is unavailable".into(),
        );
    };
    let Some(source_root) = &receipt.completion_root else {
        return (
            "source_unavailable".into(),
            "completion receipt has no source root".into(),
        );
    };
    if repo(project).is_err()
        || !crate::roots::get_root(project, source_root).is_ok_and(|root| {
            matches!(root.metadata_status.as_str(), "recorded" | "legacy_unknown")
        })
    {
        return (
            "source_unavailable".into(),
            "completion root is unavailable, foreign or unsupported".into(),
        );
    }
    let Some(target_root) = published_tip(target) else {
        return (
            "source_unpublished".into(),
            "executor has no published root".into(),
        );
    };
    if let Some(accepted) = &edge.acceptance {
        if accepted.source_root == *source_root
            && !accepted.accepted_by.trim().is_empty()
            && !accepted.evidence.trim().is_empty()
            && published_contains(target, &accepted.target_root)
            && crate::roots::product_tree_matches(target, &accepted.target_product_tree)
                .unwrap_or(false)
        {
            return (
                "satisfied".into(),
                "native coordinator accepted compatibility on unchanged published target source"
                    .into(),
            );
        }
        return (
            "acceptance_stale".into(),
            "native compatibility acceptance no longer matches source/publication".into(),
        );
    }
    if edge.outputs.is_empty() {
        return (
            "compatibility_unknown".into(),
            "declare exact required outputs or record native compatibility acceptance".into(),
        );
    }
    for output in &edge.outputs {
        if root_output(project, source_root, &output.path).as_deref() != Some(&output.sha256) {
            return (
                "source_identity_mismatch".into(),
                format!(
                    "completion root does not provide required output {}",
                    output.path
                ),
            );
        }
        if root_output(target, &target_root, &output.path).as_deref() != Some(&output.sha256) {
            return (
                "source_unpublished".into(),
                format!(
                    "required output {} is absent from executor's published root",
                    output.path
                ),
            );
        }
        if current_output(target, &output.path).as_deref() != Some(&output.sha256) {
            return (
                "source_changed".into(),
                format!("executor output {} is changed or unavailable", output.path),
            );
        }
    }
    (
        "satisfied".into(),
        "required outputs match completion root, published executor root and current bytes".into(),
    )
}

/// Record native judgment against a real published executor source identity.
pub fn accept_source(
    project: &Path,
    id: &str,
    prerequisite: &str,
    expected_revision: &str,
    actor: &str,
    evidence: &str,
) -> Result<PlanMeta, String> {
    let (meta, _) = load(project, id)
        .filter(|(p, _)| p.id == id)
        .ok_or("exact plan id required")?;
    if revision(project, id)? != expected_revision {
        return Err("plan revision changed".into());
    }
    if evidence.trim().is_empty() {
        return Err("compatibility evidence is empty".into());
    }
    let mut declaration = meta
        .execution
        .ok_or("dependencies are not declared")?
        .declaration;
    let fork = declaration
        .assignment
        .as_ref()
        .and_then(|a| a.fork.clone())
        .or_else(|| claimed_fork(project, id));
    let target =
        execution_target(project, fork.as_deref()).ok_or("assigned checkout unavailable")?;
    let upstream = load(project, prerequisite)
        .filter(|(p, _)| p.id == prerequisite)
        .ok_or("prerequisite unavailable")?
        .0;
    let receipt = upstream
        .completion_receipt
        .ok_or("prerequisite completion receipt missing")?;
    let edge = declaration
        .dependencies
        .iter_mut()
        .find(|d| d.plan_id == prerequisite && d.kind == DependencyKind::Source)
        .ok_or("source dependency not declared")?;
    if upstream.status != "done"
        || receipt.body_digest != edge.body_digest
        || body_digest(&body_path(project, prerequisite)).as_deref() != Some(&edge.body_digest)
    {
        return Err("prerequisite completion/body mismatch".into());
    }
    let source_root = receipt
        .completion_root
        .ok_or("completion source unavailable")?;
    crate::roots::get_root(project, &source_root)
        .map_err(|e| format!("completion source unavailable: {e}"))?;
    let target_root = published_tip(&target).ok_or("executor publication unavailable")?;
    let product = crate::roots::current_product_tree(&target).map_err(|e| e.to_string())?;
    // A publication must describe these bytes, not just be an ancestor object.
    let repository = repo(&target)?;
    let published = repository
        .find_commit(
            target_root
                .parse()
                .map_err(|e: git2::Error| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .tree()
        .map_err(|e| e.to_string())?;
    let current = repository
        .find_tree(product.parse().map_err(|e: git2::Error| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let difference = repository
        .diff_tree_to_tree(Some(&published), Some(&current), None)
        .map_err(|e| e.to_string())?;
    if difference.deltas().any(|d| {
        d.new_file()
            .path()
            .or_else(|| d.old_file().path())
            .is_none_or(|p| !p.starts_with(".stateroot"))
    }) {
        return Err(
            "executor source has unpublished changes; publish before accepting compatibility"
                .into(),
        );
    }
    edge.acceptance = Some(SourceAcceptance {
        source_root,
        target_root,
        target_product_tree: product,
        accepted_by: actor.into(),
        evidence: evidence.into(),
    });
    declare_execution(project, id, expected_revision, declaration, actor)
}

fn reason(code: &str, detail: impl Into<String>, prerequisite: Option<String>) -> ReadinessReason {
    ReadinessReason {
        code: code.into(),
        detail: detail.into(),
        prerequisite,
    }
}

pub fn graph(project: &Path) -> PlanGraph {
    let all: BTreeMap<_, _> = list(project)
        .into_iter()
        .map(|p| (p.id.clone(), p))
        .collect();
    let mut nodes = Vec::new();
    for plan in all.values() {
        let declaration = plan.execution.as_ref().map(|e| &e.declaration);
        let assignment = declaration.and_then(|d| d.assignment.clone());
        let fork = assignment
            .as_ref()
            .and_then(|a| a.fork.clone())
            .or_else(|| claimed_fork(project, &plan.id));
        let target = execution_target(project, fork.as_deref());
        let mut blockers = Vec::new();
        let automatic_owner = if assignment.as_ref().is_some_and(|a| a.dispatch.is_some()) {
            match automatic_dispatch_owner(project) {
                Ok(owner) => {
                    if let Err(detail) = validate_automatic_authority(project, &owner, plan) {
                        blockers.push(reason("dispatch_authority_mismatch", detail, None));
                    }
                    Some(owner)
                }
                Err(detail) => {
                    blockers.push(reason("dispatch_owner_unavailable", detail, None));
                    None
                }
            }
        } else {
            None
        };
        // The authorized programme's accepted prerequisite evidence comes
        // from its owner, even when this projection is read in an executor.
        let authority_project = automatic_owner.as_deref().unwrap_or(project);
        let owner_plans: Option<BTreeMap<_, _>> = (authority_project != project).then(|| {
            list(authority_project)
                .into_iter()
                .map(|p| (p.id.clone(), p))
                .collect()
        });
        let authority_plans = owner_plans.as_ref().unwrap_or(&all);
        let mut dependencies = Vec::new();
        if let Some(declaration) = declaration {
            if let Err(error) = validate_declaration(&plan.id, declaration) {
                blockers.push(reason("invalid_graph", error, None));
            }
            for edge in &declaration.dependencies {
                let (state, detail) = if reaches(
                    &edge.plan_id,
                    &plan.id,
                    authority_plans,
                    &mut BTreeSet::new(),
                ) {
                    (
                        "cycle".into(),
                        "imported dependency graph contains a cycle".into(),
                    )
                } else if !valid_id(&edge.plan_id)
                    || edge.outputs.iter().any(|o| !valid_path(&o.path))
                {
                    (
                        "invalid_reference".into(),
                        "dependency reference is malformed".into(),
                    )
                } else {
                    dependency_state(authority_project, target.as_deref(), edge, authority_plans)
                };
                if state != "satisfied" {
                    blockers.push(reason(&state, &detail, Some(edge.plan_id.clone())));
                }
                let evidence_basis = if state != "satisfied" {
                    "unavailable"
                } else if edge.kind == DependencyKind::Completion {
                    "completion_receipt"
                } else if edge.acceptance.is_some() {
                    "native_compatibility_acceptance"
                } else {
                    "published_required_output_identity"
                };
                dependencies.push(DependencyReadiness {
                    dependency: edge.clone(),
                    state,
                    reason: detail,
                    evidence_basis: evidence_basis.into(),
                });
            }
        } else {
            blockers.push(reason(
                "dependencies_not_declared",
                "legacy plan makes no independence claim",
                None,
            ));
        }
        if body_digest(&body_path(project, &plan.id)).is_none() {
            blockers.push(reason(
                "plan_body_unavailable",
                "plan body is unreadable",
                None,
            ));
        }
        if drifted_since_approval(project, plan) {
            blockers.push(reason(
                "approval_drift",
                "plan body changed since approval",
                None,
            ));
        }
        if target.is_none() {
            blockers.push(reason(
                "target_unavailable",
                "assigned executor checkout unavailable",
                None,
            ));
        }
        if let Some(target) = &target {
            if target != project
                && body_digest(&body_path(target, &plan.id))
                    != body_digest(&body_path(project, &plan.id))
            {
                blockers.push(reason(
                    "target_plan_mismatch",
                    "executor does not have the declared plan body",
                    None,
                ));
            }
        }
        let mut workers = BTreeMap::new();
        let mut stores = vec![project.to_path_buf()];
        if let Some(target) = &target {
            if target != project {
                stores.push(target.clone());
            }
        }
        if let Some(owner) = automatic_owner {
            // Read the authority last: copied historical fork records must
            // never shadow its newer reservation/outcome for the same key.
            stores.retain(|store| store.canonicalize().ok().as_ref() != Some(&owner));
            stores.push(owner);
        }
        for store in stores {
            for entry in crate::delegation_status::read_entries(&store) {
                match entry {
                    crate::delegation_status::DelegationEntry::Record(_, record)
                        if record["plan_id"] == plan.id =>
                    {
                        let id = record["id"].as_str().unwrap_or_default().to_string();
                        workers.insert(
                            id.clone(),
                            WorkerObservation {
                                id,
                                status: crate::delegation_status::interpret(&record)
                                    .as_str()
                                    .into(),
                            },
                        );
                    }
                    crate::delegation_status::DelegationEntry::Degraded {
                        reason: detail, ..
                    } => blockers.push(reason("worker_evidence_degraded", detail, None)),
                    _ => {}
                }
            }
        }
        let workers: Vec<_> = workers.into_values().collect();
        let worker_state = if workers.iter().any(|w| w.status == "unknown") {
            "unknown"
        } else if workers.iter().any(|w| w.status == "running") {
            "running"
        } else if !workers.is_empty() {
            "ended"
        } else if assignment.is_some() || fork.is_some() || plan.status() == PlanStatus::Active {
            "awaiting_activity"
        } else {
            "unassigned"
        };
        if matches!(worker_state, "running" | "unknown" | "ended") {
            blockers.push(reason(
                &format!("worker_{worker_state}"),
                "existing worker observation requires reconciliation before another launch",
                None,
            ));
        }
        let readiness = match plan.status() {
            PlanStatus::Done => "complete",
            PlanStatus::Abandoned => "abandoned",
            _ if declaration.and_then(|d| d.superseded_by.as_ref()).is_some() => "superseded",
            PlanStatus::Draft => "not_authorized",
            _ if declaration.is_none() => "dependencies_not_declared",
            _ if !blockers.is_empty() => "blocked",
            _ => "ready",
        };
        let dispatch_key = assignment
            .as_ref()
            .and_then(|a| a.dispatch.as_ref())
            .map(|d| {
                format!(
                    "plan-{}",
                    &digest(format!("{}\0{}", plan.id, d.operation_id).as_bytes())[..48]
                )
            });
        nodes.push(PlanReadiness {
            id: plan.id.clone(),
            title: plan.title.clone(),
            status: plan.status.clone(),
            revision: revision(project, &plan.id).unwrap_or_default(),
            body_digest: body_digest(&body_path(project, &plan.id)),
            dependencies_declared: declaration.is_some(),
            dependencies,
            assignment,
            target_fork: fork,
            target_root: target.as_deref().and_then(published_tip),
            worker_state: worker_state.into(),
            workers,
            readiness: readiness.into(),
            blockers,
            owned_paths: declaration
                .map(|d| d.owned_paths.clone())
                .unwrap_or_default(),
            dispatch_key,
        });
    }
    // Unsupported/malformed imported plans stay visible as nodes, not missing work.
    if let Ok(entries) = std::fs::read_dir(plans_dir(project)) {
        for entry in entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        {
            let id = entry
                .path()
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if all.contains_key(&id) {
                continue;
            }
            nodes.push(PlanReadiness {
                id,
                title: String::new(),
                status: "unsupported".into(),
                revision: String::new(),
                body_digest: None,
                dependencies_declared: false,
                dependencies: Vec::new(),
                assignment: None,
                target_fork: None,
                target_root: None,
                worker_state: "unknown".into(),
                workers: Vec::new(),
                readiness: "unsupported".into(),
                blockers: vec![reason(
                    "unsupported",
                    "metadata is unreadable, malformed or unsupported; retained unchanged",
                    None,
                )],
                owned_paths: Vec::new(),
                dispatch_key: None,
            });
        }
    }
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let mut overlaps = Vec::new();
    for (i, left) in nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.readiness == "ready")
    {
        for right in nodes.iter().skip(i + 1).filter(|n| n.readiness == "ready") {
            let mut paths = BTreeSet::new();
            for a in &left.owned_paths {
                for b in &right.owned_paths {
                    if Path::new(a).starts_with(b) || Path::new(b).starts_with(a) {
                        paths.insert(a.clone());
                        paths.insert(b.clone());
                    }
                }
            }
            if !paths.is_empty() {
                overlaps.push(OwnershipOverlap {
                    plans: vec![left.id.clone(), right.id.clone()],
                    paths: paths.into_iter().collect(),
                });
            }
        }
    }
    let ready = nodes
        .iter()
        .filter(|n| n.readiness == "ready")
        .map(|n| n.id.clone())
        .collect();
    let observation_id =
        digest(&serde_json::to_vec(&(&nodes, &overlaps)).expect("serializable graph"));
    PlanGraph {
        schema_version: SCHEMA_GRAPH.into(),
        observation_id,
        nodes,
        ready,
        ownership_overlaps: overlaps,
    }
}

#[cfg(test)]
mod tests;
