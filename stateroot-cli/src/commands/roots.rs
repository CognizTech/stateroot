//! `stateroot snap|log|show|diff|revert|fork|receipt` — git-plumbing roots
//! (M2). Append-only history; the user's branch log is never touched.

use std::path::Path;

use stateroot_core::roots as engine;

use super::{note, truncate, Ctx};

const LOCAL_HARNESS: &str = "cli";

/// `stateroot snap [--reason R] [--harness H]`
pub fn snap(ctx: &Ctx, reason: Option<&str>, harness: Option<&str>) -> anyhow::Result<()> {
    ctx.require_project()?;
    let resolved_harness = if let Some(raw) = harness {
        super::active_harness::canonical_id(raw)?
    } else if let Some(observed) = super::active_harness::read(&ctx.cwd)? {
        observed
    } else {
        LOCAL_HARNESS.to_string()
    };
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))?;
    let snap_ctx = stateroot_core::snap_context::SnapContext {
        home,
        harness: Some(resolved_harness.clone()),
    };
    let (manifest, transition) = engine::create_root(
        &ctx.cwd,
        &resolved_harness,
        reason.unwrap_or(""),
        Some(&snap_ctx),
    )?;
    println!("root {}", manifest.id);
    // Same contract as checkpoint: the next harness sees who worked last.
    stateroot_core::local_store::stamp_handoff_activity(&ctx.cwd, &resolved_harness, "root");
    println!(
        "coverage: {}",
        if manifest.coverage == "state_only" {
            "state-only (files not synced)".to_string()
        } else {
            format!("files: {} pinned", manifest.files_pinned)
        }
    );
    println!(
        "transition {} ({} -> {})",
        short(&transition.id),
        short(&transition.from_root),
        short(&transition.to_root)
    );
    if manifest.coverage == "state_only" {
        note!("hint: state-only coverage — the project tree is empty or fully ignored");
    }
    if manifest.tree_bytes > stateroot_core::roots::TREE_SIZE_WARN_BYTES {
        note!(
            "warning: root tree is {} MB — syncs carry this much history; consider .staterootignore for large assets",
            manifest.tree_bytes / (1024 * 1024)
        );
    }
    crate::telemetry::activity(&ctx.config_dir, &ctx.cwd, Some(resolved_harness.as_str()));
    Ok(())
}

/// `stateroot log` — root lineage with coverage lines and fork markers,
/// then the local checkpoint/handoff tails.
pub fn log(ctx: &Ctx, json_output: bool) -> anyhow::Result<()> {
    ctx.require_project()?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&engine::lineage_projection(&ctx.cwd)?)?
        );
        return Ok(());
    }
    let entries = engine::lineage(&ctx.cwd)?;
    if entries.is_empty() {
        println!("no roots yet — run `stateroot snap` to create one");
    } else {
        println!("## Roots ({})", entries.len());
        for entry in &entries {
            let m = &entry.manifest;
            let mut line = format!("  {}", short(&m.id));
            if !entry.mainline {
                line.push_str("  (fork)");
            } else if entry.fork_point {
                line.push_str("  <fork point>");
            }
            if !m.created_at.is_empty() {
                line.push_str(&format!("  {}", m.created_at));
            }
            let coverage = if m.coverage == "state_only" {
                "state-only".to_string()
            } else if m.coverage == "unknown" {
                "coverage unknown".to_string()
            } else {
                format!("files: {}", m.files_pinned)
            };
            line.push_str(&format!("  [{coverage}]"));
            if !m.created_reason.is_empty() {
                line.push_str(&format!("  {}", truncate(&m.created_reason, 60)));
            }
            println!("{line}");
        }
    }

    // Local tails (unchanged from M1).
    let root = stateroot_core::local_store::root(&ctx.cwd);
    let episodic = std::fs::read_to_string(root.join(stateroot_core::local_store::EPISODIC_PATH))
        .unwrap_or_default();
    let records: Vec<&str> = episodic.lines().filter(|l| !l.trim().is_empty()).collect();
    if !records.is_empty() {
        println!();
        println!("## Checkpoints ({})", records.len());
        for line in records.iter().rev().take(10) {
            let parsed: serde_json::Value =
                serde_json::from_str(line).unwrap_or(serde_json::Value::Null);
            let ts = parsed.get("ts").and_then(|v| v.as_str()).unwrap_or("?");
            let note_text = parsed.get("note").and_then(|v| v.as_str()).unwrap_or("");
            println!("  {ts} {}", truncate(note_text, 90));
        }
    }
    let history: Vec<String> =
        std::fs::read_dir(root.join(stateroot_core::local_store::HANDOFF_HISTORY_DIR))
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| e.file_name().to_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
    if !history.is_empty() {
        println!();
        println!("## Handoffs ({})", history.len());
    }
    Ok(())
}

