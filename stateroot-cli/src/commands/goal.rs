//! `stateroot goal` — contract-first project goals (Phase E).
//!
//! The contract has six required parts; the client validates them BEFORE any
//! server call and lists the missing ones (the server 400s the same way).
//! Goal docs sync locally to `.stateroot/goals/<id>.json`.

use serde_json::{json, Value};

use super::{ensure_auth, note, truncate, Ctx};

/// Options for `stateroot goal create` (all six contract parts + budget/plan).
pub struct GoalCreateOptions {
    /// Objective / outcome text.
    pub objective: Option<String>,
    /// Done-when check text.
    pub done_when: Option<String>,
    /// Verification surface (test|benchmark|artifact|command).
    pub surface: String,
    /// Repeatable constraints.
    pub constraints: Vec<String>,
    /// Repeatable boundaries.
    pub boundaries: Vec<String>,
    /// Iteration policy text.
    pub iteration_policy: Option<String>,
    /// Blocked-stop condition text.
    pub blocked_stop: Option<String>,
    /// Budget cap in turns.
    pub max_turns: Option<u32>,
    /// Budget cap in seconds.
    pub max_seconds: Option<u64>,
    /// `;`-split plan steps.
    pub plan: Option<String>,
}

fn get_str<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

/// The six contract parts as (key, present) — client-side validation list.
fn missing_contract_parts(options: &GoalCreateOptions) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if options
        .objective
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        missing.push("objective (--objective)");
    }
    if options
        .done_when
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        missing.push("completion_criteria (--done-when)");
    }
    if options.constraints.is_empty() {
        missing.push("constraints (--constraint, repeatable)");
    }
    if options.boundaries.is_empty() {
        missing.push("boundaries (--boundary, repeatable)");
    }
    if options
        .iteration_policy
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        missing.push("iteration_policy (--iteration-policy)");
    }
    if options
        .blocked_stop
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        missing.push("blocked_stop_condition (--blocked-stop)");
    }
    missing
}

/// `stateroot goal create` — contract-first; zero server calls when the
/// contract is incomplete.
pub async fn create(ctx: &Ctx, options: &GoalCreateOptions) -> anyhow::Result<()> {
    let missing = missing_contract_parts(options);
    if !missing.is_empty() {
        anyhow::bail!(
            "goal contract is incomplete — missing: {}",
            missing.join(", ")
        );
    }
    let objective = options.objective.as_deref().unwrap_or("");
    let done_when = options.done_when.as_deref().unwrap_or("");
    let mut budget = serde_json::Map::new();
    if let Some(turns) = options.max_turns {
        budget.insert("max_turns".to_string(), json!(turns));
    }
    if let Some(seconds) = options.max_seconds {
        budget.insert("max_seconds".to_string(), json!(seconds));
    }
    let plan: Vec<Value> = options
        .plan
        .as_deref()
        .unwrap_or("")
        .split(';')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|step| json!({"step": step, "status": "pending"}))
        .collect();
    let body = json!({
        "objective": objective,
        "completion_criteria": [{
            "verification_surface": options.surface,
            "check": done_when,
        }],
        "constraints": options.constraints,
        "boundaries": options.boundaries,
        "iteration_policy": options.iteration_policy,
        "blocked_stop_condition": options.blocked_stop,
        "budget": Value::Object(budget),
        "plan": plan,
        "harness": "cli",
    });

    let project = ctx.require_project()?;
    let cred = ensure_auth(ctx).await?;
    let client = ctx.stateroot_client(Some(cred))?;
    let goal = client.create_goal(&project.project_id, &body).await?;
    println!(
        "goal created: {} (lifecycle: {})",
        get_str(&goal, "id"),
        get_str(&goal, "lifecycle")
    );
    if let Some(previous) = goal.get("previous_active_goal").and_then(|v| v.as_str()) {
        println!("note: previous active goal {previous} was paused");
    }
    println!("objective: {}", truncate(objective, 120));
    Ok(())
}

/// `stateroot goal list [--lifecycle <l>]`.
pub async fn list(ctx: &Ctx, lifecycle: Option<&str>) -> anyhow::Result<()> {
    let project = ctx.require_project()?;
    let cred = ensure_auth(ctx).await?;
    let client = ctx.stateroot_client(Some(cred))?;
    let goals = client.list_goals(&project.project_id, lifecycle).await?;
    if goals.is_empty() {
        println!("no goals — create one with `stateroot goal create`");
        return Ok(());
    }
    println!(
        "{:<14} {:<14} {:<50} {:<10} CREATED BY",
        "ID", "LIFECYCLE", "OBJECTIVE", "STEPS"
    );
    for goal in &goals {
        let completed = goal
            .get("steps_completed")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let pending = goal
            .get("steps_pending")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        println!(
            "{:<14} {:<14} {:<50} {:<10} {}",
            short_id(get_str(goal, "id")),
            get_str(goal, "lifecycle"),
            truncate(get_str(goal, "objective"), 50),
            format!("{completed}/{pending}"),
            get_str(goal, "created_by_harness")
        );
    }
    Ok(())
}

