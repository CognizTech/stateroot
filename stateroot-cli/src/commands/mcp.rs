//! `stateroot mcp …` — cross-harness MCP federation.

use anyhow::{anyhow, Result};

use super::{note, Ctx};

/// `stateroot mcp scan`
pub fn scan(ctx: &Ctx, json: bool) -> Result<()> {
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow!(e))?;
    let project =
        stateroot_core::local_store::is_stateroot_dir(&ctx.cwd).then_some(ctx.cwd.as_path());
    let found = stateroot_core::mcp_federation::discover_all(Some(&home), project)
        .map_err(|e| anyhow!(e))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&found)?);
        return Ok(());
    }
    if found.is_empty() {
        println!("no MCP servers discovered");
        return Ok(());
    }
    println!("Discovered {} MCP server(s):", found.len());
    for server in &found {
        let cloud = if stateroot_core::mcp_federation::is_cloud_eligible(server.transport_hint) {
            "cloud"
        } else {
            "local-only"
        };
        println!(
            "  {} @ {} ({}) transport={} ({}) digest={}",
            server.name,
            server.origin_harness,
            server.scope,
            server.transport_hint.as_str(),
            cloud,
            &server.entry_digest[..8.min(server.entry_digest.len())]
        );
    }
    Ok(())
}

/// `stateroot mcp sync` — pull into canonical store and project across harnesses.
pub fn sync(ctx: &Ctx, dry_run: bool, pull: bool, push: bool) -> Result<()> {
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow!(e))?;
    let project =
        stateroot_core::local_store::is_stateroot_dir(&ctx.cwd).then_some(ctx.cwd.as_path());
    let options = stateroot_core::mcp_federation::SyncOptions {
        dry_run,
        pull: pull || !push,
        push: push || !pull,
        cmd_probe: test_cmd_probes(),
    };
    let actions = stateroot_core::mcp_federation::sync(Some(&home), project, &options)
        .map_err(|e| anyhow!(e))?;
    for action in &actions {
        println!("  [{}] {} — {}", action.action, action.name, action.detail);
    }
    println!("{} action(s).", actions.len());
    Ok(())
}

/// Hidden test seam (mirrors `STATEROOT_TEST_HOME`): when
/// `STATEROOT_TEST_CMD_PROBES` is set, harness binary detection answers from
/// this comma-separated allowlist instead of probing the host PATH.
fn test_cmd_probes() -> Option<Vec<String>> {
    std::env::var("STATEROOT_TEST_CMD_PROBES").ok().map(|raw| {
        raw.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// `stateroot mcp status`
pub fn status(ctx: &Ctx, json: bool) -> Result<()> {
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow!(e))?;
    let project =
        stateroot_core::local_store::is_stateroot_dir(&ctx.cwd).then_some(ctx.cwd.as_path());
    let report = stateroot_core::mcp_federation::status_report(Some(&home), project)
        .map_err(|e| anyhow!(e))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "discovered={} global_canonical={} project_canonical={} cloud_eligible={}",
        report
            .get("discovered")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        report
            .get("global_canonical")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        report
            .get("project_canonical")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        report
            .get("cloud_eligible")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    );
    if let Some(servers) = report.get("servers") {
        for scope in ["global", "project"] {
            if let Some(rows) = servers.get(scope).and_then(|v| v.as_array()) {
                for row in rows {
                    let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let hint = row
                        .get("transport_hint")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let cloud = row
                        .get("cloud_eligible")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    println!("  {scope}: {name} transport={hint} cloud_eligible={cloud}");
                }
            }
        }
    }
    Ok(())
}

/// `stateroot mcp remove <name>` — drop from canonical store(s) + ledger.
pub fn remove(ctx: &Ctx, name: &str) -> Result<()> {
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow!(e))?;
    let project =
        stateroot_core::local_store::is_stateroot_dir(&ctx.cwd).then_some(ctx.cwd.as_path());
    let actions = stateroot_core::mcp_federation::remove_server(Some(&home), project, name)
        .map_err(|e| anyhow!(e))?;
    for action in &actions {
        println!("  [{}] {} — {}", action.action, action.name, action.detail);
    }
    Ok(())
}

/// `stateroot mcp accept-theirs <name>` — adopt the harness-side copy.
pub fn accept_theirs(ctx: &Ctx, name: &str, from: Option<&str>) -> Result<()> {
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow!(e))?;
    let project =
        stateroot_core::local_store::is_stateroot_dir(&ctx.cwd).then_some(ctx.cwd.as_path());
    let actions = stateroot_core::mcp_federation::accept_theirs(Some(&home), project, name, from)
        .map_err(|e| anyhow!(e))?;
    for action in &actions {
        println!("  [{}] {} — {}", action.action, action.name, action.detail);
    }
    Ok(())
}

/// `stateroot mcp doctor`
pub fn doctor(ctx: &Ctx) -> Result<()> {
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow!(e))?;
    let project =
        stateroot_core::local_store::is_stateroot_dir(&ctx.cwd).then_some(ctx.cwd.as_path());
    let report = stateroot_core::mcp_federation::doctor_report(Some(&home), project)
        .map_err(|e| anyhow!(e))?;
    let issues = report
        .get("issues")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let warnings = report
        .get("warnings")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    println!(
        "mcp targets={} discovered={} cloud_eligible={}",
        report
            .get("mcp_targets")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        report
            .get("discovered")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        report
            .get("cloud_eligible")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    );
    for warning in &warnings {
        if let Some(text) = warning.as_str() {
            note!("warning: {text}");
        }
    }
    if issues.is_empty() {
        println!("no collisions or conflicts");
    } else {
        note!("{} issue(s):", issues.len());
        for issue in &issues {
            println!(
                "  [{}] {} — {}",
                issue.get("action").and_then(|v| v.as_str()).unwrap_or("?"),
                issue.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
                issue.get("detail").and_then(|v| v.as_str()).unwrap_or("")
            );
        }
    }
    Ok(())
}
