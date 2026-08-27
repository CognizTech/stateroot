//! `stateroot install` / `stateroot uninstall` — CLI wrapper over
//! `stateroot_core::harness_install` (the machinery moved into the core
//! crate so other frontends, e.g. a GUI setup app, can share it).
//!
//! Home resolution: `STATEROOT_TEST_HOME` wins (tests), otherwise `$HOME`.
//! Every write is either an idempotent marked block or a read-merge-write
//! JSON merge with a `.bak` backup — foreign config is never clobbered.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Result;
use include_dir::{include_dir, Dir};
use stateroot_core::harness_install::{self as core, SkillBundle};

use super::{note, Ctx};

#[allow(unused_imports)]
pub use stateroot_core::harness_install::{
    all_specs, home_dir, spec_exists, HarnessSpec, InstallToggles, ENV_TEST_HOME,
};

static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets/stateroot-skill");

/// The one-agent block template (a CLI-managed asset, intentionally outside
/// the drift-guarded canonical skill bundle).
const ONE_AGENT_BLOCK_TEMPLATE: &str = include_str!("../../assets/one-agent-block.md");

/// Project-local AGENTS.md block: portable protocol without a session-start
/// resume instruction (resume lives only on harness-specific surfaces).
const AGENTS_BLOCK_TEMPLATE: &str =
    include_str!("../../assets/stateroot-skill/assets/agents-block.md");

/// Render the one-agent block with an optional persona section and a
/// harness-specific resume command (`stateroot resume --harness <id>`).
pub fn render_one_agent_block(persona: Option<&str>, harness_id: &str) -> String {
    let persona_section = match persona {
        Some(p) if !p.trim().is_empty() => {
            let body = p.trim();
            if body.to_ascii_lowercase().contains("working relationship") {
                body.to_string()
            } else {
                format!("### Working relationship\n\n{body}")
            }
        }
        _ => "_(no working relationship synced yet — run `stateroot persona sync`)_".to_string(),
    };
    let resume = super::harness_display::resume_command(harness_id);
    let current_harness = super::harness_display::normalize(harness_id);
    ONE_AGENT_BLOCK_TEMPLATE
        .replace("{{PERSONA}}", &persona_section)
        .replace("{{RESUME_CMD}}", &resume)
        .replace("{{CURRENT_HARNESS}}", &current_harness)
}

/// Project AGENTS.md convenience block — checkpoint/handoff rules only.
pub fn render_project_agents_block() -> String {
    AGENTS_BLOCK_TEMPLATE.to_string()
}

/// The embedded skill bundle, converted once into the core `SkillBundle`
/// shape (paths relative to the bundle root).
fn bundle() -> &'static SkillBundle {
    static BUNDLE: OnceLock<SkillBundle> = OnceLock::new();
    BUNDLE.get_or_init(|| {
        let mut files = Vec::new();
        collect_dir(&ASSETS, Path::new(""), &mut files);
        let claude_command_md = ASSETS
            .get_file("assets/claude-command.md")
            .map(|f| f.contents().to_vec());
        SkillBundle {
            files,
            claude_command_md,
        }
    })
}

fn collect_dir(dir: &Dir<'_>, prefix: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(subdir) => {
                let name = subdir
                    .path()
                    .file_name()
                    .unwrap_or_else(|| subdir.path().as_os_str());
                collect_dir(subdir, &prefix.join(name), out);
            }
            include_dir::DirEntry::File(file) => {
                let name = file
                    .path()
                    .file_name()
                    .unwrap_or_else(|| file.path().as_os_str());
                out.push((prefix.join(name), file.contents().to_vec()));
            }
        }
    }
}

/// Embedded product skill files for portable seeding (relative POSIX paths).
pub fn product_skill_files() -> Vec<(String, Vec<u8>)> {
    bundle()
        .files
        .iter()
        .map(|(path, bytes)| (path.to_string_lossy().replace('\\', "/"), bytes.clone()))
        .collect()
}