/// `stateroot show <hash>`
pub fn show(ctx: &Ctx, hash: &str) -> anyhow::Result<()> {
    ctx.require_project()?;
    let m = engine::get_root(&ctx.cwd, hash).map_err(|e| anyhow::anyhow!(e))?;
    println!("root {}", m.id);
    if !m.parents.is_empty() {
        let parents: Vec<String> = m.parents.iter().map(|p| short(p)).collect();
        println!("parents: {}", parents.join(", "));
    }
    println!(
        "coverage: {}",
        if m.coverage == "state_only" {
            "state-only (files not synced)".to_string()
        } else {
            format!("files: {} pinned", m.files_pinned)
        }
    );
    println!("created_at: {}", m.created_at);
    println!("created_by: {}", m.created_by_harness);
    if !m.created_reason.is_empty() {
        println!("reason: {}", m.created_reason);
    }
    Ok(())
}

/// `stateroot diff <a> <b> [--content]`
pub fn diff(ctx: &Ctx, from: &str, to: &str, content: bool) -> anyhow::Result<()> {
    ctx.require_project()?;
    let body =
        engine::diff_roots(&ctx.cwd, from, to, content, 20, 200).map_err(|e| anyhow::anyhow!(e))?;
    println!(
        "diff {} → {}",
        short(body["from_root"].as_str().unwrap_or("")),
        short(body["to_root"].as_str().unwrap_or(""))
    );
    for (section, title) in [("files", "Files"), ("state", "State (.stateroot/)")] {
        let items = body[section].as_array().cloned().unwrap_or_default();
        if items.is_empty() {
            continue;
        }
        println!("\n## {title}");
        for item in &items {
            println!(
                "  {} {}",
                item.get("status").and_then(|v| v.as_str()).unwrap_or("?"),
                item.get("path").and_then(|v| v.as_str()).unwrap_or("?")
            );
        }
    }
    let contents = body["contents"].as_array().cloned().unwrap_or_default();
    if content {
        for entry in &contents {
            let path = entry.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            println!("\n### {path}");
            if entry.get("binary").and_then(|v| v.as_bool()) == Some(true) {
                println!("(binary file differs)");
            } else if entry.get("content_available").and_then(|v| v.as_bool()) == Some(false) {
                let reason = entry
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                println!("(content unavailable: {reason})");
            } else {
                let diff_text = entry.get("diff").and_then(|v| v.as_str()).unwrap_or("");
                println!("{diff_text}");
                if entry.get("truncated").and_then(|v| v.as_bool()) == Some(true) {
                    println!("(… file diff truncated)");
                }
            }
        }
        if body.get("truncated").and_then(|v| v.as_bool()) == Some(true) {
            note!("diff truncated — caps are 20 files / 200 lines per file");
        }
    }
    Ok(())
}

