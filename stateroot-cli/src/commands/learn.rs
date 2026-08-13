//! `stateroot learn record` — the review-loop entry point (M3).
//!
//! Learnings and memories activate immediately so the next harness inherits
//! them. Soul and skill still file a proposal (identity / executable
//! capability). Distill remains a separate, gated path for inferred notes.

use anyhow::Result;
use stateroot_core::learnings as core_learnings;
use stateroot_core::learnings::Recorded;
use stateroot_core::proposals as core_proposals;

use super::Ctx;

/// `stateroot learn record "<note>" [--user]`
pub fn record(ctx: &Ctx, note: &str, user: bool) -> Result<()> {
    ctx.require_project()?;
    let note = note.trim();
    if note.is_empty() {
        anyhow::bail!("empty note — nothing to record");
    }
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))?;
    let scope = if user { "user" } else { "project" };
    let (class, recorded) =
        core_learnings::record_note(&ctx.cwd, &home, note, scope, None, "learn record")
            .map_err(|e| anyhow::anyhow!(e))?;
    match recorded {
        Recorded::Learning { id, new } => {
            let verb = if new { "recorded" } else { "already had" };
            println!("{verb} learning {id} [active; {scope}]");
        }
        Recorded::Memory { path } => {
            println!("recorded memory [active; {scope}] → {}", path.display());
        }
        Recorded::NeedsProposal => {
            let (title, payload) = match class.kind.as_str() {
                "soul" => (
                    "soul observation (proposed)".to_string(),
                    serde_json::json!({"content": note, "origin": "learn record", "scope": scope}),
                ),
                "skill" => (
                    format!("procedure candidate: {}", super::truncate(note, 60)),
                    serde_json::json!({"content": note, "origin": "learn record", "scope": scope}),
                ),
                other => (
                    format!("{other}: {}", super::truncate(note, 60)),
                    serde_json::json!({"content": note, "scope": scope}),
                ),
            };
            let proposal = core_proposals::create(
                &ctx.cwd,
                &class.kind,
                &title,
                &format!("classified as {} ({}; {scope})", class.kind, class.category),
                payload,
                serde_json::json!({"route": "learn record", "scope": scope}),
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "recorded → proposal {} [{}; pending; {scope}]",
                &proposal.id[..8],
                class.kind
            );
            println!(
                "review with: stateroot proposals show {}",
                &proposal.id[..8]
            );
        }
    }
    Ok(())
}
