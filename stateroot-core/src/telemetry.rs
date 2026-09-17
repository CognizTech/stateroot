//! Privacy-minimal accelerator telemetry: anonymous installation identity,
//! activation/repeat/cross-harness event semantics, and a durable local spool.
//!
//! Metric semantics (the product contract — do not loosen):
//!
//! - The measurable entity is an **anonymous installation** (`install_id`, a
//!   random UUID minted locally). It is never derived from Git identity,
//!   editor IDs, hostname, username, MAC, IP, or the StateRoot logical user.
//! - An installation becomes **activated** only after StateRoot successfully
//!   delivers a non-empty continuity digest in a recognized harness while
//!   attached to an initialized project (`continuity_activated`, once per
//!   installation). Installs, marketplace hits, `--version`, `init`, and
//!   detected harnesses never activate.
//! - `active_day` fires at most once per installation/project/harness/UTC day
//!   from qualifying activity (continuity delivery, checkpoint/root boundary,
//!   handoff write/accept, completed delegation).
//! - `harness_transition` fires when the previous qualifying harness on the
//!   same opaque project differs from the one that just delivered — read
//!   before update, emitted only after destination delivery succeeded.
//! - `install_observed` (kind install/update) stays acquisition evidence,
//!   separate from activation.
//!
//! Privacy: the opaque `project_id` is a keyed BLAKE3 digest of a random
//! machine-local secret + the canonical StateRoot project identity, so fork
//! worktrees of one project share an id without transmitting paths or
//! enabling cross-machine correlation. No project paths/names, remotes,
//! session ids, root hashes, prompts, commands, filenames, handoff content,
//! usernames, hostnames, or editor workspace data ever enter the spool.
//!
//! Delivery: events are appended to a bounded machine-local spool with stable
//! event ids; a detached drain POSTs them and deletes each file only after
//! HTTP 2xx. `STATEROOT_NO_PING=1` is the single opt-out; dev builds
//! (`-dev.*`) emit nothing. Telemetry failure must never fail, delay, or
//! alter a StateRoot operation — every public entrypoint here is synchronous,
//! local-only, and its errors are for tests; call sites swallow them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fs_lock::FileLock;

/// Wire/state schema version.
pub const SCHEMA_VERSION: u32 = 1;
/// The single telemetry opt-out (preserved from the legacy ping).
pub const OPT_OUT_ENV: &str = "STATEROOT_NO_PING";
/// Test-only override allowing telemetry on dev builds. Never set in
/// production; tests use it to exercise the pipeline with a temp home.
pub const FORCE_ENV: &str = "STATEROOT_TELEMETRY_FORCE";

/// Event kinds on the wire.
pub const EVENT_INSTALL_OBSERVED: &str = "install_observed";
pub const EVENT_CONTINUITY_ACTIVATED: &str = "continuity_activated";
pub const EVENT_ACTIVE_DAY: &str = "active_day";
pub const EVENT_HARNESS_TRANSITION: &str = "harness_transition";

/// Cohorts.
pub const COHORT_MEASURED_NEW: &str = "measured_new";
pub const COHORT_LEGACY_EXISTING: &str = "legacy_existing";

/// Hard bound on queued events; activation/transition records are evicted
/// last when the bound is reached.
pub const MAX_SPOOL_EVENTS: usize = 2000;
/// One drain batch size.
pub const DRAIN_BATCH: usize = 50;
/// Drain single-flight lock is stale after this many seconds (crashed worker).
pub const DRAIN_LOCK_STALE_SECS: u64 = 120;
/// Backoff ceiling in minutes (bounded exponential from 1 minute).
pub const BACKOFF_CAP_MINUTES: i64 = 240;

/// Legacy pre-telemetry version marker (cohort detection + migration seed).
pub const LEGACY_MARKER: &str = "last_seen_version";

/// One telemetry event (versioned envelope). `Option` fields are omitted on
/// the wire when absent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelemetryEvent {
    pub schema_version: u32,
    pub event_id: String,
    pub install_id: String,
    pub event: String,
    /// YYYY-MM-DD (UTC).
    pub occurred_on: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_harness: Option<String>,
    pub cli_version: String,
    pub os_arch: String,
    pub cohort: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl TelemetryEvent {
    /// The allowlisted HTTP header set for this event (empty-body POST).
    /// Header names are part of the frozen wire contract with the website
    /// ingestion path — do not rename without a schema bump.
    pub fn headers(&self) -> Vec<(&'static str, String)> {
        let mut out: Vec<(&'static str, String)> = vec![
            ("x-sr-schema", self.schema_version.to_string()),
            ("x-sr-event-id", self.event_id.clone()),
            ("x-sr-install-id", self.install_id.clone()),
            ("x-sr-event", self.event.clone()),
            ("x-sr-occurred-on", self.occurred_on.clone()),
            ("x-sr-cli-version", self.cli_version.clone()),
            ("x-sr-os-arch", self.os_arch.clone()),
            ("x-sr-cohort", self.cohort.clone()),
        ];
        if let Some(v) = &self.project_id {
            out.push(("x-sr-project", v.clone()));
        }
        if let Some(v) = &self.harness {
            out.push(("x-sr-harness", v.clone()));
        }
        if let Some(v) = &self.from_harness {
            out.push(("x-sr-from-harness", v.clone()));
        }
        if let Some(v) = &self.channel {
            out.push(("x-sr-channel", v.clone()));
        }
        if let Some(v) = &self.kind {
            out.push(("x-sr-kind", v.clone()));
        }
        out
    }
}