/// Seed/update `~/.stateroot/skills/stateroot` from the embedded product bundle.
pub fn seed_product_skill(home: &Path) -> Result<stateroot_core::skill_federation::SyncAction> {
    let files = product_skill_files();
    stateroot_core::skill_federation::ensure_product_skill_package(home, &files)
        .map_err(|err| anyhow::anyhow!(err))
}

/// Install one harness spec with the CLI's embedded bundle (signature kept
/// stable for the setup wizard).
pub(crate) fn install_spec(
    home: &Path,
    spec: &HarnessSpec,
    block: &str,
    toggles: InstallToggles,
) -> Vec<String> {
    core::install_spec(home, spec, block, toggles, Some(bundle()))
}

/// `stateroot install` — machine-level integration.
pub async fn install(ctx: &Ctx) -> Result<()> {
    let home = home_dir()?;
    let specs: Vec<HarnessSpec> = all_specs(&home)
        .into_iter()
        .filter(|spec| spec_exists(&home, spec.id))
        .collect();
    if specs.is_empty() {
        println!(
            "no harness roots found under {} — nothing to do",
            home.display()
        );
        return Ok(());
    }

    // Persona first: the one-agent block embeds it. Resume command is
    // harness-specific so each integration surface invokes resume once with
    // the correct `--harness` id.
    let persona = super::persona::sync_best_effort(ctx).await;

    let mut installed: Vec<String> = Vec::new();
    println!("Installing stateroot globally (home: {}):", home.display());
    for spec in &specs {
        // Wave-2: each harness gets its own projection of the same soul.
        let persona_h = super::persona::for_harness(ctx, spec.id, persona.as_deref()).await;
        let block = render_one_agent_block(persona_h.as_deref(), spec.id);
        let actions = install_spec(&home, spec, &block, InstallToggles::default());
        for action in &actions {
            println!("  {}: {action}", spec.id);
        }
        if actions.is_empty() {
            println!("  {}: detected", spec.id);
        }
        if let Some(guidance) = spec.guidance {
            println!("  note: {guidance}");
        }
        installed.push(spec.id.to_string());
    }

    // Second pass: non-legacy registry rows, grouped by tier.
    install_registry_tiers(ctx, &home, persona.as_deref(), &mut installed).await;

    match seed_product_skill(&home) {
        Ok(action) => println!("  product skill: {} — {}", action.action, action.detail),
        Err(err) => note!("warning: product skill seed failed ({err:#})"),
    }
    if let Err(err) = stateroot_core::skill_federation::refresh_product_projections(&home, None) {
        note!("warning: product projection refresh failed ({err})");
    }
    match stateroot_core::rules::sync(&ctx.cwd, &home) {
        Ok(report) => println!(
            "  rules: product-intent {} · imported {}",
            if report.seeded { "seeded" } else { "current" },
            report.imported
        ),
        Err(err) => note!("warning: rules sync failed ({err})"),
    }

    // Re-arm project convenience layers (AGENTS.md block, harness command/rule
    // stubs) for every registered project still on disk. Init writes them once
    // and the protocol text evolves — self-update re-arms install, and install
    // keeps the project stubs current with it.
    if let Ok(registry) = stateroot_core::config::load_registry(&ctx.config_dir) {
        let block = render_project_agents_block();
        for path in registry.projects.keys() {
            let dir = Path::new(path);
            if !dir.is_dir() {
                continue;
            }
            let actions = super::skill::ensure_convenience_layer(dir, &block);
            for action in actions {
                println!("  {}: {action}", path);
            }
            // Collaboration defaults reach existing projects too
            // (write-if-absent; user edits win).
            let root = stateroot_core::local_store::root(dir);
            let mut created = Vec::new();
            if let Err(err) = stateroot_core::local_store::ensure_collab_files(&root, &mut created)
            {
                note!("warning: collab files failed for {}: {err}", root.display());
            }
            for item in created {
                println!("  {}: {item}", path);
            }
        }
    }

    // Record for `init`'s one-time global install + `uninstall`.
    let mut config = ctx.config.clone();
    config.installed_harnesses = installed.clone();
    stateroot_core::config::save_config(&ctx.config_dir, &config)?;
    println!();
    println!("Installed for: {}", installed.join(", "));
    Ok(())
}

