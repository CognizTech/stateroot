//! Hidden `stateroot _drain-finalize` — spool-first session-end worker.
//!
//! The stop/session_end hook enqueues `finalize`/`snap`/`ingest` onto the
//! existing outbox and returns. This process drains that queue oldest-first,
//! behind a single-flight lock (1h liveness, same pattern as
//! `update-in-progress`). Replay of a consumed ingest_key is a no-op.

use std::path::Path;

use anyhow::Result;
use serde_json::Value;
use stateroot_core::local_store::{self, now_rfc3339, FINALIZE_MAX_ATTEMPTS};

use super::{detached, Ctx};

/// Survives into the linked CLI so `strings` can prove this workstream.
#[used]
static WS2_SPOOL_FIRST_MARKER: &str = "WS2_SPOOL_FIRST_FINALIZE";

/// Kick a detached drain. No-op inside the test harness (enqueue is the
/// hook's only heavy-path change; tests call [`run_drain`] directly).
pub fn kick(project_dir: &Path) {
    if cfg!(test) || std::env::var_os("STATEROOT_TEST_CMD_PROBES").is_some() {
        return;
    }
    let log = local_store::root(project_dir).join(local_store::FINALIZE_LOG_PATH);
    let _ = detached::spawn_self(&["_drain-finalize".to_string()], project_dir, &log);
}