/// Drain bookkeeping persisted in the state file (visible local accounting).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct DrainMeta {
    /// Consecutive failed drain runs (drives bounded exponential backoff).
    pub failures: u32,
    /// RFC3339 timestamp before which no drain is attempted.
    pub backoff_until: Option<String>,
    /// Events evicted at the queue bound.
    pub dropped: u64,
    /// Events permanently rejected by the server (schema).
    pub permanent_rejects: u64,
    /// Events acknowledged (HTTP 2xx) over the installation's lifetime.
    pub delivered: u64,
    /// Last drain error, for operator visibility.
    pub last_error: Option<String>,
}

/// Versioned machine-local telemetry state (`local/telemetry/state.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TelemetryState {
    pub schema_version: u32,
    /// Random telemetry UUID — never derived from any machine/user identity.
    pub install_id: String,
    /// 64-hex random key for the keyed BLAKE3 project digest.
    pub secret: String,
    pub cohort: String,
    /// YYYY-MM-DD the telemetry identity was minted.
    pub created_on: String,
    /// Last CLI version that fired `install_observed` (replaces the legacy
    /// single version marker).
    pub last_seen_version: String,
    pub activated: bool,
    pub activated_on: Option<String>,
    /// Opaque project digest → last qualifying harness (transition detection).
    pub last_harness_by_project: BTreeMap<String, String>,
    /// Sent `project|harness|YYYY-MM-DD` daily keys (pruned to recent days).
    pub sent_daily_keys: Vec<String>,
    pub drain: DrainMeta,
}

impl Default for TelemetryState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            install_id: String::new(),
            secret: String::new(),
            cohort: COHORT_MEASURED_NEW.to_string(),
            created_on: String::new(),
            last_seen_version: String::new(),
            activated: false,
            activated_on: None,
            last_harness_by_project: BTreeMap::new(),
            sent_daily_keys: Vec::new(),
            drain: DrainMeta::default(),
        }
    }
}

// ---------------------------------------------------------------------
// Paths and gate
// ---------------------------------------------------------------------

/// `<config>/local/telemetry/` — identity, spool, and drain bookkeeping.
pub fn state_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("local").join("telemetry")
}

fn state_path(config_dir: &Path) -> PathBuf {
    state_dir(config_dir).join("state.json")
}

fn state_lock_path(config_dir: &Path) -> PathBuf {
    state_dir(config_dir).join("state.lock")
}

/// Spool directory: one `<event_id>.json` file per queued event.
pub fn spool_dir(config_dir: &Path) -> PathBuf {
    state_dir(config_dir).join("spool")
}

/// Single-flight drain lock path.
pub fn drain_lock_path(config_dir: &Path) -> PathBuf {
    state_dir(config_dir).join("drain.lock")
}

/// Permanent-reject / bound-drop accounting log (operator-visible).
pub fn dropped_log_path(config_dir: &Path) -> PathBuf {
    state_dir(config_dir).join("dropped.jsonl")
}

fn legacy_marker_path(config_dir: &Path) -> PathBuf {
    config_dir.join("local").join(LEGACY_MARKER)
}

/// Telemetry gate: opted out via `STATEROOT_NO_PING`, or a dev build
/// (`-dev.*`) without the test-only force flag. Nothing is ever written or
/// sent when this is false.
pub fn allowed(version: &str) -> bool {
    if std::env::var_os(OPT_OUT_ENV).is_some() {
        return false;
    }
    if version.contains("-dev.") && std::env::var_os(FORCE_ENV).is_none() {
        return false;
    }
    true
}

/// `linux-x64` / `macos-aarch64` / `windows-x64` — deliberately coarse.
pub fn os_arch() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows-x64"
    } else if cfg!(target_os = "macos") {
        "macos-aarch64"
    } else {
        "linux-x64"
    }
}

/// Header-safe scalar: keep `[A-Za-z0-9._-]`, truncate.
fn sanitize_scalar(raw: &str, max: usize) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(max)
        .collect()
}

fn today_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn now_utc() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

// ---------------------------------------------------------------------
// State load/create (callers hold the state lock)
// ---------------------------------------------------------------------

fn mint_identity() -> (String, String) {
    let install_id = uuid::Uuid::new_v4().to_string();
    // 32 random bytes (two UUIDs), hex-encoded — the BLAKE3 keyed-digest key.
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let secret: String = key.iter().map(|b| format!("{b:02x}")).collect();
    (install_id, secret)
}

