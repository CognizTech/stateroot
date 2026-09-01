//! Persona injection scheduler (binding strategy).
//!
//! Problem: the persona + user-profile block was re-injected on EVERY user
//! message — hundreds of tokens × dozens of times per session, and the
//! repetition teaches models to tune it out. The scheduler decides per hook
//! event whether to print FULL (the current full block, unchanged in
//! content), COMPRESSED (a 1–2 line pointer), or NOTHING.
//!
//! Rules (binding):
//! 1. FULL fires ONLY on (a) session start / first deliverable event of a
//!    session, (b) the first deliverable event AFTER a compaction boundary,
//!    (c) content change (the persona+user content hash differs from the
//!    last FULL injection).
//! 2. COMPRESSED fires every 8th prompt_submit since the last FULL.
//! 3. DEDUPE: no injection of any kind within 3 prompts OR 60 seconds of
//!    the previous injection (state existing).
//! 4. State is one small JSON per session key in the USER-GLOBAL local dir
//!    (`~/.stateroot/local/persona-injection/<sha256(key)>.json` — the
//!    quarantined lane, never synced), keyed per project_dir + session
//!    (session id from the hook payload when present, else the project
//!    dir). Per-key files: concurrent hooks from many harnesses never
//!    clobber each other's records (the old whole-map load/save lost
//!    updates under parallel hook load).
//! 5. No state yet → FULL.
//!
//! Compaction boundaries (pre_compact / post_compaction) never print and
//! never mark an injection: harnesses discard their stdout (kimi
//! explicitly ignores PreCompact return values), so a "FULL" there was
//! written into the void while the state believed identity had landed.
//! They only ARM `pending_compaction`; the next event whose harness contract
//! can inject context delivers the FULL (usually prompt_submit; Cursor uses
//! postToolUse).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// True when a compaction boundary was observed but no prompt_submit
    /// has delivered the re-anchoring FULL yet. Set by pre_compact /
    /// post_compaction hook events; cleared by the next FULL.
    #[serde(default)]
    pub pending_compaction: bool,
    /// The session key this record belongs to (debugging aid; the file
    /// name is the key's hash).
    #[serde(default)]
    pub key: String,
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

fn state_dir(home: &Path) -> PathBuf {
    home.join(".stateroot/local/persona-injection")
}

/// One file per session key: `<dir>/<sha256(key)>.json`. Concurrent hooks
/// from different sessions/harnesses write disjoint files, so parallel
/// hook load can no longer discard one another's updates (the old
/// whole-map read-modify-write race).
fn state_file(home: &Path, key: &str) -> PathBuf {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(key.as_bytes());
    state_dir(home).join(format!("{:x}.json", hasher.finalize()))
}

