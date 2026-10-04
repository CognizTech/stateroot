//! Dot is a registered caller of the normal StateRoot workflow, not a second store.
use std::{ffi::OsString, path::Path, process::Command};

use crate::cli::DotAction;
use anyhow::{bail, Context, Result};
use stateroot_core::{config, harness_install, local_store, skill_federation};

const SKILL: &[u8] = include_bytes!("../../assets/stateroot-dot/SKILL.md");

pub fn seed_skill(home: &Path) -> Result<()> {
    skill_federation::ensure_named_product_skill_package(
        home,
        "stateroot-dot",
        &[("SKILL.md".into(), SKILL.to_vec())],
    )
    .map_err(anyhow::Error::msg)?;
    Ok(())
}

fn install(project: &Path) -> Result<()> {
    let home = harness_install::home_dir()?;
    // The same canonical package writer and projection engine used by other harnesses.
    super::install::seed_product_skill(&home)?;
    seed_skill(project)?;
    skill_federation::refresh_product_projections(&home, Some(project))
        .map_err(anyhow::Error::msg)?;
    let config_dir = config::config_dir()?;
    let mut cfg = config::load_config(&config_dir)?;
    if !cfg.installed_harnesses.iter().any(|id| id == "dot") {
        cfg.installed_harnesses.push("dot".into());
    }
    config::save_config(&config_dir, &cfg)?;
    enrollment(&config_dir, project, true)?;
    println!("Dot integration installed: canonical product skills and local skill projections. Connect this computer to the dot and invoke stateroot-dot; no cloud command hooks were installed.");
    Ok(())
}

fn enrollment(config_dir: &Path, project: &Path, installed: bool) -> Result<()> {
    if let Some(mut entry) = config::lookup_project(config_dir, project)? {
        entry.harnesses_installed.retain(|id| id != "dot");
        if installed {
            entry.harnesses_installed.push("dot".into());
        }
        config::register_project(config_dir, project, entry)?;
    }
    Ok(())
}

fn owned_path(root: &Path, rel: &str) -> Result<Option<std::path::PathBuf>> {
    let path = root.join(rel);
    if !path.exists() {
        return Ok(None);
    }
    let owned = ["skill.federation.json", ".stateroot-projection.json"]
        .iter()
        .any(|meta| {
            std::fs::read(path.join(meta))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .is_some_and(|v| {
                    v["slug"] == "stateroot-dot"
                        && (v["ownership_class"] == "statesmith_authored"
                            || (v["managed_by"] == "stateroot" && v["projection_kind"].is_string()))
                })
        });
    if !owned {
        bail!("refusing to remove unmanaged dot skill {}", path.display());
    }
    let resolved_root = root.canonicalize()?;
    let resolved_path = path.canonicalize()?;
    if !resolved_path.starts_with(&resolved_root) {
        bail!("dot skill resolves outside its selected root");
    }
    Ok(Some(path))
}

fn uninstall(project: &Path) -> Result<()> {
    let home = harness_install::home_dir()?;
    let mut paths = Vec::new();
    // Validate every target before removing any package.
    for root in [home.as_path(), project] {
        for rel in [
            ".agents/skills/stateroot-dot",
            ".stateroot/skills/stateroot-dot",
        ] {
            if let Some(path) = owned_path(root, rel)? {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths.dedup();
    for path in paths {
        std::fs::remove_dir_all(&path)
            .with_context(|| format!("cannot remove {}", path.display()))?;
    }
    let config_dir = config::config_dir()?;
    let mut cfg = config::load_config(&config_dir)?;
    cfg.installed_harnesses.retain(|id| id != "dot");
    config::save_config(&config_dir, &cfg)?;
    enrollment(&config_dir, project, false)?;
    println!("Dot skills removed; project state, shared skills and other harnesses retained.");
    Ok(())
}

fn native(project: &Path, args: Vec<OsString>) -> Result<()> {
    if args.first().is_none_or(|arg| arg == "dot") {
        bail!("provide a normal StateRoot command; nested dot dispatch is not supported");
    }
    let status = Command::new(std::env::current_exe()?)
        .arg("--project")
        .arg(project)
        .args(["--actor", "dot"])
        .args(args)
        .current_dir(project)
        .status()
        .context("cannot execute the StateRoot workflow")?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

pub fn run(project: &Path, action: &DotAction) -> Result<()> {
    let argv = |args: &[&str]| args.iter().map(OsString::from).collect::<Vec<_>>();
    match action {
        DotAction::Install => install(project),
        DotAction::Uninstall => uninstall(project),
        DotAction::Init => {
            native(project, argv(&["init"]))?;
            install(project)
        }
        DotAction::Resume => native(project, argv(&["resume", "--harness", "dot", "--force"])),
        DotAction::Checkpoint { note } => native(
            project,
            vec!["checkpoint".into(), "--note".into(), note.into()],
        ),
        DotAction::Handoff { input } => native(
            project,
            vec![
                "handoff".into(),
                "write".into(),
                "--from".into(),
                "dot".into(),
                "--input".into(),
                input.as_os_str().into(),
            ],
        ),
        DotAction::Recall { query } => native(
            project,
            vec!["memory".into(), "recall".into(), query.into()],
        ),
        DotAction::Snap { reason } => native(
            project,
            vec!["snap".into(), "--reason".into(), reason.into()],
        ),
        DotAction::Run { args } | DotAction::Native(args) => native(project, args.clone()),
        DotAction::Read { path, shared } => {
            if *shared {
                bail!("--shared requires --shared-state DIR");
            }
            if path.as_os_str().is_empty()
                || path
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                bail!("read requires a relative store path without traversal");
            }
            let root = local_store::root(project).canonicalize()?;
            let target = root.join(path).canonicalize()?;
            if !target.starts_with(&root) {
                bail!("selected file is outside the project store");
            }
            print!("{}", std::fs::read_to_string(target)?);
            Ok(())
        }
        DotAction::Skill => {
            print!("{}", String::from_utf8_lossy(SKILL));
            Ok(())
        }
    }
}
