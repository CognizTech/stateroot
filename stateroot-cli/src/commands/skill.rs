//! `stateroot skill …` — project-level convenience layer, skill listing and
//! inspection.
//!
//! v1.1 semantics: the project-level layer is exactly three files — the
//! AGENTS.md marked block, `.claude/commands/stateroot.md`, and
//! `.cursor/rules/stateroot.mdc` — always installed by `init`. Machine-level
//! harness integration lives in `commands/install.rs`.

use std::path::Path;

use anyhow::{anyhow, Context as _, Result};
use include_dir::{include_dir, Dir};

use super::blocks::ensure_marked_block;
use super::{note, Ctx};

fn home(ctx: &Ctx) -> Result<std::path::PathBuf> {
    let _ = ctx;
    stateroot_core::harness_install::home_dir().map_err(|e| anyhow!(e))
}

/// The bundled skill assets, embedded at compile time.
static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets/stateroot-skill");

fn embedded_file(rel: &str) -> Result<&'static [u8]> {
    ASSETS
        .get_file(rel)
        .map(|f| f.contents())
        .ok_or_else(|| anyhow!("embedded skill asset missing: {rel}"))
}

/// Bytes of an embedded convenience asset (`assets/claude-command.md`,
/// `assets/cursor-rule.mdc`) — used by `remove`'s ours-check and its tests.
#[allow(dead_code)]
pub fn convenience_asset(rel: &str) -> Option<&'static [u8]> {
    ASSETS.get_file(rel).map(|f| f.contents())
}

fn write_file(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Install the project-level convenience layer into `dir`:
/// AGENTS.md marked block (protocol only — persona is global), `.claude/commands/stateroot.md`,
/// `.cursor/rules/stateroot.mdc`. Always all three (v1.1); returns
/// human-readable action descriptions.
pub fn ensure_convenience_layer(dir: &Path, block_content: &str) -> Vec<String> {
    let mut actions = Vec::new();
    match ensure_marked_block(&dir.join("AGENTS.md"), block_content) {
        Ok(true) => actions.push("AGENTS.md block updated".to_string()),
        Ok(false) => actions.push("AGENTS.md block already up to date".to_string()),
        Err(err) => note!("  ! AGENTS.md block failed: {err:#}"),
    }
    match write_file(
        &dir.join(".claude/commands/stateroot.md"),
        embedded_file("assets/claude-command.md").unwrap_or(b"# stateroot\n"),
    ) {
        Ok(()) => actions.push(".claude/commands/stateroot.md".to_string()),
        Err(err) => note!("  ! claude command stub failed: {err:#}"),
    }
    match write_file(
        &dir.join(".cursor/rules/stateroot.mdc"),
        embedded_file("assets/cursor-rule.mdc").unwrap_or(b"# stateroot\n"),
    ) {
        Ok(()) => actions.push(".cursor/rules/stateroot.mdc".to_string()),
        Err(err) => note!("  ! cursor rule failed: {err:#}"),
    }
    actions
}

/// `stateroot skill install` — (re)materialize the convenience layer in cwd.
pub fn install(ctx: &Ctx) -> Result<()> {
    let block = super::install::render_project_agents_block();
    let actions = ensure_convenience_layer(&ctx.cwd, &block);
    for action in &actions {
        println!("  - {action}");
    }
    println!("Convenience layer installed ({} files).", actions.len());
    Ok(())
}

/// `stateroot skill list` — pooled native + portable + delegated capabilities.
pub async fn list(ctx: &Ctx) -> Result<()> {
    let pooled = stateroot_core::skill_federation::discover_all(&ctx.cwd, None)
        .map_err(|err| anyhow!(err))?;
    if !pooled.is_empty() {
        println!("Federated skills (native origins + portable packages):");
        for skill in &pooled {
            let route = match skill.lifecycle.as_str() {
                "reference_only" => format!("delegate → {}", skill.native_harness),
                "external_only" => format!("external-only → {}", skill.native_harness),
                _ => format!("portable from {}", skill.harness),
            };
            // Wave-2 scope ladder surface: scope / lifecycle / visibility.
            let mut badges = vec![skill.scope.clone(), skill.lifecycle.clone()];
            if !skill.visibility.is_empty() {
                badges.push(skill.visibility.clone());
            }
            let badges = badges.join("; ");
            if skill.description.is_empty() {
                println!("  {} [{}; {}]", skill.slug, badges, route);
            } else {
                println!(
                    "  {} — {} [{}; {}]",
                    skill.slug,
                    super::truncate(&skill.description, 100),
                    badges,
                    route
                );
            }
        }
        return Ok(());
    }
    println!("no skills discovered — `stateroot skill scan` lists harness roots");
    Ok(())
}

/// `stateroot skill show <slug>` — print the local SKILL.md, or fetch it
/// from the server (slug → detail → skill-md).
pub async fn show(ctx: &Ctx, slug: &str) -> Result<()> {
    if let Some(text) = stateroot_core::local_store::read_local_skill(&ctx.cwd, slug) {
        print!("{text}");
        return Ok(());
    }
    if let Ok(found) = stateroot_core::skill_federation::discover_all(&ctx.cwd, None) {
        if let Some(skill) = found.iter().find(|skill| {
            skill.slug.eq_ignore_ascii_case(slug) || skill.name.eq_ignore_ascii_case(slug)
        }) {
            if skill.lifecycle == "reference_only" || skill.lifecycle == "external_only" {
                println!("# {} via {}", skill.name, skill.native_harness);
                println!();
                println!("lifecycle: {}", skill.lifecycle);
                println!("source harness: {}", skill.harness);
                println!("invocation: {}", skill.native_invocation);
                if let Some(reasons) = skill
                    .compatibility
                    .get("reasons")
                    .and_then(|value| value.as_array())
                {
                    for reason in reasons.iter().filter_map(|value| value.as_str()) {
                        println!("requirement: {reason}");
                    }
                }
                return Ok(());
            }
            let source = Path::new(&skill.source_path);
            for name in ["SKILL.md", "skill.md"] {
                let path = source.join(name);
                if path.is_file() {
                    print!("{}", std::fs::read_to_string(&path)?);
                    return Ok(());
                }
            }
        }
    }
    anyhow::bail!("skill '{slug}' not found locally");
}

/// `stateroot skill promote <slug>` — activate a skill package and project
/// it to installed harnesses. Optional audit proposal is recorded, not gated.
pub async fn promote(ctx: &Ctx, slug: &str, rationale: Option<&str>) -> Result<()> {
    ctx.require_project()?;
    let home = home(ctx)?;
    let scope = if stateroot_core::skill_federation::activate_skill(
        &ctx.cwd,
        &home,
        "project",
        slug,
    )
    .map_err(|e| anyhow!(e))?
    {
        "project"
    } else if stateroot_core::skill_federation::activate_skill(&ctx.cwd, &home, "user", slug)
        .map_err(|e| anyhow!(e))?
    {
        "user"
    } else {
        anyhow::bail!("skill '{slug}' not found in project or user store");
    };
    let options = stateroot_core::skill_federation::SyncOptions {
        dry_run: false,
        push: true,
        pull: false,
        cmd_probe: None,
    };
    let _ = stateroot_core::skill_federation::sync_project(&ctx.cwd, &options, None);
    let _ = stateroot_core::proposals::create(
        &ctx.cwd,
        "skill",
        &format!("activate skill {slug}"),
        rationale.unwrap_or("stateroot skill promote"),
        serde_json::json!({"slug": slug, "scope": scope}),
        serde_json::json!({"route": "skill promote", "status": "active"}),
    );
    println!("skill '{slug}' activated and projected ({scope})");
    Ok(())
}

/// `stateroot skill scan` — discover packages across all registered harnesses.
pub fn scan(ctx: &Ctx, json: bool) -> Result<()> {
    let found =
        stateroot_core::skill_federation::discover_all(&ctx.cwd, None).map_err(|e| anyhow!(e))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&found)?);
        return Ok(());
    }
    if found.is_empty() {
        println!("no skills discovered");
        return Ok(());
    }
    println!("Discovered {} skill package(s):", found.len());
    for skill in &found {
        let marker = if skill.lifecycle == "reference_only" {
            " [reference-only]"
        } else {
            ""
        };
        println!(
            "  {} @ {} ({}) digest={}{}",
            skill.slug,
            skill.harness,
            skill.scope,
            &skill.package_digest[..8.min(skill.package_digest.len())],
            marker
        );
    }
    Ok(())
}

