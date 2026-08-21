//! `stateroot delegate` — spawn another harness CLI as a bounded subagent.
//!
//! The caller stays the face; the subagent is labor. The task goes out as a
//! prompt string, the child runs piped under a timeout, and the caller
//! receives a bounded tail of its stdout — never a transcript dump. Every
//! run is persisted (full log plus a `stateroot.delegation.v1` record under
//! `.stateroot/delegations/`) and appended to the episodic log as lineage.

use std::time::Duration;

use anyhow::Result;
use serde_json::json;
use stateroot_core::local_store::{self, now_rfc3339};
use stateroot_core::skill_federation::{binary_probe, load_registry, normalize_harness};

use super::{harness, harness_cli, truncate, Ctx};
use crate::cli::DelegateArgs;

/// Anti-recursion cap (the `delegationDepth` lesson): at this depth a
/// subagent may not spawn further subagents.
const MAX_DELEGATION_DEPTH: u32 = 2;
/// Env var carrying the current delegation depth; children get depth + 1.
const DEPTH_ENV: &str = "STATEROOT_DELEGATION_DEPTH";

/// Prompt prefix: the minimal subagent contract (strings only, per doctrine).
const SUBAGENT_CONTRACT: &str = "You are a subagent delegated via StateRoot. Do the task in this project; project context is available via the stateroot digest. End with a concise final conclusion — the caller receives only your final output.";

/// Parse the depth env value; anything missing/unparseable is depth 0.
fn parse_depth(raw: Option<&str>) -> u32 {
    raw.and_then(|raw| raw.trim().parse().ok()).unwrap_or(0)
}

fn delegation_depth() -> u32 {
    parse_depth(std::env::var(DEPTH_ENV).ok().as_deref())
}

/// Last `max` chars of `text` — the bounded tail returned to callers.
fn tail(text: &str, max: usize) -> String {
    let len = text.chars().count();
    if len <= max {
        text.to_string()
    } else {
        text.chars().skip(len - max).collect()
    }
}

/// Run `stateroot delegate`; the returned code mirrors the child's exit.
pub fn run(ctx: &Ctx, args: &DelegateArgs) -> Result<i32> {
    ctx.require_project()?;

    // Resolve: the named harness must exist, be cli-mode with a command, and
    // its binary must probe on PATH — delegate fails loudly, unlike init.
    let registry = load_registry().map_err(|e| anyhow::anyhow!(e))?;
    let cli_mode: Vec<String> = registry
        .harnesses
        .iter()
        .filter(|e| e.delegation.mode == "cli" && e.delegation.command.is_some())
        .map(|e| e.id.clone())
        .collect();
    let id = normalize_harness(&args.to);
    let Some(entry) = registry.harnesses.iter().find(|e| e.id == id) else {
        anyhow::bail!(
            "unknown harness '{}' — cli-mode harnesses: {}",
            args.to,
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

    // Depth guard: a subagent may not spawn further subagents.
    let depth = delegation_depth();
    if depth >= MAX_DELEGATION_DEPTH {
        anyhow::bail!(
            "delegation depth cap reached ({DEPTH_ENV}={depth}) — a subagent may not spawn further subagents"
        );
    }

    // Minimal subagent contract, then the task verbatim.
    let prompt = format!("{SUBAGENT_CONTRACT}\n\n{}", args.task);

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
    let timeout = Duration::from_secs(args.timeout_secs);
    let output = harness_cli::run_capture(&ctx.cwd, &id, spec, &prompt, &policy, timeout)?;

    let outcome = if output.timed_out {
        "timed_out"
    } else if output.status.success() {
        "completed"
    } else {
        "failed"
    };

    // Persist: full log plus the `stateroot.delegation.v1` record.
    let ts = now_rfc3339();
    let stamp = ts.replace([':', '.'], "-");
    let dir = local_store::root(&ctx.cwd).join("delegations");
    std::fs::create_dir_all(&dir)?;
    let log_name = format!("{stamp}-{id}-d{depth}.log");
    let log_body = format!(
        "delegation to {id} · depth {depth} · {ts}\noutcome: {outcome} · exit_code: {:?} · duration_ms: {}\n\n--- stdout ---\n{}\n\n--- stderr ---\n{}\n",
        output.status.code(),
        output.duration.as_millis(),
        output.stdout,
        output.stderr,
    );
    std::fs::write(dir.join(&log_name), log_body)?;
    let log_rel = format!(".stateroot/delegations/{log_name}");
    let record = json!({
        "schema_version": "stateroot.delegation.v1",
        "id": format!("{stamp}-{id}"),
        "ts": ts,
        "depth": depth,
        "harness": id,
        "task": args.task,
        "command": command,
        "exit_code": output.status.code(),
        "duration_ms": output.duration.as_millis(),
        "log": log_rel,
        "outcome": outcome,
    });
    std::fs::write(
        dir.join(format!("{stamp}-{id}.json")),
        format!("{}\n", serde_json::to_string_pretty(&record)?),
    )?;

    // Lineage: the delegation shows up in digests like any other activity.
    let episodic = json!({
        "ts": now_rfc3339(),
        "harness": "cli",
        "note": format!(
            "delegated to {id}: {} → {outcome} ({}s)",
            truncate(&args.task, 160),
            output.duration.as_secs()
        ),
        "files": [],
    });
    local_store::append_episodic(&ctx.cwd, &episodic)?;

    // Same honesty as init seeding: a nominally successful piped pty harness
    // may still produce nothing. Failures take the stderr-tail path instead.
    if output.stdout.is_empty() && outcome == "completed" {
        anyhow::bail!(
            "harness '{id}' returned empty stdout (exit {:?}) — pty-mode harnesses may misbehave when piped; full log: {log_rel}",
            output.status.code()
        );
    }

    let stdout_tail = tail(&output.stdout, args.max_output_chars);
    let stderr_tail = tail(&output.stderr, args.max_output_chars);
    if args.json {
        let mut envelope = record;
        envelope["stdout_tail"] = json!(stdout_tail);
        envelope["stderr_tail"] = json!(stderr_tail);
        println!("{}", serde_json::to_string_pretty(&envelope)?);
    } else {
        let exit_label = match output.status.code() {
            Some(code) => code.to_string(),
            None => "killed".to_string(),
        };
        println!(
            "delegated to {id} · exit {exit_label} · {}s · full log: {log_rel}",
            output.duration.as_secs()
        );
        if output.timed_out {
            println!("timed out after {}s", timeout.as_secs());
        }
        println!("{stdout_tail}");
        if outcome != "completed" && !stderr_tail.is_empty() {
            println!("--- stderr tail ---\n{stderr_tail}");
        }
    }
    Ok(match outcome {
        "completed" => 0,
        _ => output.status.code().unwrap_or(1),
    })
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
}
