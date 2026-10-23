//! `stateroot delegate` — spawn another harness CLI as a DETACHED subagent.
//!
//! Async-only by design: the spawn path writes a `stateroot.delegation.v1`
//! record with `status: "running"` and a pid, launches a detached worker
//! (this same binary with hidden `--_worker`), prints the delegation id and
//! exits 0 immediately. Nothing is ever killed and nothing blocks — the
//! harness runs to its natural end; observation is pull-based
//! (`delegate list` / `delegate status <id>`) and completions surface in the
//! digest's `## Recent Delegations` section. The caller stays the face; the
//! subagent is labor.

use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Stdio;

use anyhow::Result;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use stateroot_core::local_store::{self, now_rfc3339};
use stateroot_core::skill_federation::{binary_probe, load_registry, normalize_harness};

use super::{harness, harness_cli, truncate, Ctx};
use crate::cli::{DelegateAction, DelegateArgs};

/// Anti-recursion cap (the `delegationDepth` lesson): at this depth a
/// subagent may not spawn further subagents.
const MAX_DELEGATION_DEPTH: u32 = 2;
/// Env var carrying the current delegation depth; the worker runs at
/// parent+1, its harness child at parent+2 — the guard then refuses.
const DEPTH_ENV: &str = "STATEROOT_DELEGATION_DEPTH";
/// Log-tail cap for `delegate status <id>` (chars, from the end).
const STATUS_TAIL_CAP: usize = 8000;
/// Prompt prefix: the minimal subagent contract (strings only, per doctrine).
const SUBAGENT_CONTRACT: &str = "You are a subagent delegated via StateRoot. Do the task in this project; project context is available via the stateroot digest. End with a concise final conclusion — the caller receives only your final output.";

/// Parse the depth env value; anything missing/unparseable is depth 0.
fn parse_depth(raw: Option<&str>) -> u32 {
    raw.and_then(|raw| raw.trim().parse().ok()).unwrap_or(0)
}

fn delegation_depth() -> u32 {
    parse_depth(std::env::var(DEPTH_ENV).ok().as_deref())
}

/// Last `max` chars of `text` — the bounded tail shown to observers.
fn tail(text: &str, max: usize) -> String {
    let len = text.chars().count();
    if len <= max {
        text.to_string()
    } else {
        text.chars().skip(len - max).collect()
    }
}

/// The delegations store directory for one project.
fn delegations_dir(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join("delegations")
}

/// Append one event to a record's bounded history (64 events / 32 KiB of
/// JSON — honest, lossy observability: drops are counted, never silent).
fn append_event(record: &mut Value, event: &str, detail: &str) {
    const MAX_EVENTS: usize = 64;
    const MAX_BYTES: usize = 32 * 1024;
    let obj = record.as_object_mut().expect("record object");
    let mut dropped = obj
        .get("dropped_events")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    {
        let events = obj.entry("events").or_insert_with(|| json!([]));
        let arr = events.as_array_mut().expect("events array");
        let seq = dropped + arr.len() as u64 + 1;
        arr.push(json!({"seq": seq, "ts": now_rfc3339(), "event": event, "detail": detail}));
        while arr.len() > MAX_EVENTS {
            arr.remove(0);
            dropped += 1;
        }
        while arr.len() > 1
            && serde_json::to_string(&*arr).map(|s| s.len()).unwrap_or(0) > MAX_BYTES
        {
            arr.remove(0);
            dropped += 1;
        }
    }
    if dropped > 0 {
        obj.insert("dropped_events".into(), json!(dropped));
    }
}

fn save_record(path: &Path, record: &Value) -> Result<()> {
    stateroot_core::safe_io::atomic_replace_json(path, record)?;
    Ok(())
}

/// Run `stateroot delegate` (spawn by default; `list` / `status` observe).
pub fn run(ctx: &Ctx, args: &DelegateArgs) -> Result<i32> {
    match &args.action {
        Some(DelegateAction::List) => {
            list(ctx)?;
            Ok(0)
        }
        Some(DelegateAction::Status { id }) => {
            status(ctx, id)?;
            Ok(0)
        }
        Some(DelegateAction::Cancel { id }) => {
            cancel(ctx, id)?;
            Ok(0)
        }
        None if args._worker => worker(ctx, args),
        None if args.plan.is_some() => spawn_plan(ctx, args),
        None => spawn(ctx, args),
    }
}

/// Provision one isolated lineage for an approved plan, then reuse the normal
/// idempotent detached-worker path.  The plan is activated only in its fork
/// checkout, so simultaneous plans never demote each other on the trunk.
fn spawn_plan(ctx: &Ctx, args: &DelegateArgs) -> Result<i32> {
    ctx.require_project()?;
    if args.task.is_some() || args.worktree.is_some() {
        anyhow::bail!(
            "--plan owns the task and worktree; do not combine it with --task or --worktree"
        );
    }
    let plan_id = args.plan.as_deref().expect("guarded");
    let (plan, _) = stateroot_core::plans::load(&ctx.cwd, plan_id)
        .ok_or_else(|| anyhow::anyhow!("unknown plan `{plan_id}` — run `stateroot plan list`"))?;
    if plan.status != "approved" {
        anyhow::bail!(
            "plan {} is {} — approve it before running in parallel",
            plan.id,
            plan.status
        );
    }
    let to = args
        .to
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("delegate --plan requires --to <harness>"))?;
    resolve(to)?;
    let digest = Sha256::digest(plan.id.as_bytes());
    let key = format!(
        "plan-{}",
        digest[..12]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    if let Some((_path, existing)) = load_record(&ctx.cwd, &key) {
        let mut retry = args.clone();
        retry.plan = None;
        retry.key = Some(key.clone());
        retry.task = existing
            .get("task")
            .and_then(Value::as_str)
            .map(str::to_string);
        retry.worktree = existing
            .get("worktree")
            .and_then(Value::as_str)
            .map(str::to_string);
        if retry.task.is_none() || retry.worktree.is_none() {
            anyhow::bail!(
                "parallel delegation {key} is incomplete — inspect its record before retrying"
            );
        }
        return spawn(ctx, &retry);
    }
    let base = stateroot_core::roots::latest_root(&ctx.cwd)?.ok_or_else(|| {
        anyhow::anyhow!("no trunk root yet — run `stateroot snap` before parallel work")
    })?;
    let (fork_id, _) = stateroot_core::roots::fork_root(&ctx.cwd, &base, None, "cli")?;
    let worktree = ctx
        .cwd
        .parent()
        .unwrap_or(&ctx.cwd)
        .join(format!(".stateroot-work-{fork_id}"));
    stateroot_core::roots::fork_materialize(&ctx.cwd, &fork_id, &worktree, Some(&plan.id))?;
    stateroot_core::plans::transition(
        &worktree,
        &plan.id,
        stateroot_core::plans::PlanStatus::Active,
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    let mut launch = args.clone();
    launch.plan = None;
    launch.key = Some(key);
    launch.worktree = Some(worktree.to_string_lossy().to_string());
    launch.task = Some(format!(
        "Execute the plan at .stateroot/plans/{}.md. Read it first. Do not re-plan.",
        plan.id
    ));
    // `spawn` normally prints the record before this wrapper can attach the
    // fork metadata. Render the completed envelope ourselves instead.
    launch.json = false;
    launch._quiet = true;
    let result = spawn(ctx, &launch);
    if result.is_ok() {
        if let Some((path, mut record)) = load_record(&ctx.cwd, launch.key.as_deref().expect("set"))
        {
            record["fork_id"] = json!(fork_id);
            record["plan_id"] = json!(plan.id);
            save_record(&path, &record)?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&record)?);
            }
        }
    }
    result
}

