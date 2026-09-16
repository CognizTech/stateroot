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
use std::process::Stdio;

use anyhow::Result;
use serde_json::{json, Value};
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
        None => spawn(ctx, args),
    }
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
    // replayed spawn re-attaches (live) or resubmits (lost) instead of
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
    if let Some((_path, existing)) = load_record(&ctx.cwd, &record_id) {
        if let Some(outcome) = existing.get("outcome").and_then(Value::as_str) {
            anyhow::bail!(
                "delegation key `{record_id}` already finished ({outcome}) — pick a new key"
            );
        }
        let prior = existing.get("fingerprint").cloned().unwrap_or(json!(null));
        if prior != json!(null) && prior != fingerprint {
            anyhow::bail!(
                "delegation key `{record_id}` exists with a different request — pick a new key"
            );
        }
        if existing.get("status").and_then(Value::as_str) == Some("starting") {
            // A reservation whose spawn never landed: resubmit under the
            // same key (recover-before-cancel pattern).
        } else {
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
        "fingerprint": fingerprint,
        "log": log_rel,
    });
    if let Some(w) = &args.worktree {
        record["worktree"] = json!(w);
    }
    append_event(&mut record, "reserve", "starting reserved under key lock");
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
    if args.json {
        println!("{}", serde_json::to_string_pretty(&record)?);
    } else {
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
            capture_outcome_root(&ctx.cwd, &record_root, record_id, &id);
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
fn capture_outcome_root(work_dir: &Path, record_root: &Path, record_id: &str, harness: &str) {
    let Some((path, mut record)) = load_record(record_root, record_id) else {
        return;
    };
    match stateroot_core::roots::snap_if_changed(
        work_dir,
        harness,
        "auto: delegation outcome",
        None,
    ) {
        Ok(stateroot_core::roots::SnapOutcome::Created(manifest, _)) => {
            record["outcome_root"] = json!(manifest.id);
            append_event(&mut record, "capture", "worktree captured to fork lineage");
        }
        Ok(stateroot_core::roots::SnapOutcome::Unchanged { root }) => {
            record["outcome_root"] = json!(root);
            append_event(
                &mut record,
                "capture",
                "no changes; tip already describes the work",
            );
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
    let output = harness_cli::run_capture(&ctx.cwd, &id, &spec, &prompt, &policy, None)?;
    Ok((id, output))
}

/// Rewrite a record file with final fields (status → outcome).
fn finalize(
    record_root: &Path,
    record_id: &str,
    outcome: &str,
    exit_code: Option<i32>,
    duration_ms: u128,
    log_append: &str,
) -> Result<()> {
    let Some((path, mut record)) = load_record(record_root, record_id) else {
        anyhow::bail!("worker record `{record_id}` is gone — cannot finalize");
    };
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

/// Every delegation record file, newest first.
pub(crate) fn read_records(project_dir: &Path) -> Vec<(PathBuf, Value)> {
    let dir = delegations_dir(project_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, Value)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let path = e.path();
            let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
            Some((path, value))
        })
        .collect();
    out.sort_by(|a, b| {
        let ts_of = |r: &Value| {
            r.get("ts")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        ts_of(&b.1).cmp(&ts_of(&a.1))
    });
    out
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
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            out.status.success() && !text.contains("No tasks are running")
        })
        .unwrap_or(false)
}

/// The live status of one record: final outcome when written; `running` when
/// the pid is alive; `lost` when the worker died before writing (reaped —
/// the record is updated so the loss is recorded, never silent).
pub(crate) fn live_status(path: &Path, record: &Value) -> String {
    if let Some(outcome) = record.get("outcome").and_then(Value::as_str) {
        return outcome.to_string();
    }
    if record.get("status").and_then(Value::as_str) == Some("running") {
        let pid = record.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32;
        if pid != 0 && pid_alive(pid) {
            return "running".to_string();
        }
        // Reap: worker died before writing an outcome.
        let mut reaped = record.clone();
        let obj = reaped.as_object_mut().unwrap();
        obj.remove("status");
        obj.insert("outcome".into(), json!("lost"));
        obj.insert("ended_at".into(), json!(now_rfc3339()));
        let _ = stateroot_core::safe_io::atomic_replace_json(path, &reaped);
        return "lost".to_string();
    }
    "unknown".to_string()
}

/// Run `stateroot delegate list`.
fn list(ctx: &Ctx) -> Result<()> {
    ctx.require_project()?;
    let records = read_records(&ctx.cwd);
    if records.is_empty() {
        println!("no delegations recorded");
        return Ok(());
    }
    for (path, record) in &records {
        let status = live_status(path, record);
        let id = record.get("id").and_then(Value::as_str).unwrap_or("");
        let harness = record.get("harness").and_then(Value::as_str).unwrap_or("");
        let task = record.get("task").and_then(Value::as_str).unwrap_or("");
        println!("{} · {} · {} · {}", id, harness, status, truncate(task, 60));
    }
    Ok(())
}

/// Run `stateroot delegate status <id>` — the record plus a bounded log tail.
fn status(ctx: &Ctx, id: &str) -> Result<()> {
    ctx.require_project()?;
    let Some((path, record)) = load_record(&ctx.cwd, id) else {
        anyhow::bail!("no delegation matches `{id}` — run `stateroot delegate list`");
    };
    let live = live_status(&path, &record);
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
    capture_outcome_root(&work_dir, &ctx.cwd, &record_id, &harness);
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

/// The last N delegation records for the digest section.
pub(crate) fn recent_delegations(
    project_dir: &Path,
    count: usize,
) -> Vec<(String, String, String)> {
    read_records(project_dir)
        .iter()
        .take(count)
        .map(|(path, record)| {
            let ts = record.get("ts").and_then(Value::as_str).unwrap_or("");
            let short_ts: String = ts.chars().take(16).collect();
            let harness = record.get("harness").and_then(Value::as_str).unwrap_or("");
            let task = record.get("task").and_then(Value::as_str).unwrap_or("");
            let status = live_status(path, record);
            (
                short_ts,
                format!("{harness} · {status}"),
                truncate(task, 120),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use stateroot_core::skill_federation::build_launch_argv_from_spec;

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

    #[test]
    fn running_record_reaps_to_lost_on_a_dead_pid() {
        let dir = tempfile::tempdir().expect("dir");
        let project = dir.path().join("proj");
        std::fs::create_dir_all(project.join(".stateroot/delegations")).expect("mkdir");
        let record = json!({
            "schema_version": "stateroot.delegation.v1",
            "id": "2026-test-claude",
            "ts": "2026-08-26T10:00:00Z",
            "depth": 0,
            "harness": "claude",
            "task": "never finishes",
            "command": "claude",
            "status": "running",
            // A pid that cannot be alive (well past any real pid).
            "pid": 4_000_000u32,
            "log": ".stateroot/delegations/2026-test-claude-d0.log",
        });
        let dir_path = project.join(".stateroot/delegations");
        write_record(&dir_path, &record).expect("write");
        let (path, loaded) = load_record(&project, "2026-test-claude").expect("record");
        assert_eq!(live_status(&path, &loaded), "lost");
        // Reaped on disk: outcome recorded, never a silent running-forever.
        let (_, reaped) = load_record(&project, "2026-test-claude").expect("record");
        assert_eq!(reaped["outcome"], "lost");
        assert!(reaped.get("status").is_none());
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
