//! `stateroot remove` — remove a stateroot project: the `.stateroot/` tree,
//! the `projects.toml` registry entry, the `init`-installed convenience
//! layer, and our git plumbing refs (`refs/stateroot/*`). Dual-mode: the
//! local removal always happens; the server-side deletion runs only when
//! the project is linked to a server id AND the user is logged in AND the
//! cloud preview gate is on AND a cloud base_url is configured.
//!
//! Safety model (ported from the monorepo): destructive actions require
//! `--yes`, an interactive confirmation (default NO), or are previewed with
//! `--dry-run`. User files and machine-level installs are never touched.
//! Stub files are deleted only when byte-identical to the bundled asset
//! (modified = kept with a note). AGENTS.md keeps foreign content — the
//! marked block is excised, the file deleted only when block-only.

use std::path::{Path, PathBuf};

use serde_json::Value;
use stateroot_core::config::{self, ProjectEntry};
use stateroot_core::local_store;

use super::{auth as gh, blocks, note, skill, stdin_is_tty, Ctx};

/// What to do with AGENTS.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentsMdAction {
    /// The marked block is the file's entire content (init-created): delete.
    DeleteFile,
    /// Mixed content: excise the block, keep the file.
    RemoveBlock,
    /// No stateroot block present: leave untouched.
    NoBlock,
}

/// What to do with a convenience stub file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StubAction {
    /// Byte-identical to the bundled asset: delete.
    Delete,
    /// Modified since install: keep (note it).
    KeepModified,
}

/// The full removal plan (computed before any write).
struct Plan {
    project_dir: PathBuf,
    entry: ProjectEntry,
    stateroot_dir: bool,
    agents_md: AgentsMdAction,
    stubs: Vec<(PathBuf, StubAction)>,
    registered: bool,
    /// Refs under refs/stateroot/* present in the project's repo.
    stateroot_refs: Vec<String>,
    /// Server deletion applies (all gates on) and is not skipped.
    server: bool,
    /// The server path would apply but --keep-server-state skipped it.
    server_kept: bool,
}