/// `stateroot revert <hash> [--yes]` — append-only revert to a root's tree.
pub fn revert(ctx: &Ctx, hash: &str, yes: bool) -> anyhow::Result<()> {
    ctx.require_project()?;
    if !yes {
        let manifest = engine::get_root(&ctx.cwd, hash).map_err(|e| anyhow::anyhow!(e))?;
        println!("stateroot revert — plan");
        println!(
            "  action  : NEW root whose tree equals {}",
            short(&manifest.id)
        );
        println!("  coverage: files: {} pinned", manifest.files_pinned);
        println!("  effect  : append-only — existing roots are never rewritten");
        if !super::stdin_is_tty() {
            anyhow::bail!(
                "refusing to revert without confirmation (non-interactive) — re-run with --yes"
            );
        }
        let proceed = dialoguer::Confirm::new()
            .with_prompt("Proceed with revert?")
            .default(false)
            .interact()?;
        if !proceed {
            println!("aborted — nothing changed");
            return Ok(());
        }
    }
    let (manifest, transition) =
        engine::revert_to_root(&ctx.cwd, hash, LOCAL_HARNESS).map_err(|e| anyhow::anyhow!(e))?;
    println!(
        "reverted to {} — new root {}",
        short(transition.evidence["revert_to"].as_str().unwrap_or("")),
        short(&manifest.id)
    );
    println!("transition {}", short(&transition.id));
    Ok(())
}

/// `stateroot fork <hash> [--branch NAME] [--worktree PATH] [--plan ID]`
/// — branch ref from the root; with --worktree, an isolated checkout whose
/// snaps chain on the fork ref (WS5 parallel execution).
pub fn fork(
    ctx: &Ctx,
    hash: &str,
    branch: Option<&str>,
    worktree: Option<&str>,
    plan: Option<&str>,
) -> anyhow::Result<()> {
    ctx.require_project()?;
    let (name, refname) =
        engine::fork_root(&ctx.cwd, hash, branch, LOCAL_HARNESS).map_err(|e| anyhow::anyhow!(e))?;
    println!("fork {name} → {refname}");
    if let Some(path) = worktree {
        engine::fork_materialize(&ctx.cwd, &name, Path::new(path), plan)
            .map_err(|e| anyhow::anyhow!(e))?;
        println!("worktree: {path} (fork context stamped; snaps there chain on {refname})");
        println!("HEAD: detached at the fork root (user branches untouched)");
        if let Some(plan) = plan {
            println!("plan claimed: {plan}");
        }
    } else {
        println!("materialize with: stateroot fork {hash} --worktree <path>");
    }
    Ok(())
}

