//! StateRoot-managed session identity for harnesses whose hook payloads carry
//! no conversation id (IDE/ACP adapters today — kimi-code under Cursor fired
//! hooks with neither `session_id` nor a usable `cwd`; tomorrow it may be
//! another harness).
//!
//! When the payload is anonymous, we anchor on `harness | hook-process-cwd`
//! and mint a session id, rotating it at real conversation boundaries:
//! a `session_start` event, the first event after a `session_end`, or an idle
//! gap past [`ANON_SESSION_GAP_MINUTES`]. The id is injected into the payload
//! at the hook boundary, so every downstream consumer (persona scheduler,
//! delivery ledger, todo federation) sees a true per-session id with no
//! further changes.
//!
//! Registry: `~/.stateroot/local/session-registry.json`, tmp+rename writes.
//! Concurrent same-anchor hook events are rare and sequential in practice; a
//! lost race costs at most one extra rotation (a duplicate FULL digest), never
//! a lost one.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::local_store;

/// Idle gap after which an anonymous anchor counts as a new session. Only a
/// fallback for harnesses that never fire `session_start`; real boundaries
/// come from the event stream.
pub const ANON_SESSION_GAP_MINUTES: i64 = 45;
/// Registry entries idle longer than this are pruned on write.
const PRUNE_DAYS: i64 = 7;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AnchorEntry {
    session_id: String,
    last_seen: String,
    last_event: String,
}

/// Registry file under the user's state home.
pub fn registry_path(home: &Path) -> PathBuf {
    home.join(".stateroot/local/session-registry.json")
}

fn mint(anchor: &str, now: &str) -> String {
    use sha2::{Digest, Sha256};
    // Rotation logic uses the second-precision `now`; minting needs stronger
    // uniqueness — two sessions can legitimately start within the same second.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut h = Sha256::new();
    h.update(anchor.as_bytes());
    h.update(now.as_bytes());
    h.update(nanos.to_string().as_bytes());
    h.update(seq.to_string().as_bytes());
    h.update(std::process::id().to_string().as_bytes());
    let hex = format!("{:x}", h.finalize());
    format!("anon-{}", &hex[..16])
}

fn parse_ts(s: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(s).ok()
}

/// The rotation decision for one anonymous anchor event, pure and testable.
/// Returns the session id and whether a rotation happened.
fn resolve_with(now: &str, anchor: &str, event: &str, entry: Option<&AnchorEntry>) -> String {
    let Some(entry) = entry else {
        return mint(anchor, now);
    };
    let rotate = event == "session_start"
        || entry.last_event == "session_end"
        || match (parse_ts(&entry.last_seen), parse_ts(now)) {
            (Some(then), Some(now)) => (now - then).num_minutes() > ANON_SESSION_GAP_MINUTES,
            _ => false,
        };
    if rotate {
        mint(anchor, now)
    } else {
        entry.session_id.clone()
    }
}