/// Resolve the named harness to (id, command, delegation spec) or a loud
/// error listing the cli-mode harnesses (delegate fails loudly, unlike init).
fn resolve(
    name: &str,
) -> Result<(
    String,
    String,
    stateroot_core::skill_federation::DelegationSpec,
)> {
    let registry = load_registry().map_err(|e| anyhow::anyhow!(e))?;
    let cli_mode: Vec<String> = registry
        .harnesses
        .iter()
        .filter(|e| e.delegation.mode == "cli" && e.delegation.command.is_some())
        .map(|e| e.id.clone())
        .collect();
    let id = normalize_harness(name);
    let Some(entry) = registry.harnesses.iter().find(|e| e.id == id) else {
        anyhow::bail!(
            "unknown harness '{name}' — cli-mode harnesses: {}",
            cli_mode.join(", ")
        );
    };
    let spec = &entry.delegation;
    let Some(command) = spec.command.clone().filter(|_| spec.mode == "cli") else {
        anyhow::bail!(
            "harness '{id}' has no CLI delegation (mode '{}') — cli-mode harnesses: {}",
            spec.mode,
            cli_mode.join(", ")
        );
    };
    if !binary_probe(None)(&command) {
        anyhow::bail!(
            "harness '{id}' binary '{command}' not found on PATH — cli-mode harnesses: {}",
            cli_mode.join(", ")
        );
    }
    Ok((id, command, spec.clone()))
}

/// Idempotency-key validation (repair Phase 6B): a strict charset makes
/// traversal and reserved names impossible by construction — keys stay
/// readable and filenames stay safe on every OS.
fn validate_key(key: &str) -> Result<()> {
    let ok = !key.is_empty()
        && key.len() <= 64
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if ok {
        Ok(())
    } else {
        anyhow::bail!(
            "delegation key `{key}` is not a safe id (1–64 chars of A–Z a–z 0–9 . _ -; anything else risks traversal or reserved names)"
        )
    }
}

/// Per-key lock for spawn/cancel/finalize transitions (repair Phase 6B):
/// one state machine per key, fail-closed acquisition.
fn key_lock(dir: &Path, record_id: &str) -> Result<stateroot_core::safe_io::ResourceLock> {
    let path = dir.join("locks").join(format!("{record_id}.lock"));
    stateroot_core::safe_io::ResourceLock::acquire(path)
        .map_err(|e| anyhow::anyhow!("delegation lock for {record_id}: {e}"))
}