/// Resolve the project for removal: walk up from cwd for `.stateroot/`
/// (manifest optional — partial artifacts still need removal), then fall
/// back to the registry entry for cwd.
fn resolve_project(ctx: &Ctx) -> anyhow::Result<(PathBuf, ProjectEntry)> {
    let mut dir = Some(ctx.cwd.as_path());
    while let Some(d) = dir {
        if d.join(".stateroot").is_dir() {
            let manifest = local_store::read_manifest(d).ok().flatten();
            let manifest_id = manifest
                .as_ref()
                .and_then(|m| m.get("project_id"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let registered =
                config::lookup_project(&ctx.config_dir, d).map_err(|e| anyhow::anyhow!(e))?;
            let project_id = if !manifest_id.is_empty() {
                manifest_id
            } else {
                registered
                    .as_ref()
                    .map(|e| e.project_id.clone())
                    .unwrap_or_default()
            };
            let entry = registered.unwrap_or_else(|| ProjectEntry {
                workspace_id: project_id.clone(),
                name: manifest
                    .as_ref()
                    .and_then(|m| m.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                project_id,
                ..Default::default()
            });
            return Ok((d.to_path_buf(), entry));
        }
        dir = d.parent();
    }
    if let Some(entry) =
        config::lookup_project(&ctx.config_dir, &ctx.cwd).map_err(|e| anyhow::anyhow!(e))?
    {
        return Ok((ctx.cwd.clone(), entry));
    }
    anyhow::bail!(
        "not a stateroot project (no .stateroot/ here or above, no registry entry) — nothing to remove"
    )
}

fn agents_md_action(project_dir: &Path) -> AgentsMdAction {
    let path = project_dir.join("AGENTS.md");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return AgentsMdAction::NoBlock;
    };
    let Some(begin) = text.find(blocks::BLOCK_BEGIN) else {
        return AgentsMdAction::NoBlock;
    };
    let Some(end) = text
        .find(blocks::BLOCK_END)
        .map(|e| e + blocks::BLOCK_END.len())
    else {
        // Malformed (begin without end): never clobber.
        return AgentsMdAction::NoBlock;
    };
    if text[..begin].trim().is_empty() && text[end..].trim().is_empty() {
        AgentsMdAction::DeleteFile
    } else {
        AgentsMdAction::RemoveBlock
    }
}

fn stub_action(path: &Path, asset: Option<&[u8]>) -> Option<StubAction> {
    if !path.is_file() {
        return None;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Some(StubAction::KeepModified);
    };
    if asset == Some(bytes.as_slice()) {
        Some(StubAction::Delete)
    } else {
        Some(StubAction::KeepModified)
    }
}

fn collect_stateroot_refs(project_dir: &Path) -> Vec<String> {
    if !project_dir.join(".git").exists() {
        return Vec::new();
    }
    let Ok(repo) = git2::Repository::open(project_dir) else {
        return Vec::new();
    };
    let mut refs = Vec::new();
    for prefix in [
        stateroot_core::roots::ROOTS_REF_PREFIX,
        stateroot_core::roots::FORKS_REF_PREFIX,
    ] {
        if let Ok(iter) = repo.references_glob(&format!("{prefix}*")) {
            for reference in iter.flatten() {
                if let Some(name) = reference.name() {
                    refs.push(name.to_string());
                }
            }
        }
    }
    if repo
        .refname_to_id(stateroot_core::roots::LATEST_REF)
        .is_ok()
    {
        refs.push(stateroot_core::roots::LATEST_REF.to_string());
    }
    refs.sort();
    refs
}

/// Server deletion applies only when every gate is on (corrected scope):
/// known project id, logged-in credential, cloud preview, configured
/// base_url. Otherwise the removal is silently local-only.
fn server_applies(ctx: &Ctx, entry: &ProjectEntry, keep_server_state: bool) -> (bool, bool) {
    if entry.project_id.is_empty() {
        return (false, false);
    }
    let would_apply = gh::github_token(ctx).is_some()
        && super::cloud_preview_enabled(ctx)
        && !super::cloud::base_url(ctx).trim().is_empty();
    if !would_apply {
        return (false, false);
    }
    (!keep_server_state, keep_server_state)
}

fn build_plan(ctx: &Ctx, keep_server_state: bool) -> anyhow::Result<Plan> {
    let (project_dir, entry) = resolve_project(ctx)?;
    let stateroot_dir = local_store::root(&project_dir).is_dir();
    let stubs = [
        (
            project_dir.join(".claude/commands/stateroot.md"),
            skill::convenience_asset("assets/claude-command.md"),
        ),
        (
            project_dir.join(".cursor/rules/stateroot.mdc"),
            skill::convenience_asset("assets/cursor-rule.mdc"),
        ),
    ]
    .into_iter()
    .filter_map(|(path, asset)| stub_action(&path, asset).map(|action| (path, action)))
    .collect();
    let registered = config::lookup_project(&ctx.config_dir, &project_dir)
        .map_err(|e| anyhow::anyhow!(e))?
        .is_some();
    let (server, server_kept) = server_applies(ctx, &entry, keep_server_state);
    let stateroot_refs = collect_stateroot_refs(&project_dir);
    let agents_md = agents_md_action(&project_dir);
    Ok(Plan {
        stateroot_refs,
        project_dir,
        entry,
        stateroot_dir,
        agents_md,
        stubs,
        registered,
        server,
        server_kept,
    })
}

fn print_plan(plan: &Plan) {
    let name = if plan.entry.name.is_empty() {
        plan.entry.project_id.as_str()
    } else {
        plan.entry.name.as_str()
    };
    println!("stateroot remove — plan");
    if plan.entry.project_id.is_empty() {
        println!("  project: (unregistered .stateroot/ artifact — no manifest)");
    } else {
        println!("  project: {name} ({})", plan.entry.project_id);
    }
    println!("  directory: {}", plan.project_dir.display());
    if plan.stateroot_dir {
        println!("  - delete .stateroot/ (recursive)");
    }
    if !plan.stateroot_refs.is_empty() {
        println!(
            "  - delete {} git ref(s) under refs/stateroot/ (roots, forks, latest — your branches are never touched)",
            plan.stateroot_refs.len()
        );
    }
    match plan.agents_md {
        AgentsMdAction::DeleteFile => {
            println!("  - delete AGENTS.md (contains only the stateroot block)")
        }
        AgentsMdAction::RemoveBlock => {
            println!("  - remove the stateroot block from AGENTS.md (file kept)")
        }
        AgentsMdAction::NoBlock => {}
    }
    for (path, action) in &plan.stubs {
        match action {
            StubAction::Delete => println!("  - delete {}", path.display()),
            StubAction::KeepModified => {
                println!("  - keep {} (modified since install)", path.display())
            }
        }
    }
    if plan.registered {
        println!("  - unregister from projects.toml");
    }
    if plan.server {
        println!(
            "  - server: DELETE /stateroot/projects/{}?confirm=true",
            plan.entry.project_id
        );
    } else if plan.server_kept {
        println!("  - server: kept (--keep-server-state)");
    }
    // Gates off: no mention of servers at all (the coming-soon story).
}

/// Run `stateroot remove`.
pub async fn run(
    ctx: &Ctx,
    keep_server_state: bool,
    yes: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let plan = build_plan(ctx, keep_server_state)?;

    if dry_run {
        print_plan(&plan);
        println!("dry-run — nothing was touched");
        return Ok(());
    }

    if !yes {
        print_plan(&plan);
        if !stdin_is_tty() {
            anyhow::bail!(
                "refusing to remove without confirmation (non-interactive) — re-run with --yes to proceed or --dry-run to preview"
            );
        }
        let proceed = dialoguer::Confirm::new()
            .with_prompt("Proceed with removal?")
            .default(false)
            .interact()?;
        if !proceed {
            println!("aborted — nothing removed");
            return Ok(());
        }
    }

    // --- local removal ---
    if plan.stateroot_dir {
        let root = local_store::root(&plan.project_dir);
        std::fs::remove_dir_all(&root)?;
        println!("  deleted {}", root.display());
    }

    // --- git refs (our lineage; the user's branches are never touched) ---
    if !plan.stateroot_refs.is_empty() {
        if let Ok(repo) = git2::Repository::open(&plan.project_dir) {
            let mut removed = 0usize;
            for name in &plan.stateroot_refs {
                if let Ok(mut reference) = repo.find_reference(name) {
                    if reference.delete().is_ok() {
                        removed += 1;
                    }
                }
            }
            println!("  deleted {} git ref(s) under refs/stateroot/", removed);
        }
    }

    let agents_md = plan.project_dir.join("AGENTS.md");
    match plan.agents_md {
        AgentsMdAction::DeleteFile => {
            std::fs::remove_file(&agents_md)?;
            println!(
                "  deleted {} (only contained the stateroot block)",
                agents_md.display()
            );
        }
        AgentsMdAction::RemoveBlock => {
            if blocks::remove_marked_block(&agents_md)? {
                println!("  removed the stateroot block from {}", agents_md.display());
            }
        }
        AgentsMdAction::NoBlock => {}
    }

    for (path, action) in &plan.stubs {
        match action {
            StubAction::Delete => {
                std::fs::remove_file(path)?;
                println!("  deleted {}", path.display());
            }
            StubAction::KeepModified => {
                println!(
                    "  kept {} (modified since install — delete manually if unwanted)",
                    path.display()
                );
            }
        }
    }

    if plan.registered
        && config::unregister_project(&ctx.config_dir, &plan.project_dir)
            .map_err(|e| anyhow::anyhow!(e))?
    {
        println!("  unregistered from projects.toml");
    }

    // --- server removal (gated; failure never blocks the local half) ---
    if plan.server {
        remove_server_state(ctx, &plan).await;
    } else if plan.server_kept {
        println!("  server: kept (--keep-server-state)");
    }

    println!("removed project {}", plan.entry.project_id);
    Ok(())
}

/// Server-side deletion; failures are warnings, not fatal — the local half
/// is already done by the time this runs.
async fn remove_server_state(ctx: &Ctx, plan: &Plan) {
    let Some(token) = gh::github_token(ctx) else {
        return; // gate guarantees a token, but stay honest
    };
    let base = super::cloud::base_url(ctx);
    let url = format!(
        "{base}/stateroot/projects/{}?confirm=true",
        plan.entry.project_id
    );
    let result = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| anyhow::anyhow!(e))
        .unwrap_or_else(|_| reqwest::Client::new())
        .delete(&url)
        .bearer_auth(token)
        .header("User-Agent", "stateroot-cli")
        .send()
        .await;
    match result {
        Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
            println!("  server: already clean (404 — nothing to delete)");
        }
        Ok(resp) if resp.status().is_success() => {
            println!("  server: project deleted");
            if let Ok(body) = resp.json::<Value>().await {
                let data = body.get("data").cloned().unwrap_or(body);
                if let Some(tables) = data
                    .get("deleted")
                    .and_then(|d| d.get("tables"))
                    .and_then(|t| t.as_object())
                {
                    for (table, count) in tables {
                        println!("    {table}: {}", fmt_count(count));
                    }
                }
                if let Some(fs) = data.get("deleted").and_then(|d| d.get("filesystem")) {
                    if !fs.is_null() {
                        println!("    filesystem: {}", fmt_count(fs));
                    }
                }
            }
        }
        Ok(resp) => {
            note!("warning: server deletion returned HTTP {} (server state may remain)", resp.status());
        }
        Err(err) => note!("warning: server unreachable — local removal is complete; server state may remain ({err})"),
    }
}

/// Compact rendering for one count value (number, or raw JSON otherwise).
fn fmt_count(value: &Value) -> String {
    match value {
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}
