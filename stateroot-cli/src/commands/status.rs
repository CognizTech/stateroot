//! `stateroot status` — the single decision-ready project brief:
//! checkout/root state, current handoff and freshness, plan state, due
//! obligations, unresolved attention, service health, boundary-journal
//! health. Local only — no server calls.

use serde_json::json;
use stateroot_core::local_store::{self, now_rfc3339};
use stateroot_core::plans;

use super::Ctx;

struct Brief {
    handoff_line: String,
    handoff_stale: bool,
    plan_line: String,
    plan_directive: String,
}

fn gather(ctx: &Ctx) -> Brief {
    // Handoff + freshness (same boundary rule as the digest).
    let handoff = local_store::read_handoff_local(&ctx.cwd).ok().flatten();
    let mut handoff_stale = false;
    let handoff_line = match &handoff {
        Some(packet) => {
            let objective = packet
                .get("objective")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let harness = packet
                .get("created_by_harness")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let seq = packet.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
            let boundary = packet
                .get("written_at")
                .and_then(|v| v.as_str())
                .filter(|t| !t.is_empty())
                .or_else(|| packet.get("created_at").and_then(|v| v.as_str()));
            if let Some(boundary) = boundary {
                let latest = local_store::recent_episodic(&ctx.cwd, 1)
                    .into_iter()
                    .next()
                    .and_then(|rec| rec.get("ts").and_then(|v| v.as_str()).map(str::to_string));
                if let Some(activity) = latest {
                    if stateroot_core::continuity::ts_newer(&activity, boundary) {
                        handoff_stale = true;
                    }
                }
            }
            format!(
                "seq {seq} by {harness} — {}",
                super::truncate(objective, 100)
            )
        }
        None => "none yet".to_string(),
    };

    // Plan state + state-aware directive.
    let (plan_line, plan_directive) = match plans::current(&ctx.cwd) {
        Some((plan, _)) => {
            let directive = stateroot_core::continuity::plan_directive(&ctx.cwd, &plan);
            let mut line = format!("{} ({}) — {}", plan.title, plan.status, plan.id);
            if let Some((done, total)) =
                stateroot_core::todo_federation::plan_todo_progress(&ctx.cwd, &plan.id)
            {
                line.push_str(&format!(" · todos {done}/{total}"));
            }
            (line, directive.as_str().to_string())
        }
        None => ("none".to_string(), String::new()),
    };

    Brief {
        handoff_line,
        handoff_stale,
        plan_line,
        plan_directive,
    }
}

/// Run `stateroot status [--json]`.
pub fn run(ctx: &Ctx, json_out: bool) -> anyhow::Result<()> {
    let Some(project) = ctx.current_project()? else {
        println!("not a stateroot project — run `stateroot init`");
        return Ok(());
    };
    let root = local_store::root(&ctx.cwd);

    // Reconcile first: the brief reads a fresh projection, and the
    // machine-local projection file is what the editor surfaces consume.
    let assessment = if ctx.config.continuity.enabled {
        match stateroot_core::continuity::reconcile(
            &ctx.cwd,
            &ctx.config_dir,
            &ctx.config.continuity,
        ) {
            Ok(a) => Some(a),
            Err(err) => {
                super::note!("continuity reconcile: {err}");
                stateroot_core::continuity::read_projection(&ctx.cwd)
            }
        }
    } else {
        stateroot_core::continuity::read_projection(&ctx.cwd)
    };

    let brief = gather(ctx);

    // Counts and boundary-journal health.
    let episodic = std::fs::read_to_string(root.join(local_store::EPISODIC_PATH))
        .map(|text| text.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    let skills = stateroot_core::skill_federation::discover_all(&ctx.cwd, None)
        .map(|v| v.len())
        .unwrap_or(0);
    let persona = super::persona::read_cache(&ctx.config_dir).is_some();
    let jobs = stateroot_core::finalize_journal::load_all(&ctx.cwd);
    let jobs_active = jobs.iter().filter(|j| j.state == "active").count();
    let jobs_manual = jobs
        .iter()
        .filter(|j| j.state == "manual_attention")
        .count();
    let latest_root = stateroot_core::roots::latest_root(&ctx.cwd).ok().flatten();
    let advisory = assessment
        .as_ref()
        .and_then(|a| stateroot_core::continuity::current_advisory(&ctx.cwd, a));

    if json_out {
        let payload = json!({
            "schema_version": "stateroot.status.v1",
            "generated_at": now_rfc3339(),
            "project": { "name": project.name, "project_id": project.project_id },
            "root": latest_root,
            "handoff": { "summary": brief.handoff_line, "stale": brief.handoff_stale },
            "plan": { "summary": brief.plan_line, "directive": brief.plan_directive },
            "continuity": assessment,
            "advisory": advisory,
            "boundary_journal": { "active": jobs_active, "manual_attention": jobs_manual },
            "counts": { "checkpoints": episodic, "federated_skills": skills },
            "persona_cached": persona,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("project: {} ({})", project.name, project.project_id);
        if let Some(root) = &latest_root {
            println!("root: {}", &root[..12.min(root.len())]);
        }
        println!(
            "handoff: {}{}",
            brief.handoff_line,
            if brief.handoff_stale { "  [STALE]" } else { "" }
        );
        println!("plan: {}", brief.plan_line);
        if !brief.plan_directive.is_empty() {
            println!("plan directive: {}", brief.plan_directive);
        }
        if let Some(a) = &assessment {
            println!(
                "obligations: {} open · {} corrupt event(s) preserved",
                a.open_obligations, a.corrupt_obligation_events
            );
            if let Some(section) =
                stateroot_core::continuity::needs_attention_markdown(a, advisory.as_deref())
            {
                print!("{section}");
            } else {
                println!("attention: nothing needs attention");
            }
            println!(
                "service: {}",
                if a.service_running {
                    "running".to_string()
                } else if a.service_registered {
                    format!(
                        "registered ({}) but not heartbeating",
                        a.service_kind.as_deref().unwrap_or("?")
                    )
                } else {
                    "not registered (hooks/CLI reconcile on activity)".to_string()
                }
            );
        } else {
            println!("continuity: disabled");
        }
        println!(
            "boundary journal: {jobs_active} active · {jobs_manual} parked for manual attention"
        );
        println!("checkpoints: {episodic}");
        println!("federated skills: {skills}");
        println!("persona cached: {}", if persona { "yes" } else { "no" });
    }
    // Safe entrypoint for the boundary journal (cheap due-scan kick).
    super::drain_finalize::kick(&ctx.cwd);
    Ok(())
}