/// The spawn path: reserve `starting` with the request fingerprint under
/// the key lock, launch the detached worker, transition to `running`.
fn spawn(ctx: &Ctx, args: &DelegateArgs) -> Result<i32> {
    ctx.require_project()?;
    let to = args
        .to
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("delegate spawn requires --to <harness>"))?;
    let task = args
        .task
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("delegate spawn requires --task <text>"))?;
    let (id, command, _spec) = resolve(to)?;

    // Depth guard: a subagent may not spawn further subagents.
    let depth = delegation_depth();
    if depth >= MAX_DELEGATION_DEPTH {
        anyhow::bail!(
            "delegation depth cap reached ({DEPTH_ENV}={depth}) — a subagent may not spawn further subagents"
        );
    }

    let ts = now_rfc3339();
    let stamp = ts.replace([':', '.'], "-");
    // Idempotency key (WS5): a caller-supplied key IS the record id, so a
    // replayed spawn re-attaches (live) or resubmits (failed/lost) instead of
    // blind double-spawning — OpenViking's Idempotency-Key pattern.
    if let Some(key) = &args.key {
        validate_key(key)?;
    }
    let record_id = args.key.clone().unwrap_or_else(|| format!("{stamp}-{id}"));
    let dir = delegations_dir(&ctx.cwd);
    std::fs::create_dir_all(&dir)?;

    // The request fingerprint: a same-key request that differs is a
    // different task — rejected, never silently re-keyed.
    let fingerprint = json!({
        "harness": id,
        "task": task,
        "worktree": args.worktree,
    });

    // Per-key state machine under the key lock (repair Phase 6B).
    let _guard = key_lock(&dir, &record_id)?;

    // Idempotency gate: an existing record with the same key decides.
    // Failed and lost workers are deliberately retryable under that same
    // identity.  A corrected runner or a transient harness failure must not
    // force a human to mint a new key, but terminal success/cancellation stay
    // immutable.  Keep a compact attempt history on the replacement record.
    let mut attempt = 1_u64;
    let mut retries = Vec::new();
    // A terminal record's pid is dead by definition — a live pid at that
    // number can only be the OS recycling it for an unrelated process, which
    // once misread a retry as "already running" and silently no-opped it.
    // Terminal retries skip the pid check entirely.
    let mut terminal_retry = false;
    if let Some((_path, existing)) = load_record(&ctx.cwd, &record_id) {
        let prior = existing.get("fingerprint").cloned().unwrap_or(json!(null));
        if prior != json!(null) && prior != fingerprint {
            anyhow::bail!(
                "delegation key `{record_id}` exists with a different request — pick a new key"
            );
        }
        if let Some(outcome) = existing.get("outcome").and_then(Value::as_str) {
            if !matches!(outcome, "failed" | "lost") {
                anyhow::bail!(
                    "delegation key `{record_id}` already finished ({outcome}) — pick a new key"
                );
            }
            attempt = existing.get("attempt").and_then(Value::as_u64).unwrap_or(1) + 1;
            retries = existing
                .get("retries")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            retries.push(json!({
                "attempt": attempt - 1,
                "outcome": outcome,
                "exit_code": existing.get("exit_code").cloned().unwrap_or(Value::Null),
                "duration_ms": existing.get("duration_ms").cloned().unwrap_or(Value::Null),
                "ended_at": existing.get("ended_at").cloned().unwrap_or(Value::Null),
                "outcome_root": existing.get("outcome_root").cloned().unwrap_or(Value::Null),
                "log": existing.get("log").cloned().unwrap_or(Value::Null),
            }));
            terminal_retry = true;
        }
        if !terminal_retry {
            if existing.get("status").and_then(Value::as_str) == Some("starting") {
                // A reservation whose spawn never landed: resubmit under the
                // same key (recover-before-cancel pattern).
            } else {
                if !stateroot_core::delegation_status::owns_namespace(&existing) {
                    anyhow::bail!(
                        "delegation key `{record_id}` has unverified process ownership on this host; observe or retry it from its original runtime"
                    );
                }
                let pid = existing.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32;
                if pid != 0 && pid_alive(pid) {
                    println!(
                        "delegation {record_id} already running (pid {pid}) — same key, no double-spawn"
                    );
                    return Ok(0);
                }
                // Lost worker: fall through and resubmit under the same key.
            }
        }
    }

    // The work directory: a fork worktree isolates the subagent (WS5);
    // records and lineage stay with the CALLING project. Validated as a
    // fork of THIS project (repair Phase 6B): an arbitrary directory with
    // `.stateroot` is not enough.
    let work_dir: PathBuf = match &args.worktree {
        Some(w) => {
            let p = PathBuf::from(w);
            if !p.is_dir() {
                anyhow::bail!("worktree {w} is not a directory");
            }
            let context = stateroot_core::local_store::fork_context(&p).ok_or_else(|| {
                anyhow::anyhow!(
                    "worktree {w} has no fork context — materialize with `stateroot fork <root> --worktree`"
                )
            })?;
            let fork_record = ctx
                .cwd
                .join(".stateroot/forks")
                .join(format!("{}.json", context.fork));
            if !fork_record.is_file() {
                anyhow::bail!(
                    "worktree {w} is a fork checkout, but fork `{}` is not registered in this project",
                    context.fork
                );
            }
            p
        }
        None => ctx.cwd.clone(),
    };

    let log_name = format!("{stamp}-{id}-d{depth}.log");
    let log_path = dir.join(&log_name);
    let log_rel = format!(".stateroot/delegations/{log_name}");

    // Reserve `starting` with the request fingerprint BEFORE spawning
    // (repair Phase 6B): a crash between reservation and spawn leaves a
    // resumable state, and a same-key replay with a different request is
    // rejected against this fingerprint.
    let mut record = json!({
        "schema_version": "stateroot.delegation.v2",
        "id": record_id,
        "ts": ts,
        "depth": depth,
        "harness": id,
        "task": task,
        "command": command,
        "status": "starting",
        "attempt": attempt,
        "fingerprint": fingerprint,
        "log": log_rel,
        "process_namespace": stateroot_core::delegation_status::process_namespace(),
    });
    if !retries.is_empty() {
        record["retries"] = Value::Array(retries);
    }
    if let Some(w) = &args.worktree {
        record["worktree"] = json!(w);
    }
    append_event(&mut record, "reserve", "starting reserved under key lock");
    if attempt > 1 {
        append_event(
            &mut record,
            "retry",
            &format!("retrying failed/lost delegation as attempt {attempt}"),
        );
    }
    write_record(&dir, &record)?;

    // Detached worker = this binary in hidden worker mode; its stdout/stderr
    // redirect into the delegation log (diagnostics + worker header line).
    // The fds MUST be O_APPEND: the worker keeps writing after finalize()
    // appends the outcome sections (e.g. the auto-update's tracing WARN at
    // process exit), and a stale shared offset would overwrite that content.
    let (log_file, log_err) = super::detached::open_log_append(&log_path)?;
    let mut worker_args = vec![
        "delegate".to_string(),
        "--to".to_string(),
        to.to_string(),
        "--task".to_string(),
        task.to_string(),
        "--_worker".to_string(),
        "--record-id".to_string(),
        record_id.clone(),
        "--record-in".to_string(),
        ctx.cwd.to_string_lossy().to_string(),
    ];
    if let Some(w) = &args.worktree {
        worker_args.push("--worktree".to_string());
        worker_args.push(w.clone());
    }
    for skill in &args.skills {
        worker_args.push("--skill".to_string());
        worker_args.push(skill.clone());
    }
    if args.ambient_skills {
        worker_args.push("--ambient-skills".to_string());
    }
    let child = {
        let mut cmd = std::process::Command::new(
            std::env::current_exe().map_err(|e| anyhow::anyhow!("resolve own binary: {e}"))?,
        );
        cmd.args(&worker_args)
            .current_dir(&work_dir)
            .env(DEPTH_ENV, (depth + 1).to_string())
            .stdout(log_file)
            .stderr(log_err);
        let plan = super::detached::DetachPlan {
            args: worker_args.clone(),
            setsid: cfg!(unix),
            breakaway: cfg!(windows),
            stdin_null: true,
        };
        debug_assert!(!super::detached::argv_looks_like_secret(&plan.args));
        super::detached::apply_detach_flags(&mut cmd, &plan);
        cmd.spawn()?
    };
    let pid = child.id();
    drop(child); // detached: no wait, no kill, ever.

    // Transition starting → running with the pid (the reservation already
    // exists; update it in place under the key lock we still hold).
    {
        let obj = record.as_object_mut().expect("record object");
        obj.insert("status".into(), json!("running"));
        obj.insert("pid".into(), json!(pid));
    }
    append_event(&mut record, "spawn", &format!("pid {pid}"));
    write_record(&dir, &record)?;
    if !args._quiet && args.json {
        println!("{}", serde_json::to_string_pretty(&record)?);
    } else if !args._quiet {
        println!(
            "delegated to {id} · delegation {} · running in background (pid {pid})",
            record["id"].as_str().unwrap_or("")
        );
        println!(
            "  log: {log_rel} · observe: `stateroot delegate status {}`",
            record["id"].as_str().unwrap_or("")
        );
    }
    Ok(0)
}

/// The worker: run the delegation to its natural end and finalize the record.
/// Every failure mode lands IN the record — never silently.
fn worker(ctx: &Ctx, args: &DelegateArgs) -> Result<i32> {
    ctx.require_project()?;
    let record_id = args
        .record_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("worker requires --record-id <id>"))?;
    let to = args
        .to
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("worker requires --to <harness>"))?;
    let task = args
        .task
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("worker requires --task <text>"))?;
    // The record and the lineage note belong to the CALLING project even
    // when the work runs in a fork worktree (WS5 --worktree).
    let record_root: PathBuf = args
        .record_in
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| ctx.cwd.clone());
    let started = std::time::Instant::now();
    // The attempt this worker is serving, read from the reservation that
    // spawned it. A same-key retry re-reserves the record with a higher
    // attempt while a slow capture from an earlier attempt may still be
    // in flight — the guard below stops that late write from clobbering
    // the newer reservation.
    let my_attempt = load_record(&record_root, record_id)
        .and_then(|(_, record)| record.get("attempt").and_then(Value::as_u64))
        .unwrap_or(1);
    let result = worker_run(ctx, args, to, task);
    match result {
        Ok((id, output)) => {
            let outcome = if output.status.success() {
                "completed"
            } else {
                "failed"
            };
            finalize(
                &record_root,
                record_id,
                my_attempt,
                outcome,
                output.status.code(),
                started.elapsed().as_millis(),
                &format!(
                    "\noutcome: {outcome} · exit_code: {:?} · duration_ms: {}\n\n--- stdout ---\n{}\n\n--- stderr ---\n{}\n",
                    output.status.code(),
                    started.elapsed().as_millis(),
                    output.stdout,
                    output.stderr
                ),
            )?;
            // Outcome → immutable root (repair Phase 6B): a delegated task
            // is not terminal until its worktree is captured into the fork
            // lineage. Failed outcomes capture too (salvage semantics).
            capture_outcome_root(&ctx.cwd, &record_root, record_id, &id, my_attempt);
            episodic_lineage(
                &record_root,
                &id,
                task,
                outcome,
                started.elapsed().as_secs(),
            )?;
            Ok(output.status.code().unwrap_or(1))
        }
        Err(err) => {
            let _ = finalize(
                &record_root,
                record_id,
                my_attempt,
                "failed",
                None,
                started.elapsed().as_millis(),
                &format!("\noutcome: failed · worker error: {err:#}\n"),
            );
            Err(err)
        }
    }
}

