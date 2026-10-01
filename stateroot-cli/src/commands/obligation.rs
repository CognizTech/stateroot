//! `stateroot obligation` — durable, federated future-work items.

use anyhow::anyhow;

use stateroot_core::local_store::now_rfc3339;
use stateroot_core::obligations::{self, NewObligation, Obligation};

use super::{note, reconcile_quiet, truncate, Ctx};

fn actor(ctx: &Ctx) -> String {
    super::active_harness::read(&ctx.cwd)
        .ok()
        .flatten()
        .unwrap_or_else(|| "cli".to_string())
}

pub fn add(
    ctx: &Ctx,
    task: &str,
    due: Option<&str>,
    in_duration: Option<&str>,
    assign: Option<&str>,
    plan: Option<&str>,
    operation_id: Option<&str>,
) -> anyhow::Result<()> {
    ctx.require_project()?;
    let due_at = match (due, in_duration) {
        (Some(raw), None) => Some(obligations::normalize_rfc3339(raw).map_err(|e| anyhow!(e))?),
        (None, Some(raw)) => {
            let secs = obligations::parse_duration_secs(raw).map_err(|e| anyhow!(e))?;
            Some(obligations::in_duration(&now_rfc3339(), secs).map_err(|e| anyhow!(e))?)
        }
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("clap conflicts_with"),
    };
    let (obligation, created) = obligations::add(
        &ctx.cwd,
        NewObligation {
            task: task.to_string(),
            due_at,
            assign: assign.map(str::to_string),
            plan_id: plan.map(str::to_string),
            operation_id: operation_id
                .map(str::to_string)
                .unwrap_or_else(obligations::mint_operation_id),
            actor: actor(ctx),
        },
    )
    .map_err(|e| anyhow!(e))?;
    if created {
        println!(
            "recorded obligation {} ({})",
            obligation.id,
            obligation
                .due_at
                .as_ref()
                .map(|d| format!("due {d}"))
                .unwrap_or_else(|| "no due date".into())
        );
    } else {
        println!(
            "obligation {} already recorded (idempotent operation {})",
            obligation.id, obligation.operation_id
        );
    }
    reconcile_quiet(ctx);
    Ok(())
}

fn row(ctx: &Ctx, obligation: &Obligation, now: &str) -> String {
    let _ = ctx;
    let state = if obligation.due(now) {
        "due".to_string()
    } else {
        obligation.state().as_str().to_string()
    };
    let mut line = format!(
        "{}  {:<9} {}",
        &obligation.id[..8.min(obligation.id.len())],
        state,
        truncate(&obligation.task, 80)
    );
    if let Some(due) = &obligation.due_at {
        line.push_str(&format!("  · due {due}"));
    }
    if let Some(assign) = &obligation.assign {
        line.push_str(&format!("  · → {assign}"));
    }
    if let Some(plan) = &obligation.plan_id {
        line.push_str(&format!("  · plan {}", truncate(plan, 40)));
    }
    line
}

