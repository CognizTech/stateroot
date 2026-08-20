//! Persona injection scheduler (binding strategy).
//!
//! Problem: the persona + user-profile block was re-injected on EVERY user
//! message — hundreds of tokens × dozens of times per session, and the
//! repetition teaches models to tune it out. The scheduler decides per hook
//! event whether to print FULL (the current full block, unchanged in
//! content), COMPRESSED (a 1–2 line pointer), or NOTHING.
//!
//! Rules (binding):
//! 1. FULL fires ONLY on (a) session start / first prompt_submit of a
//!    session, (b) pre_compact / post_compaction events, (c) content change
//!    (the persona+user content hash differs from the last FULL injection).
//! 2. COMPRESSED fires every 15th prompt_submit since the last FULL.
//! 3. DEDUPE: no injection of any kind within 3 prompts OR 60 seconds of
//!    the previous injection (state existing).
//! 4. State is a small JSON in the USER-GLOBAL local dir
//!    (`~/.stateroot/local/persona-injection.json` — the quarantined lane,
//!    never synced), keyed per project_dir + session (session id from the
//!    hook payload when present, else the project dir).
//! 5. No state yet → FULL.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::local_store::SESSION_STALE_MINUTES;

/// Decision for one hook event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Print the full block (content unchanged from today).
    Full,
    /// Print the 1–2 line pointer (~20 tokens).
    Compressed,
    /// Print nothing.
    Nothing,
}

/// Persisted per-session record.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InjectionState {
    /// RFC 3339 of the last FULL injection ("" when never).
    #[serde(default)]
    pub last_full_ts: String,
    /// RFC 3339 of the last injection of any kind ("" when never).
    #[serde(default)]
    pub last_any_ts: String,
    /// sha256 of the persona+user content at the last FULL injection.
    #[serde(default)]
    pub content_hash: String,
    /// prompt_submit events handled since the last FULL injection.
    #[serde(default)]
    pub prompts_since_full: i64,
    /// prompt_submit events handled since the last injection of any kind.
    #[serde(default)]
    pub prompts_since_any: i64,
    /// True once the session has had its start-injection (a session_start
    /// FULL is one-per-session; later session_starts stay silent).
    #[serde(default)]
    pub started: bool,
}

/// Session key: payload session id when present, else the project dir.
pub fn session_key(project_dir: &Path, payload: &Value) -> String {
    ["session_id", "conversation_id"]
        .iter()
        .find_map(|field| payload.get(field).and_then(|v| v.as_str()))
        .filter(|s| !s.trim().is_empty())
        .map(|s| format!("{}:{}", project_dir.display(), s.trim()))
        .unwrap_or_else(|| project_dir.display().to_string())
}

fn state_path(home: &Path) -> PathBuf {
    home.join(".stateroot/local/persona-injection.json")
}

/// Load all session records (missing file = empty map).
pub fn load_states(home: &Path) -> BTreeMap<String, InjectionState> {
    let Ok(text) = std::fs::read_to_string(state_path(home)) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn save_states(home: &Path, states: &BTreeMap<String, InjectionState>) -> std::io::Result<()> {
    let path = state_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let pretty = serde_json::to_string_pretty(states)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, format!("{pretty}\n"))
}

const DEDUPE_PROMPTS: i64 = 3;
const DEDUPE_SECONDS: i64 = 60;
const COMPRESSED_EVERY: i64 = 15;
// SESSION_STALE_MINUTES lives in `local_store` — shared with the
// digest-delivery ledger, which applies the same new-session staleness
// rule to resume dedupe.