/// Snapshot the delegated worktree into the fork lineage and record the
/// root — completion/cancellation is not terminal until this lands
/// (best-effort: the error is recorded in the record's events, never
/// silently dropped on the floor).
fn capture_outcome_root(
    work_dir: &Path,
    record_root: &Path,
    record_id: &str,
    harness: &str,
    attempt: u64,
) {
    capture_outcome_root_with_guard(work_dir, record_root, record_id, harness, attempt, None);
}

#[cfg(test)]
thread_local! {
    static TEST_AFTER_OUTCOME_SNAPSHOT: std::cell::RefCell<Option<OutcomeCaptureObserver>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
type OutcomeCaptureObserver = Box<dyn FnOnce(&Path)>;

fn capture_outcome_root_with_guard(
    work_dir: &Path,
    record_root: &Path,
    record_id: &str,
    harness: &str,
    attempt: u64,
    held_key_guard: Option<&stateroot_core::safe_io::ResourceLock>,
) {
    let Some((_, captured)) = load_record(record_root, record_id) else {
        return;
    };
    // Attempt guard (see finalize): a re-reserved record belongs to a newer
    // attempt; the older attempt's root pointer must not clobber it.
    if captured.get("attempt").and_then(Value::as_u64) != Some(attempt) {
        return;
    }
    // Terminal delegation capture is an explicit semantic milestone, not
    // ordinary changed-project-file detection: the outcome root must seal
    // the final live store (closed plan, return handoff, late captured
    // intelligence) even when no product file moved since the code root.
    // `snap_if_changed` intentionally ignores `.stateroot/` deltas and
    // would reuse the pre-intelligence root here.
    let result = stateroot_core::roots::create_root(
        work_dir,
        harness,
        "milestone: delegation outcome",
        None,
    );
    #[cfg(test)]
    if let Some(observer) = TEST_AFTER_OUTCOME_SNAPSHOT.with(|slot| slot.borrow_mut().take()) {
        observer(record_root);
    }
    // Snapshotting can be slow. Serialize only the final read/check/save with
    // retry reservations; cancellation already owns this same key guard.
    let _guard = if held_key_guard.is_none() {
        match key_lock(&delegations_dir(record_root), record_id) {
            Ok(guard) => Some(guard),
            Err(error) => {
                tracing::warn!("delegation {record_id} outcome capture was not attached: {error}");
                return;
            }
        }
    } else {
        None
    };
    let Some((path, mut record)) = load_record(record_root, record_id) else {
        return;
    };
    if [
        "id",
        "attempt",
        "pid",
        "process_namespace",
        "fingerprint",
        "ts",
        "log",
        "harness",
        "command",
        "outcome",
        "status",
    ]
    .iter()
    .any(|field| record.get(*field) != captured.get(*field))
    {
        return;
    }
    match result {
        Ok((manifest, _transition)) => {
            record["outcome_root"] = json!(manifest.id);
            append_event(&mut record, "capture", "worktree captured to fork lineage");
        }
        Err(err) => {
            append_event(&mut record, "capture-error", &format!("{err}"));
        }
    }
    let _ = save_record(&path, &record);
}

/// The worker's run path (today's flow, minus any kill condition).
fn worker_run(
    ctx: &Ctx,
    args: &DelegateArgs,
    to: &str,
    task: &str,
) -> Result<(String, harness_cli::HarnessOutput)> {
    let (id, _command, spec) = resolve(to)?;
    let depth = delegation_depth();
    if depth >= MAX_DELEGATION_DEPTH {
        anyhow::bail!(
            "delegation depth cap reached ({DEPTH_ENV}={depth}) — a subagent may not spawn further subagents"
        );
    }
    // Header line lands in the log via the spawn-time redirect.
    println!(
        "delegation to {id} · depth {depth} · {} · running (pid {})",
        now_rfc3339(),
        std::process::id()
    );

    let prompt = format!("{SUBAGENT_CONTRACT}\n\n{task}");
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))?;
    let skill_paths = args
        .skills
        .iter()
        .map(|slug| harness::canonical_skill_path(ctx, &home, slug))
        .collect::<Result<Vec<_>>>()?
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let policy = harness_cli::LaunchPolicy {
        skill_paths,
        ambient_skills: args.ambient_skills,
        env: vec![(DEPTH_ENV.to_string(), (depth + 1).to_string())],
    };
    // NO cap: the harness runs to its natural end. Its own internal limits
    // belong to the harness, not to us.
    let output = harness_cli::run_capture_streaming(&ctx.cwd, &id, &spec, &prompt, &policy, None)?;
    Ok((id, output))
}

/// Rewrite a record file with final fields (status → outcome).
fn finalize(
    record_root: &Path,
    record_id: &str,
    attempt: u64,
    outcome: &str,
    exit_code: Option<i32>,
    duration_ms: u128,
    log_append: &str,
) -> Result<()> {
    let _guard = key_lock(&delegations_dir(record_root), record_id)?;
    let Some((path, mut record)) = load_record(record_root, record_id) else {
        anyhow::bail!("worker record `{record_id}` is gone — cannot finalize");
    };
    // Attempt guard: a same-key retry may have re-reserved the record while
    // this worker was finishing. A late write from the older attempt must
    // never overwrite the newer reservation.
    if record.get("attempt").and_then(Value::as_u64) != Some(attempt) {
        return Ok(());
    }
    if let Some(log_rel) = record.get("log").and_then(Value::as_str) {
        let log_path = record_root.join(log_rel);
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        file.write_all(log_append.as_bytes())?;
    }
    let obj = record.as_object_mut().unwrap();
    obj.remove("status");
    obj.insert("outcome".into(), json!(outcome));
    obj.insert("exit_code".into(), json!(exit_code));
    obj.insert("duration_ms".into(), json!(duration_ms));
    obj.insert("ended_at".into(), json!(now_rfc3339()));
    append_event(&mut record, "finalize", outcome);
    save_record(&path, &record)?;
    Ok(())
}

fn write_record(dir: &Path, record: &Value) -> Result<()> {
    let id = record
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("record without id"))?;
    stateroot_core::safe_io::atomic_replace_json(&dir.join(format!("{id}.json")), record)?;
    Ok(())
}

/// Load one record by id (exact, or a unique prefix).
fn load_record(project_dir: &Path, id: &str) -> Option<(PathBuf, Value)> {
    let all = read_records(project_dir);
    if let Some(found) = all
        .iter()
        .find(|(_, r)| r.get("id").and_then(Value::as_str) == Some(id))
    {
        return Some(found.clone());
    }
    let matches: Vec<&(PathBuf, Value)> = all
        .iter()
        .filter(|(_, r)| {
            r.get("id")
                .and_then(Value::as_str)
                .is_some_and(|rid| rid.starts_with(id))
        })
        .collect();
    if matches.len() == 1 {
        Some(matches[0].clone())
    } else {
        None
    }
}