/// Refresh global harness instruction blocks after soul/persona changes.
pub(crate) fn refresh_global_instruction_blocks(config_dir: &Path, home: &Path) {
    let persona = super::persona::resolve_for_harness(config_dir, None);
    let Some(persona) = persona else {
        return;
    };
    for spec in all_specs(home)
        .into_iter()
        .filter(|spec| spec_exists(home, spec.id))
    {
        let block = render_one_agent_block(Some(&persona), spec.id);
        let _ = install_spec(home, &spec, &block, InstallToggles::default());
    }
    use stateroot_core::harness_install::registry::{adapters, quirk_detected};
    let legacy: Vec<&str> = adapters().iter().filter_map(|q| q.legacy_id).collect();
    for quirk in adapters()
        .iter()
        .filter(|q| !legacy.contains(&q.id))
        .filter(|q| quirk_detected(home, q))
    {
        let block = render_one_agent_block(Some(&persona), quirk.id);
        let _ = stateroot_core::harness_install::install_quirk_full(home, quirk, &block);
    }
}

/// Install detected non-legacy registry harnesses via `install_quirk_full`
/// (instruction block + MCP + tier installer: Tier A hooks, Tier B plugin,
/// Tier C MCP, managed placeholders), printing tier-grouped output.
async fn install_registry_tiers(
    ctx: &Ctx,
    home: &Path,
    persona: Option<&str>,
    installed: &mut Vec<String>,
) {
    use stateroot_core::harness_install::registry::{adapters, quirk_detected, Tier};

    let legacy: Vec<&str> = adapters().iter().filter_map(|q| q.legacy_id).collect();
    let rows: Vec<_> = adapters()
        .iter()
        .filter(|q| !legacy.contains(&q.id))
        .filter(|q| quirk_detected(home, q))
        .collect();
    if rows.is_empty() {
        return;
    }
    for (tier, label) in [
        (Tier::A, "tier A (native hooks)"),
        (Tier::B, "tier B (generated plugin)"),
        (Tier::C, "tier C (MCP / managed)"),
    ] {
        let group: Vec<_> = rows.iter().filter(|q| q.tier == tier).collect();
        if group.is_empty() {
            continue;
        }
        println!("  {label}:");
        for quirk in group {
            let persona_h = super::persona::for_harness(ctx, quirk.id, persona).await;
            let block = render_one_agent_block(persona_h.as_deref(), quirk.id);
            for action in stateroot_core::harness_install::install_quirk_full(home, quirk, &block) {
                println!("    {}: {action}", quirk.id);
            }
            if quirk.id == "pi" {
                println!(
                    "    pi: launch with `stateroot harness run pi` to isolate it from shared .agents skills"
                );
            }
            installed.push(quirk.id.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::render_one_agent_block;

    #[test]
    fn one_agent_block_uses_working_relationship() {
        let block = render_one_agent_block(
            Some("## Working relationship\n\nBe direct. Disagree once with evidence.\n"),
            "codex",
        );
        assert!(
            block.contains("### Working relationship") || block.contains("## Working relationship")
        );
        assert!(block.contains("Be direct."));
        assert!(!block.contains("### Persona\n"));
        assert!(block.contains("stateroot resume --harness codex"));
        assert!(block.contains("stateroot handoff write --from codex"));
        assert!(block.contains("auto-injected") || block.contains("Never run resume twice"));
        assert!(
            block.contains("head") && block.contains("line"),
            "one-agent block must forbid truncating resume: {block}"
        );
        assert!(!block.contains("{{RESUME_CMD}}"));
        assert!(!block.contains("{{CURRENT_HARNESS}}"));
    }

    #[test]
    fn one_agent_block_is_harness_specific() {
        let cursor = render_one_agent_block(None, "cursor");
        let codex = render_one_agent_block(None, "codex");
        assert!(cursor.contains("stateroot resume --harness cursor"));
        assert!(codex.contains("stateroot resume --harness codex"));
        assert!(!cursor.contains("stateroot resume --harness codex"));
    }
}