/// `stateroot goal show [id]` (default: the active one).
pub async fn show(ctx: &Ctx, goal_id: Option<&str>) -> anyhow::Result<()> {
    let project = ctx.require_project()?;
    let cred = ensure_auth(ctx).await?;
    let client = ctx.stateroot_client(Some(cred))?;
    let goal = match goal_id {
        Some(id) => client
            .get_goal(&project.project_id, id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("goal not found: {id}"))?,
        None => {
            let goals = client
                .list_goals(&project.project_id, Some("active"))
                .await?;
            match goals.first() {
                Some(goal) => goal.clone(),
                None => {
                    println!("no active goal");
                    return Ok(());
                }
            }
        }
    };
    print_goal(&goal);
    Ok(())
}

fn print_goal(goal: &Value) {
    println!("goal {}", get_str(goal, "id"));
    println!("lifecycle: {}", get_str(goal, "lifecycle"));
    println!("objective: {}", get_str(goal, "objective"));
    if let Some(criteria) = goal.get("completion_criteria").and_then(|v| v.as_array()) {
        for criterion in criteria {
            println!(
                "done-when [{}]: {}",
                get_str(criterion, "verification_surface"),
                get_str(criterion, "check")
            );
        }
    }
    for (key, title) in [("constraints", "constraints"), ("boundaries", "boundaries")] {
        if let Some(items) = goal.get(key).and_then(|v| v.as_array()) {
            if !items.is_empty() {
                println!("{title}:");
                for item in items {
                    println!("  - {}", item.as_str().unwrap_or(""));
                }
            }
        }
    }
    if !get_str(goal, "iteration_policy").is_empty() {
        println!("iteration policy: {}", get_str(goal, "iteration_policy"));
    }
    if !get_str(goal, "blocked_stop_condition").is_empty() {
        println!("blocked-stop: {}", get_str(goal, "blocked_stop_condition"));
    }
    if let Some(budget) = goal.get("budget").and_then(|v| v.as_object()) {
        if !budget.is_empty() {
            println!("budget: {}", Value::Object(budget.clone()));
        }
    }
    let completed = goal
        .get("steps_completed")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let pending = goal
        .get("steps_pending")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    println!("steps: {completed} completed, {pending} pending");
    if let Some(plan) = goal.get("plan").and_then(|v| v.as_array()) {
        for step in plan {
            println!("  [{}] {}", get_str(step, "status"), get_str(step, "step"));
        }
    }
    if let Some(evidence) = goal.get("evidence").and_then(|v| v.as_array()) {
        if !evidence.is_empty() {
            println!("evidence:");
            for entry in evidence {
                let kind = get_str(entry, "kind");
                let summary = get_str(entry, "summary");
                let reference = get_str(entry, "ref");
                if reference.is_empty() {
                    println!("  - [{kind}] {summary}");
                } else {
                    println!("  - [{kind}] {summary} ({reference})");
                }
            }
        }
    }
    println!(
        "created by: {} at {}",
        get_str(goal, "created_by_harness"),
        get_str(goal, "created_at")
    );
}

/// `stateroot goal pause|resume|block|clear <id>` — lifecycle transition;
/// 409 reasons print verbatim.
pub async fn action(ctx: &Ctx, goal_id: &str, action: &str) -> anyhow::Result<()> {
    let project = ctx.require_project()?;
    let cred = ensure_auth(ctx).await?;
    let client = ctx.stateroot_client(Some(cred))?;
    match client
        .goal_action(&project.project_id, goal_id, action)
        .await
    {
        Ok(goal) => println!(
            "goal {} → lifecycle: {}",
            short_id(get_str(&goal, "id")),
            get_str(&goal, "lifecycle")
        ),
        Err(err) => {
            println!("{action} rejected: {err}");
        }
    }
    Ok(())
}

/// `stateroot goal complete <id> [--evidence "<summary>"]`.
pub async fn complete(ctx: &Ctx, goal_id: &str, evidence: Option<&str>) -> anyhow::Result<()> {
    let project = ctx.require_project()?;
    let cred = ensure_auth(ctx).await?;
    let client = ctx.stateroot_client(Some(cred))?;
    let evidence = evidence.map(|summary| vec![json!({"kind": "note", "summary": summary})]);
    match client
        .goal_complete(&project.project_id, goal_id, evidence)
        .await
    {
        Ok(goal) => {
            println!(
                "goal {} completed (lifecycle: {})",
                short_id(get_str(&goal, "id")),
                get_str(&goal, "lifecycle")
            );
            if let Some(boundary) = goal.get("root_boundary") {
                println!(
                    "root boundary: root {} · transition {}",
                    get_str(boundary, "root_id"),
                    get_str(boundary, "transition_id")
                );
            }
        }
        Err(err) => {
            // 409 unmet criteria — the server lists them in the message.
            println!("complete rejected (unmet criteria): {err}");
        }
    }
    Ok(())
}

