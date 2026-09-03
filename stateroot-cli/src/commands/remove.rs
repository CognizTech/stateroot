//! `stateroot remove` — remove a stateroot project: the `.stateroot/` tree,
//! the `projects.toml` registry entry, the `init`-installed convenience
//! layer, and our git plumbing refs (`refs/stateroot/*`). Fully local.
//!
//! Safety model: destructive actions require `--yes`, an interactive
//! confirmation (default NO), or are previewed with `--dry-run`. User files
//! and machine-level installs are never touched. Stub files are deleted only
//! when byte-identical to the bundled asset (modified = kept with a note).
//! AGENTS.md keeps foreign content — the marked block is excised, the file
//! deleted only when block-only.

use std::path::{Path, PathBuf};

use stateroot_core::config::{self, ProjectEntry};
use stateroot_core::local_store;

use super::{blocks, skill, stdin_is_tty, Ctx};

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

/// One cross-scope trace purged only under `--full`.
#[derive(Debug)]
struct FullTarget {
    /// What this trace is (printed in the plan).
    kind: &'static str,
    /// File or directory to delete.
    path: PathBuf,
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
    /// Cross-scope traces (workspace bubble, persona keys, transcripts).
    full_targets: Vec<FullTarget>,
    /// kimi-code session ids to mark deleted in session_index.jsonl.
    kimi_session_marks: Vec<String>,
    /// Session-registry anchors to prune (harness|cwd mentioning the path).
    registry_prune: usize,
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

/// Path spellings a trace may carry: native (`/mnt/d/x`), normalized
/// (`d:/x`), and Windows-native (`D:\x`).
fn path_spellings(project_dir: &Path) -> Vec<String> {
    let native = project_dir.to_string_lossy().to_string();
    let norm = stateroot_core::path_identity::normalize_host_path(&native);
    let mut out = vec![native, norm.clone()];
    if norm.len() >= 2 && norm.as_bytes()[1] == b':' {
        let drive = norm[..1].to_ascii_uppercase();
        out.push(format!("{}:{}", drive, norm[2..].replace('/', "\\")));
    }
    out
}

fn mentions_project(haystack: &str, spellings: &[String]) -> bool {
    spellings.iter().any(|s| haystack.contains(s.as_str()))
}

/// Collect the cross-scope traces `--full` purges: the workspace bubble, the
/// persona-injection keys, session-registry anchors, and the harness-native
/// transcript sessions (kimi-code, claude-code) bound to this project path.
fn collect_full(
    home: &Path,
    project_dir: &Path,
    workspace_id: &str,
) -> (Vec<FullTarget>, Vec<String>, usize) {
    let spellings = path_spellings(project_dir);
    let mut targets = Vec::new();
    let mut kimi_marks = Vec::new();

    // Workspace bubble (learnings and any future workspace-scoped state).
    if !workspace_id.is_empty() {
        let bubble = home.join(".stateroot/workspaces").join(workspace_id);
        if bubble.is_dir() {
            targets.push(FullTarget {
                kind: "workspace learnings/state bubble",
                path: bubble,
            });
        }
    }

    // Persona-injection cadence state keyed to this project path.
    let persona_dir = home.join(".stateroot/local/persona-injection");
    if let Ok(entries) = std::fs::read_dir(&persona_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            let Some(key) = value.get("key").and_then(|v| v.as_str()) else {
                continue;
            };
            if mentions_project(key, &spellings) {
                targets.push(FullTarget {
                    kind: "persona-injection state",
                    path,
                });
            }
        }
    }

    // Session-registry anchors whose hook cwd was this project.
    let registry = stateroot_core::session_identity::registry_path(home);
    let registry_prune = std::fs::read_to_string(&registry)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|obj| {
            obj.keys()
                .filter(|anchor| mentions_project(anchor, &spellings))
                .count()
        })
        .unwrap_or(0);

    // kimi-code session transcripts bound to this path (session_index.jsonl).
    let kimi_index = home.join(".kimi-code/session_index.jsonl");
    if let Ok(text) = std::fs::read_to_string(&kimi_index) {
        let mut deleted = std::collections::BTreeSet::new();
        let mut live: Vec<(String, String)> = Vec::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let Some(id) = v.get("sessionId").and_then(|s| s.as_str()) else {
                continue;
            };
            if v.get("deleted").and_then(|d| d.as_bool()).unwrap_or(false) {
                deleted.insert(id.to_string());
                continue;
            }
            let Some(work_dir) = v.get("workDir").and_then(|w| w.as_str()) else {
                continue;
            };
            if mentions_project(work_dir, &spellings) {
                if let Some(dir) = v.get("sessionDir").and_then(|s| s.as_str()) {
                    live.push((id.to_string(), dir.to_string()));
                }
            }
        }
        for (id, dir) in live {
            if deleted.contains(&id) {
                continue;
            }
            let path = PathBuf::from(&dir);
            if path.is_dir() {
                targets.push(FullTarget {
                    kind: "kimi-code session transcript",
                    path,
                });
                kimi_marks.push(id);
            }
        }
    }

    // claude-code transcript dir for this path (slug = path with dashes).
    let claude_projects = home.join(".claude/projects");
    if claude_projects.is_dir() {
        for spelling in &spellings {
            let slug = spelling.replace(['/', '\\'], "-").replace(':', "");
            let dir = claude_projects.join(&slug);
            if dir.is_dir() && !targets.iter().any(|t| t.path == dir) {
                targets.push(FullTarget {
                    kind: "claude-code transcript dir",
                    path: dir,
                });
            }
        }
    }

    (targets, kimi_marks, registry_prune)
}

