//! `skills` section — import skills from hermes/openclaw-style skill trees
//! into the local `.stateroot/skills/` with a provenance header.

use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;

use super::{Prompter, WizardCtx, WizardSection};

/// Skills import section.
pub struct SkillsSection;

/// One discovered skill on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscoveredSkill {
    /// Skill slug (the SKILL.md parent dir name).
    slug: String,
    /// Category (the dir above the slug dir under the skills root).
    category: String,
    /// The skill directory (parent of SKILL.md).
    dir: PathBuf,
    /// Provenance label for the header.
    origin: String,
}

/// Walk `root` (max depth 6) collecting skills: `<root>/<category>/<slug>/SKILL.md`
/// or `<root>/<slug>/SKILL.md` (category "misc").
fn scan_tree(root: &Path, origin: &str, out: &mut Vec<DiscoveredSkill>) {
    fn walk(dir: &Path, depth: usize, root: &Path, origin: &str, out: &mut Vec<DiscoveredSkill>) {
        if depth > 6 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path.join("SKILL.md").is_file() {
                let slug = entry.file_name().to_string_lossy().to_string();
                let category = path
                    .parent()
                    .filter(|parent| *parent != root)
                    .and_then(|parent| parent.file_name())
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "misc".to_string());
                out.push(DiscoveredSkill {
                    slug,
                    category,
                    dir: path.clone(),
                    origin: origin.to_string(),
                });
                continue; // don't descend into a skill dir
            }
            walk(&path, depth + 1, root, origin, out);
        }
    }
    walk(root, 0, root, origin, out);
}

/// Discover skills in the default locations plus an optional custom path.
fn discover(ctx: &WizardCtx, custom: Option<&str>) -> Vec<DiscoveredSkill> {
    let mut found = Vec::new();
    scan_tree(&ctx.home.join(".hermes/skills"), "hermes-agent", &mut found);
    // openclaw skills live under **/skills/** — scan the openclaw root.
    scan_tree(&ctx.home.join(".openclaw"), "openclaw", &mut found);
    if let Some(custom) = custom {
        scan_tree(Path::new(custom), custom, &mut found);
    }
    // openclaw scan also walks non-skill dirs — those simply yield nothing.
    found.sort_by(|a, b| a.slug.cmp(&b.slug));
    found.dedup_by(|a, b| a.slug == b.slug && a.dir == b.dir);
    found
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// Prepend the provenance header to the copied SKILL.md.
fn add_provenance(skill_dir: &Path, origin: &str) -> std::io::Result<()> {
    let skill_md = skill_dir.join("SKILL.md");
    let content = std::fs::read_to_string(&skill_md)?;
    let date = &stateroot_core::local_store::now_rfc3339()[..10];
    let header = format!("<!-- imported from {origin} on {date} -->\n");
    std::fs::write(&skill_md, format!("{header}{content}"))
}

#[async_trait]
impl WizardSection for SkillsSection {
    fn id(&self) -> &'static str {
        "skills"
    }

    fn title(&self) -> &'static str {
        "Skills import"
    }

    async fn is_configured(&self, ctx: &WizardCtx) -> Result<bool> {
        Ok(stateroot_core::local_store::is_stateroot_dir(&ctx.core.cwd)
            && !stateroot_core::local_store::list_local_skills(&ctx.core.cwd).is_empty())
    }

    async fn run(&self, ctx: &mut WizardCtx, prompter: &mut dyn Prompter) -> Result<Vec<String>> {
        let mut actions = Vec::new();

        // Skills import writes into the project's `.stateroot/skills/` — so it
        // requires an initialized project. Creating a bare `.stateroot/` in a
        // non-project directory is exactly the artifact `stateroot remove`
        // must then clean up.
        if !stateroot_core::local_store::is_stateroot_dir(&ctx.core.cwd) {
            return Ok(vec![
                "not a stateroot project — run `stateroot init` first; skipping skills import"
                    .to_string(),
            ]);
        }

        let custom = if ctx.non_interactive {
            // Scripted runs only (`--config skills.custom_path`) — the wizard
            // never asks interactively; standard locations are auto-scanned.
            prompter
                .input("skills.custom_path", "", "")
                .await
                .map(|s| s.trim().to_string())
                .ok()
                .filter(|s| !s.is_empty())
        } else {
            None
        };

        let found = discover(ctx, custom.as_deref());
        if found.is_empty() {
            return Ok(vec!["no skills found to import".to_string()]);
        }

        // Auto-import everything discovered — no picker. Discovered skills
        // still get their provenance header; dry-run lists instead of copying.
        let picked: Vec<usize> = (0..found.len()).collect();

        let dest_root = stateroot_core::local_store::root(&ctx.core.cwd).join("skills");
        for idx in picked {
            let skill = &found[idx];
            let dest = dest_root.join(&skill.slug);
            if ctx.dry_run {
                actions.push(format!(
                    "would copy {} → {} (+ provenance header)",
                    skill.dir.display(),
                    dest.display()
                ));
                continue;
            }
            copy_dir_recursive(&skill.dir, &dest)
                .map_err(|e| anyhow::anyhow!("copy {} failed: {e}", skill.dir.display()))?;
            add_provenance(&dest, &skill.origin).map_err(|e| {
                anyhow::anyhow!("provenance header for {} failed: {e}", dest.display())
            })?;
            actions.push(format!("imported {} ({})", skill.slug, skill.origin));
        }
        Ok(actions)
    }
}