/// Every delegation record file, newest first — parsed records only
/// (id-keyed lookups); degraded entries stay visible through
/// `delegation_status::read_entries` in the observational surfaces.
pub(crate) fn read_records(project_dir: &Path) -> Vec<(PathBuf, Value)> {
    stateroot_core::delegation_status::read_records(project_dir)
}

/// pid liveness: `/proc` on Linux/WSL, `kill -0` elsewhere on unix,
/// `tasklist` on Windows.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    if Path::new(&format!("/proc/{pid}")).exists() {
        return true;
    }
    // kill(2) signal 0 — no external binary needed.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// pid liveness on Windows: `tasklist` filter probe (no new deps).
#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            out.status.success()
                && text.lines().any(|line| {
                    let Some(rest) = line.trim().strip_prefix('"') else {
                        return false;
                    };
                    let Some((_, columns)) = rest.split_once("\",") else {
                        return false;
                    };
                    columns
                        .split(',')
                        .next()
                        .and_then(|value| value.trim_matches('"').parse::<u32>().ok())
                        == Some(pid)
                })
        })
        .unwrap_or(false)
}

/// Run `stateroot delegate list`.
fn list(ctx: &Ctx) -> Result<()> {
    ctx.require_project()?;
    let entries = stateroot_core::delegation_status::read_entries(&ctx.cwd);
    if entries.is_empty() {
        println!("no delegations recorded");
        return Ok(());
    }
    for entry in &entries {
        match entry {
            stateroot_core::delegation_status::DelegationEntry::Record(_, record) => {
                let status = stateroot_core::delegation_status::interpret(record);
                let id = record.get("id").and_then(Value::as_str).unwrap_or("");
                let harness = record.get("harness").and_then(Value::as_str).unwrap_or("");
                let task = record.get("task").and_then(Value::as_str).unwrap_or("");
                println!(
                    "{} · {} · {} · {}",
                    id,
                    harness,
                    status.as_str(),
                    truncate(task, 60)
                );
            }
            stateroot_core::delegation_status::DelegationEntry::Degraded { path, reason } => {
                let file = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                println!("{file} · degraded · {reason} (bytes preserved)");
            }
        }
    }
    Ok(())
}

/// Run `stateroot delegate status <id>` — the record plus a bounded log tail.
fn status(ctx: &Ctx, id: &str) -> Result<()> {
    ctx.require_project()?;
    let Some((_path, record)) = load_record(&ctx.cwd, id) else {
        anyhow::bail!("no delegation matches `{id}` — run `stateroot delegate list`");
    };
    let live = stateroot_core::delegation_status::interpret(&record);
    let live = live.as_str();
    let harness = record.get("harness").and_then(Value::as_str).unwrap_or("");
    let task = record.get("task").and_then(Value::as_str).unwrap_or("");
    println!(
        "delegation {} · {} · {live}",
        record["id"].as_str().unwrap_or(""),
        harness
    );
    if live == "cancelling" {
        println!(
            "  cancellation in flight — if the canceller died, `stateroot delegate cancel {id}` resumes it (never a permanent terminal lie)",
            id = record["id"].as_str().unwrap_or("")
        );
    }
    println!("  task: {}", truncate(task, 200));
    if let Some(pid) = record.get("pid").and_then(Value::as_u64) {
        println!("  pid: {pid}");
    }
    if let (Some(code), Some(ms)) = (record.get("exit_code"), record.get("duration_ms")) {
        println!("  exit_code: {code} · duration_ms: {ms}");
    }
    let log_rel = record.get("log").and_then(Value::as_str).unwrap_or("");
    let log_body = std::fs::read_to_string(ctx.cwd.join(log_rel)).unwrap_or_default();
    println!("  log: {log_rel}");
    if log_body.trim().is_empty() {
        println!("  (log is empty so far — the worker writes it at completion)");
    } else {
        println!("\n{}", tail(&log_body, STATUS_TAIL_CAP));
    }
    Ok(())
}

/// Episodic lineage note (written by the worker at completion).
fn episodic_lineage(
    record_root: &Path,
    harness_id: &str,
    task: &str,
    outcome: &str,
    secs: u64,
) -> Result<()> {
    let record = json!({
        "ts": now_rfc3339(),
        "harness": "cli",
        "note": format!(
            "delegated to {harness_id}: {} → {outcome} ({secs}s)",
            truncate(task, 160)
        ),
        "files": [],
    });
    local_store::append_episodic(record_root, &record)?;
    Ok(())
}

