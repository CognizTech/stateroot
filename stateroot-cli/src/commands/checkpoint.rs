//! `stateroot checkpoint` — append an episodic record to the local log.

use serde_json::json;
use stateroot_core::local_store::{self, now_rfc3339};

use super::Ctx;

/// Harness id recorded in local episodic records.
const LOCAL_HARNESS: &str = "cli";

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
    println!("checkpoint recorded");
    // Compact digest footer (composed locally).
    if let Some(footer) = super::resume::digest_footer(&ctx.cwd) {
        println!("{footer}");
    }
    Ok(())
}

/// Append an episodic checkpoint locally (used by hooks and handoff
/// lifecycle events). Always "projected" — the local store is the only
/// destination in this variant.
pub(crate) async fn record_checkpoint(
    ctx: &Ctx,
    note_text: &str,
    files: &[String],
) -> anyhow::Result<bool> {
    ctx.require_project()?;
    let record = json!({
        "ts": now_rfc3339(),
        "harness": LOCAL_HARNESS,
        "note": note_text,
        "files": files,
    });
    local_store::append_episodic(&ctx.cwd, &record)?;
    Ok(true)
}