/// Ensure `payload` carries a session id. A harness-provided id always wins
/// and touches nothing; anonymous payloads get the managed id for their
/// anchor, rotated at conversation boundaries.
pub fn tag_payload(home: &Path, harness: &str, event: &str, cwd: &Path, payload: &mut Value) {
    if crate::digest_delivery::session_id_from_payload(payload).is_some() {
        return;
    }
    let anchor = format!("{harness}|{}", cwd.display());
    let now = local_store::now_rfc3339();
    let path = registry_path(home);
    let mut anchors: BTreeMap<String, AnchorEntry> = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let id = resolve_with(&now, &anchor, event, anchors.get(&anchor));
    anchors.insert(
        anchor,
        AnchorEntry {
            session_id: id.clone(),
            last_seen: now.clone(),
            last_event: event.to_string(),
        },
    );
    if let Some(cutoff) = parse_ts(&now).map(|n| n - chrono::Duration::days(PRUNE_DAYS)) {
        anchors.retain(|_, e| parse_ts(&e.last_seen).map(|t| t > cutoff).unwrap_or(false));
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    if let Ok(text) = serde_json::to_string_pretty(&anchors) {
        if fs::write(&tmp, format!("{text}\n")).is_ok() {
            if path.exists() {
                let _ = fs::remove_file(&path);
            }
            let _ = fs::rename(&tmp, &path);
        }
    }
    payload["session_id"] = Value::String(id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_session_id_always_wins_and_writes_nothing() {
        let home = tempfile::tempdir().unwrap();
        let mut payload = serde_json::json!({"session_id": "native-1"});
        tag_payload(
            home.path(),
            "kimi-code",
            "user_prompt_submit",
            Path::new("/x"),
            &mut payload,
        );
        assert_eq!(payload["session_id"], "native-1");
        assert!(!registry_path(home.path()).exists());
    }

    #[test]
    fn anonymous_events_share_then_rotate_on_session_start() {
        let home = tempfile::tempdir().unwrap();
        let cwd = Path::new("/host/cwd");
        let mut p1 = serde_json::json!({});
        tag_payload(home.path(), "kimi-code", "session_start", cwd, &mut p1);
        let s1 = p1["session_id"].as_str().unwrap().to_string();
        assert!(s1.starts_with("anon-"));

        let mut p2 = serde_json::json!({});
        tag_payload(home.path(), "kimi-code", "user_prompt_submit", cwd, &mut p2);
        assert_eq!(p2["session_id"], s1, "same conversation keeps the id");

        // A new conversation (demo take two) starts with session_start.
        let mut p3 = serde_json::json!({});
        tag_payload(home.path(), "kimi-code", "session_start", cwd, &mut p3);
        let s2 = p3["session_id"].as_str().unwrap().to_string();
        assert_ne!(s1, s2, "session_start rotates the id");

        let mut p4 = serde_json::json!({});
        tag_payload(home.path(), "kimi-code", "user_prompt_submit", cwd, &mut p4);
        assert_eq!(p4["session_id"], s2);
    }

    #[test]
    fn event_after_session_end_rotates() {
        let home = tempfile::tempdir().unwrap();
        let cwd = Path::new("/host/cwd");
        let mut p1 = serde_json::json!({});
        tag_payload(home.path(), "kimi-code", "user_prompt_submit", cwd, &mut p1);
        let s1 = p1["session_id"].as_str().unwrap().to_string();

        // session_end belongs to the ending session.
        let mut p2 = serde_json::json!({});
        tag_payload(home.path(), "kimi-code", "session_end", cwd, &mut p2);
        assert_eq!(p2["session_id"], s1);

        // Whatever comes next is a new conversation.
        let mut p3 = serde_json::json!({});
        tag_payload(home.path(), "kimi-code", "user_prompt_submit", cwd, &mut p3);
        assert_ne!(p3["session_id"].as_str().unwrap(), s1);
    }

    #[test]
    fn idle_gap_rotates() {
        let entry = AnchorEntry {
            session_id: "anon-old".into(),
            last_seen: "2026-09-03T06:00:00Z".into(),
            last_event: "user_prompt_submit".into(),
        };
        let same = resolve_with(
            "2026-09-03T06:30:00Z",
            "a|/x",
            "user_prompt_submit",
            Some(&entry),
        );
        assert_eq!(same, "anon-old");
        let rotated = resolve_with(
            "2026-09-03T07:30:00Z",
            "a|/x",
            "user_prompt_submit",
            Some(&entry),
        );
        assert_ne!(rotated, "anon-old");
    }

    #[test]
    fn anchors_are_isolated_per_harness_and_cwd() {
        let home = tempfile::tempdir().unwrap();
        let mut a = serde_json::json!({});
        tag_payload(
            home.path(),
            "kimi-code",
            "session_start",
            Path::new("/w1"),
            &mut a,
        );
        let mut b = serde_json::json!({});
        tag_payload(
            home.path(),
            "kimi-code",
            "session_start",
            Path::new("/w2"),
            &mut b,
        );
        assert_ne!(a["session_id"], b["session_id"]);
        let mut c = serde_json::json!({});
        tag_payload(
            home.path(),
            "cursor",
            "session_start",
            Path::new("/w1"),
            &mut c,
        );
        assert_ne!(a["session_id"], c["session_id"]);
    }
}
