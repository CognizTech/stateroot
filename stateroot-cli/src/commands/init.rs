//! `stateroot init` — initialize a project locally: `.stateroot/` skeleton,
//! project registry entry, product skill seed + projections, convenience
//! layer. No server registration anywhere (there is none).
//!
//! M2 hook point: git-root snapshots attach here (`git init` for non-git
//! folders, plumbing-only roots under `refs/stateroot/`).

use std::path::Path;

use anyhow::Result;
use stateroot_core::{config as core_config, local_store, soul, user_profile};

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

    for action in migrate_legacy_identity(&dir, &super::install::home_dir()?)? {
        println!("  {action}");
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

    // M2: non-git folders get a silent repo for plumbing roots (the user's
    // branch log is never touched — roots live under refs/stateroot/).
    let repo_existed = dir.join(".git").exists();
    match stateroot_core::roots::ensure_repo(&dir) {
        Ok(_) if !repo_existed => {
            println!("  git repo initialized (plumbing roots under refs/stateroot/)")
        }
        Ok(_) => println!("  git repo ready (plumbing roots under refs/stateroot/)"),
        Err(err) => note!("warning: could not prepare git repo ({err}); roots unavailable"),
    }

    println!("initialized '{}' ({project_id}) at {}", name, dir.display());
    Ok(())
}

const OLD_SOUL_PLACEHOLDER: &str =
    "# Soul\n\nProject tone, values, and non-negotiable constraints.";
const OLD_USER_PLACEHOLDER: &str =
    "# User Profile\n\nStable facts about the user that help agents collaborate.";

fn migrate_legacy_identity(project: &Path, home: &Path) -> Result<Vec<String>> {
    let root = local_store::root(project);
    let old_soul = root.join(local_store::SOUL_PATH);
    let old_user = root.join(local_store::USER_PROFILE_PATH);
    let mut actions = Vec::new();

    migrate_composed_openclaw_soul(home, &mut actions)?;

    if let Ok(text) = std::fs::read_to_string(&old_soul) {
        let trimmed = text.trim();
        if trimmed == OLD_SOUL_PLACEHOLDER || trimmed.is_empty() {
            std::fs::remove_file(&old_soul)?;
            actions.push("removed obsolete project soul placeholder".into());
        } else {
            let overlay = root.join("soul").join(soul::OVERLAY_FILE);
            if overlay.exists() {
                let overlay_text = std::fs::read_to_string(&overlay)?;
                if overlay_text.trim() == trimmed {
                    std::fs::remove_file(&old_soul)?;
                    actions.push("removed duplicate legacy project soul".into());
                } else {
                    let history = root.join("soul/history");
                    std::fs::create_dir_all(&history)?;
                    let stamp = local_store::now_rfc3339().replace([':', '-'], "");
                    let archive = unique_archive_path(&history, &stamp);
                    std::fs::rename(&old_soul, &archive)?;
                    actions.push(format!(
                        "warning: preserved conflicting legacy project soul as {} and removed active SOUL.md",
                        archive.display()
                    ));
                }
            } else {
                std::fs::rename(&old_soul, &overlay)?;
                actions.push(format!("migrated project soul to {}", overlay.display()));
            }
        }
    }

    if let Ok(text) = std::fs::read_to_string(&old_user) {
        let trimmed = text.trim();
        if trimmed == OLD_USER_PLACEHOLDER || trimmed.is_empty() {
            std::fs::remove_file(&old_user)?;
            actions.push("removed obsolete project user placeholder".into());
        } else if let Some(global) = user_profile::read(home) {
            if user_profile::payloads_equal(&global, trimmed) {
                std::fs::remove_file(&old_user)?;
                actions.push("removed duplicate project user profile".into());
            } else {
                let candidate = user_profile::write_import_candidate(
                    home,
                    trimmed,
                    &format!("project:{}", project.display()),
                )?;
                std::fs::remove_file(&old_user)?;
                actions.push(format!(
                    "warning: preserved conflicting project user as {} before removing project copy",
                    candidate.display()
                ));
            }
        } else {
            actions.push(user_profile::write(
                home,
                trimmed,
                Some(&format!("project:{}", project.display())),
            )?);
            std::fs::remove_file(&old_user)?;
            actions.push("removed migrated project user profile".into());
        }
    }
    Ok(actions)
}

fn migrate_composed_openclaw_soul(home: &Path, actions: &mut Vec<String>) -> Result<()> {
    let Some(canonical) = soul::read_canonical(home) else {
        return Ok(());
    };
    let Some((corrected, extracted_user)) = split_old_openclaw_composed(&canonical) else {
        return Ok(());
    };
    if let Some(global) = user_profile::read(home) {
        if !user_profile::payloads_equal(&global, &extracted_user) {
            let candidate = user_profile::write_import_candidate(
                home,
                &extracted_user,
                "legacy-openclaw-composed-soul",
            )?;
            actions.push(format!(
                "warning: preserved USER extracted from legacy composed soul as {}",
                candidate.display()
            ));
        }
    } else {
        actions.push(user_profile::write(
            home,
            &extracted_user,
            Some("legacy-openclaw-composed-soul"),
        )?);
    }
    actions.push(soul::write_canonical(
        home,
        &corrected,
        Some("openclaw-corrected"),
    )?);
    actions.push("removed human USER section from legacy composed soul".into());
    Ok(())
}

fn unique_archive_path(history: &Path, stamp: &str) -> std::path::PathBuf {
    let base = history.join(format!("{stamp}-legacy-SOUL.md"));
    if !base.exists() {
        return base;
    }
    for n in 1.. {
        let candidate = history.join(format!("{stamp}-legacy-SOUL-{n}.md"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

fn split_old_openclaw_composed(content: &str) -> Option<(String, String)> {
    let lines = content.lines().collect::<Vec<_>>();
    let start = lines
        .iter()
        .position(|line| line.trim() == "## User (USER.md)")?;
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.starts_with("## "))
        .map(|offset| start + 1 + offset)
        .unwrap_or(lines.len());
    if !lines.iter().any(|line| {
        matches!(
            line.trim(),
            "## Identity (IDENTITY.md)" | "## Persona (SOUL.md)"
        )
    }) {
        return None;
    }
    let user = lines[start + 1..end].join("\n").trim().to_string();
    if user.is_empty() {
        return None;
    }
    let mut kept = lines[..start].to_vec();
    kept.extend_from_slice(&lines[end..]);
    Some((kept.join("\n").trim().to_string(), user))
}