/// Load state, or mint it. Cohort: a pre-existing legacy version marker means
/// this install predates telemetry v1 → `legacy_existing`, and the marker's
/// version seeds `last_seen_version` so the upgrade is not double counted.
fn load_or_create(config_dir: &Path, today: &str) -> std::io::Result<TelemetryState> {
    let path = state_path(config_dir);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let mut state: TelemetryState = serde_json::from_str(&text).unwrap_or_default();
            if state.schema_version != SCHEMA_VERSION
                || state.install_id.is_empty()
                || state.secret.len() != 64
            {
                state = fresh_state(today, &legacy_marker_contents(config_dir));
            }
            Ok(state)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let state = fresh_state(today, &legacy_marker_contents(config_dir));
            save_state(config_dir, &state)?;
            Ok(state)
        }
        Err(err) => Err(err),
    }
}

fn legacy_marker_contents(config_dir: &Path) -> Option<String> {
    std::fs::read_to_string(legacy_marker_path(config_dir))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn fresh_state(today: &str, legacy_marker: &Option<String>) -> TelemetryState {
    let (install_id, secret) = mint_identity();
    TelemetryState {
        install_id,
        secret,
        cohort: if legacy_marker.is_some() {
            COHORT_LEGACY_EXISTING.to_string()
        } else {
            COHORT_MEASURED_NEW.to_string()
        },
        created_on: today.to_string(),
        last_seen_version: legacy_marker.clone().unwrap_or_default(),
        ..Default::default()
    }
}

fn save_state(config_dir: &Path, state: &TelemetryState) -> std::io::Result<()> {
    let value = serde_json::to_value(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    crate::safe_io::atomic_replace_json(&state_path(config_dir), &value)
}

// ---------------------------------------------------------------------
// Opaque project identity
// ---------------------------------------------------------------------

/// Canonical StateRoot project identity: the manifest `project_id` (fork
/// worktrees of one project share it). `None` when not an initialized
/// project — continuity events are skipped for such directories.
pub fn project_identity(project_dir: &Path) -> Option<String> {
    let manifest = crate::local_store::read_manifest(project_dir).ok()??;
    manifest
        .get("project_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// Keyed BLAKE3 digest: secret + canonical project identity → 64-hex opaque
/// id. Not reversible to the project id without the machine-local secret.
pub fn opaque_project_id(secret: &str, project_identity: &str) -> Option<String> {
    let key_bytes = hex_decode_32(secret)?;
    let digest = blake3::keyed_hash(&key_bytes, project_identity.as_bytes());
    Some(digest.to_hex().to_string())
}

fn hex_decode_32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Normalize a harness alias to the canonical registry id. Empty and the
/// local-CLI actor label (`cli`) carry no harness on the wire.
fn normalize_harness_opt(harness: Option<&str>) -> Option<String> {
    let raw = harness?.trim();
    if raw.is_empty() || raw == "cli" {
        return None;
    }
    Some(crate::harness_identity::normalize(raw))
}

// ---------------------------------------------------------------------
// Spool
// ---------------------------------------------------------------------

fn spool_file(spool: &Path, event_id: &str) -> PathBuf {
    spool.join(format!("{event_id}.json"))
}

/// Append one event to the spool (atomic), enforcing the hard bound with
/// activation/transition priority. Returns the spool file path.
fn append_event(
    config_dir: &Path,
    state: &mut TelemetryState,
    event: &TelemetryEvent,
) -> std::io::Result<PathBuf> {
    let spool = spool_dir(config_dir);
    std::fs::create_dir_all(&spool)?;
    let pending = pending_files(&spool);
    if pending.len() >= MAX_SPOOL_EVENTS {
        evict_for_bound(config_dir, state, &pending);
    }
    let path = spool_file(&spool, &event.event_id);
    let value = serde_json::to_value(event)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    crate::safe_io::atomic_replace_json(&path, &value)?;
    Ok(path)
}

fn is_priority(event: &TelemetryEvent) -> bool {
    matches!(
        event.event.as_str(),
        EVENT_CONTINUITY_ACTIVATED | EVENT_HARNESS_TRANSITION
    )
}

/// Evict oldest non-priority events until under the bound; fall back to
/// oldest overall when everything is priority. Every eviction is accounted.
fn evict_for_bound(config_dir: &Path, state: &mut TelemetryState, pending: &[PathBuf]) {
    let mut candidates: Vec<(std::time::SystemTime, PathBuf, bool)> = pending
        .iter()
        .filter_map(|p| {
            let mtime = std::fs::metadata(p).and_then(|m| m.modified()).ok()?;
            let priority = std::fs::read_to_string(p)
                .ok()
                .and_then(|t| serde_json::from_str::<TelemetryEvent>(&t).ok())
                .map(|e| is_priority(&e))
                .unwrap_or(false);
            Some((mtime, p.clone(), priority))
        })
        .collect();
    // Oldest first, non-priority before priority.
    candidates.sort_by_key(|a| (a.2, a.0));
    let mut evict = candidates.len().saturating_sub(MAX_SPOOL_EVENTS - 1);
    for (_, path, _) in candidates {
        if evict == 0 {
            break;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(event) = serde_json::from_str::<TelemetryEvent>(&text) {
                record_drop(config_dir, &event, "queue_bound_drop");
                state.drain.dropped += 1;
            }
        }
        let _ = std::fs::remove_file(&path);
        evict -= 1;
    }
}

/// Visible accounting for events that will never be delivered.
pub fn record_drop(config_dir: &Path, event: &TelemetryEvent, reason: &str) {
    let line = serde_json::json!({
        "ts": crate::local_store::now_rfc3339(),
        "reason": reason,
        "event_id": event.event_id,
        "event": event.event,
    });
    let path = dropped_log_path(config_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write as _;
        let _ = file.write_all(format!("{line}\n").as_bytes());
    }
}

/// Pending spool files, oldest first (by modification time).
pub fn pending_files(spool: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(spool) else {
        return Vec::new();
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .filter_map(|p| {
            let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
            Some((mtime, p))
        })
        .collect();
    files.sort_by_key(|(mtime, _)| *mtime);
    files.into_iter().map(|(_, p)| p).collect()
}

/// Read one pending batch (oldest first) for the drain.
pub fn pending_batch(config_dir: &Path, limit: usize) -> Vec<(PathBuf, TelemetryEvent)> {
    pending_files(&spool_dir(config_dir))
        .into_iter()
        .take(limit)
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            let event = serde_json::from_str::<TelemetryEvent>(&text).ok()?;
            Some((p, event))
        })
        .collect()
}

/// Cheap kick check: any queued work, no active backoff, no live drain lock.
pub fn kick_needed(config_dir: &Path) -> bool {
    let spool = spool_dir(config_dir);
    let has_work = std::fs::read_dir(&spool)
        .map(|mut it| {
            it.any(|e| {
                e.ok()
                    .map(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if !has_work {
        return false;
    }
    if backoff_active(config_dir) {
        return false;
    }
    !drain_lock_live(config_dir)
}

fn backoff_active(config_dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(state_path(config_dir)) else {
        return false;
    };
    let Ok(state) = serde_json::from_str::<TelemetryState>(&text) else {
        return false;
    };
    let Some(until) = state.drain.backoff_until else {
        return false;
    };
    chrono::DateTime::parse_from_rfc3339(&until)
        .map(|t| t > now_utc())
        .unwrap_or(false)
}

/// True when a drain lock file exists and is fresh (a live worker holds it).
pub fn drain_lock_live(config_dir: &Path) -> bool {
    let path = drain_lock_path(config_dir);
    let Ok(meta) = std::fs::metadata(&path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    modified
        .elapsed()
        .map(|age| age.as_secs() < DRAIN_LOCK_STALE_SECS)
        .unwrap_or(true)
}

/// Remove a stale drain lock (crashed worker) so a new drain can start.
pub fn clear_stale_drain_lock(config_dir: &Path) {
    let path = drain_lock_path(config_dir);
    if path.exists() && !drain_lock_live(config_dir) {
        let _ = std::fs::remove_file(&path);
    }
}

// ---------------------------------------------------------------------
// Event semantics (Phase 1 + 2 primitives)
// ---------------------------------------------------------------------

fn base_event(state: &TelemetryState, event: &str, today: &str, version: &str) -> TelemetryEvent {
    TelemetryEvent {
        schema_version: SCHEMA_VERSION,
        event_id: uuid::Uuid::new_v4().to_string(),
        install_id: state.install_id.clone(),
        event: event.to_string(),
        occurred_on: today.to_string(),
        project_id: None,
        harness: None,
        from_harness: None,
        cli_version: sanitize_scalar(version, 64),
        os_arch: os_arch().to_string(),
        cohort: state.cohort.clone(),
        channel: None,
        kind: None,
    }
}

/// Prune daily keys to today/yesterday (dedup horizon is one UTC day; a
/// straddling hook at midnight keeps yesterday's key).
fn prune_daily_keys(state: &mut TelemetryState, today: &str) {
    let yesterday = (chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.pred_opt())
        .map(|d| d.format("%Y-%m-%d").to_string()))
    .unwrap_or_default();
    state
        .sent_daily_keys
        .retain(|k| k.ends_with(today) || k.ends_with(&yesterday));
}

fn daily_key(project_id: &str, harness: Option<&str>, today: &str) -> String {
    format!("{}|{}|{today}", project_id, harness.unwrap_or(""))
}

/// Acquisition: spool `install_observed` when the running version differs
/// from the last seen one. Returns true when an event was queued. Separate
/// from activation by construction.
pub fn observe_install(config_dir: &Path, version: &str, channel: &str) -> std::io::Result<bool> {
    observe_install_on(config_dir, version, channel, &today_utc())
}

pub fn observe_install_on(
    config_dir: &Path,
    version: &str,
    channel: &str,
    today: &str,
) -> std::io::Result<bool> {
    if !allowed(version) {
        return Ok(false);
    }
    let Some(_lock) = FileLock::acquire(state_lock_path(config_dir)) else {
        return Ok(false); // best-effort: another process is mid-mutation
    };
    let mut state = load_or_create(config_dir, today)?;
    let version = sanitize_scalar(version, 64);
    if state.last_seen_version == version {
        return Ok(false);
    }
    let kind = if state.last_seen_version.is_empty() {
        "install"
    } else {
        "update"
    };
    let mut event = base_event(&state, EVENT_INSTALL_OBSERVED, today, &version);
    event.channel = Some(sanitize_scalar(channel, 32));
    event.kind = Some(kind.to_string());
    state.last_seen_version = version;
    append_event(config_dir, &mut state, &event)?;
    save_state(config_dir, &state)?;
    Ok(true)
}

/// Continuity delivered: a non-empty digest was actually emitted/accepted in
/// `harness` while attached to the initialized `project_dir`. Emits
/// `continuity_activated` once per installation, `active_day` once per
/// project/harness/day, and `harness_transition` when the previous qualifying
/// harness on this project differs. Errors are for tests — call sites
/// swallow them so telemetry never alters the operation.
pub fn continuity_delivered(
    config_dir: &Path,
    project_dir: &Path,
    harness: &str,
    version: &str,
) -> std::io::Result<usize> {
    continuity_delivered_on(config_dir, project_dir, harness, version, &today_utc())
}

pub fn continuity_delivered_on(
    config_dir: &Path,
    project_dir: &Path,
    harness: &str,
    version: &str,
    today: &str,
) -> std::io::Result<usize> {
    if !allowed(version) {
        return Ok(0);
    }
    let Some(identity) = project_identity(project_dir) else {
        return Ok(0); // not an initialized project — no event
    };
    let Some(harness_id) = normalize_harness_opt(Some(harness)) else {
        return Ok(0); // continuity requires a recognized harness
    };
    let Some(_lock) = FileLock::acquire(state_lock_path(config_dir)) else {
        return Ok(0);
    };
    let mut state = load_or_create(config_dir, today)?;
    let Some(project_id) = opaque_project_id(&state.secret, &identity) else {
        return Ok(0);
    };
    let mut emitted = 0usize;

    if !state.activated {
        let mut event = base_event(&state, EVENT_CONTINUITY_ACTIVATED, today, version);
        event.project_id = Some(project_id.clone());
        event.harness = Some(harness_id.clone());
        append_event(config_dir, &mut state, &event)?;
        emitted += 1;
        state.activated = true;
        state.activated_on = Some(today.to_string());
    }

    let key = daily_key(&project_id, Some(&harness_id), today);
    if !state.sent_daily_keys.contains(&key) {
        let mut event = base_event(&state, EVENT_ACTIVE_DAY, today, version);
        event.project_id = Some(project_id.clone());
        event.harness = Some(harness_id.clone());
        append_event(config_dir, &mut state, &event)?;
        emitted += 1;
        state.sent_daily_keys.push(key);
    }

    // Read the previous qualifying harness BEFORE updating it; the caller
    // invokes us only after destination delivery succeeded.
    let previous = state.last_harness_by_project.get(&project_id).cloned();
    if let Some(prev) = previous {
        if prev != harness_id {
            let mut event = base_event(&state, EVENT_HARNESS_TRANSITION, today, version);
            event.project_id = Some(project_id.clone());
            event.from_harness = Some(prev);
            event.harness = Some(harness_id.clone());
            append_event(config_dir, &mut state, &event)?;
            emitted += 1;
        }
    }
    state.last_harness_by_project.insert(project_id, harness_id);
    // Cap the map: abandoned projects must not grow the state forever.
    while state.last_harness_by_project.len() > 256 {
        let oldest = state.last_harness_by_project.keys().next().cloned();
        if let Some(k) = oldest {
            state.last_harness_by_project.remove(&k);
        }
    }

    prune_daily_keys(&mut state, today);
    save_state(config_dir, &state)?;
    Ok(emitted)
}

/// Qualifying daily activity without continuity semantics: successful
/// checkpoint/root boundary, handoff write/accept, completed delegation.
/// Emits `active_day` only — never activation, never a transition.
pub fn record_activity(
    config_dir: &Path,
    project_dir: &Path,
    harness: Option<&str>,
    version: &str,
) -> std::io::Result<usize> {
    record_activity_on(config_dir, project_dir, harness, version, &today_utc())
}

pub fn record_activity_on(
    config_dir: &Path,
    project_dir: &Path,
    harness: Option<&str>,
    version: &str,
    today: &str,
) -> std::io::Result<usize> {
    if !allowed(version) {
        return Ok(0);
    }
    let Some(identity) = project_identity(project_dir) else {
        return Ok(0);
    };
    let harness_id = normalize_harness_opt(harness);
    let Some(_lock) = FileLock::acquire(state_lock_path(config_dir)) else {
        return Ok(0);
    };
    let mut state = load_or_create(config_dir, today)?;
    let Some(project_id) = opaque_project_id(&state.secret, &identity) else {
        return Ok(0);
    };
    let key = daily_key(&project_id, harness_id.as_deref(), today);
    if state.sent_daily_keys.contains(&key) {
        return Ok(0);
    }
    let mut event = base_event(&state, EVENT_ACTIVE_DAY, today, version);
    event.project_id = Some(project_id);
    event.harness = harness_id;
    append_event(config_dir, &mut state, &event)?;
    state.sent_daily_keys.push(key);
    prune_daily_keys(&mut state, today);
    save_state(config_dir, &state)?;
    Ok(1)
}

// ---------------------------------------------------------------------
// Drain bookkeeping (the HTTP worker lives in the CLI)
// ---------------------------------------------------------------------

/// Bounded exponential backoff after a failed drain run.
pub fn backoff_minutes(failures: u32) -> i64 {
    let shift = failures.saturating_sub(1).min(8);
    (1i64 << shift).min(BACKOFF_CAP_MINUTES)
}

/// True when no backoff is active (drain may run now).
pub fn drain_due(config_dir: &Path) -> bool {
    !backoff_active(config_dir)
}

/// Persist the outcome of one drain run. `clean` resets failures/backoff;
/// otherwise failures increment and backoff is scheduled.
pub fn record_drain_outcome(
    config_dir: &Path,
    delivered: u64,
    permanent_rejects: u64,
    clean: bool,
    error: Option<&str>,
) -> std::io::Result<()> {
    let Some(_lock) = FileLock::acquire(state_lock_path(config_dir)) else {
        return Ok(());
    };
    let mut state = load_or_create(config_dir, &today_utc())?;
    state.drain.delivered = state.drain.delivered.saturating_add(delivered);
    state.drain.permanent_rejects = state
        .drain
        .permanent_rejects
        .saturating_add(permanent_rejects);
    if clean {
        state.drain.failures = 0;
        state.drain.backoff_until = None;
        state.drain.last_error = None;
    } else {
        state.drain.failures = state.drain.failures.saturating_add(1);
        let minutes = backoff_minutes(state.drain.failures);
        let until = now_utc() + chrono::Duration::minutes(minutes);
        state.drain.backoff_until = Some(until.to_rfc3339());
        state.drain.last_error = error.map(|e| e.chars().take(200).collect());
    }
    save_state(config_dir, &state)
}

/// Acknowledge one event after HTTP 2xx: remove its spool file. The event id
/// is retained server-side for dedup, so a crash between ack and removal is
/// absorbed by the server's `event_id` primary key.
pub fn ack_event(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Permanent schema rejection: account visibly, then remove from the spool.
pub fn reject_event(config_dir: &Path, path: &Path, event: &TelemetryEvent, reason: &str) {
    let _ = reason; // reason travels in the dropped.jsonl line
    record_drop(config_dir, event, "permanent_reject");
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config() -> tempfile::TempDir {
        tempfile::tempdir().expect("config")
    }

    fn temp_project(project_id: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("project");
        crate::local_store::init_skeleton(dir.path(), project_id, "demo", "test")
            .expect("init skeleton");
        dir
    }

    /// Serialize env-mutating tests and restore `STATEROOT_NO_PING` on drop.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvGuard {
        prev: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn clear() -> Self {
            let prev = std::env::var_os(OPT_OUT_ENV);
            std::env::remove_var(OPT_OUT_ENV);
            Self { prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(OPT_OUT_ENV, v),
                None => std::env::remove_var(OPT_OUT_ENV),
            }
        }
    }

    fn spool_events(config_dir: &Path) -> Vec<TelemetryEvent> {
        pending_batch(config_dir, 10_000)
            .into_iter()
            .map(|(_, e)| e)
            .collect()
    }

    #[test]
    fn fresh_install_emits_install_observed_once() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::clear();
        let config = temp_config();
        assert!(observe_install_on(config.path(), "9.9.9", "cli", "2026-09-17").unwrap());
        // Same version: silent.
        assert!(!observe_install_on(config.path(), "9.9.9", "cli", "2026-09-17").unwrap());
        let events = spool_events(config.path());
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.event, EVENT_INSTALL_OBSERVED);
        assert_eq!(e.kind.as_deref(), Some("install"));
        assert_eq!(e.cohort, COHORT_MEASURED_NEW);
        assert_eq!(e.cli_version, "9.9.9");
        assert!(e.project_id.is_none(), "install events carry no project");
        // Update path.
        assert!(observe_install_on(config.path(), "9.9.10", "cli", "2026-09-18").unwrap());
        let events = spool_events(config.path());
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind.as_deref(), Some("update"));
    }

    #[test]
    fn legacy_marker_makes_legacy_cohort_and_seeds_version() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::clear();
        let config = temp_config();
        std::fs::create_dir_all(config.path().join("local")).unwrap();
        std::fs::write(legacy_marker_path(config.path()), "0.1.14\n").unwrap();
        // Same version as the legacy marker: no event (not double counted).
        assert!(!observe_install_on(config.path(), "0.1.14", "cli", "2026-09-17").unwrap());
        let state = load_or_create(config.path(), "2026-09-17").unwrap();
        assert_eq!(state.cohort, COHORT_LEGACY_EXISTING);
        // Upgrade: update event, still legacy cohort.
        assert!(observe_install_on(config.path(), "0.1.15", "cli", "2026-09-17").unwrap());
        let events = spool_events(config.path());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind.as_deref(), Some("update"));
        assert_eq!(events[0].cohort, COHORT_LEGACY_EXISTING);
    }

    #[test]
    fn first_delivery_activates_exactly_once() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::clear();
        let config = temp_config();
        let project = temp_project("prj-one");
        assert_eq!(
            continuity_delivered_on(
                config.path(),
                project.path(),
                "claude-code",
                "9.9.9",
                "2026-09-17"
            )
            .unwrap(),
            2 // activation + active day
        );
        // Repeated same-day deliveries: nothing new.
        assert_eq!(
            continuity_delivered_on(
                config.path(),
                project.path(),
                "claude",
                "9.9.9",
                "2026-09-17"
            )
            .unwrap(),
            0
        );
        // Next day: one active day, no second activation.
        assert_eq!(
            continuity_delivered_on(
                config.path(),
                project.path(),
                "claude",
                "9.9.9",
                "2026-09-18"
            )
            .unwrap(),
            1
        );
        let events = spool_events(config.path());
        let kinds: Vec<&str> = events.iter().map(|e| e.event.as_str()).collect();
        assert_eq!(
            kinds,
            [
                EVENT_CONTINUITY_ACTIVATED,
                EVENT_ACTIVE_DAY,
                EVENT_ACTIVE_DAY
            ]
        );
        // Alias collapsed to the canonical registry id.
        assert_eq!(events[0].harness.as_deref(), Some("claude"));
        let state = load_or_create(config.path(), "2026-09-18").unwrap();
        assert!(state.activated);
        assert_eq!(state.activated_on.as_deref(), Some("2026-09-17"));
    }

    #[test]
    fn transition_requires_same_project_and_distinct_harness() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::clear();
        let config = temp_config();
        let a = temp_project("prj-a");
        let b = temp_project("prj-b");
        continuity_delivered_on(config.path(), a.path(), "claude", "9.9.9", "2026-09-17").unwrap();
        // Same harness again: no transition.
        continuity_delivered_on(config.path(), a.path(), "claude", "9.9.9", "2026-09-17").unwrap();
        // A different project does not fabricate a transition.
        continuity_delivered_on(config.path(), b.path(), "codex", "9.9.9", "2026-09-17").unwrap();
        let before = spool_events(config.path());
        assert!(before.iter().all(|e| e.event != EVENT_HARNESS_TRANSITION));
        // Same project, new harness: exactly one verified A→B transition.
        continuity_delivered_on(config.path(), a.path(), "codex", "9.9.9", "2026-09-18").unwrap();
        let events = spool_events(config.path());
        let transitions: Vec<&TelemetryEvent> = events
            .iter()
            .filter(|e| e.event == EVENT_HARNESS_TRANSITION)
            .collect();
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].from_harness.as_deref(), Some("claude"));
        assert_eq!(transitions[0].harness.as_deref(), Some("codex"));
        // Fork worktrees share the manifest project id → one opaque project.
        let a_fork = temp_project("prj-a");
        let pa = opaque_project_id(
            &load_or_create(config.path(), "2026-09-18").unwrap().secret,
            "prj-a",
        );
        assert_eq!(project_identity(a.path()), project_identity(a_fork.path()));
        assert!(pa.is_some());
    }

    #[test]
    fn activity_never_activates_and_dedups_per_day() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::clear();
        let config = temp_config();
        let project = temp_project("prj-act");
        assert_eq!(
            record_activity_on(config.path(), project.path(), None, "9.9.9", "2026-09-17").unwrap(),
            1
        );
        assert_eq!(
            record_activity_on(config.path(), project.path(), None, "9.9.9", "2026-09-17").unwrap(),
            0
        );
        // cli actor carries no harness on the wire.
        let events = spool_events(config.path());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, EVENT_ACTIVE_DAY);
        assert!(events[0].harness.is_none());
        let state = load_or_create(config.path(), "2026-09-17").unwrap();
        assert!(!state.activated, "activity alone never activates");
    }

    #[test]
    fn opt_out_and_dev_builds_write_nothing() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::clear();
        let config = temp_config();
        let project = temp_project("prj-quiet");
        // Dev build without force: inert.
        assert!(
            !observe_install_on(config.path(), "9.9.9-dev.local", "cli", "2026-09-17").unwrap()
        );
        assert_eq!(
            continuity_delivered_on(
                config.path(),
                project.path(),
                "claude",
                "9.9.9-dev.1",
                "2026-09-17"
            )
            .unwrap(),
            0
        );
        assert!(!state_dir(config.path()).exists(), "no state written");
        // Explicit opt-out (guard restores the cleared state on drop).
        std::env::set_var(OPT_OUT_ENV, "1");
        let r = observe_install_on(config.path(), "9.9.9", "cli", "2026-09-17").unwrap();
        std::env::remove_var(OPT_OUT_ENV);
        assert!(!r);
        assert!(!state_dir(config.path()).exists());
    }

    #[test]
    fn uninitialized_project_emits_nothing() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::clear();
        let config = temp_config();
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(
            continuity_delivered_on(config.path(), plain.path(), "claude", "9.9.9", "2026-09-17")
                .unwrap(),
            0
        );
        assert!(spool_events(config.path()).is_empty());
    }

    #[test]
    fn event_headers_carry_only_the_allowlist() {
        let event = TelemetryEvent {
            project_id: Some("f".repeat(64)),
            harness: Some("claude".into()),
            from_harness: None,
            channel: None,
            kind: None,
            ..base_event(
                &TelemetryState {
                    install_id: uuid::Uuid::new_v4().to_string(),
                    secret: "a".repeat(64),
                    ..Default::default()
                },
                EVENT_ACTIVE_DAY,
                "2026-09-17",
                "9.9.9",
            )
        };
        let headers = event.headers();
        let names: Vec<&str> = headers.iter().map(|(n, _)| *n).collect();
        for name in &names {
            assert!(name.starts_with("x-sr-"), "unexpected header {name}");
        }
        assert!(headers.iter().all(|(_, v)| v.is_ascii()));
        assert!(names.contains(&"x-sr-harness"));
        assert!(!names.contains(&"x-sr-from-harness"));
    }

    #[test]
    fn drain_outcome_records_backoff_and_recovery() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::clear();
        let config = temp_config();
        record_drain_outcome(config.path(), 0, 0, false, Some("http 500")).unwrap();
        let state = load_or_create(config.path(), "2026-09-17").unwrap();
        assert_eq!(state.drain.failures, 1);
        assert!(state.drain.backoff_until.is_some());
        assert!(backoff_active(config.path()));
        assert!(!kick_needed(config.path()));
        record_drain_outcome(config.path(), 3, 1, true, None).unwrap();
        let state = load_or_create(config.path(), "2026-09-17").unwrap();
        assert_eq!(state.drain.failures, 0);
        assert_eq!(state.drain.delivered, 3);
        assert_eq!(state.drain.permanent_rejects, 1);
        assert!(state.drain.backoff_until.is_none());
    }

    #[test]
    fn backoff_minutes_is_bounded_exponential() {
        assert_eq!(backoff_minutes(1), 1);
        assert_eq!(backoff_minutes(2), 2);
        assert_eq!(backoff_minutes(3), 4);
        assert_eq!(backoff_minutes(9), BACKOFF_CAP_MINUTES);
        assert_eq!(backoff_minutes(100), BACKOFF_CAP_MINUTES);
    }

    #[test]
    fn spool_bound_evicts_oldest_non_priority_with_accounting() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::clear();
        let config = temp_config();
        let mut state = load_or_create(config.path(), "2026-09-17").unwrap();
        // Fill to the bound with non-priority events.
        for i in 0..MAX_SPOOL_EVENTS {
            let mut e = base_event(&state, EVENT_ACTIVE_DAY, "2026-09-17", "9.9.9");
            e.event_id = format!("{:032x}", i); // deterministic, still filename-safe
            append_event(config.path(), &mut state, &e).unwrap();
        }
        assert_eq!(
            pending_files(&spool_dir(config.path())).len(),
            MAX_SPOOL_EVENTS
        );
        // A priority event at the bound evicts the oldest active_day.
        let mut t = base_event(&state, EVENT_HARNESS_TRANSITION, "2026-09-17", "9.9.9");
        t.event_id = "ff".repeat(16);
        append_event(config.path(), &mut state, &t).unwrap();
        assert_eq!(
            pending_files(&spool_dir(config.path())).len(),
            MAX_SPOOL_EVENTS
        );
        assert!(state.drain.dropped > 0);
        let dropped = std::fs::read_to_string(dropped_log_path(config.path())).unwrap();
        assert!(dropped.contains("queue_bound_drop"));
        // The priority event survived.
        assert!(spool_file(&spool_dir(config.path()), &t.event_id).exists());
    }

    #[test]
    fn concurrent_identity_creation_is_atomic() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::clear();
        let config = temp_config();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let dir = config.path().to_path_buf();
            handles.push(std::thread::spawn(move || {
                let _lock = FileLock::acquire(state_lock_path(&dir));
                load_or_create(&dir, "2026-09-17").unwrap().install_id
            }));
        }
        let ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(ids.windows(2).all(|w| w[0] == w[1]), "one identity");
    }
}
