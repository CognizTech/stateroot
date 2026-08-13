//! `stateroot learnings …` + `stateroot learn record` — local learnings with
//! lifecycle, deterministic distiller, and the review-loop entry point (M3).

use anyhow::Result;
use stateroot_core::learnings as core;

use super::{truncate, Ctx};

fn home() -> Result<std::path::PathBuf> {
    stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))
}

/// `stateroot learnings list [--user] [--status S]`
pub fn list(ctx: &Ctx, user: bool, status: Option<&str>) -> Result<()> {
    ctx.require_project()?;
    let home = home()?;
    let scope = if user { "user" } else { "project" };
    let learnings = core::read_scope(&ctx.cwd, &home, scope);
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

/// `stateroot learnings accept <id> [--user]` — the user's own approval:
/// promote candidate → active directly (agent proposals still go through
/// `proposals approve`).
pub fn accept(ctx: &Ctx, id: &str, user: bool) -> Result<()> {
    ctx.require_project()?;
    let home = home()?;
    let scope = if user { "user" } else { "project" };
    if core::promote(&ctx.cwd, &home, scope, id).map_err(|e| anyhow::anyhow!(e))? {
        println!("learning {id} promoted to active ({scope})");
    } else {
        println!("no candidate learning '{id}' in {scope} scope");
    }
    Ok(())
}

/// `stateroot learnings reject <id> [--user]`
pub fn reject(ctx: &Ctx, id: &str, user: bool) -> Result<()> {
    ctx.require_project()?;
    let home = home()?;
    let scope = if user { "user" } else { "project" };
    if core::reject(&ctx.cwd, &home, scope, id).map_err(|e| anyhow::anyhow!(e))? {
        println!("learning {id} rejected (archived in _rejected.md)");
    } else {
        println!("no candidate learning '{id}' in {scope} scope");
    }
    Ok(())
}

/// `stateroot learnings edit <id> --statement <text> [--user]`
pub fn edit(ctx: &Ctx, id: &str, statement: &str, user: bool) -> Result<()> {
    ctx.require_project()?;
    let home = home()?;
    let scope = if user { "user" } else { "project" };
    if core::edit(&ctx.cwd, &home, scope, id, statement).map_err(|e| anyhow::anyhow!(e))? {
        println!("learning {id} updated");
    } else {
        println!("no learning '{id}' in {scope} scope");
    }
    Ok(())
}

/// `stateroot learnings distill` — mine episodic + spool for recurring
/// correction/preference candidates. Each NEW candidate becomes a proposal
/// (the review loop is tick-free: it runs here and on `learn record`).
pub fn distill(ctx: &Ctx) -> Result<()> {
    ctx.require_project()?;
    let home = home()?;
    let candidates = core::distill(&ctx.cwd, &home);
    if candidates.is_empty() {
        println!("distill: no new candidates");
        return Ok(());
    }
    let mut proposed = 0usize;
    for candidate in &candidates {
        let proposal = stateroot_core::proposals::create(
            &ctx.cwd,
            "learning",
            &format!("distilled: {}", truncate(&candidate.statement, 60)),
            &format!(
                "distiller ({}; category {}; confidence {:.2})",
                candidate.sources, candidate.category, candidate.confidence
            ),
            serde_json::json!({
                "id": candidate.id,
                "statement": candidate.statement,
                "category": candidate.category,
                "confidence": candidate.confidence,
                "label": candidate.label,
                "sources": candidate.sources,
                "scope": candidate.scope,
            }),
            serde_json::json!({"route": "learnings distill"}),
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        // Distilled candidates are quarantined on disk immediately (they
        // surface nowhere until activated); the proposal is the gate.
        let _ = core::append_candidate(&ctx.cwd, &home, &candidate.scope, candidate);
        proposed += 1;
        let _ = proposal;
    }
    let _ = core::maybe_complete_first_run(&ctx.cwd, &home);
    println!("distill: {proposed} candidate(s) → proposals (pending)");
    println!("review with: stateroot proposals list --status pending");
    Ok(())
}
