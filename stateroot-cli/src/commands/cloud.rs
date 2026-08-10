//! `stateroot run --cloud` / `stateroot runs` — cloud runs (Phase 3 CLI).
//!
//! Contract (server side built in parallel by agent-21):
//!   `POST /stateroot/projects/{id}/cloud-runs` {objective, from_root?,
//!   harness?, verification?} → 201 {run:{id,status,…}}
//!   `GET …/cloud-runs/{run_id}` → {run:{id,status,result_root_id?,…}}
//!   `GET …/cloud-runs/{run_id}/events` → {events:[…]}
//!   `GET …/cloud-runs` → {runs:[…]}
//!
//! Auth: the Phase-1 login credential (GitHub token) as bearer. No token →
//! honest "requires stateroot login".

use serde_json::{json, Value};

use super::{auth as gh, truncate, Ctx};

const TERMINAL: &[&str] = &["succeeded", "failed", "cancelled", "error", "completed"];

pub(crate) fn base_url(ctx: &Ctx) -> String {
    std::env::var("STATEROOT_CLOUD_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| ctx.config.cloud.base_url.clone())
        .trim_end_matches('/')
        .to_string()
}

fn poll_interval() -> std::time::Duration {
    std::env::var("STATEROOT_CLOUD_POLL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or_else(|| std::time::Duration::from_secs(2))
}

fn client(ctx: &Ctx) -> anyhow::Result<reqwest::Client> {
    let Some(token) = gh::github_token(ctx) else {
        anyhow::bail!("requires `stateroot login` — no credential in the local store");
    };
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().expect("header"),
    );
    headers.insert(
        reqwest::header::USER_AGENT,
        "stateroot-cli".parse().expect("header"),
    );
    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .build()?)
}

async fn get_run(
    client: &reqwest::Client,
    base: &str,
    project_id: &str,
    run_id: &str,
) -> anyhow::Result<Value> {
    let resp = client
        .get(format!(
            "{base}/stateroot/projects/{project_id}/cloud-runs/{run_id}"
        ))
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("cloud run status failed (HTTP {})", resp.status());
    }
    Ok(resp.json().await?)
}

async fn get_events(
    client: &reqwest::Client,
    base: &str,
    project_id: &str,
    run_id: &str,
) -> Vec<Value> {
    // Event tails are best-effort: a failing events endpoint never breaks
    // status polling.
    let Ok(resp) = client
        .get(format!(
            "{base}/stateroot/projects/{project_id}/cloud-runs/{run_id}/events"
        ))
        .send()
        .await
    else {
        return Vec::new();
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    resp.json::<Value>()
        .await
        .ok()
        .and_then(|body| body.get("events").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default()
}

/// `stateroot run --cloud "<objective>" [--from <root>] [--harness <id>] [--watch]`
pub async fn run(
    ctx: &Ctx,
    objective: &str,
    from_root: Option<&str>,
    harness: Option<&str>,
    verification: Option<&str>,
    watch: bool,
) -> anyhow::Result<()> {
    let project = ctx.require_project()?;
    let client = client(ctx)?;
    let base = base_url(ctx);

    let mut body = json!({"objective": objective});
    if let Some(root) = from_root {
        body["from_root"] = json!(root);
    }
    if let Some(harness) = harness {
        body["harness"] = json!(stateroot_core::skill_federation::normalize_harness(harness));
    }
    if let Some(verification) = verification {
        body["verification"] = json!(verification);
    }

    let resp = client
        .post(format!(
            "{base}/stateroot/projects/{}/cloud-runs",
            project.project_id
        ))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("cloud run create failed (HTTP {})", resp.status());
    }
    let created: Value = resp.json().await?;
    let run = created.get("run").cloned().unwrap_or(created.clone());
    let run_id = run
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("cloud run response missing run.id"))?
        .to_string();
    let status = run
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("queued");
    println!("cloud run {} ({status})", &run_id[..8.min(run_id.len())]);

    if !watch {
        println!("watch with: stateroot runs status {run_id}");
        return Ok(());
    }

    // --watch: poll until terminal with a compact event tail.
    let mut seen_events = 0usize;
    loop {
        tokio::time::sleep(poll_interval()).await;
        let body = get_run(&client, &base, &project.project_id, &run_id).await?;
        let run = body.get("run").cloned().unwrap_or(body.clone());
        let status = run
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let events = get_events(&client, &base, &project.project_id, &run_id).await;
        if events.len() > seen_events {
            for event in events.iter().skip(seen_events).take(3) {
                let kind = event
                    .get("kind")
                    .or_else(|| event.get("type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("event");
                let message = event
                    .get("message")
                    .or_else(|| event.get("summary"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                println!("  [{kind}] {}", truncate(message, 100));
            }
            seen_events = events.len();
        }
        if TERMINAL.contains(&status.as_str()) {
            println!("run {} → {status}", &run_id[..8.min(run_id.len())]);
            if let Some(root) = run.get("result_root_id").and_then(|v| v.as_str()) {
                println!("result root: {root}");
                println!("adopt it with: stateroot sync --pull");
            }
            if status != "succeeded" && status != "completed" {
                anyhow::bail!("cloud run {status}");
            }
            return Ok(());
        }
    }
}

/// `stateroot runs list`
pub async fn list(ctx: &Ctx) -> anyhow::Result<()> {
    let project = ctx.require_project()?;
    let client = client(ctx)?;
    let base = base_url(ctx);
    let resp = client
        .get(format!(
            "{base}/stateroot/projects/{}/cloud-runs",
            project.project_id
        ))
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("cloud runs list failed (HTTP {})", resp.status());
    }
    let body: Value = resp.json().await?;
    let runs = body
        .get("runs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if runs.is_empty() {
        println!("no cloud runs yet — `stateroot run --cloud \"<objective>\"`");
        return Ok(());
    }
    for run in &runs {
        let id = run.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let status = run.get("status").and_then(|v| v.as_str()).unwrap_or("?");
        let objective = run.get("objective").and_then(|v| v.as_str()).unwrap_or("");
        println!(
            "  {} [{}] {}",
            &id[..8.min(id.len())],
            status,
            truncate(objective, 80)
        );
    }
    Ok(())
}

/// `stateroot runs status <id>`
pub async fn status(ctx: &Ctx, run_id: &str) -> anyhow::Result<()> {
    let project = ctx.require_project()?;
    let client = client(ctx)?;
    let base = base_url(ctx);
    let body = get_run(&client, &base, &project.project_id, run_id).await?;
    let run = body.get("run").cloned().unwrap_or(body);
    let id = run.get("id").and_then(|v| v.as_str()).unwrap_or(run_id);
    let status = run
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    println!("run {id}");
    println!("status: {status}");
    if let Some(objective) = run.get("objective").and_then(|v| v.as_str()) {
        println!("objective: {}", truncate(objective, 100));
    }
    if let Some(root) = run.get("result_root_id").and_then(|v| v.as_str()) {
        println!("result root: {root}");
    }
    let events = get_events(&client, &base, &project.project_id, id).await;
    if !events.is_empty() {
        println!("events (last {}):", events.len().min(5));
        for event in events.iter().rev().take(5).rev() {
            let kind = event
                .get("kind")
                .or_else(|| event.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("event");
            let message = event
                .get("message")
                .or_else(|| event.get("summary"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            println!("  [{kind}] {}", truncate(message, 100));
        }
    }
    Ok(())
}
