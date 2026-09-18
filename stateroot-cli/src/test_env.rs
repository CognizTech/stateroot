//! Shared environment-variable lock for unit tests.
//!
//! `std::env` is process-global: any test that sets or removes an env var
//! must hold this ONE lock, or parallel tests in this binary observe each
//! other's overrides (the telemetry endpoint leak that turned the Windows
//! core leg red while Linux stayed green). Acquisition is poison-tolerant
//! so a panicking test does not cascade-fail every later env test.
#![cfg(test)]

pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