/// Acquire the single-flight lock. `false` means another drain is live.
pub fn try_acquire_lock(project_dir: &Path) -> Result<bool> {
    let path = local_store::root(project_dir).join(local_store::FINALIZE_LOCK_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if lock_is_live(&path) {
        return Ok(false);
    }
    let _ = std::fs::remove_file(&path);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(_) => {
            let entry = serde_json::json!({
                "pid": std::process::id(),
                "started_at": now_rfc3339(),
            });
            std::fs::write(&path, format!("{entry}\n"))?;
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(!lock_is_live(&path)),
        Err(err) => Err(err.into()),
    }
}

fn lock_is_live(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|entry| {
            entry
                .get("started_at")
                .and_then(|v| v.as_str())
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                .map(|at| (chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_hours() < 1)
        })
        .unwrap_or(false)
}

fn release_lock(project_dir: &Path) {
    let path = local_store::root(project_dir).join(local_store::FINALIZE_LOCK_PATH);
    let _ = std::fs::remove_file(path);
}

/// CLI entry: drain the current project's finalize outbox.
pub async fn run(ctx: &Ctx) -> Result<()> {
    run_drain(ctx).await
}

/// Drain loop (lock + process). Safe to call from tests.
pub async fn run_drain(ctx: &Ctx) -> Result<()> {
    let _ = WS2_SPOOL_FIRST_MARKER;
    if !try_acquire_lock(&ctx.cwd)? {
        return Ok(());
    }
    let result = drain_locked(ctx).await;
    release_lock(&ctx.cwd);
    result
}

async fn drain_locked(ctx: &Ctx) -> Result<()> {
    loop {
        let batch = local_store::outbox_take_batch(&ctx.cwd)?;
        if batch.is_empty() {
            break;
        }
        for op in batch {
            match process_one(ctx, op).await {
                Process::Done | Process::Skip => {}
                Process::Retry(op) => {
                    let _ = local_store::outbox_append(&ctx.cwd, &op);
                }
            }
        }
    }
    Ok(())
}

enum Process {
    Done,
    Skip,
    Retry(Value),
}

async fn process_one(ctx: &Ctx, mut op: Value) -> Process {
    let kind = op
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let key = op
        .get("ingest_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if key.is_empty() || !local_store::FINALIZE_KINDS.contains(&kind.as_str()) {
        let _ = local_store::bump_finalize_stat(&ctx.cwd, "dropped_malformed");
        return Process::Skip;
    }
    if local_store::ingest_key_consumed(&ctx.cwd, &key).unwrap_or(false) {
        return Process::Skip;
    }
    let enqueued_at = op.get("enqueued_at").and_then(|v| v.as_str()).unwrap_or("");
    if local_store::finalize_op_expired(enqueued_at) {
        let _ = local_store::bump_finalize_stat(&ctx.cwd, "dropped_expired");
        return Process::Skip;
    }
    let attempts = op.get("attempts").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    if attempts >= FINALIZE_MAX_ATTEMPTS {
        let _ = local_store::bump_finalize_stat(&ctx.cwd, "dropped_expired");
        return Process::Skip;
    }

    let harness = op
        .get("harness")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let work: anyhow::Result<()> = match kind.as_str() {
        "finalize" => super::handoff::try_auto_finalize(ctx, &harness).map(|_| ()),
        "snap" => {
            stateroot_core::roots::snap_if_changed(&ctx.cwd, &harness, "auto: turn end", None)
                .map(|_| ())
                .map_err(|err| anyhow::anyhow!("{err}"))
        }
        "ingest" => super::compiler::try_ingest(ctx, false).await.map(|_| ()),
        _ => Ok(()),
    };

    match work {
        Ok(()) => {
            if local_store::record_ingest_key_consumed(&ctx.cwd, &key, &kind).is_err() {
                bump_attempts(&mut op);
                return Process::Retry(op);
            }
            let _ = local_store::bump_finalize_stat(&ctx.cwd, "retired_ok");
            Process::Done
        }
        Err(_) => {
            bump_attempts(&mut op);
            Process::Retry(op)
        }
    }
}

fn bump_attempts(op: &mut Value) {
    let next = op.get("attempts").and_then(|v| v.as_u64()).unwrap_or(0) + 1;
    if let Some(obj) = op.as_object_mut() {
        obj.insert("attempts".into(), serde_json::json!(next));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stateroot_core::config::AppConfig;
    use stateroot_core::local_store;

    fn test_ctx(project: &Path) -> Ctx {
        Ctx {
            cwd: project.to_path_buf(),
            config_dir: project.join(".config-home"),
            config: AppConfig::default(),
        }
    }

    #[tokio::test]
    async fn same_ingest_key_replays_to_a_single_consumed_line() {
        let tmp = tempfile::tempdir().expect("tmp");
        local_store::init_skeleton(tmp.path(), "p", "n", "default").expect("init");
        let ctx = test_ctx(tmp.path());
        let key = local_store::mint_ingest_key();
        let op = serde_json::json!({
            "kind": "snap",
            "ingest_key": key,
            "harness": "cursor",
            "enqueued_at": now_rfc3339(),
            "attempts": 0,
        });
        local_store::outbox_append(tmp.path(), &op).expect("a");
        local_store::outbox_append(tmp.path(), &op).expect("b");
        run_drain(&ctx).await.expect("drain 1");
        run_drain(&ctx).await.expect("drain 2");
        let ledger = std::fs::read_to_string(
            local_store::root(tmp.path()).join(local_store::FINALIZE_CONSUMED_PATH),
        )
        .unwrap_or_default();
        let hits = ledger.lines().filter(|l| l.contains(&key)).count();
        assert_eq!(hits, 1, "{ledger}");
    }

    #[tokio::test]
    async fn live_lock_makes_second_drain_a_noop() {
        let tmp = tempfile::tempdir().expect("tmp");
        local_store::init_skeleton(tmp.path(), "p", "n", "default").expect("init");
        let keys = local_store::enqueue_finalize_trio(tmp.path(), "cursor").expect("enq");
        let lock = local_store::root(tmp.path()).join(local_store::FINALIZE_LOCK_PATH);
        std::fs::create_dir_all(lock.parent().unwrap()).expect("mkdir");
        std::fs::write(
            &lock,
            format!("{{\"pid\":1,\"started_at\":\"{}\"}}\n", now_rfc3339()),
        )
        .expect("lock");
        let ctx = test_ctx(tmp.path());
        run_drain(&ctx).await.expect("noop");
        let pending = local_store::outbox_pending(tmp.path()).expect("pending");
        assert_eq!(pending.len(), 3, "lock must leave the queue untouched");
        assert_eq!(keys.len(), 3);
    }
}