fn parse_ts(ts: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// Pure decision: given the prior state (None = first call), the canonical
/// event, the current content hash, and the current time. `canonical` is the
/// normalized hook event (`session_start` | `user_prompt_submit` |
/// `pre_compact` | `post_compaction` | other).
pub fn decide(
    state: Option<&InjectionState>,
    canonical: &str,
    content_hash: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Decision {
    let Some(state) = state else {
        return Decision::Full; // no state yet → FULL
    };
    // Content change wins over everything (new content always surfaces).
    if !content_hash.is_empty() && content_hash != state.content_hash {
        return Decision::Full;
    }
    // New-session staleness: an idle gap past the threshold means the old
    // session is over — treat the event as a fresh session's first contact
    // (and re-FULL it). A malformed anchor falls back to FULL safely.
    let anchor = if !state.last_any_ts.is_empty() {
        &state.last_any_ts
    } else {
        &state.last_full_ts
    };
    if !anchor.is_empty() {
        match parse_ts(anchor) {
            Some(last) if (now - last).num_minutes() >= SESSION_STALE_MINUTES => {
                return Decision::Full;
            }
            None => return Decision::Full,
            _ => {}
        }
    }
    // session_start is one-per-session (never re-inject on a duplicate
    // SessionStart — guarded by ANY prior FULL, marked or not).
    if canonical == "session_start" {
        return if state.last_full_ts.is_empty() {
            Decision::Full
        } else {
            Decision::Nothing
        };
    }
    // The first prompt_submit of a session is the first USABLE prompt —
    // harnesses whose session_start output is discarded (unmarked) still
    // get identity here.
    if canonical == "user_prompt_submit" && !state.started {
        return Decision::Full;
    }
    // DEDUPE next, anchored on MARKED injections only (an unmarked
    // session_start print starts no window — its content never landed).
    if state.prompts_since_any < DEDUPE_PROMPTS {
        return Decision::Nothing;
    }
    if let Some(last) = parse_ts(&state.last_any_ts) {
        if !state.last_any_ts.is_empty() && (now - last).num_seconds() < DEDUPE_SECONDS {
            return Decision::Nothing;
        }
    }
    // Compact boundaries recur; each is a real one.
    if matches!(canonical, "pre_compact" | "post_compaction") {
        return Decision::Full;
    }
    // COMPRESSED cadence on prompt_submit only.
    if canonical == "user_prompt_submit" {
        let next = state.prompts_since_full + 1;
        if next >= COMPRESSED_EVERY && next % COMPRESSED_EVERY == 0 {
            return Decision::Compressed;
        }
    }
    Decision::Nothing
}

/// The 1–2 line compressed pointer (~20 tokens).
pub fn compressed_pointer(persona_name: &str, persona_path: &Path) -> String {
    format!(
        "{persona_name} — {} (unchanged since last full injection)",
        persona_path.display()
    )
}

/// Persona display name for the pointer: first markdown heading in the
/// resolved persona text, else a stable label.
pub fn persona_name(persona_text: &str) -> String {
    for line in persona_text.lines() {
        let line = line.trim();
        if let Some(heading) = line.strip_prefix('#') {
            let heading = heading.trim();
            if !heading.is_empty() && !heading.starts_with("!") {
                return heading.to_string();
            }
        }
    }
    "StateRoot persona".to_string()
}

/// Apply a decision to the persisted state and return the updated record
/// (the caller persists it via [`record_decision`]).
pub fn apply(
    mut state: InjectionState,
    canonical: &str,
    decision: Decision,
    content_hash: &str,
    now: chrono::DateTime<chrono::Utc>,
    mark: bool,
) -> InjectionState {
    if canonical == "user_prompt_submit" {
        state.prompts_since_full += 1;
        state.prompts_since_any += 1;
    }
    match decision {
        Decision::Full => {
            state.last_full_ts = now.to_rfc3339();
            if mark {
                state.started = true;
                state.last_any_ts = now.to_rfc3339();
                state.prompts_since_any = 0;
            }
            if !content_hash.is_empty() {
                state.content_hash = content_hash.to_string();
            }
            // The injecting prompt_submit itself counts as cycle prompt #1,
            // so COMPRESSED fires on the 15th prompt of the cycle.
            state.prompts_since_full = if canonical == "user_prompt_submit" {
                1
            } else {
                0
            };
        }
        Decision::Compressed => {
            state.last_any_ts = now.to_rfc3339();
            state.prompts_since_any = 0;
        }
        Decision::Nothing => {}
    }
    state
}

/// Decide + persist for one hook event. Returns the decision; the state file
/// is updated in place (created when absent).
pub fn decide_and_record(
    home: &Path,
    key: &str,
    canonical: &str,
    content_hash: &str,
    now: chrono::DateTime<chrono::Utc>,
    mark: bool,
) -> Decision {
    let mut states = load_states(home);
    let prior = states.get(key).cloned();
    let decision = decide(prior.as_ref(), canonical, content_hash, now);
    let updated = apply(
        prior.unwrap_or_default(),
        canonical,
        decision,
        content_hash,
        now,
        mark,
    );
    states.insert(key.to_string(), updated);
    let _ = save_states(home, &states); // best-effort; hooks never fail on IO
    decision
}

/// sha256 of the persona+user content that the scheduler hashes for change
/// detection.
pub fn content_hash(identity_text: &str) -> String {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(identity_text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Now as chrono (test seam: callers inject explicit times).
pub fn utc_now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn state_with_start(
        full_ts: i64,
        any_ts: i64,
        hash: &str,
        since_full: i64,
        since_any: i64,
        started: bool,
    ) -> InjectionState {
        InjectionState {
            last_full_ts: t(full_ts).to_rfc3339(),
            last_any_ts: t(any_ts).to_rfc3339(),
            content_hash: hash.into(),
            prompts_since_full: since_full,
            prompts_since_any: since_any,
            started,
        }
    }

    fn state(
        full_ts: i64,
        any_ts: i64,
        hash: &str,
        since_full: i64,
        since_any: i64,
    ) -> InjectionState {
        InjectionState {
            last_full_ts: t(full_ts).to_rfc3339(),
            last_any_ts: t(any_ts).to_rfc3339(),
            content_hash: hash.into(),
            prompts_since_full: since_full,
            prompts_since_any: since_any,
            started: true,
        }
    }

    #[test]
    fn no_state_first_call_is_full() {
        assert_eq!(
            decide(None, "user_prompt_submit", "h1", t(100)),
            Decision::Full
        );
    }

    #[test]
    fn full_triggers_fire_on_boundaries() {
        let mut s = state_with_start(0, 0, "h1", 0, 4, false);
        s.last_full_ts = String::new(); // never injected yet
        assert_eq!(
            decide(Some(&s), "session_start", "h1", t(100)),
            Decision::Full
        );
        let s = state(0, 0, "h1", 0, 4);
        assert_eq!(
            decide(Some(&s), "pre_compact", "h1", t(100)),
            Decision::Full
        );
        assert_eq!(
            decide(Some(&s), "post_compaction", "h1", t(100)),
            Decision::Full
        );
    }

    #[test]
    fn content_change_forces_full() {
        let s = state(0, 0, "h1", 0, 4);
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h2", t(100)),
            Decision::Full
        );
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h1", t(100)),
            Decision::Nothing
        );
    }

    #[test]
    fn cadence_fires_compressed_every_fifteenth() {
        let s = state(0, 0, "h1", 13, 4);
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h1", t(100)),
            Decision::Nothing
        );
        let s = state(0, 0, "h1", 14, 4);
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h1", t(100)),
            Decision::Compressed
        );
        let s = state(0, 0, "h1", 29, 4);
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h1", t(100)),
            Decision::Compressed
        );
    }

    #[test]
    fn dedupe_prompts_and_seconds_both_suppress() {
        // < 3 prompts since last injection → Nothing regardless of time.
        let s = state(0, 0, "h1", 14, 2);
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h1", t(1000)),
            Decision::Nothing
        );
        // ≥ 3 prompts but < 60s → Nothing.
        let s = state(0, 0, "h1", 14, 4);
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h1", t(30)),
            Decision::Nothing
        );
        // ≥ 3 prompts AND ≥ 60s → cadence applies.
        let s = state(0, 0, "h1", 14, 4);
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h1", t(100)),
            Decision::Compressed
        );
        // dedupe also suppresses FULL triggers (any kind, same session).
        let s = state_with_start(0, 0, "h1", 0, 1, false);
        assert_eq!(
            decide(Some(&s), "session_start", "h1", t(30)),
            Decision::Nothing
        );
    }

    #[test]
    fn apply_records_full_compressed_and_counts() {
        let s = apply(
            InjectionState::default(),
            "user_prompt_submit",
            Decision::Compressed,
            "h1",
            t(100),
            true,
        );
        assert_eq!(s.prompts_since_full, 1);
        assert_eq!(s.prompts_since_any, 0);
        assert_eq!(s.last_any_ts, t(100).to_rfc3339());
        let s = apply(s, "session_start", Decision::Full, "h2", t(200), true);
        assert_eq!(s.prompts_since_full, 0);
        assert_eq!(s.content_hash, "h2");
        assert_eq!(s.last_full_ts, t(200).to_rfc3339());
        // A FULL prompt_submit counts as cycle prompt #1.
        let s = apply(s, "user_prompt_submit", Decision::Full, "h2", t(300), true);
        assert_eq!(s.prompts_since_full, 1);
        assert_eq!(s.last_full_ts, t(300).to_rfc3339());
    }

    #[test]
    fn pointer_is_compact_and_shaped() {
        let pointer = compressed_pointer("Yinyue", Path::new("/home/u/.stateroot/soul/SOUL.md"));
        assert!(pointer.lines().count() <= 2);
        assert!(pointer.contains("Yinyue"));
        assert!(pointer.contains("SOUL.md"));
        assert!(pointer.contains("unchanged since last full injection"));
        assert!(pointer.len() < 120, "~20 tokens: {pointer}");
    }

    #[test]
    fn persona_name_from_first_heading() {
        assert_eq!(persona_name("# Soul\n\ntext"), "Soul");
        assert_eq!(persona_name("<!-- c -->\n# Yinyue\n"), "Yinyue");
        assert_eq!(persona_name("no heading"), "StateRoot persona");
    }

    #[test]
    fn session_key_prefers_payload_id() {
        let dir = Path::new("/p");
        assert_eq!(
            session_key(dir, &serde_json::json!({"session_id": "s1"})),
            "/p:s1"
        );
        assert_eq!(session_key(dir, &serde_json::json!({})), "/p");
    }

    #[test]
    fn session_start_injects_once_per_session() {
        let mut s = state_with_start(0, 0, "h1", 0, 4, false);
        s.last_full_ts = String::new();
        assert_eq!(
            decide(Some(&s), "session_start", "h1", t(100)),
            Decision::Full
        );
        let mut s = apply(s, "session_start", Decision::Full, "h1", t(100), true);
        assert!(s.started);
        assert_eq!(
            decide(Some(&s), "session_start", "h1", t(1000)),
            Decision::Nothing
        );
        // Content change still wins over everything.
        assert_eq!(
            decide(Some(&s), "session_start", "h2", t(1000)),
            Decision::Full
        );
        s.prompts_since_any = 4;
        assert_eq!(
            decide(Some(&s), "pre_compact", "h1", t(2000)),
            Decision::Full
        );
    }

    #[test]
    fn unmarked_session_start_leaves_first_usable_prompt_to_inject() {
        let s = apply(
            InjectionState::default(),
            "session_start",
            Decision::Full,
            "h1",
            t(0),
            false, // unmarked (pi: session_start stdout is discarded)
        );
        assert!(!s.started, "unmarked — no marked start yet");
        assert!(s.last_any_ts.is_empty(), "no marked anchor yet");
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h1", t(5)),
            Decision::Full
        );
        let s = apply(s, "user_prompt_submit", Decision::Full, "h1", t(5), true);
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h1", t(10)),
            Decision::Nothing
        );
    }

    #[test]
    fn stale_entry_is_a_new_session_and_fulls_again() {
        // The reported regression: project-dir-key entry older than the
        // threshold (kimi-code has no session ids) → next prompt FULLs.
        let mut s = state(0, 0, "h1", 5, 5);
        s.started = true;
        assert_eq!(
            decide(
                Some(&s),
                "user_prompt_submit",
                "h1",
                t(SESSION_STALE_MINUTES * 60 + 1)
            ),
            Decision::Full
        );
        // Fresh entry within the threshold → NO duplicate FULL.
        let mut fresh = state(0, 0, "h1", 5, 5);
        fresh.started = true;
        fresh.last_any_ts = t(100).to_rfc3339();
        assert_eq!(
            decide(Some(&fresh), "user_prompt_submit", "h1", t(100 + 30)),
            Decision::Nothing
        );
    }

    #[test]
    fn session_keys_are_independent_of_each_others_staleness() {
        let home = tempfile::tempdir().expect("home");
        // Stale key fulls; the fresh key in the same project keeps dedupe.
        let d1 = decide_and_record(
            home.path(),
            "kimi:p",
            "user_prompt_submit",
            "h1",
            t(0),
            true,
        );
        assert_eq!(d1, Decision::Full);
        let d2 = decide_and_record(
            home.path(),
            "claude:p",
            "user_prompt_submit",
            "h1",
            t(1),
            true,
        );
        assert_eq!(d2, Decision::Full); // first prompt of its own session key
        let later = t(SESSION_STALE_MINUTES * 60 + 10);
        let d3 = decide_and_record(
            home.path(),
            "kimi:p",
            "user_prompt_submit",
            "h1",
            later,
            true,
        );
        assert_eq!(d3, Decision::Full, "stale key re-FULLs");
        let d4 = decide_and_record(
            home.path(),
            "claude:p",
            "user_prompt_submit",
            "h1",
            later,
            true,
        );
        assert_eq!(
            d4,
            Decision::Full,
            "claude key is ALSO stale now (same wall clock)"
        );
        // Right after each stale FULL, the following prompt dedupes again.
        let d5 = decide_and_record(
            home.path(),
            "kimi:p",
            "user_prompt_submit",
            "h1",
            t(SESSION_STALE_MINUTES * 60 + 20),
            true,
        );
        assert_eq!(d5, Decision::Nothing);
    }

    #[test]
    fn after_stale_full_the_cycle_restarts() {
        let home = tempfile::tempdir().expect("home");
        let d1 = decide_and_record(home.path(), "p", "user_prompt_submit", "h1", t(0), true);
        assert_eq!(d1, Decision::Full);
        let later = t(SESSION_STALE_MINUTES * 60 + 5);
        let d2 = decide_and_record(home.path(), "p", "user_prompt_submit", "h1", later, true);
        assert_eq!(d2, Decision::Full);
        // Counters restarted: immediate next prompt is deduped, and the
        // compressed cadence lands on the 15th of the NEW cycle.
        let d3 = decide_and_record(
            home.path(),
            "p",
            "user_prompt_submit",
            "h1",
            t(SESSION_STALE_MINUTES * 60 + 15),
            true,
        );
        assert_eq!(d3, Decision::Nothing);
        let mut now = SESSION_STALE_MINUTES * 60 + 15;
        let mut last = Decision::Nothing;
        for _ in 3..=15 {
            now += 61;
            last = decide_and_record(home.path(), "p", "user_prompt_submit", "h1", t(now), true);
        }
        assert_eq!(last, Decision::Compressed);
    }

    #[test]
    fn malformed_anchor_falls_back_to_full_safely() {
        let mut s = state(0, 0, "h1", 5, 5);
        s.started = true;
        s.last_any_ts = "not-a-timestamp".into();
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h1", t(999_999)),
            Decision::Full
        );
    }

    #[test]
    fn state_round_trips_through_the_file_and_sessions_are_independent() {
        let home = tempfile::tempdir().expect("home");
        let d1 = decide_and_record(home.path(), "p:s1", "user_prompt_submit", "h1", t(0), true);
        assert_eq!(d1, Decision::Full);
        // Second session: independent record → its own FULL (kimi-style
        // first prompt_submit of a session = its start).
        let d2 = decide_and_record(home.path(), "p:s2", "user_prompt_submit", "h1", t(1), true);
        assert_eq!(d2, Decision::Full);
        // Same session, immediate next prompt → dedupe.
        let d3 = decide_and_record(home.path(), "p:s1", "user_prompt_submit", "h1", t(2), true);
        assert_eq!(d3, Decision::Nothing);
        let states = load_states(home.path());
        assert_eq!(states.len(), 2);
        assert_eq!(states["p:s1"].content_hash, "h1");
    }

    #[test]
    fn full_flow_cadence_over_a_session() {
        let home = tempfile::tempdir().expect("home");
        let key = "p:s1";
        let mut now = 0i64;
        let mut seq = String::new();
        // First prompt: FULL. Then 14 quiet prompts (dedupe spaced out),
        // then a COMPRESSED at the 15th.
        let first = decide_and_record(home.path(), key, "user_prompt_submit", "h1", t(now), true);
        assert_eq!(first, Decision::Full);
        // Prompts 2–14: silent (13 of them). Prompt 15: COMPRESSED.
        for _ in 2..=14 {
            now += 61;
            let d = decide_and_record(home.path(), key, "user_prompt_submit", "h1", t(now), true);
            seq.push(match d {
                Decision::Full => 'F',
                Decision::Compressed => 'C',
                Decision::Nothing => '.',
            });
        }
        now += 61;
        let fifteenth =
            decide_and_record(home.path(), key, "user_prompt_submit", "h1", t(now), true);
        assert_eq!(fifteenth, Decision::Compressed, "sequence: {seq}");
        assert_eq!(seq.matches('.').count(), 13, "sequence: {seq}");
    }
}
