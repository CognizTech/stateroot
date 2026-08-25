//! `stateroot projects` — the window into the global project registry.
//!
//! The registry (`projects.toml`, written at every `init`) knows every
//! initialized project on this machine. This command is how a personal
//! agent with a fixed workspace (openclaw) — or any harness working across
//! repos — discovers what exists, then moves into the requested project and
//! resumes there. Missing directories are marked, never silently dropped.

use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use super::Ctx;

/// The registry as JSON rows (shared by the CLI table and the MCP tool).
pub fn collect(ctx: &Ctx) -> Result<Value> {
    let registry =
        stateroot_core::config::load_registry(&ctx.config_dir).map_err(|e| anyhow::anyhow!(e))?;
    let mut rows: Vec<Value> = Vec::new();
    for (path, entry) in &registry.projects {
        let dir = Path::new(path);
        let exists = dir.is_dir();
        let hints = if exists {
            read_hints(dir)
        } else {
            Hints::default()
        };
        rows.push(json!({
            "name": if entry.name.is_empty() { fallback_name(path) } else { entry.name.clone() },
            "path": path,
            "project_id": entry.project_id,
            "registered_at": entry.created_at,
            "on_disk": exists,
            "phase": hints.phase,
            "objective": hints.objective,
            "handoff": hints.handoff,
            "active_plan": hints.active_plan,
            "last_root": hints.last_root,
        }));
    }
    Ok(json!({ "projects": rows }))
}

/// Run `stateroot projects [--json] [--prune]`.
pub fn run(ctx: &Ctx, json_out: bool, prune: bool) -> Result<()> {
    if prune {
        let registry = stateroot_core::config::load_registry(&ctx.config_dir)
            .map_err(|e| anyhow::anyhow!(e))?;
        let missing: Vec<String> = registry
            .projects
            .keys()
            .filter(|path| !Path::new(path).is_dir())
            .cloned()
            .collect();
        if missing.is_empty() {
            println!("nothing to prune — every registered project is on disk");
            return Ok(());
        }
        for path in &missing {
            stateroot_core::config::unregister_project(&ctx.config_dir, Path::new(path))
                .map_err(|e| anyhow::anyhow!(e))?;
            println!("  pruned {path} (directory gone)");
        }
        println!(
            "pruned {} entr{}",
            missing.len(),
            if missing.len() == 1 { "y" } else { "ies" }
        );
        return Ok(());
    }
    let rows = collect(ctx)?;
    let rows = rows
        .get("projects")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "projects": rows }))?
        );
        return Ok(());
    }
    if rows.is_empty() {
        println!("no projects registered — `stateroot init` registers a project");
        return Ok(());
    }
    println!(
        "{:<24} {:<8} {:<10} {:<10} PATH",
        "NAME", "PHASE", "HANDOFF", "ON DISK"
    );
    for row in &rows {
        let name = row["name"].as_str().unwrap_or("");
        let phase = row["phase"].as_str().unwrap_or("");
        let handoff = row["handoff"].as_str().unwrap_or("");
        let on_disk = if row["on_disk"].as_bool().unwrap_or(false) {
            "yes"
        } else {
            "MISSING"
        };
        let path = row["path"].as_str().unwrap_or("");
        println!("{name:<24.24} {phase:<8.8} {handoff:<10.10} {on_disk:<10} {path}");
        let objective = row["objective"].as_str().unwrap_or("");
        let plan = row["active_plan"].as_str().unwrap_or("");
        let last = row["last_root"].as_str().unwrap_or("");
        if !objective.is_empty() {
            println!("  objective: {}", shorten(objective, 100));
        }
        if !plan.is_empty() {
            println!("  active plan: {plan}");
        }
        if !last.is_empty() {
            println!("  last root: {last}");
        }
    }
    println!("\ncd into a project to work it: its digest, plans, memory, and lineage are there.");
    Ok(())
}

#[derive(Default)]
struct Hints {
    phase: String,
    objective: String,
    handoff: String,
    active_plan: String,
    last_root: String,
}

/// Cheap live hints from the project store — no scans, no transcripts.
fn read_hints(dir: &Path) -> Hints {
    let root = dir.join(".stateroot");
    let state = std::fs::read_to_string(root.join("project/state.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let handoff = std::fs::read_to_string(root.join("handoffs/current.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let active_plan =
        stateroot_core::plans::active_or_approved(dir).map(|(meta, _)| meta.title.clone());
    let last_root = stateroot_core::roots::latest_root(dir)
        .ok()
        .flatten()
        .and_then(|hash| stateroot_core::roots::get_root(dir, &hash).ok())
        .map(|m| {
            format!(
                "{} · {}",
                m.id.chars().take(12).collect::<String>(),
                m.created_at
            )
        })
        .unwrap_or_default();
    Hints {
        phase: state
            .as_ref()
            .and_then(|s| s.get("current_phase"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        objective: state
            .as_ref()
            .and_then(|s| s.get("objective"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        handoff: handoff
            .as_ref()
            .and_then(|h| h.get("seq"))
            .and_then(|v| v.as_i64())
            .map(|seq| format!("seq {seq}"))
            .unwrap_or_default(),
        active_plan: active_plan.unwrap_or_default(),
        last_root,
    }
}

fn fallback_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
        .to_string()
}

fn shorten(text: &str, cap: usize) -> String {
    let one_line = text.replace('\n', " ");
    if one_line.chars().count() <= cap {
        return one_line;
    }
    format!("{}…", one_line.chars().take(cap).collect::<String>())
}
