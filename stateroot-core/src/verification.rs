//! Optional command observations, not semantic acceptance or execution authority.
//! Started/finished records are immutable and share the existing plans writer.
use crate::{local_store, plans, roots};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const SCHEMA: &str = "stateroot.verification.v1";
const STARTED: &str = "started; completion unavailable";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandObservation {
    pub schema_version: String,
    pub id: String,
    pub project_id: String,
    pub plan_id: String,
    pub plan_body_sha256: String,
    pub product_tree: String,
    pub source_root: Option<String>,
    pub actor: String,
    pub command: Vec<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub exit_code: Option<i32>,
    pub status: String,
    pub output_digests: Vec<String>,
    pub error: Option<String>,
}

fn directory(project: &Path) -> PathBuf {
    local_store::root(project).join("plans/verification")
}

fn valid_id(id: &str) -> bool {
    id.strip_prefix("v_")
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}

fn project_identity(project: &Path) -> Result<String, String> {
    if std::fs::metadata(local_store::root(project).join(local_store::MANIFEST_PATH))
        .is_ok_and(|metadata| metadata.len() > 256 * 1024)
    {
        return Err("project identity metadata exceeds its observation budget".into());
    }
    let manifest = local_store::read_manifest(project)
        .map_err(|e| e.to_string())?
        .ok_or("project identity unavailable")?;
    if manifest["schema_version"] != local_store::SCHEMA_MANIFEST_V1 {
        return Err("unsupported project manifest schema".into());
    }
    manifest["project_id"]
        .as_str()
        .map(str::to_owned)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "project identity unavailable".into())
}

fn save(project: &Path, record: &CommandObservation, phase: &str) -> Result<(), String> {
    let _guard = plans::write_guard(project)?;
    let dir = directory(project);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.{}.json", record.id, phase));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())?;
    local_store::report_written(
        project,
        &format!("plans/verification/{}.{}.json", record.id, phase),
    );
    Ok(())
}

pub fn start(
    project: &Path,
    plan: &str,
    actor: &str,
    command: &[String],
) -> Result<CommandObservation, String> {
    if command.is_empty() {
        return Err("verification command is empty".into());
    }
    if command.len() > 1024 || command.iter().map(String::len).sum::<usize>() > 256 * 1024 {
        return Err(
            "verification argv metadata exceeds its capture budget; no command executed".into(),
        );
    }
    let (meta, body) = plans::load(project, plan)
        .ok_or_else(|| format!("unknown or unsupported plan `{plan}`"))?;
    let record = CommandObservation {
        schema_version: SCHEMA.into(),
        id: format!("v_{}", uuid::Uuid::now_v7()),
        project_id: project_identity(project)?,
        plan_id: meta.id,
        plan_body_sha256: plans::body_digest(&body).ok_or("plan body unreadable")?,
        product_tree: roots::current_product_tree(project).map_err(|e| e.to_string())?,
        source_root: roots::latest_root(project).map_err(|e| e.to_string())?,
        actor: actor.into(),
        command: command.to_vec(),
        started_at: local_store::now_rfc3339(),
        finished_at: None,
        exit_code: None,
        status: STARTED.into(),
        output_digests: Vec::new(),
        error: None,
    };
    save(project, &record, "started")?;
    Ok(record)
}

/// Staleness is input identity, not a clock-based expiry. Legacy prose is
/// deliberately not parsed or promoted into this observation tier.
pub fn inputs_match(project: &Path, plan: &str, record: &CommandObservation) -> Result<(), String> {
    if record.schema_version != SCHEMA || !valid_id(&record.id) {
        return Err("unsupported verification schema or identity".into());
    }
    let (meta, body) = plans::load(project, plan).ok_or("plan unavailable or unsupported")?;
    if project_identity(project)? != record.project_id
        || meta.id != record.plan_id
        || plans::body_digest(&body).as_deref() != Some(&record.plan_body_sha256)
        || roots::current_product_tree(project).map_err(|e| e.to_string())? != record.product_tree
    {
        return Err("verification is stale or belongs to another project/plan/source".into());
    }
    Ok(())
}

