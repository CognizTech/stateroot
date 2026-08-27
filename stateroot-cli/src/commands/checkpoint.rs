//! `stateroot checkpoint` — append an episodic record to the local log.

use serde_json::json;
use stateroot_core::local_store::{self, now_rfc3339};

use super::Ctx;

/// Harness id recorded in local episodic records from the CLI itself.
pub(crate) const LOCAL_HARNESS: &str = "cli";

/// Run `stateroot checkpoint`.
pub fn run(ctx: &Ctx, note_text: &str, files: &[String]) -> anyhow::Result<()> {
    ctx.require_project()?;
    let record = json!({
        "ts": now_rfc3339(),
        "harness": LOCAL_HARNESS,
        "note": note_text,
        "files": files,
    });
    local_store::append_episodic(&ctx.cwd, &record)?;
    // The next harness should see who worked last even when no formal
    // handoff exists — stamp the current packet (additive, in place).
    local_store::stamp_handoff_activity(&ctx.cwd, LOCAL_HARNESS, "checkpoint");
    println!("checkpoint recorded");
    // Compact digest footer (composed locally).
    if let Some(footer) = super::resume::digest_footer(&ctx.cwd) {
        println!("{footer}");
    }
    Ok(())
}

/// Append an episodic checkpoint locally (used by hooks and handoff
/// lifecycle events). Always "projected" — the local store is the only
/// destination in this variant. `harness` is the actor the checkpoint is
/// attributed to (the firing harness, or `cli` for lifecycle events).
pub(crate) async fn record_checkpoint(
    ctx: &Ctx,
    harness: &str,
    note_text: &str,
    files: &[String],
) -> anyhow::Result<bool> {
    ctx.require_project()?;
    let record = json!({
        "ts": now_rfc3339(),
        "harness": harness,
        "note": note_text,
        "files": files,
    });
    local_store::append_episodic(&ctx.cwd, &record)?;
    local_store::stamp_handoff_activity(&ctx.cwd, harness, "checkpoint");
    Ok(true)
}
