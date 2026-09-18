//! Hidden `stateroot _drain-telemetry` — the detached half of the telemetry
//! spool. Kicked from safe user-facing entrypoints (never from hooks), it
//! POSTs queued events with the allowlisted header set and an empty body,
//! deleting each spool file only after HTTP 2xx acknowledgement.
//!
//! Durability contract: event ids are stable across retries (the server
//! dedups by `event_id`); network errors, 429, and 5xx stop the run and
//! schedule bounded exponential backoff; a permanent schema rejection
//! (400/422) is accounted in `dropped.jsonl` and removed from the queue.
//! Every error is swallowed — telemetry never fails a StateRoot operation.

use anyhow::Result;
use stateroot_core::telemetry as core;

use super::Ctx;

/// Hard cap for one drain run — the worker must never outstay its welcome.
const DRAIN_TIME_CAP: std::time::Duration = std::time::Duration::from_secs(8);
/// Per-request timeout.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
/// At most four batches (200 events) per run.
const MAX_BATCHES: usize = 4;

/// CLI entry: drain the telemetry spool, fail-silent by contract.
pub async fn run(ctx: &Ctx) -> Result<()> {
    let _ = drain(ctx).await;
    Ok(())
}

/// Drain loop, separated from [`run`] so tests can call it directly.
pub async fn drain(ctx: &Ctx) -> std::io::Result<()> {
    if !core::allowed(crate::cli::BUILD_VERSION) {
        return Ok(());
    }
    core::clear_stale_drain_lock(&ctx.config_dir);
    let Some(_flight) =
        stateroot_core::fs_lock::FileLock::acquire(core::drain_lock_path(&ctx.config_dir))
    else {
        return Ok(()); // another worker holds the flight
    };
    if !core::drain_due(&ctx.config_dir) {
        return Ok(());
    }
    let client = match reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() {
        Ok(client) => client,
        Err(_) => return Ok(()),
    };
    let url = crate::telemetry::endpoint();
    let deadline = std::time::Instant::now() + DRAIN_TIME_CAP;
    let mut delivered = 0u64;
    let mut rejected = 0u64;
    let mut stop_error: Option<String> = None;

    'outer: for _ in 0..MAX_BATCHES {
        let batch = core::pending_batch(&ctx.config_dir, core::DRAIN_BATCH);
        if batch.is_empty() {
            break;
        }
        for (path, event) in batch {
            if std::time::Instant::now() >= deadline {
                stop_error = Some("time cap reached".to_string());
                break 'outer;
            }
            let mut request = client.post(&url);
            for (name, value) in event.headers() {
                if let Ok(value) = reqwest::header::HeaderValue::from_str(&value) {
                    request = request.header(name, value);
                }
            }
            // Empty body by contract — everything travels in the allowlisted
            // headers, and the server's telemetry log records only those.
            match request.body(Vec::new()).send().await {
                Ok(response) if response.status().is_success() => {
                    core::ack_event(&path);
                    delivered += 1;
                }
                Ok(response)
                    if response.status() == reqwest::StatusCode::BAD_REQUEST
                        || response.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY =>
                {
                    // Permanent schema rejection: visible local accounting,
                    // no retry — the event will never be accepted as-is.
                    core::reject_event(&ctx.config_dir, &path, &event, "permanent_reject");
                    rejected += 1;
                }
                Ok(response) => {
                    stop_error = Some(format!("http {}", response.status().as_u16()));
                    break 'outer;
                }
                Err(err) => {
                    stop_error = Some(format!("{err}"));
                    break 'outer;
                }
            }
        }
    }

    let clean = stop_error.is_none();
    core::record_drain_outcome(
        &ctx.config_dir,
        delivered,
        rejected,
        clean,
        stop_error.as_deref(),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Test ctx over a temp config dir (no project needed for the drain).
    fn test_ctx(config_dir: &std::path::Path) -> Ctx {
        Ctx {
            cwd: config_dir.to_path_buf(),
            config_dir: config_dir.to_path_buf(),
            config: Default::default(),
        }
    }

    fn queue_event(config_dir: &std::path::Path, kind: &str) -> String {
        // Events normally enter via observe_install/continuity_delivered;
        // the drain only needs valid spool files, so mint them directly.
        let state_dir = core::spool_dir(config_dir);
        std::fs::create_dir_all(&state_dir).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let event = serde_json::json!({
            "schema_version": 1,
            "event_id": id,
            "install_id": uuid::Uuid::new_v4().to_string(),
            "event": kind,
            "occurred_on": "2026-09-17",
            "cli_version": "9.9.9",
            "os_arch": "linux-x64",
            "cohort": "measured_new",
        });
        std::fs::write(
            state_dir.join(format!("{id}.json")),
            serde_json::to_string(&event).unwrap(),
        )
        .unwrap();
        id
    }

    /// One-shot HTTP sink yielding the captured request (request line +
    /// headers), responding with the given status code.
    fn one_shot_server(status: u16) -> (String, std::sync::mpsc::Receiver<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                use std::io::Read as _;
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = match status {
                    204 => "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n".to_string(),
                    code => format!(
                        "HTTP/1.1 {code} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    ),
                };
                use std::io::Write as _;
                let _ = stream.write_all(body.as_bytes());
                let _ = tx.send(request);
            }
        });
        (addr, rx)
    }

    // Env mutation is process-global, so the std Mutex must cover the whole
    // drain call; each tokio::test runs its own current-thread runtime, so
    // the guard never blocks another task within a runtime.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn acked_events_leave_the_spool() {
        let _lock = crate::test_env::env_lock();
        let _force = EnvGuard::set(core::FORCE_ENV, "1");
        let config = tempfile::tempdir().unwrap();
        let id = queue_event(config.path(), "install_observed");
        let (url, rx) = one_shot_server(204);
        let _url = EnvGuard::set("STATEROOT_TELEMETRY_URL", &url);
        drain(&test_ctx(config.path())).await.unwrap();
        let request = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("one request");
        assert!(request.starts_with("POST "), "{request}");
        assert!(
            request.contains(&format!("x-sr-event-id: {id}")),
            "stable event id rides the wire: {request}"
        );
        assert!(
            request.contains("x-sr-event: install_observed"),
            "{request}"
        );
        // The body is empty by construction (`.body(Vec::new())`); everything
        // travels in the allowlisted x-sr-* headers above.
        assert!(core::pending_batch(config.path(), 10).is_empty(), "acked");
        // A clean run resets backoff bookkeeping.
        assert!(core::drain_due(config.path()));
    }

    // Env mutation is process-global, so the std Mutex must cover the whole
    // drain call; each tokio::test runs its own current-thread runtime, so
    // the guard never blocks another task within a runtime.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn server_errors_retain_the_event_with_backoff() {
        let _lock = crate::test_env::env_lock();
        let _force = EnvGuard::set(core::FORCE_ENV, "1");
        let config = tempfile::tempdir().unwrap();
        let id = queue_event(config.path(), "active_day");
        let (url, _rx) = one_shot_server(500);
        let _url = EnvGuard::set("STATEROOT_TELEMETRY_URL", &url);
        drain(&test_ctx(config.path())).await.unwrap();
        let pending = core::pending_batch(config.path(), 10);
        assert_eq!(pending.len(), 1, "5xx retains the event");
        assert_eq!(pending[0].1.event_id, id, "event id survives retry");
        assert!(!core::drain_due(config.path()), "backoff scheduled");
    }

    // Env mutation is process-global, so the std Mutex must cover the whole
    // drain call; each tokio::test runs its own current-thread runtime, so
    // the guard never blocks another task within a runtime.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn schema_rejection_is_accounted_not_retried() {
        let _lock = crate::test_env::env_lock();
        let _force = EnvGuard::set(core::FORCE_ENV, "1");
        let config = tempfile::tempdir().unwrap();
        queue_event(config.path(), "active_day");
        let (url, _rx) = one_shot_server(400);
        let _url = EnvGuard::set("STATEROOT_TELEMETRY_URL", &url);
        drain(&test_ctx(config.path())).await.unwrap();
        assert!(core::pending_batch(config.path(), 10).is_empty());
        let dropped = std::fs::read_to_string(core::dropped_log_path(config.path())).unwrap();
        assert!(dropped.contains("permanent_reject"), "{dropped}");
    }

    // Env mutation is process-global, so the std Mutex must cover the whole
    // drain call; each tokio::test runs its own current-thread runtime, so
    // the guard never blocks another task within a runtime.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn offline_drain_keeps_events_until_acknowledgement() {
        let _lock = crate::test_env::env_lock();
        let _force = EnvGuard::set(core::FORCE_ENV, "1");
        let config = tempfile::tempdir().unwrap();
        let id = queue_event(config.path(), "continuity_activated");
        // Nothing listens — the run fails, the event survives.
        let _url = EnvGuard::set("STATEROOT_TELEMETRY_URL", "http://127.0.0.1:9");
        drain(&test_ctx(config.path())).await.unwrap();
        let pending = core::pending_batch(config.path(), 10);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1.event_id, id);
    }
}
