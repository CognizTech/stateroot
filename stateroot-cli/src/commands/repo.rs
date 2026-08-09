//! `stateroot repo link|status` — bind the project to a GitHub repo for
//! refs sync (Phase 1). The binding lives in `.stateroot/manifest.json` under
//! `github` — synced with the roots like everything else.

use serde_json::{json, Value};
use stateroot_core::local_store;

use super::{auth as gh, note, Ctx};

fn manifest_path(ctx: &Ctx) -> std::path::PathBuf {
    local_store::root(&ctx.cwd).join(local_store::MANIFEST_PATH)
}

/// Read the github binding from the manifest, if any.
pub fn binding(ctx: &Ctx) -> Option<Value> {
    let text = std::fs::read_to_string(manifest_path(ctx)).ok()?;
    let manifest: Value = serde_json::from_str(&text).ok()?;
    manifest.get("github").cloned()
}

/// The clone/push URL for the binding.
pub fn remote_url(_ctx: &Ctx, binding: &Value) -> String {
    let repo = binding.get("repo").and_then(|v| v.as_str()).unwrap_or("");
    format!("{}/{repo}.git", gh::git_base())
}

/// Parse `owner/repo` (also accepts full https/ssh URLs).
fn parse_repo(input: &str) -> anyhow::Result<String> {
    let trimmed = input.trim().trim_end_matches(".git").trim_end_matches('/');
    let without_scheme = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("git@github.com:"))
        .unwrap_or(trimmed);
    let parts: Vec<&str> = without_scheme.split('/').collect();
    if parts.len() == 2 && parts.iter().all(|p| !p.is_empty()) {
        Ok(parts.join("/"))
    } else {
        anyhow::bail!("expected <owner/repo> (or a github.com URL), got '{input}'")
    }
}

/// `stateroot repo link <owner/repo> [--layout same-repo|companion]`
pub async fn link(ctx: &Ctx, repo: &str, layout: Option<&str>) -> anyhow::Result<()> {
    ctx.require_project()?;
    let repo = parse_repo(repo)?;
    let layout = layout.unwrap_or("same-repo");
    anyhow::ensure!(
        matches!(layout, "same-repo" | "companion"),
        "layout must be same-repo or companion"
    );

    // Verify access with the token when one exists (honest warning otherwise).
    let Some(token) = gh::github_token(ctx) else {
        anyhow::bail!("no github credential — run `stateroot login --via github` first");
    };
    let owner = repo.split('/').next().unwrap_or("");
    let effective_repo = if layout == "companion" {
        let project = ctx.require_project()?;
        format!("{}/{}-stateroot", owner, project.name)
    } else {
        repo.clone()
    };
    let resp = reqwest::Client::new()
        .get(format!("{}/repos/{effective_repo}", gh::api_base()))
        .bearer_auth(&token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "stateroot-cli")
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND && layout == "companion" {
        anyhow::bail!(
            "companion repo '{effective_repo}' not found — create it first (`gh repo create {effective_repo} --private`), then re-link"
        );
    }
    if !resp.status().is_success() {
        anyhow::bail!(
            "cannot access {effective_repo} (HTTP {}) — check the repo name and token scope",
            resp.status()
        );
    }

    // Persist the binding in the manifest (synced with the roots).
    let path = manifest_path(ctx);
    let mut manifest: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    manifest["github"] = json!({
        "repo": effective_repo,
        "layout": layout,
        "linked_at": stateroot_core::local_store::now_rfc3339(),
    });
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )?;
    println!("linked: {effective_repo} ({layout})");
    if layout == "same-repo" {
        println!("refs live at refs/stateroot/* — invisible to your branch list");
    }
    Ok(())
}

/// `stateroot repo status`
pub fn status(ctx: &Ctx) -> anyhow::Result<()> {
    ctx.require_project()?;
    match binding(ctx) {
        Some(binding) => {
            println!(
                "linked: {} ({}) since {}",
                binding.get("repo").and_then(|v| v.as_str()).unwrap_or("?"),
                binding
                    .get("layout")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?"),
                binding
                    .get("linked_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?"),
            );
            println!("remote: {}", remote_url(ctx, &binding));
            let sync_state = local_store::root(&ctx.cwd).join("local/sync-state.json");
            if let Ok(text) = std::fs::read_to_string(sync_state) {
                if let Ok(state) = serde_json::from_str::<Value>(&text) {
                    println!(
                        "last sync: {}",
                        state
                            .get("last_sync_at")
                            .and_then(|v| v.as_str())
                            .unwrap_or("never")
                    );
                }
            } else {
                println!("last sync: never");
            }
        }
        None => {
            println!("not linked — `stateroot repo link <owner/repo>`");
            note!("(sync works against any git remote the manifest points at)");
        }
    }
    Ok(())
}
