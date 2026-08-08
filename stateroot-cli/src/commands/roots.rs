//! `stateroot snap|log|show|diff|revert|fork|receipt` — git-plumbing roots
//! (M2). Append-only history; the user's branch log is never touched.

use stateroot_core::roots as engine;

use super::{note, truncate, Ctx};

const LOCAL_HARNESS: &str = "cli";

/// `stateroot snap [--reason R]`
pub fn snap(ctx: &Ctx, reason: Option<&str>) -> anyhow::Result<()> {
    ctx.require_project()?;
    let (manifest, transition) =
        engine::create_root(&ctx.cwd, LOCAL_HARNESS, reason.unwrap_or(""))?;
    println!("root {}", manifest.id);
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
    Ok(())
}

/// `stateroot log` — root lineage with coverage lines and fork markers,
/// then the local checkpoint/handoff tails.
pub fn log(ctx: &Ctx) -> anyhow::Result<()> {
    ctx.require_project()?;
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

/// `stateroot fork <hash> [--branch NAME]` — branch ref from the root.
pub fn fork(ctx: &Ctx, hash: &str, branch: Option<&str>) -> anyhow::Result<()> {
    ctx.require_project()?;
    let (name, refname) =
        engine::fork_root(&ctx.cwd, hash, branch, LOCAL_HARNESS).map_err(|e| anyhow::anyhow!(e))?;
    println!("fork {name} → {refname}");
    println!("materialize with: git worktree add .stateroot/worktrees/{name} {refname}");
    Ok(())
}

/// `stateroot receipt <transition>` — markdown from transition + git delta.
pub fn receipt(ctx: &Ctx, id_prefix: &str) -> anyhow::Result<()> {
    ctx.require_project()?;
    let md = engine::render_receipt(&ctx.cwd, id_prefix).map_err(|e| anyhow::anyhow!(e))?;
    print!("{md}");
    Ok(())
}

fn short(hash: &str) -> String {
    if hash.is_empty() {
        return "∅".into();
    }
    hash.chars().take(12).collect()
}
