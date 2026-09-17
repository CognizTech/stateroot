//! CLI glue for the core telemetry pipeline (see `stateroot_core::telemetry`
//! for the metric definitions and privacy contract).
//!
//! Everything here is fail-silent: telemetry never fails, delays, or alters a
//! StateRoot operation. Acquisition (`install_observed`) is spooled on version
//! change; every later safe entrypoint kicks the detached single-flight drain.
//! Hook paths append locally only — they never touch the network.

use std::path::Path;

use stateroot_core::telemetry as core;

use super::cli::BUILD_VERSION;
use super::commands::{detached, Ctx};

/// Default ingestion endpoint (tests override via STATEROOT_TELEMETRY_URL).
pub const TELEMETRY_URL: &str = "https://stateroot.dev/api/telemetry/v1/event";

/// Endpoint resolution (env override is the test seam).
pub fn endpoint() -> String {
    std::env::var("STATEROOT_TELEMETRY_URL").unwrap_or_else(|_| TELEMETRY_URL.to_string())
}

/// Spool the install/update acquisition event when the version changed.
/// Replaces the legacy one-shot GET ping: the marker lives in the versioned
/// telemetry state now, so offline installs are durably queued instead of
/// permanently lost. Local-only; never blocks.
pub fn observe_install(config_dir: &Path, version: &str) {
    let _ = core::observe_install(config_dir, version, "cli");
}

/// Kick the detached single-flight drain when queued work is due. Cheap: one
/// directory scan plus a state read; a fresh lock or active backoff never
/// spawns. Spawn failure is recorded in the telemetry dir (never silent).
pub fn kick_drain(ctx: &Ctx) {
    if cfg!(test) || std::env::var_os("STATEROOT_TEST_CMD_PROBES").is_some() {
        return;
    }
    if !core::allowed(BUILD_VERSION) || !core::kick_needed(&ctx.config_dir) {
        return;
    }
    core::clear_stale_drain_lock(&ctx.config_dir);
    let log = core::state_dir(&ctx.config_dir).join("drain-spawn.log");
    if let Err(err) = detached::spawn_self(&["_drain-telemetry".to_string()], &ctx.cwd, &log) {
        let line = serde_json::json!({
            "ts": stateroot_core::local_store::now_rfc3339(),
            "error": format!("{err:#}"),
        });
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
        {
            use std::io::Write as _;
            let _ = file.write_all(format!("{line}\n").as_bytes());
        }
    }
}

/// Continuity digest actually delivered in `harness` (hook injection printed
/// / explicit resume delivered). Local append only — safe on hook latency.
pub fn continuity_delivered(config_dir: &Path, project_dir: &Path, harness: &str) {
    let _ = core::continuity_delivered(config_dir, project_dir, harness, BUILD_VERSION);
}

/// Qualifying daily activity (checkpoint/root boundary, handoff write/accept,
/// completed delegation). Never activates, never transitions.
pub fn activity(config_dir: &Path, project_dir: &Path, harness: Option<&str>) {
    let _ = core::record_activity(config_dir, project_dir, harness, BUILD_VERSION);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn endpoint_defaults_and_overrides() {
        let _lock = ENV_LOCK.lock().unwrap();
        assert_eq!(endpoint(), TELEMETRY_URL);
        let _guard = EnvGuard::set("STATEROOT_TELEMETRY_URL", "http://127.0.0.1:9/event");
        assert_eq!(endpoint(), "http://127.0.0.1:9/event");
    }

    #[test]
    fn observe_install_is_fail_silent_and_local() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _force = EnvGuard::set(core::FORCE_ENV, "1");
        let dir = tempfile::tempdir().unwrap();
        observe_install(dir.path(), "9.9.9");
        // One queued event, version marker moved into the telemetry state.
        assert_eq!(core::pending_batch(dir.path(), 10).len(), 1);
        observe_install(dir.path(), "9.9.9");
        assert_eq!(core::pending_batch(dir.path(), 10).len(), 1);
    }
}
