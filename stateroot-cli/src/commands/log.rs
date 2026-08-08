//! `stateroot log` — local history: episodic checkpoints + handoff lineage
//! (M1 is pre-git-roots; transitions arrive with M2).

use stateroot_core::local_store;

use super::{truncate, Ctx};

/// Run `stateroot log`.
pub fn run(ctx: &Ctx) -> anyhow::Result<()> {
    ctx.require_project()?;
    let root = local_store::root(&ctx.cwd);

    let episodic_path = root.join(local_store::EPISODIC_PATH);
    let episodic = std::fs::read_to_string(&episodic_path).unwrap_or_default();
    let records: Vec<&str> = episodic.lines().filter(|l| !l.trim().is_empty()).collect();
    println!("## Checkpoints ({})", records.len());
    for line in records.iter().rev().take(20) {
        let parsed: serde_json::Value =
            serde_json::from_str(line).unwrap_or(serde_json::Value::Null);
        let ts = parsed.get("ts").and_then(|v| v.as_str()).unwrap_or("?");
        let harness = parsed
            .get("harness")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let note = parsed.get("note").and_then(|v| v.as_str()).unwrap_or("");
        println!("  {ts} [{harness}] {}", truncate(note, 100));
    }
    if records.is_empty() {
        println!("  (none yet — `stateroot checkpoint --note ...`)");
    }

    println!();
    let history_dir = root.join(local_store::HANDOFF_HISTORY_DIR);
    let mut handoffs: Vec<String> = std::fs::read_dir(&history_dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    handoffs.sort();
    println!("## Handoffs ({})", handoffs.len());
    for name in handoffs.iter().rev().take(10) {
        println!("  {name}");
    }
    if handoffs.is_empty() {
        println!("  (none yet — `stateroot handoff write --to <harness>`)");
    }
    Ok(())
}