pub fn list(ctx: &Ctx, all: bool, json: bool) -> anyhow::Result<()> {
    ctx.require_project()?;
    let now = now_rfc3339();
    let obligations = obligations::list(&ctx.cwd);
    let (_, corrupt_events) = obligations::read_events(&ctx.cwd);
    let shown: Vec<&Obligation> = obligations
        .iter()
        .filter(|o| all || !o.state().is_terminal())
        .collect();
    if json {
        let payload = serde_json::json!({
            "schema_version": "stateroot.obligations.v1",
            "corrupt_events": corrupt_events,
            "obligations": shown,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    if shown.is_empty() {
        println!("no obligations");
        return Ok(());
    }
    let mut open: Vec<&&Obligation> = shown.iter().filter(|o| !o.state().is_terminal()).collect();
    open.sort_by_key(|o| o.due_at.clone().unwrap_or_else(|| "9999".into()));
    for obligation in open {
        println!("{}", row(ctx, obligation, &now));
    }
    if all {
        for obligation in shown.iter().filter(|o| o.state().is_terminal()) {
            println!("{}", row(ctx, obligation, &now));
        }
    }
    if corrupt_events > 0 {
        note!(
            "warning: {corrupt_events} corrupt obligation event line(s) preserved in events.jsonl"
        );
    }
    Ok(())
}

pub fn show(ctx: &Ctx, id: &str) -> anyhow::Result<()> {
    ctx.require_project()?;
    let obligation = obligations::load(&ctx.cwd, id)
        .ok_or_else(|| anyhow!("unknown obligation `{id}` — run `stateroot obligation list`"))?;
    println!("obligation {}", obligation.id);
    println!("  task:      {}", obligation.task);
    println!("  state:     {}", obligation.state().as_str());
    if let Some(due) = &obligation.due_at {
        println!("  due:       {due}");
    }
    if let Some(assign) = &obligation.assign {
        println!("  assign:    {assign}");
    }
    if let Some(plan) = &obligation.plan_id {
        println!("  plan:      {plan}");
    }
    println!(
        "  created:   {} by {}",
        obligation.created_at, obligation.created_by
    );
    if let Some(until) = &obligation.snoozed_until {
        println!("  snoozed:   until {until}");
    }
    if let (Some(at), Some(by)) = (&obligation.completed_at, &obligation.completed_by) {
        println!("  completed: {at} by {by}");
    }
    if let Some(evidence) = &obligation.evidence {
        println!("  evidence:  {evidence}");
    }
    if let (Some(at), Some(reason)) = (&obligation.cancelled_at, &obligation.cancel_reason) {
        println!("  cancelled: {at} — {reason}");
    }
    println!("  operation: {}", obligation.operation_id);
    let (events, corrupt) = obligations::read_events(&ctx.cwd);
    let count = events
        .iter()
        .filter(|e| e.obligation_id == obligation.id)
        .count();
    println!("  events:    {count} recorded");
    if corrupt > 0 {
        note!("warning: {corrupt} corrupt obligation event line(s) preserved in events.jsonl");
    }
    Ok(())
}

pub fn done(ctx: &Ctx, id: &str, evidence: &str, operation_id: Option<&str>) -> anyhow::Result<()> {
    ctx.require_project()?;
    let obligation = obligations::done(
        &ctx.cwd,
        id,
        evidence,
        &actor(ctx),
        operation_id
            .map(str::to_string)
            .unwrap_or_else(obligations::mint_operation_id)
            .as_str(),
    )
    .map_err(|e| anyhow!(e))?;
    println!(
        "obligation {} done — {}",
        obligation.id,
        truncate(evidence, 80)
    );
    reconcile_quiet(ctx);
    Ok(())
}

pub fn snooze(ctx: &Ctx, id: &str, until: &str, operation_id: Option<&str>) -> anyhow::Result<()> {
    ctx.require_project()?;
    let obligation = obligations::snooze(
        &ctx.cwd,
        id,
        until,
        &actor(ctx),
        operation_id
            .map(str::to_string)
            .unwrap_or_else(obligations::mint_operation_id)
            .as_str(),
    )
    .map_err(|e| anyhow!(e))?;
    println!(
        "obligation {} snoozed until {}",
        obligation.id,
        obligation.snoozed_until.as_deref().unwrap_or(until)
    );
    reconcile_quiet(ctx);
    Ok(())
}

pub fn cancel(ctx: &Ctx, id: &str, reason: &str, operation_id: Option<&str>) -> anyhow::Result<()> {
    ctx.require_project()?;
    let obligation = obligations::cancel(
        &ctx.cwd,
        id,
        reason,
        &actor(ctx),
        operation_id
            .map(str::to_string)
            .unwrap_or_else(obligations::mint_operation_id)
            .as_str(),
    )
    .map_err(|e| anyhow!(e))?;
    println!(
        "obligation {} cancelled — {}",
        obligation.id,
        truncate(reason, 80)
    );
    reconcile_quiet(ctx);
    Ok(())
}
