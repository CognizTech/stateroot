//! `stateroot proposals …` — the shared approval gate (M3).

use anyhow::Result;
use stateroot_core::proposals as core;

use super::{truncate, Ctx};

/// `stateroot proposals list [--status pending|approved|rejected]`
pub fn list(ctx: &Ctx, status: Option<&str>) -> Result<()> {
    ctx.require_project()?;
    let proposals = core::list(&ctx.cwd, status).map_err(|e| anyhow::anyhow!(e))?;
    if proposals.is_empty() {
        println!(
            "no proposals{}",
            status.map(|s| format!(" ({s})")).unwrap_or_default()
        );
        return Ok(());
    }
    for p in &proposals {
        println!(
            "  {} [{}; {}] {} — {}",
            &p.id[..8],
            p.kind,
            p.status,
            truncate(&p.title, 60),
            truncate(&p.rationale, 60)
        );
    }
    Ok(())
}

/// `stateroot proposals show <id>`
pub fn show(ctx: &Ctx, id_prefix: &str) -> Result<()> {
    ctx.require_project()?;
    let p = core::get(&ctx.cwd, id_prefix).map_err(|e| anyhow::anyhow!(e))?;
    println!("proposal {}", p.id);
    println!("kind: {}", p.kind);
    println!("status: {}", p.status);
    println!("title: {}", p.title);
    println!("rationale: {}", p.rationale);
    println!("created_at: {}", p.created_at);
    if !p.decided_at.is_empty() {
        println!("decided: {} by {}", p.decided_at, p.decided_by);
    }
    println!("payload:");
    println!("{}", serde_json::to_string_pretty(&p.payload)?);
    Ok(())
}

/// `stateroot proposals approve <id> [--edit <json>]`
pub fn approve(ctx: &Ctx, id_prefix: &str, edit: Option<&str>) -> Result<()> {
    ctx.require_project()?;
    let edit_payload = match edit {
        Some(raw) => Some(serde_json::from_str(raw)?),
        None => None,
    };
    let decided = core::decide(&ctx.cwd, id_prefix, true, "cli", edit_payload)
        .map_err(|e| anyhow::anyhow!(e))?;
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))?;
    let note = core::activate(&ctx.cwd, &home, &decided);
    println!("proposal {} approved", &decided.id[..8]);
    println!("{note}");
    if decided.kind == "soul" {
        super::soul::refresh_persona_cache_pub(ctx);
    }
    Ok(())
}

/// `stateroot proposals reject <id>`
pub fn reject(ctx: &Ctx, id_prefix: &str) -> Result<()> {
    ctx.require_project()?;
    let decided =
        core::decide(&ctx.cwd, id_prefix, false, "cli", None).map_err(|e| anyhow::anyhow!(e))?;
    println!("proposal {} rejected (kept for audit)", &decided.id[..8]);
    Ok(())
}
