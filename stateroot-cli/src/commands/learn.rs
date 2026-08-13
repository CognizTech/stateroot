//! `stateroot learn record` — write a learning (taste / convention / judgment).
//!
//! Always a learning. Facts go to `memory_save`. Identity stays on soul.
//! Procedures stay on `skill_propose`. Scope comes only from flags.

use anyhow::Result;
use stateroot_core::learnings as core_learnings;

use super::Ctx;

/// Resolve CLI scope flags to a learnings scope key.
pub fn resolve_scope(user: bool, workspace: bool, domain: Option<&str>) -> Result<String> {
    let n = usize::from(user) + usize::from(workspace) + usize::from(domain.is_some());
    if n > 1 {
        anyhow::bail!("use only one of --user / --workspace / --domain");
    }
    if user {
        return Ok("user".into());
    }
    if workspace {
        return Ok("workspace".into());
    }
    if let Some(slug) = domain {
        let slug = slug.trim();
        if slug.is_empty() {
            anyhow::bail!("--domain requires a non-empty slug");
        }
        return Ok(format!("domain:{slug}"));
    }
    Ok("project".into())
}

/// `stateroot learn record "<note>" [--user|--workspace|--domain <slug>]`
pub fn record(
    ctx: &Ctx,
    note: &str,
    user: bool,
    workspace: bool,
    domain: Option<&str>,
) -> Result<()> {
    ctx.require_project()?;
    let note = note.trim();
    if note.is_empty() {
        anyhow::bail!("empty note — nothing to record");
    }
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))?;
    let scope = resolve_scope(user, workspace, domain)?;
    let (id, new, category) =
        core_learnings::record_note(&ctx.cwd, &home, note, &scope, "learn record")
            .map_err(|e| anyhow::anyhow!(e))?;
    let verb = if new { "recorded" } else { "already had" };
    println!("{verb} learning {id} [active; {scope}; {category}]");
    Ok(())
}