pub fn finish(
    project: &Path,
    mut record: CommandObservation,
    exit_code: Option<i32>,
    error: Option<String>,
    output_digests: Vec<String>,
) -> Result<CommandObservation, String> {
    check_started(project, &record)?;
    record.finished_at = Some(local_store::now_rfc3339());
    record.exit_code = exit_code;
    record.error = error;
    record.output_digests = output_digests;
    record.status = if inputs_match(project, &record.plan_id, &record).is_err() {
        "stale inputs"
    } else if record.error.is_some() || exit_code != Some(0) {
        "observed command failure"
    } else {
        "observed exit zero; semantic acceptance not established"
    }
    .into();
    save(project, &record, "finished")?;
    Ok(record)
}

pub fn load(project: &Path, id: &str) -> Result<CommandObservation, String> {
    if !valid_id(id) {
        return Err("invalid verification id".into());
    }
    let bytes = bounded_record_bytes(&directory(project).join(format!("{id}.finished.json")))?;
    let record: CommandObservation = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if record.id != id || record.schema_version != SCHEMA {
        return Err("unsupported or mismatched verification record".into());
    }
    check_started(project, &record)?;
    Ok(record)
}

fn bounded_record_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("verification metadata byte budget exceeded".into());
    }
    Ok(bytes)
}

fn check_started(project: &Path, record: &CommandObservation) -> Result<(), String> {
    if !valid_id(&record.id) || record.schema_version != SCHEMA {
        return Err("unsupported verification identity".into());
    }
    let original: serde_json::Value = serde_json::from_slice(&bounded_record_bytes(
        &directory(project).join(format!("{}.started.json", record.id)),
    )?)
    .map_err(|e| e.to_string())?;
    let mut expected = record.clone();
    expected.finished_at = None;
    expected.exit_code = None;
    expected.status = STARTED.into();
    expected.output_digests.clear();
    expected.error = None;
    if serde_json::to_value(expected).map_err(|e| e.to_string())? != original {
        return Err("verification attempt does not match its original started record".into());
    }
    Ok(())
}

pub fn validate(project: &Path, plan: &str, id: &str) -> Result<CommandObservation, String> {
    let record = load(project, id)?;
    inputs_match(project, plan, &record)?;
    if record.finished_at.is_none()
        || record.exit_code != Some(0)
        || record.error.is_some()
        || record.status != "observed exit zero; semantic acceptance not established"
    {
        return Err(
            "verification did not observe successful completion on unchanged inputs".into(),
        );
    }
    Ok(record)
}