/// `stateroot skill sync` — federate project+global packages in a project,
/// or global packages only when invoked from a machine-global directory.
pub async fn sync(ctx: &Ctx, dry_run: bool, pull: bool, push: bool) -> Result<()> {
    let options = stateroot_core::skill_federation::SyncOptions {
        dry_run,
        push,
        pull: pull || !push,
        cmd_probe: None,
    };
    let project_scoped = stateroot_core::local_store::is_stateroot_dir(&ctx.cwd);
    let home = stateroot_core::harness_install::home_dir().map_err(|err| anyhow!(err))?;
    if !dry_run {
        match super::install::seed_product_skill(&home) {
            Ok(action) => println!("  [{}] {} — {}", action.action, action.slug, action.detail),
            Err(err) => note!("warning: product skill seed failed ({err:#})"),
        }
    }
    let actions = if project_scoped {
        stateroot_core::skill_federation::sync_project(&ctx.cwd, &options, Some(&home))
    } else {
        stateroot_core::skill_federation::sync_global(&home, &options)
    }
    .map_err(|e| anyhow!(e))?;
    for action in &actions {
        println!("  [{}] {} — {}", action.action, action.slug, action.detail);
    }
    println!("{} action(s).", actions.len());

    Ok(())
}

/// `stateroot skill status`
pub fn status(ctx: &Ctx, json: bool) -> Result<()> {
    let report =
        stateroot_core::skill_federation::status_report(&ctx.cwd, None).map_err(|e| anyhow!(e))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "discovered={} portable={} (global={}, project={}) reference_only={} external_only={}",
            report
                .get("discovered")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            report.get("portable").and_then(|v| v.as_u64()).unwrap_or(0),
            report
                .get("portable_global")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            report
                .get("portable_project")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            report
                .get("reference_only")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            report
                .get("external_only")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        );
    }
    Ok(())
}

/// `stateroot skill doctor`
pub fn doctor(ctx: &Ctx) -> Result<()> {
    let notes = stateroot_core::skill_federation::doctor(&ctx.cwd, None).map_err(|e| anyhow!(e))?;
    for note in notes {
        println!("{note}");
    }
    Ok(())
}
