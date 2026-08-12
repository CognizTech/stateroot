//! Build enriched transition evidence for root snapshots.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::handoff_continuity;
use crate::learnings;
use crate::local_store;
use crate::roots;
use crate::skill_federation;

/// Optional inputs when creating a snapshot root.
#[derive(Debug, Clone, Default)]
pub struct SnapContext {
    /// User home (transcript + global learnings/skills).
    pub home: PathBuf,
    /// Observed harness id (falls back to the `harness` argument).
    pub harness: Option<String>,
}

fn active_learning_ids(project_dir: &Path, home: &Path) -> Vec<String> {
    learnings::read_scope(project_dir, home, "project")
        .into_iter()
        .filter(|l| l.status == "active")
        .map(|l| l.id)
        .collect()
}

fn current_handoff_seq(project_dir: &Path) -> Option<u64> {
    let packet = local_store::read_handoff_local(project_dir)
        .ok()
        .flatten()?;
    packet.get("seq").and_then(Value::as_u64)
}

fn count_files_changed(project_dir: &Path, from_root: &str, to_root: &str) -> Option<u64> {
    if from_root.is_empty() {
        return Some(0);
    }
    let delta = roots::diff_roots(project_dir, from_root, to_root, false, 0, 0).ok()?;
    Some(
        delta
            .get("files")
            .and_then(Value::as_array)
            .map(|items| items.len() as u64)
            .unwrap_or(0),
    )
}

/// Assemble transition evidence for a snapshot (verified + observed only).
pub fn build_snap_evidence(
    project_dir: &Path,
    harness: &str,
    reason: &str,
    from_root: &str,
    to_root: &str,
    ctx: Option<&SnapContext>,
) -> Value {
    let mut evidence = json!({ "reason": reason });

    let home = ctx.map(|c| c.home.as_path());
    let observed_harness = ctx
        .and_then(|c| c.harness.as_deref())
        .filter(|h| !h.is_empty())
        .unwrap_or(harness);

    if let Some(home) = home {
        let learning_ids = active_learning_ids(project_dir, home);
        let skill_slugs = skill_federation::active_portable_slugs(project_dir, home);
        if !learning_ids.is_empty() || !skill_slugs.is_empty() {
            evidence["context"] = json!({
                "learning_ids": learning_ids,
                "skill_slugs": skill_slugs,
            });
        }

        if let Some(session) =
            handoff_continuity::latest_verified_session(home, project_dir, observed_harness)
        {
            let mut activity = json!({
                "transcript_ref": crate::transcripts::source_id(&session),
                "outcome": session.outcome.as_str(),
                "tool_events": session.tool_events,
            });
            if !session.files_touched.is_empty() {
                activity["files_touched"] = json!(session.files_touched);
            }
            if !session.failed_approaches.is_empty() {
                activity["failed_approaches"] = json!(session.failed_approaches);
            }
            evidence["activity"] = activity;
        }
    }

    if let Some(count) = count_files_changed(project_dir, from_root, to_root) {
        evidence["verified"] = json!({ "files_changed": count });
    }

    if let Some(seq) = current_handoff_seq(project_dir) {
        evidence["handoff_seq"] = json!(seq);
    }

    evidence
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_includes_reason_and_verified_count_for_genesis() {
        let dir = tempfile::tempdir().expect("tmpdir");
        std::fs::create_dir_all(dir.path().join(".stateroot")).unwrap();
        let evidence = build_snap_evidence(dir.path(), "cli", "genesis", "", "abc", None);
        assert_eq!(evidence["reason"], "genesis");
        assert_eq!(evidence["verified"]["files_changed"], 0);
    }
}
