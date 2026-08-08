//! `stateroot learn record` — the review-loop entry point (M3).
//!
//! Every note is classified (soul | memory | skill | learning) and becomes a
//! PROPOSAL — never a direct write. The loop is tick-free: it runs here and
//! on `learnings distill`.

use anyhow::Result;
use stateroot_core::learnings as core_learnings;
use stateroot_core::proposals as core_proposals;

use super::Ctx;

/// `stateroot learn record "<note>"`
pub fn record(ctx: &Ctx, note: &str) -> Result<()> {
    ctx.require_project()?;
    let note = note.trim();
    if note.is_empty() {
        anyhow::bail!("empty note — nothing to record");
    }
    let class = core_learnings::classify_note(note);
    let (title, payload) = match class.kind.as_str() {
        "soul" => (
            "soul observation (proposed)".to_string(),
            serde_json::json!({"content": note, "origin": "learn record"}),
        ),
        "memory" => (
            format!("memory note: {}", super::truncate(note, 60)),
            serde_json::json!({"content": note, "scope": "project"}),
        ),
        "skill" => (
            format!("procedure candidate: {}", super::truncate(note, 60)),
            serde_json::json!({"content": note, "origin": "learn record"}),
        ),
        _ => {
            let candidate = core_learnings::Learning::candidate(
                note,
                &class.category,
                0.45,
                "learn record",
                "project",
            );
            (
                format!("learning: {}", super::truncate(note, 60)),
                serde_json::json!({
                    "id": candidate.id,
                    "statement": candidate.statement,
                    "category": candidate.category,
                    "confidence": candidate.confidence,
                    "label": candidate.label,
                    "sources": candidate.sources,
                    "scope": candidate.scope,
                }),
            )
        }
    };
    let proposal = core_proposals::create(
        &ctx.cwd,
        &class.kind,
        &title,
        &format!("classified as {} ({})", class.kind, class.category),
        payload,
        serde_json::json!({"route": "learn record"}),
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    println!(
        "recorded → proposal {} [{}; pending]",
        &proposal.id[..8],
        class.kind
    );
    println!(
        "review with: stateroot proposals show {}",
        &proposal.id[..8]
    );
    Ok(())
}
