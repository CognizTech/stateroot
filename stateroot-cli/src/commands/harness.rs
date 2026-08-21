//! StateRoot-owned local harness launches.
//!
//! The command intentionally leaves harness-owned configuration alone. It
//! passes launch policy as argv, which is portable across Unix and Windows and
//! does not depend on aliases, shims, or shell startup files.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context as _, Result};
use stateroot_core::skill_federation::{
    build_launch_argv_from_spec, load_registry, normalize_harness, HarnessEntry,
};

use super::Ctx;

pub(crate) fn is_single_slug(slug: &str) -> bool {
    !slug.is_empty()
        && Path::new(slug)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

pub(crate) fn canonical_skill_path(ctx: &Ctx, home: &Path, slug: &str) -> Result<PathBuf> {
    if !is_single_slug(slug) {
        anyhow::bail!("skill must be a single StateRoot slug, got `{slug}`");
    }
    for root in [
        ctx.cwd.join(".stateroot/skills"),
        home.join(".stateroot/skills"),
    ] {
        let candidate = root.join(slug);
        if candidate.join("SKILL.md").is_file() {
            return Ok(candidate);
        }
    }
    anyhow::bail!(
        "StateRoot skill `{slug}` is not available in this project or user store; \
         run `stateroot skill list` to inspect available packages"
    )
}

fn entry_for(harness: &str) -> Result<(String, HarnessEntry)> {
    let id = normalize_harness(harness);
    let registry = load_registry().map_err(|err| anyhow!(err))?;
    let entry = registry
        .harnesses
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| anyhow!("unknown harness `{harness}`"))?;
    Ok((id, entry))
}

/// Launch one locally installed harness. Pi runs in isolated-skill mode by
/// default; other registry rows keep their declared argv unchanged.
pub fn run(
    ctx: &Ctx,
    harness: &str,
    objective: Option<&str>,
    skills: &[String],
    ambient_skills: bool,
    dry_run: bool,
) -> Result<()> {
    let (id, entry) = entry_for(harness)?;
    let home = stateroot_core::harness_install::home_dir().map_err(|err| anyhow!(err))?;
    let skill_paths = skills
        .iter()
        .map(|slug| canonical_skill_path(ctx, &home, slug))
        .collect::<Result<Vec<_>>>()?;
    let skill_paths = skill_paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let argv =
        build_launch_argv_from_spec(&entry.delegation, objective, &skill_paths, ambient_skills)
            .ok_or_else(|| anyhow!("harness `{id}` has no local launch command"))?;
    let (command, args) = argv
        .split_first()
        .ok_or_else(|| anyhow!("harness `{id}` produced an empty launch command"))?;

    if dry_run {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "harness": id,
                "cwd": ctx.cwd,
                "command": command,
                "args": args,
                "ambient_skills": ambient_skills,
            }))?
        );
        return Ok(());
    }

    let status = Command::new(command)
        .args(args)
        .current_dir(&ctx.cwd)
        .status()
        .with_context(|| format!("launching {id} via `{command}`"))?;
    if !status.success() {
        anyhow::bail!("{id} exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stateroot_core::skill_federation::DelegationSpec;

    #[test]
    fn pi_registry_launch_uses_only_explicit_skills() {
        let (_, entry) = entry_for("pi").expect("Pi registry entry");
        assert_eq!(
            build_launch_argv_from_spec(
                &entry.delegation,
                None,
                &["/tmp/state-skill".into()],
                false,
            ),
            Some(vec![
                "pi".into(),
                "--no-skills".into(),
                "--skill".into(),
                "/tmp/state-skill".into(),
            ])
        );
    }

    #[test]
    fn pi_ambient_opt_in_omits_the_isolation_flag() {
        let spec = DelegationSpec {
            command: Some("pi".into()),
            skill_isolation: stateroot_core::skill_federation::SkillIsolationPolicy {
                isolated_by_default: true,
                disable_discovery_arg: "--no-skills".into(),
                explicit_skill_arg: "--skill".into(),
            },
            ..Default::default()
        };
        assert_eq!(
            build_launch_argv_from_spec(&spec, Some("hello"), &[], true),
            Some(vec!["pi".into(), "hello".into()])
        );
    }
}
