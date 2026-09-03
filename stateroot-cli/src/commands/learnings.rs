//! `stateroot learnings …` + `stateroot learn record` — local learnings with
//! lifecycle and deterministic distiller (M3).

use anyhow::Result;
use stateroot_core::learnings as core;

use super::learn::resolve_scope;
use super::{truncate, Ctx};

fn home() -> Result<std::path::PathBuf> {
    stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))
}

/// `stateroot learnings list [--user|--workspace|--domain <slug>] [--status S]`
pub fn list(
    ctx: &Ctx,
    user: bool,
    workspace: bool,
    domain: Option<&str>,
    status: Option<&str>,
) -> Result<()> {
    ctx.require_project()?;
    let home = home()?;
    let scope = resolve_scope(user, workspace, domain)?;
    let learnings = core::read_scope(&ctx.cwd, &home, &scope);
    let filtered: Vec<_> = learnings
        .iter()
        .filter(|l| status.map(|s| l.status == s).unwrap_or(true))
        .collect();
    if filtered.is_empty() {
        println!(
            "no learnings ({scope} scope{})",
            status.map(|s| format!(", status {s}")).unwrap_or_default()
        );
        return Ok(());
    }
    println!("Learnings ({scope} scope):");
    for l in filtered {
        println!(
            "  {} [{}; {:.2}; {}] {} — {}",
            &l.id[..12.min(l.id.len())],
            l.status,
            l.confidence,
            l.category,
            truncate(&l.statement, 80),
            l.sources
        );
    }
    Ok(())
}

/// `stateroot learnings accept <id> …`
pub fn accept(
    ctx: &Ctx,
    id: &str,
    user: bool,
    workspace: bool,
    domain: Option<&str>,
) -> Result<()> {
    ctx.require_project()?;
    let home = home()?;
    let scope = resolve_scope(user, workspace, domain)?;
    if core::promote(&ctx.cwd, &home, &scope, id).map_err(|e| anyhow::anyhow!(e))? {
        println!("learning {id} promoted to active ({scope})");
    } else {
        println!("no candidate learning '{id}' in {scope} scope");
    }
    Ok(())
}

/// `stateroot learnings reject <id> …`
pub fn reject(
    ctx: &Ctx,
    id: &str,
    user: bool,
    workspace: bool,
    domain: Option<&str>,
) -> Result<()> {
    ctx.require_project()?;
    let home = home()?;
    let scope = resolve_scope(user, workspace, domain)?;
    if core::reject(&ctx.cwd, &home, &scope, id).map_err(|e| anyhow::anyhow!(e))? {
        println!("learning {id} rejected (archived in _rejected.md)");
    } else {
        println!("no candidate learning '{id}' in {scope} scope");
    }
    Ok(())
}

/// `stateroot learnings edit <id> --statement <text> …`
pub fn edit(
    ctx: &Ctx,
    id: &str,
    statement: &str,
    user: bool,
    workspace: bool,
    domain: Option<&str>,
) -> Result<()> {
    ctx.require_project()?;
    let home = home()?;
    let scope = resolve_scope(user, workspace, domain)?;
    if core::edit(&ctx.cwd, &home, &scope, id, statement).map_err(|e| anyhow::anyhow!(e))? {
        println!("learning {id} updated");
    } else {
        println!("no learning '{id}' in {scope} scope");
    }
    Ok(())
}

/// `stateroot learnings distill` — mine episodic + spool into the wiki inbox
/// (deterministic compile). Does not activate learnings; taste stays on
/// `learn record`.
pub fn distill(ctx: &Ctx) -> Result<()> {
    ctx.require_project()?;
    let home = home()?;
    let added = core::distill_to_inbox(&ctx.cwd, &home).map_err(|e| anyhow::anyhow!(e))?;
    if added > 0 {
        let _ = stateroot_core::wiki::append_log(
            &ctx.cwd,
            &format!("learnings distill: {added} bullet(s) → inbox"),
        );
    }
    let _ = stateroot_core::memory_index::rebuild_if_needed(&ctx.cwd, &home);
    if added == 0 {
        println!("distill: no new notes");
    } else {
        println!("distill: {added} note(s) → wiki/pages/_inbox.md");
    }
    Ok(())
}
