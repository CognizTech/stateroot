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

/// The spawn path: record `running`, launch the detached worker, exit 0.
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
    let record_id = format!("{stamp}-{id}");
    let dir = delegations_dir(&ctx.cwd);
    std::fs::create_dir_all(&dir)?;
    let log_name = format!("{stamp}-{id}-d{depth}.log");
    let log_path = dir.join(&log_name);
    let log_rel = format!(".stateroot/delegations/{log_name}");

    // Detached worker = this binary in hidden worker mode; its stdout/stderr
    // redirect into the delegation log (diagnostics + worker header line).
    let log_file = std::fs::File::create(&log_path)?;
    let log_err = log_file.try_clone()?;
    let mut worker_args = vec![
        "delegate".to_string(),
        "--to".to_string(),
        to.to_string(),
        "--task".to_string(),
        task.to_string(),
        "--_worker".to_string(),
        "--record-id".to_string(),
        record_id.clone(),
    ];
    for skill in &args.skills {
        worker_args.push("--skill".to_string());
        worker_args.push(skill.clone());
    }
    if args.ambient_skills {
        worker_args.push("--ambient-skills".to_string());
    }
    let child = std::process::Command::new(
        std::env::current_exe().map_err(|e| anyhow::anyhow!("resolve own binary: {e}"))?,
    )
    .args(&worker_args)
    .current_dir(&ctx.cwd)
    .env(DEPTH_ENV, (depth + 1).to_string())
    .stdin(Stdio::null())
    .stdout(log_file)
    .stderr(log_err)
    .spawn()?;
    let pid = child.id();
    drop(child); // detached: no wait, no kill, ever.

    // The running record exists before the parent exits, so `list` sees
    // `running` even if the worker dies instantly.
    let record = json!({
        "schema_version": "stateroot.delegation.v1",
        "id": record_id,
        "ts": ts,
        "depth": depth,
        "harness": id,
        "task": task,
        "command": command,
        "status": "running",
        "pid": pid,
        "log": log_rel,
    });
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
                ctx,
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
            episodic_lineage(ctx, &id, task, outcome, started.elapsed().as_secs())?;
            Ok(output.status.code().unwrap_or(1))
        }
        Err(err) => {
            let _ = finalize(
                ctx,
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
    ctx: &Ctx,
    record_id: &str,
    outcome: &str,
    exit_code: Option<i32>,
    duration_ms: u128,
    log_append: &str,
) -> Result<()> {
    let Some((path, mut record)) = load_record(&ctx.cwd, record_id) else {
        anyhow::bail!("worker record `{record_id}` is gone — cannot finalize");
    };
    if let Some(log_rel) = record.get("log").and_then(Value::as_str) {
        let log_path = ctx.cwd.join(log_rel);
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
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&record)?),
    )?;
    Ok(())
}

fn write_record(dir: &Path, record: &Value) -> Result<()> {
    let id = record
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("record without id"))?;
    std::fs::write(
        dir.join(format!("{id}.json")),
        format!("{}\n", serde_json::to_string_pretty(record)?),
    )?;
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
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
        let _ = std::fs::write(
            path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&reaped).unwrap_or_default()
            ),
        );
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
    ctx: &Ctx,
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
    local_store::append_episodic(&ctx.cwd, &record)?;
    Ok(())
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
}
