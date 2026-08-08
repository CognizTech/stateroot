//! `stateroot init` — initialize a project locally: `.stateroot/` skeleton,
//! project registry entry, product skill seed + projections, convenience
//! layer. No server registration anywhere (there is none).
//!
//! M2 hook point: git-root snapshots attach here (`git init` for non-git
//! folders, plumbing-only roots under `refs/stateroot/`).

use std::path::Path;

use anyhow::Result;
use stateroot_core::{config as core_config, local_store};

use super::{note, Ctx};

/// Run `stateroot init [DIR|--path DIR] [--name NAME]`.
pub async fn run(ctx: &Ctx, args: crate::cli::InitArgs) -> Result<()> {
    let dir = match (args.dir, args.path) {
        (Some(d), None) => Path::new(&d).to_path_buf(),
        (None, Some(p)) => Path::new(&p).to_path_buf(),
        (None, None) => ctx.cwd.clone(),
        (Some(_), Some(_)) => anyhow::bail!("pass either DIR or --path, not both"),
    };
    let dir = if dir.is_absolute() {
        dir
    } else {
        ctx.cwd.join(dir)
    };
    if !dir.is_dir() {
        anyhow::bail!("project directory does not exist: {}", dir.display());
    }
    if local_store::is_stateroot_dir(&dir) {
        println!("already a stateroot project (reusing manifest)");
    }
    let name = args.name.unwrap_or_else(|| {
        dir.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_string()
    });
    let project_id = {
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(dir.display().to_string().as_bytes());
        format!("local-{}", &format!("{:x}", hasher.finalize())[..12])
    };

    let created = local_store::init_skeleton(&dir, &project_id, &name, "local")
        .map_err(|e| anyhow::anyhow!(e))?;
    for path in &created {
        println!("  created {path}");
    }
    if created.is_empty() {
        println!("  layout already present");
    }

    // Project registry (projects.toml) so other directories resolve it.
    let entry = core_config::ProjectEntry {
        project_id: project_id.clone(),
        workspace_id: project_id.clone(),
        name: name.clone(),
        harnesses_installed: ctx.config.installed_harnesses.clone(),
        created_at: local_store::now_rfc3339(),
        ..Default::default()
    };
    core_config::register_project(&ctx.config_dir, &dir, entry).map_err(|e| anyhow::anyhow!(e))?;

    // Product skill + projections (fully local federation).
    let home = super::install::home_dir()?;
    match super::install::seed_product_skill(&home) {
        Ok(action) => println!("  product skill {} — {}", action.action, action.detail),
        Err(err) => note!("warning: product skill seed failed ({err:#})"),
    }
    if let Err(err) =
        stateroot_core::skill_federation::refresh_product_projections(&home, Some(&dir))
    {
        note!("warning: product projection refresh failed ({err})");
    }

    // Project-level convenience layer (AGENTS.md block + harness stubs).
    let block = super::install::render_project_agents_block();
    for action in super::skill::ensure_convenience_layer(&dir, &block) {
        println!("  {action}");
    }

    println!("initialized '{}' ({project_id}) at {}", name, dir.display());
    // M2: this is where `git init` + the first plumbing root attach.
    Ok(())
}