/// Two-phase cancel with a full process-tree stop and a partial capture
/// (repair Phase 6B — owner-ratified contract): persist `cancelling`,
/// signal the worker's whole process GROUP (the detached worker is a group
/// leader via setsid/breakaway, so its harness children are covered),
/// escalate TERM → KILL, verify the group is gone, snapshot the partial
/// worktree into the fork lineage, and only then record
/// `cancelled_with_root`. A crash after `cancelling` is resumed by the
/// next read of the record — never a permanent terminal lie.
fn cancel(ctx: &Ctx, id: &str) -> Result<()> {
    ctx.require_project()?;
    let Some((_path, record)) = load_record(&ctx.cwd, id) else {
        anyhow::bail!("no delegation matches `{id}` — run `stateroot delegate list`");
    };
    if let Some(outcome) = record.get("outcome").and_then(Value::as_str) {
        if outcome != "cancelling" {
            println!("delegation {id} is already {outcome}");
            return Ok(());
        }
    }
    let record_id = record
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(id)
        .to_string();
    let dir = delegations_dir(&ctx.cwd);
    let _guard = key_lock(&dir, &record_id)?;
    // Re-read under the lock (cancel and worker finalize serialize).
    let Some((path, mut record)) = load_record(&ctx.cwd, &record_id) else {
        anyhow::bail!("delegation record `{record_id}` vanished mid-cancel");
    };
    if let Some(outcome) = record.get("outcome").and_then(Value::as_str) {
        if outcome != "cancelling" {
            println!("delegation {record_id} is already {outcome}");
            return Ok(());
        }
    }
    let pid = record.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32;

    if !stateroot_core::delegation_status::owns_namespace(&record) {
        anyhow::bail!("cannot cancel delegation {record_id}: its process ownership is unverified on this host; use the original runtime");
    }

    // Phase 1: persisted cancelling — observers see it immediately.
    {
        let obj = record.as_object_mut().unwrap();
        obj.remove("status");
        obj.insert("outcome".into(), json!("cancelling"));
    }
    append_event(&mut record, "cancel-requested", &format!("pid {pid}"));
    save_record(&path, &record)?;

    // Stop the process tree: TERM the group, wait, escalate to KILL.
    if pid != 0 && group_alive(pid) {
        signal_group(pid, Signal::Term);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while group_alive(pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if group_alive(pid) {
            append_event(
                &mut record,
                "cancel-escalate",
                "TERM ignored; sending KILL to the group",
            );
            signal_group(pid, Signal::Kill);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while group_alive(pid) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    let confirmed = pid == 0 || !group_alive(pid);
    append_event(
        &mut record,
        if confirmed {
            "cancel-confirmed"
        } else {
            "cancel-unconfirmed"
        },
        "process tree verified",
    );
    save_record(&path, &record)?;

    // Phase 2: partial capture BEFORE the terminal record — cancellation
    // is not terminal until the partial work is a root in the fork lineage.
    let harness = record
        .get("harness")
        .and_then(Value::as_str)
        .unwrap_or("cli")
        .to_string();
    let work_dir = record
        .get("worktree")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| ctx.cwd.clone());
    let cancel_attempt = record.get("attempt").and_then(Value::as_u64).unwrap_or(1);
    capture_outcome_root_with_guard(
        &work_dir,
        &ctx.cwd,
        &record_id,
        &harness,
        cancel_attempt,
        Some(&_guard),
    );
    let Some((path, mut record)) = load_record(&ctx.cwd, &record_id) else {
        anyhow::bail!("delegation record `{record_id}` vanished mid-cancel");
    };
    {
        let obj = record.as_object_mut().unwrap();
        obj.insert("outcome".into(), json!("cancelled_with_root"));
        obj.insert("cancel_confirmed".into(), json!(confirmed));
        obj.insert("ended_at".into(), json!(now_rfc3339()));
    }
    append_event(
        &mut record,
        "cancel-finalized",
        "partial work captured; recorded",
    );
    save_record(&path, &record)?;
    let log_rel = record.get("log").and_then(Value::as_str).unwrap_or("");
    println!(
        "delegation {record_id} cancelled_with_root{} — partial work kept (log: {log_rel})",
        if confirmed {
            ""
        } else {
            " (process tree NOT confirmed dead)"
        }
    );
    Ok(())
}

enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
fn signal_group(pgid: u32, signal: Signal) {
    let sig = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // kill(2) with a negative pid signals the whole process group — direct
    // syscall, no /bin/kill arg-parsing variance across distros/procps
    // versions (the CI ubuntu runner silently rejected the CLI form while
    // WSL accepted it).
    unsafe {
        libc::kill(-(pgid as i32), sig);
    }
}

#[cfg(windows)]
fn signal_group(pid: u32, signal: Signal) {
    let _ = signal;
    // taskkill /T covers the process tree rooted at the worker.
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(unix)]
fn group_alive(pgid: u32) -> bool {
    // kill(2) with a negative pid and signal 0 probes the process group.
    unsafe { libc::kill(-(pgid as i32), 0) == 0 }
}

#[cfg(windows)]
fn group_alive(pid: u32) -> bool {
    pid_alive(pid)
}

/// The last N delegation entries for the digest section. Records render
/// with the shared status interpretation; degraded entries stay visible as
/// concise preserved-record notes (never silently omitted).
pub(crate) fn recent_delegations(
    project_dir: &Path,
    count: usize,
) -> Vec<(String, String, String)> {
    stateroot_core::delegation_status::read_entries(project_dir)
        .iter()
        .take(count)
        .map(|entry| match entry {
            stateroot_core::delegation_status::DelegationEntry::Record(_, record) => {
                let ts = record.get("ts").and_then(Value::as_str).unwrap_or("");
                let short_ts: String = ts.chars().take(16).collect();
                let harness = record.get("harness").and_then(Value::as_str).unwrap_or("");
                let task = record.get("task").and_then(Value::as_str).unwrap_or("");
                let status = stateroot_core::delegation_status::interpret(record);
                (
                    short_ts,
                    format!("{harness} · {}", status.as_str()),
                    truncate(task, 120),
                )
            }
            stateroot_core::delegation_status::DelegationEntry::Degraded { path, reason } => {
                let file = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (
                    "undated".to_string(),
                    "record · degraded".to_string(),
                    truncate(&format!("{file} ({reason}, bytes preserved)"), 120),
                )
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use stateroot_core::skill_federation::build_launch_argv_from_spec;

    #[test]
    fn outcome_capture_never_overwrites_a_replaced_attempt_or_worker() {
        for replacement in [
            json!({"attempt": 2, "pid": 202, "outcome": "failed"}),
            json!({"attempt": 1, "pid": 202, "outcome": "failed"}),
            json!({"attempt": 1, "pid": 101, "process_namespace": "another-host", "outcome": "failed"}),
        ] {
            let project = tempfile::tempdir().unwrap();
            let dir = delegations_dir(project.path());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(project.path().join("work.txt"), "captured work").unwrap();
            let record = json!({"id":"capture-race","attempt":1,"pid":101,"process_namespace":"fixture-host","fingerprint":{"task":"fixture"},"ts":"first","log":"old.log","harness":"codex","command":"codex","outcome":"failed","events":[]});
            write_record(&dir, &record).unwrap();
            let mut newer = record.clone();
            for (key, value) in replacement.as_object().unwrap() {
                newer[key] = value.clone();
            }
            append_event(&mut newer, "reserve", "newer record must survive");
            let expected = newer.clone();
            TEST_AFTER_OUTCOME_SNAPSHOT.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |project| {
                    let dir = delegations_dir(project);
                    let _guard = key_lock(&dir, "capture-race").unwrap();
                    write_record(&dir, &newer).unwrap();
                }));
            });
            capture_outcome_root(project.path(), project.path(), "capture-race", "codex", 1);
            assert_eq!(
                load_record(project.path(), "capture-race").unwrap().1,
                expected
            );
            if expected["attempt"] == 2 {
                finalize(
                    project.path(),
                    "capture-race",
                    1,
                    "failed",
                    Some(1),
                    0,
                    "old completion\n",
                )
                .unwrap();
                assert_eq!(
                    load_record(project.path(), "capture-race").unwrap().1,
                    expected
                );
                assert!(
                    !project.path().join("old.log").exists(),
                    "superseded completion must not append to the old log"
                );
            }
            assert!(stateroot_core::roots::latest_root(project.path()).unwrap().is_some(), "captured work must remain in immutable lineage even when its old record was superseded");
        }
    }

    #[test]
    fn outcome_capture_appends_to_fresh_history_for_the_same_worker() {
        let project = tempfile::tempdir().unwrap();
        let dir = delegations_dir(project.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(project.path().join("work.txt"), "captured work").unwrap();
        let record = json!({"id":"capture-history","attempt":1,"pid":101,"process_namespace":"fixture-host","outcome":"failed","events":[]});
        write_record(&dir, &record).unwrap();
        TEST_AFTER_OUTCOME_SNAPSHOT.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |project| {
                let dir = delegations_dir(project);
                let _guard = key_lock(&dir, "capture-history").unwrap();
                let (_, mut fresh) = load_record(project, "capture-history").unwrap();
                append_event(&mut fresh, "observation", "arrived while capturing");
                fresh["retained_context"] = json!("preserve this");
                write_record(&dir, &fresh).unwrap();
            }));
        });
        capture_outcome_root(
            project.path(),
            project.path(),
            "capture-history",
            "codex",
            1,
        );
        let (_, captured) = load_record(project.path(), "capture-history").unwrap();
        assert_eq!(captured["outcome"], "failed");
        assert_eq!(captured["retained_context"], "preserve this");
        assert_eq!(captured["events"][0]["event"], "observation");
        assert_eq!(captured["events"][1]["event"], "capture");
        assert_eq!(
            captured["outcome_root"].as_str(),
            stateroot_core::roots::latest_root(project.path())
                .unwrap()
                .as_deref()
        );
        capture_outcome_root(
            &project.path().join("work.txt"),
            project.path(),
            "capture-history",
            "codex",
            1,
        );
        let (_, failed_capture) = load_record(project.path(), "capture-history").unwrap();
        assert_eq!(failed_capture["outcome"], "failed");
        assert_eq!(failed_capture["outcome_root"], captured["outcome_root"]);
        assert_eq!(
            failed_capture["events"].as_array().unwrap().last().unwrap()["event"],
            "capture-error"
        );
    }

    #[test]
    fn outcome_capture_reuses_cancel_guard_and_does_not_save_when_unlocked_capture_cannot_lock() {
        let project = tempfile::tempdir().unwrap();
        let dir = delegations_dir(project.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(project.path().join("work.txt"), "cancelled work").unwrap();
        let record = json!({"id":"capture-locked","attempt":1,"pid":101,"process_namespace":"fixture-host","outcome":"cancelling","events":[]});
        write_record(&dir, &record).unwrap();
        let guard = key_lock(&dir, "capture-locked").unwrap();
        assert!(finalize(
            project.path(),
            "capture-locked",
            1,
            "failed",
            Some(1),
            0,
            "must not append\n"
        )
        .is_err());
        assert_eq!(
            load_record(project.path(), "capture-locked").unwrap().1,
            record
        );
        capture_outcome_root(project.path(), project.path(), "capture-locked", "codex", 1);
        assert_eq!(
            load_record(project.path(), "capture-locked").unwrap().1,
            record,
            "lock failure must never save the stale record"
        );
        capture_outcome_root_with_guard(
            project.path(),
            project.path(),
            "capture-locked",
            "codex",
            1,
            Some(&guard),
        );
        let (_, captured) = load_record(project.path(), "capture-locked").unwrap();
        assert_eq!(captured["outcome"], "cancelling");
        assert!(captured["outcome_root"].is_string());
    }

    /// Terminal outcome capture is an explicit semantic milestone: the
    /// product work was already snapshotted, then ONLY StateRoot working
    /// intelligence moves (plan closed, return handoff authored) before the
    /// worker's outcome lands. The reported outcome root must seal that
    /// final store state — reusing the pre-intelligence code root strands
    /// the closed-plan/handoff history outside the fork lineage (real corpus
    /// merge lost 124 inbox notes + 1 log entry to exactly this).
    #[test]
    fn outcome_capture_seals_store_only_terminal_intelligence() {
        let project = tempfile::tempdir().unwrap();
        let dir = delegations_dir(project.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(project.path().join("work.txt"), "captured work").unwrap();
        let record = json!({"id":"capture-store-only","attempt":1,"pid":101,"process_namespace":"fixture-host","fingerprint":{"task":"fixture"},"ts":"first","log":"old.log","harness":"codex","command":"codex","outcome":"completed","events":[]});
        write_record(&dir, &record).unwrap();
        let (code_root, _) = stateroot_core::roots::create_root(
            project.path(),
            "codex",
            "test: product work already immutable",
            None,
        )
        .unwrap();
        // After the code root, only working intelligence changes.
        std::fs::create_dir_all(project.path().join(".stateroot/handoffs")).unwrap();
        std::fs::write(
            project.path().join(".stateroot/handoffs/7-return.md"),
            "final worker intelligence: plan closed, handoff authored",
        )
        .unwrap();
        capture_outcome_root(
            project.path(),
            project.path(),
            "capture-store-only",
            "codex",
            1,
        );
        let (_, captured) = load_record(project.path(), "capture-store-only").unwrap();
        let outcome_root = captured["outcome_root"]
            .as_str()
            .expect("outcome root recorded");
        assert_ne!(
            outcome_root, code_root.id,
            "terminal capture must not reuse a code root that predates final store intelligence"
        );
        let sealed = stateroot_core::roots::get_root(project.path(), outcome_root).unwrap();
        assert_eq!(
            sealed.parents,
            vec![code_root.id.clone()],
            "the closure root descends from the code root"
        );
        let repo = git2::Repository::open(project.path()).unwrap();
        let tree = repo
            .find_commit(git2::Oid::from_str(outcome_root).unwrap())
            .unwrap()
            .tree()
            .unwrap();
        let blob = repo
            .find_blob(
                tree.get_path(Path::new(".stateroot/handoffs/7-return.md"))
                    .expect("final intelligence sealed into the outcome root")
                    .id(),
            )
            .unwrap();
        assert_eq!(
            std::str::from_utf8(blob.content()).unwrap(),
            "final worker intelligence: plan closed, handoff authored"
        );
        // Truthful scope: no eligible product file moved between the code
        // root and the closure root — the delta is exactly the store state.
        let diff = stateroot_core::roots::diff_roots(
            project.path(),
            &code_root.id,
            outcome_root,
            false,
            10,
            10,
        )
        .unwrap();
        assert_eq!(diff["files"], json!([]), "no product file moves");
        assert!(
            diff["state"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["path"] == ".stateroot/handoffs/7-return.md"),
            "store-only delta is the sealed intelligence: {diff}"
        );
    }

    /// Fork-bound form: the worker runs in a fork worktree while the caller
    /// owns the record store. The outcome root must seal the fork's final
    /// store-only intelligence and be readable from the caller's record.
    #[test]
    fn outcome_capture_seals_store_only_intelligence_for_a_fork_worktree() {
        let caller = tempfile::tempdir().unwrap();
        let fork = tempfile::tempdir().unwrap();
        let dir = delegations_dir(caller.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(fork.path().join("work.txt"), "fork work").unwrap();
        let record = json!({"id":"capture-fork-store-only","attempt":1,"pid":101,"process_namespace":"fixture-host","fingerprint":{"task":"fixture"},"ts":"first","log":"fork.log","harness":"codex","command":"codex","outcome":"completed","worktree":fork.path().to_string_lossy(),"events":[]});
        write_record(&dir, &record).unwrap();
        let (code_root, _) = stateroot_core::roots::create_root(
            fork.path(),
            "codex",
            "test: fork product work already immutable",
            None,
        )
        .unwrap();
        std::fs::create_dir_all(fork.path().join(".stateroot/plans")).unwrap();
        std::fs::write(
            fork.path().join(".stateroot/plans/plan_done.md"),
            "status: done at terminal capture",
        )
        .unwrap();
        capture_outcome_root(
            fork.path(),
            caller.path(),
            "capture-fork-store-only",
            "codex",
            1,
        );
        let (_, captured) = load_record(caller.path(), "capture-fork-store-only").unwrap();
        let outcome_root = captured["outcome_root"]
            .as_str()
            .expect("outcome root recorded in the caller store");
        assert_ne!(
            outcome_root, code_root.id,
            "fork terminal capture must seal the fork's final store intelligence"
        );
        let repo = git2::Repository::open(fork.path()).unwrap();
        let tree = repo
            .find_commit(git2::Oid::from_str(outcome_root).unwrap())
            .unwrap()
            .tree()
            .unwrap();
        let blob = repo
            .find_blob(
                tree.get_path(Path::new(".stateroot/plans/plan_done.md"))
                    .expect("fork final intelligence sealed into the outcome root")
                    .id(),
            )
            .unwrap();
        assert_eq!(
            std::str::from_utf8(blob.content()).unwrap(),
            "status: done at terminal capture"
        );
        let product = stateroot_core::roots::current_product_tree(fork.path()).unwrap();
        let code_product = {
            let code_tree = repo
                .find_commit(git2::Oid::from_str(&code_root.id).unwrap())
                .unwrap()
                .tree()
                .unwrap();
            let mut builder = repo.treebuilder(Some(&code_tree)).unwrap();
            builder.remove(".stateroot").unwrap();
            builder.write().unwrap().to_string()
        };
        assert_eq!(
            product, code_product,
            "eligible product source is unchanged between code root and closure root"
        );
    }

    #[test]
    fn depth_parsing_defaults_to_zero() {
        assert_eq!(parse_depth(None), 0);
        assert_eq!(parse_depth(Some("")), 0);
        assert_eq!(parse_depth(Some("abc")), 0);
        assert_eq!(parse_depth(Some("1")), 1);
        assert_eq!(parse_depth(Some(" 2 ")), 2);
        assert!(parse_depth(Some("2")) >= MAX_DELEGATION_DEPTH);
    }

    #[test]
    fn append_event_caps_count_and_bytes_with_a_dropped_counter() {
        let mut record = json!({"id": "r1"});
        for i in 0..70 {
            append_event(&mut record, "tick", &format!("event {i}"));
        }
        let events = record["events"].as_array().expect("events");
        assert_eq!(events.len(), 64, "count-capped");
        assert_eq!(record["dropped_events"], json!(6));
        assert_eq!(events[0]["event"], "tick");
        // Monotonic seq continues across the drop boundary.
        assert_eq!(events[0]["seq"], json!(7));
        // Byte cap: a huge detail drops ALL older history but never the
        // newest entry, even when that entry alone exceeds the budget.
        let big = "x".repeat(40 * 1024);
        append_event(&mut record, "big", &big);
        let events = record["events"].as_array().expect("events");
        assert_eq!(events.len(), 1, "older history must be dropped");
        assert_eq!(events.last().expect("last")["event"], "big");
    }

    #[test]
    fn tail_bounds_by_chars_from_the_end() {
        assert_eq!(tail("hello", 10), "hello");
        let big: String = "x".repeat(20 * 1024);
        assert_eq!(tail(&big, 8000).chars().count(), 8000);
        // Multibyte content must split on char boundaries, never mid-char.
        assert_eq!(tail("héllo wörld", 4), "örld");
    }

    #[test]
    fn delegate_prompt_renders_through_the_registry_spec() {
        let registry = load_registry().expect("registry");
        let claude = registry
            .harnesses
            .iter()
            .find(|e| e.id == "claude")
            .expect("claude entry");
        let prompt = format!("{SUBAGENT_CONTRACT}\n\ndo it");
        assert_eq!(
            build_launch_argv_from_spec(&claude.delegation, Some(&prompt), &[], false),
            Some(vec![
                "claude".into(),
                "--print".into(),
                "--permission-mode".into(),
                "bypassPermissions".into(),
                prompt,
            ])
        );
    }

    /// Consumer composition: the CLI's digest surface renders the shared
    /// status decision, degraded records stay visible, and reads never
    /// rewrite worker evidence. (The status cases themselves live in the
    /// core contract matrix — this pins the CLI to it.)
    #[test]
    fn recent_delegations_render_shared_status_and_preserve_all_records() {
        let dir = tempfile::tempdir().expect("dir");
        let project = dir.path().join("proj");
        let delegations = project.join(".stateroot/delegations");
        std::fs::create_dir_all(&delegations).expect("mkdir");
        // Owned, running, live pid → running.
        write_record(
            &delegations,
            &json!({
                "schema_version": "stateroot.delegation.v2",
                "id": "d-live", "ts": "2026-10-22T00:00:02Z", "harness": "codex", "task": "live work",
                "status": "running", "pid": std::process::id(),
                "process_namespace": stateroot_core::delegation_status::process_namespace(),
                "log": ".stateroot/delegations/d-live.log",
            }),
        )
        .expect("write");
        // Foreign namespace with a dead-pid coincidence → unknown, never lost.
        write_record(
            &delegations,
            &json!({
                "schema_version": "stateroot.delegation.v2",
                "id": "d-foreign", "ts": "2026-10-22T00:00:01Z", "harness": "kimi", "task": "foreign work",
                "status": "running", "pid": 4_000_000u32,
                "process_namespace": "another-host",
                "log": ".stateroot/delegations/d-foreign.log",
            }),
        )
        .expect("write");
        // Malformed bytes → degraded observation, bytes preserved.
        std::fs::write(delegations.join("broken.json"), b"{not json".as_slice()).expect("broken");

        let rows = recent_delegations(&project, 5);
        let rendered: Vec<String> = rows
            .iter()
            .map(|(ts, who, task)| format!("{ts}|{who}|{task}"))
            .collect();
        assert!(
            rendered.iter().any(|row| row.contains("codex · running")),
            "{rendered:?}"
        );
        assert!(
            rendered.iter().any(|row| row.contains("kimi · unknown")),
            "{rendered:?}"
        );
        assert!(
            rendered
                .iter()
                .any(|row| row.contains("degraded") && row.contains("broken.json")),
            "{rendered:?}"
        );
        assert_eq!(
            std::fs::read(delegations.join("broken.json")).expect("bytes"),
            b"{not json".as_slice()
        );
        let (_, retained) = load_record(&project, "d-foreign").expect("record");
        assert_eq!(retained["status"], "running");
        assert!(
            retained.get("outcome").is_none(),
            "status reads never reap a record"
        );
    }

    /// The delegation log's fds must be O_APPEND end-to-end: a late worker
    /// write (auto-update WARN at process exit) must never overwrite the
    /// sections finalize() appended. Pre-fix this exact interleave ate the
    /// outcome line and the head of the child stdout under load.
    #[cfg(unix)]
    #[test]
    fn delegation_log_fds_are_append_only_so_late_writes_cannot_overwrite() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().expect("dir");
        let log = dir.path().join("d.log");
        let (mut out, mut err) = crate::commands::detached::open_log_append(&log).expect("open");
        writeln!(out, "delegation header").expect("header");
        // finalize() appends via its own fd.
        let mut fin = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .expect("finalize open");
        fin.write_all(b"\noutcome: completed\n\n--- stdout ---\ncopilot argv: --allow-all-tools --prompt=the contract\nconclusion\n\n--- stderr ---\n")
            .expect("append");
        // The worker's late write via the ORIGINAL (shared-offset) fds.
        writeln!(err, "WARN late write").expect("late");
        drop((out, err, fin));
        let text = std::fs::read_to_string(&log).expect("read");
        let header_at = text.find("delegation header").unwrap_or(usize::MAX);
        let outcome_at = text.find("outcome: completed").unwrap_or(usize::MAX);
        let argv_at = text.find("copilot argv").unwrap_or(usize::MAX);
        let warn_at = text.find("WARN late write").unwrap_or(usize::MAX);
        assert!(
            header_at < outcome_at && outcome_at < argv_at && argv_at < warn_at,
            "ordering destroyed: {text}"
        );
    }
}