pub fn output_digest(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Read-only bounded observation inventory. Before/after input equality is
/// reported as such; it is not workspace isolation or semantic acceptance.
pub fn summary(project: &Path) -> serde_json::Value {
    let mut observed = 0usize;
    let mut stale = 0usize;
    let mut failed = 0usize;
    let mut incomplete_attempts = 0usize;
    let mut unavailable = 0usize;
    let mut incomplete = false;
    let mut source_cache = std::collections::BTreeMap::new();
    let mut started = std::collections::BTreeSet::new();
    let mut finished = std::collections::BTreeSet::new();
    let metadata = plans::diagnostic_plans(project);
    let plans: std::collections::BTreeMap<_, _> = metadata
        .iter()
        .map(|meta| {
            let hash = plans::diagnostic_body_digest(&plans::body_path(project, &meta.id));
            (meta.id.clone(), hash)
        })
        .collect();
    let identity = project_identity(project).ok();
    let entries = match std::fs::read_dir(directory(project)) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => {
            unavailable += 1;
            incomplete = true;
            None
        }
    };
    if let Some(entries) = entries {
        for (index, entry) in entries.enumerate() {
            if index >= 512 {
                incomplete = true;
                break;
            }
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(_) => {
                    unavailable += 1;
                    continue;
                }
            };
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if let Some(id) = name.strip_suffix(".started.json") {
                let original = bounded_record_bytes(&path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<CommandObservation>(&bytes).ok());
                if original.is_some_and(|record| {
                    valid_id(id)
                        && record.id == id
                        && record.schema_version == SCHEMA
                        && record.status == STARTED
                        && !record.command.is_empty()
                }) {
                    started.insert(id.to_string());
                } else {
                    unavailable += 1;
                }
            }
            if let Some(id) = name.strip_suffix(".finished.json") {
                finished.insert(id.to_string());
                let record = match load(project, id) {
                    Ok(record) => record,
                    Err(_) => {
                        unavailable += 1;
                        continue;
                    }
                };
                if record.finished_at.is_none()
                    || !matches!(
                        record.status.as_str(),
                        "observed exit zero; semantic acceptance not established"
                            | "observed command failure"
                            | "stale inputs"
                    )
                {
                    unavailable += 1;
                    continue;
                }
                if record.exit_code != Some(0) || record.error.is_some() {
                    failed += 1;
                    continue;
                }
                if identity.is_none()
                    || plans
                        .get(&record.plan_id)
                        .is_some_and(|hash| hash.is_none())
                {
                    unavailable += 1;
                    incomplete = true;
                    continue;
                }
                if identity.as_deref() != Some(&record.project_id)
                    || plans.get(&record.plan_id).and_then(|hash| hash.as_deref())
                        != Some(&record.plan_body_sha256)
                    || record.status == "stale inputs"
                {
                    stale += 1;
                    continue;
                }
                let source = source_cache
                    .entry(record.product_tree.clone())
                    .or_insert_with(|| roots::product_tree_matches(project, &record.product_tree));
                match source {
                    Ok(true) => observed += 1,
                    Ok(false) => stale += 1,
                    Err(_) => {
                        unavailable += 1;
                        incomplete = true;
                    }
                }
                if source_cache.len() >= 8 {
                    incomplete = true;
                    break;
                }
            }
        }
    }
    incomplete_attempts += started.difference(&finished).count();
    let declared = metadata
        .iter()
        .filter(|meta| {
            meta.completion_receipt
                .as_ref()
                .is_some_and(|receipt| receipt.verification_ids.is_empty())
        })
        .count();
    let legacy = metadata
        .iter()
        .filter(|meta| meta.status == "done" && meta.completion_receipt.is_none())
        .count();
    serde_json::json!({"bound_command_observations":observed,"declared_completions":declared,"legacy_completions_without_receipts":legacy,"stale":stale,"failed":failed,"unfinished":incomplete_attempts,"unavailable_or_unsupported":unavailable,"incomplete":incomplete,"scope":"matching observed before/after eligible source and plan body; semantic acceptance, execution isolation and environment reproduction not established"})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, plans::PlanMeta) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(local_store::root(dir.path())).unwrap();
        std::fs::write(local_store::root(dir.path()).join("manifest.json"),
            serde_json::to_vec(&serde_json::json!({"schema_version":local_store::SCHEMA_MANIFEST_V1,"project_id":"project-one"})).unwrap()).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "*.ignored\n").unwrap();
        std::fs::write(dir.path().join("code.txt"), "checked bytes").unwrap();
        let plan = plans::record(
            dir.path(),
            "fixture",
            "codex",
            None,
            "# Original requirements",
        )
        .unwrap();
        (dir, plan)
    }

    #[test]
    fn observations_bind_actual_inputs_not_bookkeeping_or_age() {
        let (dir, plan) = fixture();
        let started = start(dir.path(), &plan.id, "codex", &["fixture".into()]).unwrap();
        let finished = finish(
            dir.path(),
            started.clone(),
            Some(0),
            None,
            vec!["digest".into()],
        )
        .unwrap();
        assert!(validate(dir.path(), &plan.id, &finished.id).is_ok());
        let before =
            std::fs::read(directory(dir.path()).join(format!("{}.finished.json", finished.id)))
                .unwrap();
        let root_before = roots::latest_root(dir.path()).unwrap();
        assert_eq!(summary(dir.path())["bound_command_observations"], 1);
        assert_eq!(
            std::fs::read(directory(dir.path()).join(format!("{}.finished.json", finished.id)))
                .unwrap(),
            before
        );
        assert_eq!(roots::latest_root(dir.path()).unwrap(), root_before);
        std::fs::write(
            local_store::root(dir.path()).join("bookkeeping.json"),
            "later",
        )
        .unwrap();
        std::fs::write(dir.path().join("private.ignored"), "private").unwrap();
        assert!(validate(dir.path(), &plan.id, &finished.id).is_ok());
        assert!(finish(dir.path(), started.clone(), Some(0), None, Vec::new()).is_err());
        let mut forged = started;
        forged.id = format!("v_{}", uuid::Uuid::now_v7());
        assert!(finish(dir.path(), forged, Some(0), None, Vec::new()).is_err());
        let other =
            plans::record(dir.path(), "other", "codex", None, "# Same requirements").unwrap();
        assert!(validate(dir.path(), &other.id, &finished.id).is_err());
        std::fs::write(dir.path().join("code.txt"), "different source").unwrap();
        assert!(validate(dir.path(), &plan.id, &finished.id).is_err());
        assert_eq!(summary(dir.path())["stale"], 1);
    }

    #[test]
    fn changed_criteria_source_failure_and_interruption_are_not_success() {
        let (dir, plan) = fixture();
        let interrupted = start(dir.path(), &plan.id, "codex", &["fixture".into()]).unwrap();
        assert!(validate(dir.path(), &plan.id, &interrupted.id).is_err());
        let failed = finish(dir.path(), interrupted, Some(1), None, Vec::new()).unwrap();
        assert!(validate(dir.path(), &plan.id, &failed.id).is_err());
        let changed = start(dir.path(), &plan.id, "codex", &["fixture".into()]).unwrap();
        std::fs::write(
            local_store::root(dir.path())
                .join("plans")
                .join(format!("{}.md", plan.id)),
            "# Changed requirements",
        )
        .unwrap();
        let changed = finish(dir.path(), changed, Some(0), None, Vec::new()).unwrap();
        assert_eq!(changed.status, "stale inputs");
        assert!(validate(dir.path(), &plan.id, &changed.id).is_err());
        let moved = start(dir.path(), &plan.id, "codex", &["fixture".into()]).unwrap();
        std::fs::write(dir.path().join("code.txt"), "during execution").unwrap();
        assert_eq!(
            finish(dir.path(), moved, Some(0), None, Vec::new())
                .unwrap()
                .status,
            "stale inputs"
        );
    }

    #[test]
    fn foreign_and_future_records_never_complete_a_plan() {
        let (dir, plan) = fixture();
        let observation = start(dir.path(), &plan.id, "codex", &["fixture".into()]).unwrap();
        let observation = finish(dir.path(), observation, Some(0), None, Vec::new()).unwrap();
        let manifest = local_store::root(dir.path()).join("manifest.json");
        std::fs::write(
            &manifest,
            serde_json::to_vec(&serde_json::json!({"schema_version":local_store::SCHEMA_MANIFEST_V1,"project_id":"another-project"})).unwrap(),
        )
        .unwrap();
        assert!(validate(dir.path(), &plan.id, &observation.id).is_err());
        let path = directory(dir.path()).join(format!("{}.finished.json", observation.id));
        let mut future = serde_json::to_value(observation.clone()).unwrap();
        future["schema_version"] = serde_json::json!("stateroot.verification.v99");
        let bytes = serde_json::to_vec(&future).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(load(dir.path(), &observation.id).is_err());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert!(validate(dir.path(), &plan.id, "../foreign").is_err());
    }
}