/// `stateroot merge <fork>…` — fold fork lineages into the trunk (3-way
/// merge → one N-parent root; conflicts report paths, never half-apply).
#[allow(clippy::too_many_arguments)]
pub fn merge(
    ctx: &Ctx,
    forks: &[String],
    json_output: bool,
    resolve_ours: &[String],
    cleanup: bool,
    prepare: bool,
    continue_attempt: Option<&str>,
    status: Option<&str>,
    abort: Option<&str>,
    evidence: &[String],
) -> anyhow::Result<()> {
    ctx.require_project()?;
    let print_attempt = |attempt: &engine::MergeAttempt| -> anyhow::Result<()> {
        if json_output {
            println!("{}", serde_json::to_string_pretty(attempt)?);
        } else {
            println!("merge attempt {}: {}", attempt.id, attempt.state);
            println!("trunk: {}", attempt.trunk_tip);
            for fork in &attempt.forks {
                println!("fork {}: {}", fork.name, fork.tip);
            }
            if attempt.conflicts.is_empty() {
                println!(
                    "clean — publish with: stateroot merge --continue {}",
                    attempt.id
                );
            } else {
                println!("agent reconciliation required:");
                for conflict in &attempt.conflicts {
                    let kind = conflict.kind.as_deref().unwrap_or("other");
                    println!("  {} ({}, {})", conflict.path, conflict.fork, kind);
                }
                if let Some(worktree) = &attempt.worktree {
                    println!("reconciliation worktree: {worktree}");
                    println!(
                        "edit and test there, then: stateroot merge --continue {} --evidence \"<tests run>\"",
                        attempt.id
                    );
                }
            }
        }
        Ok(())
    };
    if let Some(id) = status {
        return print_attempt(&engine::merge_attempt(&ctx.cwd, id)?);
    }
    if let Some(id) = abort {
        let attempt = engine::abort_merge_attempt(&ctx.cwd, id)?;
        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema_version": "stateroot.merge-attempt.abort.v1",
                    "id": attempt.id,
                    "aborted": true,
                }))?
            );
        } else {
            println!("aborted merge attempt {}", attempt.id);
        }
        return Ok(());
    }
    if prepare {
        return print_attempt(&engine::prepare_merge_attempt(
            &ctx.cwd,
            forks,
            LOCAL_HARNESS,
        )?);
    }
    if let Some(id) = continue_attempt {
        let (manifest, transition, merged) = engine::continue_merge_attempt(&ctx.cwd, id, evidence)
            .map_err(|e| anyhow::anyhow!(e))?;
        if json_output {
            let payload = serde_json::json!({
                "schema_version": "stateroot.merge.continue.v1",
                "attempt": id,
                "trunk_root": manifest.id,
                "merged_forks": merged.iter().map(|fork| serde_json::json!({"name": fork.name, "tip": fork.tip})).collect::<Vec<_>>(),
                "transition": transition.id,
                "phases": transition.evidence["phases"].clone(),
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        } else {
            println!(
                "merged {} → root {}",
                merged
                    .iter()
                    .map(|fork| fork.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                short(&manifest.id)
            );
        }
        return Ok(());
    }
    if cleanup {
        let results =
            engine::cleanup_merged_forks(&ctx.cwd, forks).map_err(|e| anyhow::anyhow!(e))?;
        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema_version": "stateroot.merge.cleanup.v1",
                    "forks": results,
                }))?
            );
            return Ok(());
        }
        for result in results {
            if result.cleaned {
                println!("cleaned {}", result.name);
            } else {
                println!(
                    "cleanup pending {}: {}",
                    result.name,
                    result.pending.as_deref().unwrap_or("retry later")
                );
            }
        }
        return Ok(());
    }
    let (manifest, transition, merged) =
        engine::merge_forks_resolving(&ctx.cwd, forks, LOCAL_HARNESS, resolve_ours)
            .map_err(|e| anyhow::anyhow!(e))?;
    if json_output {
        let projection = engine::lineage_projection(&ctx.cwd)?;
        let cleanup = projection["forks"]
            .as_array()
            .map(|all| {
                all.iter()
                    .filter(|fork| merged.iter().any(|m| fork["name"] == m.name))
                    .map(|fork| fork["cleanup"].clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": "stateroot.merge.v1",
                "trunk_root": manifest.id,
                "merged_forks": merged.iter().map(|fork| serde_json::json!({"name": fork.name, "tip": fork.tip})).collect::<Vec<_>>(),
                "skipped_contained": transition.evidence["skipped_contained"].clone(),
                "materialized": true,
                "cleanup": cleanup,
                "phases": transition.evidence["phases"].clone(),
            }))?
        );
        return Ok(());
    }
    println!(
        "merged {} → root {} ({} parents)",
        merged
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        short(&manifest.id),
        manifest.parents.len()
    );
    println!("transition {}", short(&transition.id));
    println!("the fork refs stay as history");
    println!(
        "worktree cleanup is deferred — run: stateroot merge --cleanup {}",
        merged
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    );
    Ok(())
}

/// `stateroot receipt <transition>` — markdown from transition + git delta.
pub fn receipt(ctx: &Ctx, id_prefix: &str) -> anyhow::Result<()> {
    ctx.require_project()?;
    let md = engine::render_receipt(&ctx.cwd, id_prefix).map_err(|e| anyhow::anyhow!(e))?;
    print!("{md}");
    Ok(())
}

/// `stateroot compare <a> <b>` — experiment semantics across two roots.
pub fn compare(ctx: &Ctx, a: &str, b: &str) -> anyhow::Result<()> {
    ctx.require_project()?;
    let md = engine::compare_roots(&ctx.cwd, a, b).map_err(|e| anyhow::anyhow!(e))?;
    print!("{md}");
    Ok(())
}

fn short(hash: &str) -> String {
    if hash.is_empty() {
        return "∅".into();
    }
    hash.chars().take(12).collect()
}
