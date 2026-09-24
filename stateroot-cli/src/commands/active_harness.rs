//! Project-local evidence of the harness currently driving the CLI.
//!
//! The marker is a PER-HARNESS ledger, not a single slot: parallel harnesses
//! each record their own stamp under a mandatory lock, and reads resolve
//! only when exactly one harness is present. Two active harnesses make "the
//! current harness" genuinely ambiguous — guessing would misattribute real
//! work, so ambiguity resolves to None and callers ask for an explicit id.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use serde_json::{json, Map, Value};
use stateroot_core::local_store;
use stateroot_core::local_store::now_rfc3339;
use stateroot_core::safe_io::{atomic_replace_json, ResourceLock};

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

/// Harness ids present in the marker, migrating the legacy single-slot shape
/// (`{"harness": id, "recorded_at": …}`) into the per-harness ledger view.
fn ledger_entries(marker: &Value) -> Vec<String> {
    let mut ids: Vec<String> = marker
        .get("harnesses")
        .and_then(Value::as_object)
        .map(|h| h.keys().cloned().collect())
        .unwrap_or_default();
    if let Some(legacy) = marker.get("harness").and_then(Value::as_str) {
        if !ids.iter().any(|id| id == legacy) {
            ids.push(legacy.to_string());
        }
    }
    ids.sort();
    ids
}

/// Record direct local evidence that a harness is active for this project.
pub fn record(project_dir: &Path, harness: &str) -> anyhow::Result<String> {
    let canonical = canonical_id(harness)?;
    let path = marker_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    // Read-merge-write under a mandatory lock so parallel harnesses never
    // lose each other's stamps to a last-writer-wins race.
    let lock = ResourceLock::acquire(path.with_extension("lock"))
        .map_err(|err| anyhow!("could not lock active harness marker: {err}"))?;
    let existing: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| json!({}));
    let legacy_stamp = existing.get("recorded_at").cloned();
    let legacy_id = existing.get("harness").and_then(Value::as_str);
    let mut harnesses = Map::new();
    for id in ledger_entries(&existing) {
        // Prior stamps keep their original time; only ours refreshes.
        let stamp = existing
            .get("harnesses")
            .and_then(|h| h.get(&id))
            .cloned()
            .or_else(|| {
                if legacy_id == Some(id.as_str()) {
                    legacy_stamp.clone().map(|at| json!({"recorded_at": at}))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| json!({}));
        harnesses.insert(id, stamp);
    }
    harnesses.insert(canonical.clone(), json!({ "recorded_at": now_rfc3339() }));
    let marker = json!({ "harnesses": Value::Object(harnesses) });
    atomic_replace_json(&path, &marker)
        .with_context(|| format!("could not write {}", path.display()))?;
    drop(lock);
    Ok(canonical)
}

/// Read the active harness only when it is unambiguous: exactly one recorded
/// harness resolves to it; zero or several resolve to None.
pub fn read(project_dir: &Path) -> anyhow::Result<Option<String>> {
    let path = marker_path(project_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("could not read {}", path.display())),
    };
    let marker: Value = serde_json::from_str(&text)
        .with_context(|| format!("invalid active harness marker at {}", path.display()))?;
    let ids = ledger_entries(&marker);
    match ids.as_slice() {
        [only] => canonical_id(only).map(Some),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_single_slot_marker_still_resolves() {
        let dir = tempfile::tempdir().expect("tmp");
        let marker = json!({"harness": "claude", "recorded_at": "2026-09-24T00:00:00Z"});
        std::fs::create_dir_all(marker_path(dir.path()).parent().expect("parent")).expect("mkdir");
        std::fs::write(marker_path(dir.path()), marker.to_string()).expect("seed");
        assert_eq!(read(dir.path()).expect("read"), Some("claude".to_string()));
    }

    #[test]
    fn one_harness_resolves_unambiguously() {
        let dir = tempfile::tempdir().expect("tmp");
        record(dir.path(), "kimi-code").expect("record");
        assert_eq!(read(dir.path()).expect("read"), Some("kimi".to_string()));
    }

    #[test]
    fn two_harnesses_are_ambiguous_and_neither_stamp_is_lost() {
        let dir = tempfile::tempdir().expect("tmp");
        record(dir.path(), "claude-code").expect("first");
        record(dir.path(), "codex").expect("second");
        assert_eq!(read(dir.path()).expect("read"), None);
        let text = std::fs::read_to_string(marker_path(dir.path())).expect("marker");
        let marker: Value = serde_json::from_str(&text).expect("json");
        let harnesses = marker["harnesses"].as_object().expect("ledger");
        assert_eq!(harnesses.len(), 2, "both stamps survive: {harnesses:?}");
        assert!(harnesses["claude"]["recorded_at"].is_string());
        assert!(harnesses["codex"]["recorded_at"].is_string());
    }

    #[test]
    fn re_recording_refreshes_only_the_owning_harness() {
        let dir = tempfile::tempdir().expect("tmp");
        record(dir.path(), "claude-code").expect("first");
        let first: Value = serde_json::from_str(
            &std::fs::read_to_string(marker_path(dir.path())).expect("marker"),
        )
        .expect("json");
        let first_at = first["harnesses"]["claude"]["recorded_at"].clone();
        record(dir.path(), "codex").expect("second");
        let second: Value = serde_json::from_str(
            &std::fs::read_to_string(marker_path(dir.path())).expect("marker"),
        )
        .expect("json");
        assert_eq!(
            second["harnesses"]["claude"]["recorded_at"], first_at,
            "another harness's stamp must not move"
        );
    }
}
