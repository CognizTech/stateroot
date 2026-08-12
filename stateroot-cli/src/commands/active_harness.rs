//! Project-local evidence of the harness currently driving the CLI.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use serde_json::{json, Value};
use stateroot_core::local_store;
use stateroot_core::local_store::now_rfc3339;

const ACTIVE_HARNESS_PATH: &str = "local/active-harness.json";

fn marker_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(ACTIVE_HARNESS_PATH)
}

/// Normalize an id or alias and reject values absent from the shared registry.
pub fn canonical_id(input: &str) -> anyhow::Result<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("harness id is empty");
    }
    let canonical = stateroot_core::skill_federation::normalize_harness(trimmed);
    let registry = stateroot_core::skill_federation::load_registry().map_err(|err| anyhow!(err))?;
    if registry.harnesses.iter().any(|entry| entry.id == canonical) {
        Ok(canonical)
    } else {
        bail!("unknown harness '{input}'");
    }
}

/// Record direct local evidence that a harness is active for this project.
pub fn record(project_dir: &Path, harness: &str) -> anyhow::Result<String> {
    let canonical = canonical_id(harness)?;
    let path = marker_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let marker = json!({
        "harness": canonical.clone(),
        "recorded_at": now_rfc3339(),
    });
    let text = serde_json::to_string_pretty(&marker)?;
    std::fs::write(&path, format!("{text}\n"))
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok(canonical)
}

/// Read and validate the last locally observed active harness.
pub fn read(project_dir: &Path) -> anyhow::Result<Option<String>> {
    let path = marker_path(project_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("could not read {}", path.display())),
    };
    let marker: Value = serde_json::from_str(&text)
        .with_context(|| format!("invalid active harness marker at {}", path.display()))?;
    let harness = marker
        .get("harness")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("active harness marker has no harness id"))?;
    canonical_id(harness).map(Some)
}