fn build_plan(ctx: &Ctx, full: bool) -> anyhow::Result<Plan> {
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
    let stateroot_refs = collect_stateroot_refs(&project_dir);
    let agents_md = agents_md_action(&project_dir);
    let (full_targets, kimi_session_marks, registry_prune) = if full {
        match stateroot_core::harness_install::home_dir() {
            Ok(home) => collect_full(&home, &project_dir, &entry.workspace_id),
            Err(_) => (Vec::new(), Vec::new(), 0),
        }
    } else {
        (Vec::new(), Vec::new(), 0)
    };
    Ok(Plan {
        stateroot_refs,
        project_dir,
        entry,
        stateroot_dir,
        agents_md,
        stubs,
        registered,
        full_targets,
        kimi_session_marks,
        registry_prune,
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
    if !plan.full_targets.is_empty() || plan.registry_prune > 0 {
        println!("  --full cross-scope purge:");
        for target in &plan.full_targets {
            println!("  - delete {} ({})", target.path.display(), target.kind);
        }
        if !plan.kimi_session_marks.is_empty() {
            println!(
                "  - mark {} kimi-code session(s) deleted in session_index.jsonl",
                plan.kimi_session_marks.len()
            );
        }
        if plan.registry_prune > 0 {
            println!(
                "  - prune {} session-registry anchor(s) keyed to this path",
                plan.registry_prune
            );
        }
    }
}

/// Run `stateroot remove`.
pub async fn run(ctx: &Ctx, yes: bool, dry_run: bool, full: bool) -> anyhow::Result<()> {
    let plan = build_plan(ctx, full)?;

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

    if plan.stateroot_dir {
        let root = local_store::root(&plan.project_dir);
        std::fs::remove_dir_all(&root)?;
        println!("  deleted {}", root.display());
    }

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

    for target in &plan.full_targets {
        if target.path.is_dir() {
            std::fs::remove_dir_all(&target.path)?;
        } else if target.path.exists() {
            std::fs::remove_file(&target.path)?;
        }
        println!("  deleted {} ({})", target.path.display(), target.kind);
    }
    if !plan.kimi_session_marks.is_empty() {
        if let Ok(home) = stateroot_core::harness_install::home_dir() {
            let index = home.join(".kimi-code/session_index.jsonl");
            if index.is_file() {
                let mut lines = String::new();
                for id in &plan.kimi_session_marks {
                    lines.push_str(&format!("{{\"sessionId\":\"{id}\",\"deleted\":true}}\n"));
                }
                use std::io::Write as _;
                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&index) {
                    let _ = f.write_all(lines.as_bytes());
                }
            }
        }
    }
    if plan.registry_prune > 0 {
        if let Ok(home) = stateroot_core::harness_install::home_dir() {
            let spellings = path_spellings(&plan.project_dir);
            let registry = stateroot_core::session_identity::registry_path(&home);
            if let Ok(text) = std::fs::read_to_string(&registry) {
                if let Ok(serde_json::Value::Object(mut map)) =
                    serde_json::from_str::<serde_json::Value>(&text)
                {
                    let before = map.len();
                    map.retain(|anchor, _| !mentions_project(anchor, &spellings));
                    if map.len() != before {
                        let tmp = registry.with_extension("json.tmp");
                        if let Ok(out) = serde_json::to_string_pretty(&map) {
                            if std::fs::write(&tmp, format!("{out}\n")).is_ok() {
                                let _ = std::fs::remove_file(&registry);
                                let _ = std::fs::rename(&tmp, &registry);
                            }
                        }
                    }
                }
            }
            println!("  pruned session-registry anchor(s)");
        }
    }

    println!("removed project {}", plan.entry.project_id);
    Ok(())
}