/// `stateroot goal draft` — LLM-assisted contract draft via the server
/// endpoint; the 503 GOAL_DRAFT_UNAVAILABLE path falls back to the local
/// contract template. NEVER creates a goal (server contract).
pub async fn draft(ctx: &Ctx, rough_objective: &str) -> anyhow::Result<()> {
    if rough_objective.trim().is_empty() {
        anyhow::bail!("goal draft requires a non-empty objective");
    }
    let project = ctx.require_project()?;
    let cred = ctx.try_credential().await?;
    if let Some(token) = cred {
        let client = ctx.stateroot_client(Some(token))?;
        match client
            .goal_draft(&project.project_id, rough_objective)
            .await
        {
            Ok(draft) => {
                println!("goal draft (server, LLM-assisted):");
                println!();
                print_draft(&draft);
                println!();
                println!("These fields map 1:1 onto `stateroot goal create` flags");
                println!("(--objective/--done-when/--surface/--constraint/--boundary/--iteration-policy/--blocked-stop/--plan).");
                return Ok(());
            }
            Err(err) if err.is_goal_draft_unavailable() => {
                println!("server drafting unavailable — showing the contract template");
            }
            Err(err) => {
                note!("warning: draft endpoint failed ({err}); showing the contract template")
            }
        }
    }
    print_template(rough_objective);
    Ok(())
}

/// Render the server's draft contract readably (all six parts + plan).
fn print_draft(draft: &Value) {
    println!("objective:            {}", get_str(draft, "objective"));
    if let Some(criteria) = draft.get("completion_criteria").and_then(|v| v.as_array()) {
        for criterion in criteria {
            println!(
                "completion_criteria:  [{}] {}",
                get_str(criterion, "verification_surface"),
                get_str(criterion, "check")
            );
        }
    }
    for (key, title) in [("constraints", "constraints"), ("boundaries", "boundaries")] {
        let items: Vec<&str> = draft
            .get(key)
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|i| i.as_str()).collect())
            .unwrap_or_default();
        if items.is_empty() {
            println!("{title}:            []");
        } else {
            println!("{title}:");
            for item in items {
                println!("  - {item}");
            }
        }
    }
    if !get_str(draft, "iteration_policy").is_empty() {
        println!(
            "iteration_policy:     {}",
            get_str(draft, "iteration_policy")
        );
    }
    if !get_str(draft, "blocked_stop_condition").is_empty() {
        println!(
            "blocked_stop_condition: {}",
            get_str(draft, "blocked_stop_condition")
        );
    }
    if let Some(budget) = draft.get("budget").and_then(|v| v.as_object()) {
        if !budget.is_empty() {
            println!("budget:               {}", Value::Object(budget.clone()));
        }
    }
    if let Some(plan) = draft.get("plan").and_then(|v| v.as_array()) {
        if !plan.is_empty() {
            println!("plan:");
            for step in plan {
                println!("  [{}] {}", get_str(step, "status"), get_str(step, "step"));
            }
        }
    }
}

/// The local contract template (fallback when drafting is unavailable).
fn print_template(rough_objective: &str) {
    println!("goal draft (local template):");
    println!();
    println!("objective:            {rough_objective}");
    println!("completion_criteria:  [verification_surface: test|benchmark|artifact|command, check: \"<done-when>\"]");
    println!("constraints:          [\"<must-hold invariant 1>\", …]");
    println!("boundaries:           [\"<never-do 1>\", …]");
    println!("iteration_policy:     \"<how the agent may iterate / when to stop>\"");
    println!("blocked_stop_condition: \"<when blocked, stop and surface instead of grinding>\"");
    println!();
    println!("Fill each part, then: stateroot goal create --objective … --done-when … \\");
    println!("  --constraint … --boundary … --iteration-policy … --blocked-stop …");
}

/// Read the local synced goal docs (`<id>.json` under `.stateroot/goals/`).
pub fn read_local_goals(project_dir: &std::path::Path) -> Vec<Value> {
    let dir = stateroot_core::local_store::root(project_dir).join("goals");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(goal) =
            serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap_or_default())
        {
            out.push(goal);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn debug_read_local_goals_finds_active() {
        let project = tempfile::tempdir().expect("p");
        let dir = project.path().join(".stateroot/goals");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("g1.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "id": "g1", "lifecycle": "active", "objective": "x"
            }))
            .unwrap(),
        )
        .unwrap();
        let goals = super::read_local_goals(project.path());
        assert_eq!(goals.len(), 1, "goals: {goals:?}");
        let active = goals
            .into_iter()
            .find(|g| g.get("lifecycle").and_then(|v| v.as_str()) == Some("active"));
        assert!(active.is_some());
    }
}
