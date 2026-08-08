//! `stateroot status` — local project status (no server calls).

use stateroot_core::local_store;

use super::Ctx;

/// Run `stateroot status`.
pub fn run(ctx: &Ctx) -> anyhow::Result<()> {
    let Some(project) = ctx.current_project()? else {
        println!("not a stateroot project — run `stateroot init`");
        return Ok(());
    };
    println!("project: {} ({})", project.name, project.project_id);
    let root = local_store::root(&ctx.cwd);

    // Current handoff.
    match local_store::read_handoff_local(&ctx.cwd) {
        Ok(Some(packet)) => {
            let objective = packet
                .get("objective")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let harness = packet
                .get("created_by_harness")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let seq = packet.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
            println!(
                "handoff: seq {seq} by {harness} — {}",
                super::truncate(objective, 100)
            );
        }
        Ok(None) => println!("handoff: none yet"),
        Err(err) => println!("handoff: unreadable ({err})"),
    }

    // Counts.
    let episodic = std::fs::read_to_string(root.join(local_store::EPISODIC_PATH))
        .map(|text| text.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    let skills = stateroot_core::skill_federation::discover_all(&ctx.cwd, None)
        .map(|v| v.len())
        .unwrap_or(0);
    let persona = super::persona::read_cache(&ctx.config_dir).is_some();
    println!("checkpoints: {episodic}");
    println!("federated skills: {skills}");
    println!("persona cached: {}", if persona { "yes" } else { "no" });
    Ok(())
}
