//! Machine-local ledger for identity/resume digest delivery.
//!
//! Hook injection and `stateroot resume` share one file under
//! `.stateroot/local/` so a consumed digest is not reprinted, a missed
//! session-start can still inject on the first prompt, and two chats are
//! not collapsed into one delivery slot.

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::local_store::{self, SESSION_STALE_MINUTES};
use crate::skill_federation::normalize_harness;

/// Schema id written into the ledger.
pub const SCHEMA_VERSION: &str = "stateroot.digest_delivery.v1";
/// Near-simultaneous hook retries without a session id are collapsed.
pub const RETRY_DEBOUNCE_MS: i64 = 5_000;
/// Ring-buffer cap.
pub const MAX_ENTRIES: usize = 64;

/// Why a digest is being considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryIntent {
    /// Ordinary session identity/resume.
    Session,
    /// Compaction re-injection (never suppressed by session delivery).
    Compact,
}

/// Which CLI surface is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryChannel {
    /// Lifecycle hook stdout.
    Hook,
    /// `stateroot resume`.
    Resume,
}

/// Outcome of [`should_deliver`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryDecision {
    /// Print the digest.
    pub deliver: bool,
    /// Stable reason token.
    pub reason: &'static str,
}

impl DeliveryDecision {
    fn yes(reason: &'static str) -> Self {
        Self {
            deliver: true,
            reason,
        }
    }

