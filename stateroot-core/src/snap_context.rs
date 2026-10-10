//! Build enriched transition evidence for root snapshots.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use std::io::Read;

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
    /// Explicit boundary identity; absence must not guess the latest session.
    pub session_id: Option<String>,
    /// Machine-local exact native locator (never pinned as a host path).
    pub transcript: Option<String>,
    /// Capture acknowledgement frozen with the exact boundary.
    pub capture_watermark: Option<Value>,
    /// Explicit references captured with the boundary, never a host inventory.
    pub artifact_refs: Vec<crate::fidelity::ArtifactRef>,
    /// Compact existing WS3 observations; never inferred versions.
    pub components: Vec<crate::fidelity::EnvironmentComponent>,
}

/// Compact evidence fingerprint; never label a partial transcript as complete.
pub fn transcript_fingerprint(path: &Path) -> Value {
    const LIMIT: u64 = 4 * 1024 * 1024;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return json!({"status":"unavailable: transcript read error"}),
    };
    if file.metadata().is_ok_and(|metadata| metadata.len() > LIMIT) {
        return json!({"status":"omitted: transcript fingerprint byte budget","limit":LIMIT});
    }
    let mut reader = file.take(LIMIT + 1);
    let mut hash = blake3::Hasher::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 8192];
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(_) => return json!({"status":"unavailable: transcript read error"}),
        };
        bytes += count as u64;
        if bytes > LIMIT {
            return json!({"status":"omitted: transcript fingerprint byte budget","limit":LIMIT});
        }
        hash.update(&buffer[..count]);
    }
    json!({"status":"complete","algorithm":"blake3","bytes":bytes,"digest":hash.finalize().to_hex().to_string()})
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
                "provenance": "native_observation",
                "availability": "available; delivery and use not established",
                "learning_ids": learning_ids,
                "skill_slugs": skill_slugs,
            });
        }

        let session = ctx.and_then(|context| {
            context.session_id.as_deref().and_then(|id| {
                handoff_continuity::verified_session(
                    home,
                    project_dir,
                    observed_harness,
                    id,
                    context.transcript.as_deref(),
                )
            })
        });
        if let Some(session) = session {
            let mut activity = json!({
                "provenance": "native_observation",
                "session_id": session.session_id,
                "lineage_ref": roots::lineage_refname(project_dir),
                "source_fingerprint": ctx.and_then(|context| context.transcript.as_ref()).map(|path|transcript_fingerprint(Path::new(path))),
                "transcript_ref": crate::transcripts::source_id(&session),
                "outcome": session.outcome.as_str(),
                "tool_events": session.tool_events,
                "capture_watermark":ctx.and_then(|context| context.capture_watermark.as_ref()),
            });
            if !session.files_touched.is_empty() {
                activity["files_touched"] = json!(session.files_touched);
            }
            if !session.failed_approaches.is_empty() {
                activity["failed_approaches"] = json!(session.failed_approaches);
            }
            evidence["activity"] = activity;
        } else {
            let captured = ctx
                .and_then(|context| context.capture_watermark.as_ref())
                .filter(|watermark| {
                    watermark["status"] == "present"
                        && watermark["harness"].as_str() == Some(observed_harness)
                        && watermark["session_id"].as_str()
                            == ctx.and_then(|context| context.session_id.as_deref())
                });
            evidence["activity"] = if let Some(watermark) = captured {
                json!({"provenance":"native_observation","session_id":watermark["session_id"],"lineage_ref":roots::lineage_refname(project_dir),"capture_watermark":watermark,"unavailable":"exact captured hook evidence available; verified native transcript history unavailable"})
            } else {
                json!({"provenance": "unknown", "unavailable": "no exact verified boundary session"})
            };
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
    fn oversized_transcript_fingerprints_never_claim_a_partial_digest_complete() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("large.jsonl");
        std::fs::File::create(&source)
            .unwrap()
            .set_len(8 * 1024 * 1024)
            .unwrap();
        let fingerprint = transcript_fingerprint(&source);
        assert_eq!(
            fingerprint["status"],
            "omitted: transcript fingerprint byte budget"
        );
        assert!(fingerprint.get("digest").is_none());
        let absent = transcript_fingerprint(&fixture.path().join("absent"));
        assert_eq!(absent["status"], "unavailable: transcript read error");
    }

    #[test]
    fn evidence_includes_reason_and_verified_count_for_genesis() {
        let dir = tempfile::tempdir().expect("tmpdir");
        std::fs::create_dir_all(dir.path().join(".stateroot")).unwrap();
        let evidence = build_snap_evidence(dir.path(), "cli", "genesis", "", "abc", None);
        assert_eq!(evidence["reason"], "genesis");
        assert_eq!(evidence["verified"]["files_changed"], 0);
    }

    #[test]
    fn explicit_session_binding_never_guesses_newer_and_availability_is_not_use() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let store = home.path().join(".claude/projects/example");
        std::fs::create_dir_all(&store).unwrap();
        for (id, timestamp) in [
            ("older", "2026-10-08T01:00:00Z"),
            ("newer", "2026-10-08T02:00:00Z"),
        ] {
            std::fs::write(store.join(format!("{id}.jsonl")), json!({"type":"user","sessionId":id,"cwd":project.path(),"timestamp":timestamp,"message":{"role":"user","content":id}}).to_string()).unwrap();
        }
        let ctx = SnapContext {
            home: home.path().into(),
            session_id: Some("older".into()),
            transcript: Some(store.join("older.jsonl").display().to_string()),
            ..Default::default()
        };
        let evidence =
            build_snap_evidence(project.path(), "claude", "bound", "", "fixture", Some(&ctx));
        assert_eq!(evidence["activity"]["session_id"], "older");
        assert_eq!(evidence["activity"]["provenance"], "native_observation");
        let missing = SnapContext {
            session_id: None,
            ..ctx
        };
        assert_eq!(
            build_snap_evidence(
                project.path(),
                "claude",
                "unbound",
                "",
                "fixture",
                Some(&missing)
            )["activity"]["provenance"],
            "unknown"
        );
    }
}