/// Load one session record (missing or unreadable file = no state).
pub fn load_state(home: &Path, key: &str) -> Option<InjectionState> {
    let text = std::fs::read_to_string(state_file(home, key)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Persist one session record (temp file + rename; best-effort by the
/// caller). Same-key writers are a single session's own hooks — rare and
/// sequential — so last-writer-wins is acceptable there.
fn save_state(home: &Path, key: &str, state: &InjectionState) -> std::io::Result<()> {
    let dir = state_dir(home);
    std::fs::create_dir_all(&dir)?;
    let mut stamped = state.clone();
    stamped.key = key.to_string();
    let pretty = serde_json::to_string_pretty(&stamped)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = dir.join(format!(".tmp-{}-{}", std::process::id(), key.len()));
    std::fs::write(&tmp, format!("{pretty}\n"))?;
    let dest = state_file(home, key);
    // Windows rename fails when the destination exists; remove first.
    if dest.exists() {
        let _ = std::fs::remove_file(&dest);
    }
    std::fs::rename(&tmp, &dest)
}

const DEDUPE_PROMPTS: i64 = 3;
const DEDUPE_SECONDS: i64 = 60;
const COMPRESSED_EVERY: i64 = 8;

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
    // NOTE: no wall-clock staleness rule here by design. Long agent turns
    // routinely idle past any fixed threshold, so a time-based re-injection
    // fires on nearly every user message. New sessions are recognized by
    // their session keys (harnesses with session ids); harnesses without
    // them get FULL on session_start/content change/compaction and the
    // COMPRESSED cadence otherwise.
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
    // COMPRESSED cadence on prompt_submit only.
    if canonical == "user_prompt_submit" {
        let next = state.prompts_since_full + 1;
        if next >= COMPRESSED_EVERY && next % COMPRESSED_EVERY == 0 {
            return Decision::Compressed;
        }
    }
    Decision::Nothing
}

/// The 1–2 line compressed pointer (~30 tokens): name + a one-line voice
/// anchor from the persona text, so sparse injections re-anchor *behavior*
/// (how to speak), not just point at a file on disk.
pub fn compressed_pointer(persona_name: &str, tagline: &str, persona_path: &Path) -> String {
    let anchor = if tagline.is_empty() {
        String::new()
    } else {
        format!(" — {tagline}")
    };
    format!(
        "{persona_name}{anchor} (unchanged since last full injection: {})",
        persona_path.display()
    )
}

/// One-line voice anchor for the compressed pointer: the first prose line of
/// the Persona/SOUL section when present, else the first non-heading line
/// (list markers and emphasis stripped). Capped — the pointer stays cheap.
pub fn persona_tagline(persona_text: &str) -> String {
    let lines: Vec<&str> = persona_text.lines().collect();
    let soul_at = lines
        .iter()
        .position(|l| l.contains("SOUL.md") && l.trim_start().starts_with('#'));
    let search_from = soul_at.map(|i| i + 1).unwrap_or(0);
    let mut fallback = "";
    for line in lines.iter().skip(search_from) {
        let cleaned = line
            .trim()
            .trim_start_matches('-')
            .trim()
            .trim_matches('*')
            .trim_matches('_')
            .trim();
        if cleaned.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        // Prefer real prose over `Field: value` frontmatter-ish lines.
        if !cleaned.contains(':') || soul_at.is_some() {
            return cleaned.chars().take(120).collect();
        }
        if fallback.is_empty() {
            fallback = cleaned;
        }
    }
    fallback.chars().take(120).collect()
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
            state.pending_compaction = false;
            if mark {
                state.started = true;
                state.last_any_ts = now.to_rfc3339();
                state.prompts_since_any = 0;
            }
            if !content_hash.is_empty() {
                state.content_hash = content_hash.to_string();
            }
            // The injecting prompt_submit itself counts as cycle prompt #1,
            // so COMPRESSED fires on the 8th prompt of the cycle.
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
/// is updated in place (created when absent). `deliverable` marks events
/// whose output actually reaches the model on this harness (the registry
/// delivery policy: session_start where it marks, prompt_submit where it
/// injects).
pub fn decide_and_record(
    home: &Path,
    key: &str,
    canonical: &str,
    content_hash: &str,
    now: chrono::DateTime<chrono::Utc>,
    mark: bool,
    deliverable: bool,
) -> Decision {
    // Compaction boundaries never print and never mark an injection:
    // harnesses discard their stdout (kimi ignores PreCompact return
    // values outright), so a "FULL" printed there lands nowhere while the
    // state would believe identity was delivered. They only ARM the flag;
    // the next deliverable event carries the FULL.
    if matches!(canonical, "pre_compact" | "post_compaction") {
        let mut state = load_state(home, key).unwrap_or_default();
        state.pending_compaction = true;
        let _ = save_state(home, key, &state); // best-effort; hooks never fail on IO
        return Decision::Nothing;
    }
    let prior = load_state(home, key);
    // A compaction rewrote the model's context: deliver the re-anchoring
    // FULL at the first event that can actually carry it (prompt_submit on
    // kimi/claude, session_start on cursor/gemini). Bypasses dedupe — the
    // persona is gone, not stale. Non-deliverable events leave the flag
    // armed for a later one.
    if deliverable
        && prior
            .as_ref()
            .map(|s| s.pending_compaction)
            .unwrap_or(false)
    {
        let updated = apply(
            prior.unwrap_or_default(),
            canonical,
            Decision::Full,
            content_hash,
            now,
            mark,
        );
        let _ = save_state(home, key, &updated); // best-effort
        return Decision::Full;
    }
    let decision = decide(prior.as_ref(), canonical, content_hash, now);
    let updated = apply(
        prior.unwrap_or_default(),
        canonical,
        decision,
        content_hash,
        now,
        mark,
    );
    let _ = save_state(home, key, &updated); // best-effort; hooks never fail on IO
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
            ..Default::default()
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
            ..Default::default()
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
        // Compact boundaries never print: they arm the flag instead, and
        // the next prompt_submit delivers the FULL.
        let home = tempfile::tempdir().expect("home");
        for event in ["pre_compact", "post_compaction"] {
            let key = format!("p:{event}");
            let d = decide_and_record(home.path(), &key, event, "h1", t(100), true, true);
            assert_eq!(d, Decision::Nothing, "{event} must not print");
            let armed = load_state(home.path(), &key).expect("state");
            assert!(armed.pending_compaction, "{event} must arm");
            let d = decide_and_record(
                home.path(),
                &key,
                "user_prompt_submit",
                "h1",
                t(200),
                true,
                true,
            );
            assert_eq!(d, Decision::Full, "prompt after {event} must FULL");
            let cleared = load_state(home.path(), &key).expect("state");
            assert!(!cleared.pending_compaction, "FULL clears the flag");
        }
    }

    #[test]
    fn armed_full_bypasses_dedupe_and_fires_once() {
        let home = tempfile::tempdir().expect("home");
        let key = "p:s1";
        // Session running: FULL at t=0, one prompt since (inside dedupe).
        let d = decide_and_record(
            home.path(),
            key,
            "user_prompt_submit",
            "h1",
            t(0),
            true,
            true,
        );
        assert_eq!(d, Decision::Full);
        let d = decide_and_record(
            home.path(),
            key,
            "user_prompt_submit",
            "h1",
            t(10),
            true,
            true,
        );
        assert_eq!(d, Decision::Nothing, "dedupe window");
        // Compaction inside the dedupe window: still FULLs next prompt.
        let d = decide_and_record(home.path(), key, "pre_compact", "h1", t(20), true, true);
        assert_eq!(d, Decision::Nothing);
        let d = decide_and_record(
            home.path(),
            key,
            "user_prompt_submit",
            "h1",
            t(30),
            true,
            true,
        );
        assert_eq!(d, Decision::Full, "armed FULL bypasses dedupe");
        // And it fires once: the following prompt is back to normal rules.
        let d = decide_and_record(
            home.path(),
            key,
            "user_prompt_submit",
            "h1",
            t(40),
            true,
            true,
        );
        assert_eq!(d, Decision::Nothing, "flag cleared after one FULL");
        // Two compactions before the next prompt arm a single FULL.
        let _ = decide_and_record(home.path(), key, "pre_compact", "h1", t(50), true, true);
        let _ = decide_and_record(home.path(), key, "post_compaction", "h1", t(60), true, true);
        let d = decide_and_record(
            home.path(),
            key,
            "user_prompt_submit",
            "h1",
            t(70),
            true,
            true,
        );
        assert_eq!(d, Decision::Full);
        let d = decide_and_record(
            home.path(),
            key,
            "user_prompt_submit",
            "h1",
            t(80),
            true,
            true,
        );
        assert_eq!(d, Decision::Nothing);
    }

    #[test]
    fn armed_flag_waits_for_a_deliverable_event() {
        let home = tempfile::tempdir().expect("home");
        let key = "p:s1";
        let d = decide_and_record(
            home.path(),
            key,
            "user_prompt_submit",
            "h1",
            t(0),
            true,
            true,
        );
        assert_eq!(d, Decision::Full);
        let _ = decide_and_record(home.path(), key, "pre_compact", "h1", t(10), true, true);
        // A non-deliverable event (e.g. cursor's capture-only prompt submit)
        // leaves the flag armed and prints nothing.
        let d = decide_and_record(
            home.path(),
            key,
            "user_prompt_submit",
            "h1",
            t(20),
            true,
            false,
        );
        assert_eq!(d, Decision::Nothing);
        assert!(
            load_state(home.path(), key)
                .expect("state")
                .pending_compaction,
            "flag stays armed"
        );
        // The first deliverable event carries the FULL (session_start on
        // cursor/gemini, prompt_submit on kimi/claude).
        let d = decide_and_record(home.path(), key, "session_start", "h1", t(30), true, true);
        assert_eq!(d, Decision::Full);
        assert!(
            !load_state(home.path(), key)
                .expect("state")
                .pending_compaction,
            "FULL clears the flag"
        );
    }

    #[test]
    fn per_key_files_are_isolated() {
        let home = tempfile::tempdir().expect("home");
        let d = decide_and_record(
            home.path(),
            "p:s1",
            "user_prompt_submit",
            "h1",
            t(0),
            true,
            true,
        );
        assert_eq!(d, Decision::Full);
        let d = decide_and_record(
            home.path(),
            "p:s2",
            "user_prompt_submit",
            "h1",
            t(1),
            true,
            true,
        );
        assert_eq!(d, Decision::Full);
        // Writing one key leaves the other untouched.
        let _ = decide_and_record(
            home.path(),
            "p:s1",
            "user_prompt_submit",
            "h1",
            t(2),
            true,
            true,
        );
        let s2 = load_state(home.path(), "p:s2").expect("s2");
        assert_eq!(s2.prompts_since_full, 1);
        assert_eq!(s2.key, "p:s2", "key stamped for debugging");
        let s1 = load_state(home.path(), "p:s1").expect("s1");
        assert_eq!(s1.prompts_since_full, 2);
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
    fn cadence_fires_compressed_every_eighth() {
        let s = state(0, 0, "h1", 6, 4);
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h1", t(100)),
            Decision::Nothing
        );
        let s = state(0, 0, "h1", 7, 4);
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h1", t(100)),
            Decision::Compressed
        );
        let s = state(0, 0, "h1", 15, 4);
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
        // ≥ 3 prompts but < 60s → Nothing (even when the cadence would fire).
        let s = state(0, 0, "h1", 7, 4);
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h1", t(30)),
            Decision::Nothing
        );
        // ≥ 3 prompts AND ≥ 60s → cadence applies.
        let s = state(0, 0, "h1", 7, 4);
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
        let pointer = compressed_pointer(
            "Yinyue",
            "speaks in riddles",
            Path::new("/home/u/.stateroot/soul/SOUL.md"),
        );
        assert!(pointer.lines().count() <= 2);
        assert!(pointer.contains("Yinyue"));
        assert!(pointer.contains("SOUL.md"));
        assert!(pointer.contains("unchanged since last full injection"));
        assert!(pointer.len() < 160, "~35 tokens: {pointer}");
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
        // Compact boundaries are no longer print events (arm-only).
        s.prompts_since_any = 4;
        assert_eq!(
            decide(Some(&s), "pre_compact", "h1", t(2000)),
            Decision::Nothing
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
    fn stale_entry_does_not_reinject() {
        // Idle gaps never re-FULL: long agent turns idle past any fixed
        // threshold, so a stale entry simply keeps its dedupe/cadence state.
        let mut s = state(0, 0, "h1", 5, 5);
        s.started = true;
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h1", t(30 * 60 + 1)),
            Decision::Nothing
        );
        // A fresh entry behaves the same.
        let mut fresh = state(0, 0, "h1", 5, 5);
        fresh.started = true;
        fresh.last_any_ts = t(100).to_rfc3339();
        assert_eq!(
            decide(Some(&fresh), "user_prompt_submit", "h1", t(100 + 30)),
            Decision::Nothing
        );
    }

    #[test]
    fn session_keys_stay_quiet_after_idle_gaps() {
        let home = tempfile::tempdir().expect("home");
        let d1 = decide_and_record(
            home.path(),
            "kimi:p",
            "user_prompt_submit",
            "h1",
            t(0),
            true,
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
            true,
        );
        assert_eq!(d2, Decision::Full); // first prompt of its own session key
        let later = t(30 * 60 + 10);
        // Neither key re-FULLs after a long idle gap — time is not a signal.
        for key in ["kimi:p", "claude:p"] {
            let d = decide_and_record(
                home.path(),
                key,
                "user_prompt_submit",
                "h1",
                later,
                true,
                true,
            );
            assert_eq!(d, Decision::Nothing, "{key} must not re-FULL on idle");
        }
        // A new session key still gets its own FULL (id-keyed detection).
        let d5 = decide_and_record(
            home.path(),
            "kimi:p:s2",
            "user_prompt_submit",
            "h1",
            later,
            true,
            true,
        );
        assert_eq!(d5, Decision::Full);
    }

    #[test]
    fn idle_gaps_do_not_restart_the_cycle() {
        // Same cadence as `full_flow_cadence_over_a_session`, but every gap
        // is 31 minutes: time never resets counters or forces FULL.
        let home = tempfile::tempdir().expect("home");
        let key = "p:s1";
        let mut now = 0i64;
        let mut seq = String::new();
        let first = decide_and_record(
            home.path(),
            key,
            "user_prompt_submit",
            "h1",
            t(now),
            true,
            true,
        );
        assert_eq!(first, Decision::Full);
        let mut last = Decision::Nothing;
        for _ in 2..=8 {
            now += 31 * 60;
            last = decide_and_record(
                home.path(),
                key,
                "user_prompt_submit",
                "h1",
                t(now),
                true,
                true,
            );
            seq.push(match last {
                Decision::Full => 'F',
                Decision::Compressed => 'C',
                Decision::Nothing => '.',
            });
        }
        assert_eq!(last, Decision::Compressed, "sequence: {seq}");
        assert_eq!(seq.matches('.').count(), 6, "sequence: {seq}");
    }

    #[test]
    fn malformed_anchor_is_ignored() {
        let mut s = state(0, 0, "h1", 5, 5);
        s.started = true;
        s.last_any_ts = "not-a-timestamp".into();
        assert_eq!(
            decide(Some(&s), "user_prompt_submit", "h1", t(999_999)),
            Decision::Nothing
        );
    }

    #[test]
    fn state_round_trips_through_the_file_and_sessions_are_independent() {
        let home = tempfile::tempdir().expect("home");
        let d1 = decide_and_record(
            home.path(),
            "p:s1",
            "user_prompt_submit",
            "h1",
            t(0),
            true,
            true,
        );
        assert_eq!(d1, Decision::Full);
        // Second session: independent record → its own FULL (kimi-style
        // first prompt_submit of a session = its start).
        let d2 = decide_and_record(
            home.path(),
            "p:s2",
            "user_prompt_submit",
            "h1",
            t(1),
            true,
            true,
        );
        assert_eq!(d2, Decision::Full);
        // Same session, immediate next prompt → dedupe.
        let d3 = decide_and_record(
            home.path(),
            "p:s1",
            "user_prompt_submit",
            "h1",
            t(2),
            true,
            true,
        );
        assert_eq!(d3, Decision::Nothing);
        let s1 = load_state(home.path(), "p:s1").expect("s1");
        let s2 = load_state(home.path(), "p:s2").expect("s2");
        assert_eq!(s1.content_hash, "h1");
        assert_eq!(s2.content_hash, "h1");
    }

    #[test]
    fn tagline_prefers_the_soul_prose_line() {
        let composed = "### Identity (IDENTITY.md)\n\n- **Name:** Marid\n- **Emoji:** 🪔\n\n### Persona (SOUL.md)\n\n*I am Marid, the jinn of the lamp. Classical. Arabian Nights, not Hollywood.*\n";
        assert_eq!(
            persona_tagline(composed),
            "I am Marid, the jinn of the lamp. Classical. Arabian Nights, not Hollywood."
        );
        // No SOUL section: first plain line wins, list markers stripped.
        assert_eq!(
            persona_tagline("# Soul\n\n- Tone: direct\n"),
            "Tone: direct"
        );
        assert!(persona_tagline("# Only headings\n").is_empty());
    }

    #[test]
    fn compressed_pointer_carries_the_voice_anchor() {
        let pointer = compressed_pointer(
            "Marid",
            "I am Marid, the jinn of the lamp.",
            Path::new("/home/u/.stateroot/soul/SOUL.md"),
        );
        assert!(pointer.contains("Marid — I am Marid, the jinn of the lamp."));
        assert!(pointer.contains("unchanged since last full injection"));
        assert!(pointer.contains("SOUL.md"));
        let bare = compressed_pointer("Persona", "", Path::new("/p/SOUL.md"));
        assert!(bare.starts_with("Persona ("));
    }

    #[test]
    fn full_flow_cadence_over_a_session() {
        let home = tempfile::tempdir().expect("home");
        let key = "p:s1";
        let mut now = 0i64;
        let mut seq = String::new();
        // First prompt: FULL. Prompts 2–7 quiet; prompt 8 COMPRESSED;
        // 9–15 quiet again; prompt 16 COMPRESSED (every-8th cadence).
        let first = decide_and_record(
            home.path(),
            key,
            "user_prompt_submit",
            "h1",
            t(now),
            true,
            true,
        );
        assert_eq!(first, Decision::Full);
        for i in 2..=16 {
            now += 61;
            let d = decide_and_record(
                home.path(),
                key,
                "user_prompt_submit",
                "h1",
                t(now),
                true,
                true,
            );
            seq.push(match d {
                Decision::Full => 'F',
                Decision::Compressed => 'C',
                Decision::Nothing => '.',
            });
            if i == 8 || i == 16 {
                assert_eq!(d, Decision::Compressed, "prompt {i}, sequence: {seq}");
            } else {
                assert_eq!(d, Decision::Nothing, "prompt {i}, sequence: {seq}");
            }
        }
        assert_eq!(seq.matches('.').count(), 13, "sequence: {seq}");
        assert_eq!(seq.matches('C').count(), 2, "sequence: {seq}");
    }
}