    fn no(reason: &'static str) -> Self {
        Self {
            deliver: false,
            reason,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Ledger {
    schema_version: String,
    entries: Vec<LedgerEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LedgerEntry {
    harness: String,
    handoff_seq: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    content_fp: String,
    intent: DeliveryIntent,
    channel: DeliveryChannel,
    #[serde(default)]
    event: String,
    delivered_at: String,
}

/// Extract a conversation/session id from a hook payload when present.
pub fn session_id_from_payload(payload: &Value) -> Option<String> {
    for key in [
        "session_id",
        "conversation_id",
        "generation_id",
        "sessionId",
        "conversationId",
        "generationId",
        "messageID",
        "composerId",
    ] {
        if let Some(id) = payload.get(key).and_then(Value::as_str) {
            let id = id.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    if let Some(nested) = payload.get("session").and_then(Value::as_object) {
        for key in ["id", "session_id", "conversation_id"] {
            if let Some(id) = nested.get(key).and_then(Value::as_str) {
                let id = id.trim();
                if !id.is_empty() {
                    return Some(id.to_string());
                }
            }
        }
    }
    None
}

/// Current handoff sequence, or `0` when none exists yet.
pub fn handoff_seq(project_dir: &Path) -> i64 {
    local_store::read_handoff_local(project_dir)
        .ok()
        .flatten()
        .and_then(|handoff| handoff.get("seq").and_then(Value::as_i64))
        .unwrap_or(0)
}

/// Fingerprint of identity + handoff state that should invalidate a prior delivery.
pub fn content_fingerprint(project_dir: &Path) -> String {
    let seq = handoff_seq(project_dir);
    let mut handoff = local_store::read_handoff_local(project_dir)
        .ok()
        .flatten()
        .unwrap_or(Value::Null);
    if let Some(obj) = handoff.as_object_mut() {
        // Acceptance is updated by resume itself — exclude from delivery fp.
        obj.remove("accepted_by");
    }
    let home = crate::harness_install::home_dir().ok();
    let user = home
        .as_deref()
        .and_then(crate::user_profile::read)
        .unwrap_or_default();
    let soul = home
        .as_ref()
        .and_then(|h| crate::soul::read_canonical(h))
        .unwrap_or_default();
    let persona_cache = crate::config::config_dir()
        .ok()
        .and_then(|dir| fs::read_to_string(dir.join("persona.md")).ok())
        .unwrap_or_default();
    let overlay = local_store::root(project_dir).join("soul/OVERLAY.md");
    let overlay_text = fs::read_to_string(overlay).unwrap_or_default();
    let payload = json!({
        "handoff": handoff,
        "overlay": overlay_text,
        "persona_cache": persona_cache,
        "seq": seq,
        "soul": soul,
        "user": user,
    });
    crate::canonical::content_hash(&payload).unwrap_or_else(|_| {
        format!(
            "sha256:seq-{seq}-len-{}",
            user.len() + soul.len() + persona_cache.len()
        )
    })
}

/// Decide whether to print a digest.
pub fn should_deliver(
    project_dir: &Path,
    harness: &str,
    intent: DeliveryIntent,
    channel: DeliveryChannel,
    payload: &Value,
    content_fp: &str,
    force: bool,
) -> DeliveryDecision {
    if force {
        return DeliveryDecision::yes("forced");
    }
    if intent == DeliveryIntent::Compact {
        return DeliveryDecision::yes("compact");
    }
    let harness = normalize_harness(harness);
    let seq = handoff_seq(project_dir);
    let session_id = session_id_from_payload(payload);
    let ledger = load_or_migrate(project_dir);
    decide(
        &ledger,
        &harness,
        seq,
        session_id.as_deref(),
        content_fp,
        channel,
        &local_store::now_rfc3339(),
    )
}

/// Record a successful print.
pub fn mark_delivered(
    project_dir: &Path,
    harness: &str,
    intent: DeliveryIntent,
    channel: DeliveryChannel,
    event: &str,
    payload: &Value,
    content_fp: &str,
) {
    let _lock = LedgerLock::acquire(project_dir);
    let mut ledger = load_or_migrate(project_dir);
    ledger.entries.push(LedgerEntry {
        harness: normalize_harness(harness),
        handoff_seq: handoff_seq(project_dir),
        session_id: session_id_from_payload(payload),
        content_fp: content_fp.to_string(),
        intent,
        channel,
        event: event.to_string(),
        delivered_at: local_store::now_rfc3339(),
    });
    if ledger.entries.len() > MAX_ENTRIES {
        let drop = ledger.entries.len() - MAX_ENTRIES;
        ledger.entries.drain(0..drop);
    }
    let _ = write_ledger(project_dir, &ledger);
}

fn decide(
    ledger: &Ledger,
    harness: &str,
    seq: i64,
    session_id: Option<&str>,
    content_fp: &str,
    channel: DeliveryChannel,
    now: &str,
) -> DeliveryDecision {
    let session_entries: Vec<&LedgerEntry> = ledger
        .entries
        .iter()
        .filter(|e| e.intent == DeliveryIntent::Session && e.harness == harness)
        .collect();

    match channel {
        DeliveryChannel::Resume => {
            // New-session staleness (same SESSION_STALE_MINUTES rule as the
            // persona scheduler): a matching entry only suppresses a reprint
            // while it is FRESH. An older match belongs to an earlier
            // session — deliver again instead of staying silenced
            // project-forever; a malformed timestamp counts as stale.
            let duplicate = session_entries.iter().any(|e| {
                e.handoff_seq == seq && e.content_fp == content_fp && is_fresh(&e.delivered_at, now)
            });
            if duplicate {
                return DeliveryDecision::no("duplicate");
            }
            DeliveryDecision::yes("fresh")
        }
        DeliveryChannel::Hook => {
            if let Some(session_id) = session_id {
                if session_entries.iter().any(|e| {
                    e.session_id.as_deref() == Some(session_id) && e.content_fp == content_fp
                }) {
                    return DeliveryDecision::no("duplicate");
                }
                if session_entries.iter().any(|e| {
                    e.session_id.as_deref() == Some(session_id) && e.content_fp != content_fp
                }) {
                    return DeliveryDecision::yes("stale_content");
                }
                return DeliveryDecision::yes("fresh");
            }
            if let Some(last) = session_entries
                .iter()
                .rev()
                .find(|e| e.session_id.is_none())
            {
                if within_debounce(&last.delivered_at, now) {
                    return DeliveryDecision::no("retry_debounce");
                }
            }
            DeliveryDecision::yes("fresh")
        }
    }
}

/// True when `delivered_at` is less than SESSION_STALE_MINUTES older than
/// `now`. Malformed timestamps count as stale (deliver fresh, never
/// suppress).
fn is_fresh(delivered_at: &str, now: &str) -> bool {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(delivered_at) else {
        return false;
    };
    let Ok(now) = chrono::DateTime::parse_from_rfc3339(now) else {
        return false;
    };
    (now - then).num_minutes() < SESSION_STALE_MINUTES
}

fn within_debounce(delivered_at: &str, now: &str) -> bool {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(delivered_at) else {
        return false;
    };
    let Ok(now) = chrono::DateTime::parse_from_rfc3339(now) else {
        return false;
    };
    (now.timestamp_millis() - then.timestamp_millis()).abs() <= RETRY_DEBOUNCE_MS
}

fn ledger_path(project_dir: &Path) -> std::path::PathBuf {
    local_store::root(project_dir).join(local_store::DIGEST_DELIVERY_PATH)
}

fn load_or_migrate(project_dir: &Path) -> Ledger {
    let path = ledger_path(project_dir);
    if let Ok(text) = fs::read_to_string(&path) {
        if let Ok(ledger) = serde_json::from_str::<Ledger>(&text) {
            if ledger.schema_version == SCHEMA_VERSION {
                return ledger;
            }
        }
    }
    let mut ledger = Ledger {
        schema_version: SCHEMA_VERSION.to_string(),
        entries: Vec::new(),
    };
    migrate_legacy(project_dir, &mut ledger);
    if !ledger.entries.is_empty() {
        let _ = write_ledger(project_dir, &ledger);
    }
    ledger
}

fn migrate_legacy(project_dir: &Path, ledger: &mut Ledger) {
    let root = local_store::root(project_dir);
    for (rel, channel) in [
        (
            local_store::LEGACY_HOOK_RESUME_MARKER,
            DeliveryChannel::Hook,
        ),
        (local_store::LEGACY_RESUME_MARKER, DeliveryChannel::Resume),
    ] {
        let path = root.join(rel);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(marker) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let harness = marker
            .get("harness")
            .and_then(Value::as_str)
            .unwrap_or("unattributed");
        ledger.entries.push(LedgerEntry {
            harness: normalize_harness(harness),
            handoff_seq: marker
                .get("handoff_seq")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            session_id: marker
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            content_fp: marker
                .get("content_fp")
                .and_then(Value::as_str)
                .unwrap_or("legacy")
                .to_string(),
            intent: DeliveryIntent::Session,
            channel,
            event: "migrated".into(),
            delivered_at: marker
                .get("delivered_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        });
    }
}

fn write_ledger(project_dir: &Path, ledger: &Ledger) -> std::io::Result<()> {
    let path = ledger_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(ledger).unwrap_or_else(|_| "{}".into());
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, format!("{text}\n"))?;
    fs::rename(&tmp, path)
}

struct LedgerLock {
    path: std::path::PathBuf,
}

impl LedgerLock {
    fn acquire(project_dir: &Path) -> Option<Self> {
        let path = local_store::root(project_dir).join("local/digest-delivery.v1.json.lock");
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        for _ in 0..40 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Some(Self { path }),
                Err(_) => thread::sleep(Duration::from_millis(15)),
            }
        }
        None
    }
}

impl Drop for LedgerLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_store::init_skeleton;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        init_skeleton(dir.path(), "p1", "demo", "default").unwrap();
        dir
    }

    #[test]
    fn session_ids_are_independent() {
        let dir = project();
        let fp = content_fingerprint(dir.path());
        mark_delivered(
            dir.path(),
            "cursor",
            DeliveryIntent::Session,
            DeliveryChannel::Hook,
            "session_start",
            &json!({"conversation_id": "a"}),
            &fp,
        );
        let again_a = should_deliver(
            dir.path(),
            "cursor",
            DeliveryIntent::Session,
            DeliveryChannel::Hook,
            &json!({"conversation_id": "a"}),
            &fp,
            false,
        );
        let chat_b = should_deliver(
            dir.path(),
            "cursor",
            DeliveryIntent::Session,
            DeliveryChannel::Hook,
            &json!({"conversation_id": "b"}),
            &fp,
            false,
        );
        assert!(!again_a.deliver);
        assert!(chat_b.deliver);
    }

    #[test]
    fn anonymous_retries_debounce_but_later_chat_redelivers() {
        let dir = project();
        let fp = content_fingerprint(dir.path());
        mark_delivered(
            dir.path(),
            "claude-code",
            DeliveryIntent::Session,
            DeliveryChannel::Hook,
            "session_start",
            &json!({}),
            &fp,
        );
        let retry = should_deliver(
            dir.path(),
            "claude",
            DeliveryIntent::Session,
            DeliveryChannel::Hook,
            &json!({}),
            &fp,
            false,
        );
        assert_eq!(retry.reason, "retry_debounce");
        assert!(!retry.deliver);

        let _lock = LedgerLock::acquire(dir.path());
        let mut ledger = load_or_migrate(dir.path());
        ledger.entries[0].delivered_at = "2020-01-01T00:00:00Z".into();
        write_ledger(dir.path(), &ledger).unwrap();
        drop(_lock);

        let later = should_deliver(
            dir.path(),
            "claude-code",
            DeliveryIntent::Session,
            DeliveryChannel::Hook,
            &json!({}),
            &fp,
            false,
        );
        assert!(later.deliver, "{}", later.reason);
    }

    #[test]
    fn hook_then_resume_is_cross_channel_duplicate() {
        let dir = project();
        let fp = content_fingerprint(dir.path());
        mark_delivered(
            dir.path(),
            "codex",
            DeliveryIntent::Session,
            DeliveryChannel::Hook,
            "session_start",
            &json!({}),
            &fp,
        );
        let resume = should_deliver(
            dir.path(),
            "codex",
            DeliveryIntent::Session,
            DeliveryChannel::Resume,
            &json!({}),
            &fp,
            false,
        );
        assert!(!resume.deliver);
        assert_eq!(resume.reason, "duplicate");
    }

    /// Rewrite the single ledger entry in place (the project() fixture
    /// records exactly one delivery per test before this runs).
    fn edit_only_entry(dir: &tempfile::TempDir, f: impl FnOnce(&mut LedgerEntry)) {
        let _lock = LedgerLock::acquire(dir.path());
        let mut ledger = load_or_migrate(dir.path());
        f(&mut ledger.entries[0]);
        write_ledger(dir.path(), &ledger).unwrap();
    }

    fn aged_rfc3339(age_minutes: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::minutes(age_minutes)).to_rfc3339()
    }

    #[test]
    fn resume_fresh_duplicate_is_still_suppressed() {
        // Same-session reprint guard: a matching entry younger than the
        // staleness threshold still collapses the duplicate.
        let dir = project();
        let fp = content_fingerprint(dir.path());
        mark_delivered(
            dir.path(),
            "kimi",
            DeliveryIntent::Session,
            DeliveryChannel::Resume,
            "resume",
            &json!({}),
            &fp,
        );
        let again = should_deliver(
            dir.path(),
            "kimi",
            DeliveryIntent::Session,
            DeliveryChannel::Resume,
            &json!({}),
            &fp,
            false,
        );
        assert!(!again.deliver);
        assert_eq!(again.reason, "duplicate");
    }

    #[test]
    fn resume_stale_entry_is_a_new_session_and_delivers() {
        // The reported regression: a matching entry older than the
        // staleness threshold belongs to an earlier session — deliver.
        let dir = project();
        let fp = content_fingerprint(dir.path());
        mark_delivered(
            dir.path(),
            "kimi",
            DeliveryIntent::Session,
            DeliveryChannel::Resume,
            "resume",
            &json!({}),
            &fp,
        );
        edit_only_entry(&dir, |e| {
            e.delivered_at = aged_rfc3339(SESSION_STALE_MINUTES + 5);
        });
        let next = should_deliver(
            dir.path(),
            "kimi",
            DeliveryIntent::Session,
            DeliveryChannel::Resume,
            &json!({}),
            &fp,
            false,
        );
        assert!(next.deliver, "{}", next.reason);
        assert_eq!(next.reason, "fresh");
    }

    #[test]
    fn resume_seq_change_delivers() {
        let dir = project();
        let fp = content_fingerprint(dir.path());
        mark_delivered(
            dir.path(),
            "kimi",
            DeliveryIntent::Session,
            DeliveryChannel::Resume,
            "resume",
            &json!({}),
            &fp,
        );
        // Recorded seq no longer matches the current handoff seq.
        edit_only_entry(&dir, |e| e.handoff_seq += 999);
        let next = should_deliver(
            dir.path(),
            "kimi",
            DeliveryIntent::Session,
            DeliveryChannel::Resume,
            &json!({}),
            &fp,
            false,
        );
        assert!(next.deliver, "{}", next.reason);
    }

    #[test]
    fn resume_content_fp_change_delivers() {
        let dir = project();
        mark_delivered(
            dir.path(),
            "kimi",
            DeliveryIntent::Session,
            DeliveryChannel::Resume,
            "resume",
            &json!({}),
            "fp-old",
        );
        let next = should_deliver(
            dir.path(),
            "kimi",
            DeliveryIntent::Session,
            DeliveryChannel::Resume,
            &json!({}),
            "fp-new",
            false,
        );
        assert!(next.deliver, "{}", next.reason);
    }

    #[test]
    fn resume_malformed_delivered_at_is_treated_as_stale() {
        let dir = project();
        let fp = content_fingerprint(dir.path());
        mark_delivered(
            dir.path(),
            "kimi",
            DeliveryIntent::Session,
            DeliveryChannel::Resume,
            "resume",
            &json!({}),
            &fp,
        );
        edit_only_entry(&dir, |e| e.delivered_at = "not-a-timestamp".into());
        let next = should_deliver(
            dir.path(),
            "kimi",
            DeliveryIntent::Session,
            DeliveryChannel::Resume,
            &json!({}),
            &fp,
            false,
        );
        assert!(next.deliver, "{}", next.reason);
    }

    #[test]
    fn hook_duplicate_suppression_has_no_staleness_rule() {
        // The Hook arm is unchanged: a session-id duplicate stays
        // suppressed even when the entry is older than the threshold
        // (its retry collapse is debounce-based, separate and correct).
        let dir = project();
        let fp = content_fingerprint(dir.path());
        mark_delivered(
            dir.path(),
            "cursor",
            DeliveryIntent::Session,
            DeliveryChannel::Hook,
            "session_start",
            &json!({"session_id": "s1"}),
            &fp,
        );
        edit_only_entry(&dir, |e| {
            e.delivered_at = aged_rfc3339(SESSION_STALE_MINUTES + 5);
        });
        let again = should_deliver(
            dir.path(),
            "cursor",
            DeliveryIntent::Session,
            DeliveryChannel::Hook,
            &json!({"session_id": "s1"}),
            &fp,
            false,
        );
        assert!(!again.deliver);
        assert_eq!(again.reason, "duplicate");
    }

    #[test]
    fn content_change_redelivers_same_session() {
        let dir = project();
        mark_delivered(
            dir.path(),
            "cursor",
            DeliveryIntent::Session,
            DeliveryChannel::Hook,
            "session_start",
            &json!({"session_id": "s1"}),
            "fp-old",
        );
        let next = should_deliver(
            dir.path(),
            "cursor",
            DeliveryIntent::Session,
            DeliveryChannel::Hook,
            &json!({"session_id": "s1"}),
            "fp-new",
            false,
        );
        assert!(next.deliver);
        assert_eq!(next.reason, "stale_content");
    }

    #[test]
    fn compact_is_never_suppressed() {
        let dir = project();
        let fp = content_fingerprint(dir.path());
        mark_delivered(
            dir.path(),
            "claude-code",
            DeliveryIntent::Session,
            DeliveryChannel::Hook,
            "session_start",
            &json!({}),
            &fp,
        );
        let compact = should_deliver(
            dir.path(),
            "claude-code",
            DeliveryIntent::Compact,
            DeliveryChannel::Hook,
            &json!({}),
            &fp,
            false,
        );
        assert!(compact.deliver);
    }

    #[test]
    fn legacy_markers_migrate() {
        let dir = project();
        let root = local_store::root(dir.path());
        // Fresh timestamp: a stale migrated marker would (correctly) be
        // treated as an earlier session under the resume staleness rule.
        let delivered_at = aged_rfc3339(5);
        fs::write(
            root.join(local_store::LEGACY_HOOK_RESUME_MARKER),
            format!(
                r#"{{"harness":"claude-code","handoff_seq":0,"content_fp":"legacy","delivered_at":"{delivered_at}"}}"#
            ),
        )
        .unwrap();
        let decision = should_deliver(
            dir.path(),
            "claude",
            DeliveryIntent::Session,
            DeliveryChannel::Resume,
            &json!({}),
            "legacy",
            false,
        );
        assert!(!decision.deliver);
        assert!(ledger_path(dir.path()).is_file());
    }

    #[test]
    fn session_id_reads_common_aliases() {
        assert_eq!(
            session_id_from_payload(&json!({"conversation_id": "c1"})).as_deref(),
            Some("c1")
        );
        assert_eq!(
            session_id_from_payload(&json!({"messageID": "m9"})).as_deref(),
            Some("m9")
        );
        assert_eq!(
            session_id_from_payload(&json!({"session": {"id": "nested"}})).as_deref(),
            Some("nested")
        );
    }
}
