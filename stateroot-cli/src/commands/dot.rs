//! Explicit project-only dot boundary. Never invokes host federation/readers.
use crate::cli::{DotAction, DotArgs};
use anyhow::{bail, Context, Result};
use serde_json::json;
use stateroot_core::{local_store as store, roots};
use std::{fs, path::Path};

const SKILL: &str = include_str!("../../assets/stateroot-dot/SKILL.md");

pub fn run(args: &DotArgs) -> Result<()> {
    let project = args
        .project
        .canonicalize()
        .context("--project must name an existing project directory")?;
    if !project.is_dir() {
        bail!("--project must be a directory");
    }
    if matches!(args.action, DotAction::Skill) {
        print!("{SKILL}");
        return Ok(());
    }
    validate_store_paths(&store::root(&project))?;
    if matches!(args.action, DotAction::Init) {
        let id = match store::read_manifest(&project)? {
            Some(manifest) => {
                if manifest["schema_version"] != store::SCHEMA_MANIFEST_V1 {
                    bail!("invalid or unsupported project manifest");
                }
                manifest["project_id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .context("existing manifest requires project_id")?
                    .to_string()
            }
            None => format!("dot-{}", uuid::Uuid::new_v4()),
        };
        let name = project
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("dot");
        store::init_skeleton(&project, &id, name, "local")?;
        println!(
            "Project store ready at {}. Use dot resume; no hooks were installed.",
            store::root(&project).display()
        );
        return Ok(());
    }
    if !store::is_stateroot_dir(&project) {
        bail!("project is not initialized; run dot --project DIR init");
    }
    let manifest = store::read_manifest(&project)?.context("missing manifest")?;
    if manifest["schema_version"] != store::SCHEMA_MANIFEST_V1
        || manifest["project_id"].as_str().is_none_or(|s| s.is_empty())
    {
        bail!("invalid or unsupported project manifest");
    }
    if let Some(packet) = store::read_handoff_local(&project)? {
        if packet["project_id"] != manifest["project_id"] {
            bail!("handoff project_id does not match the selected project");
        }
        if packet["fork_id"].as_str().is_some_and(|s| !s.is_empty()) {
            bail!("dot POC does not support fork-bound handoffs; use ordinary StateRoot in the registered worktree");
        }
    }
    let shared = args
        .shared_state
        .as_ref()
        .map(|path| -> Result<_> {
            let path = path.canonicalize().context("--shared-state must exist")?;
            if !path.is_dir() {
                bail!("--shared-state must be a directory");
            }
            validate_store_paths(&path)?;
            Ok(path)
        })
        .transpose()?;
    match &args.action {
        DotAction::Resume => {
            println!(
                "# StateRoot dot project context\n\nProject: {}\n",
                project.display()
            );
            if let Some(packet) = store::read_handoff_local(&project)? {
                print!(
                    "{}",
                    super::resume::render_handoff_digest_with(&packet, false)
                );
            }
            if let Some(root) = &shared {
                render_intelligence(root, true)?;
            }
            render_intelligence(&store::root(&project), false)?;
            if let Some(section) = super::resume::central_plan_section(Some(&project)) {
                print!("{section}");
                println!("Read its body with dot --project DIR read plans/PLAN_ID.md.");
            }
            if project.join(".git").exists() {
                if let Some(root) = roots::latest_root(&project)? {
                    println!("\n## Work State Lineage\n\nCurrent root: `{root}`");
                }
            }
            for rel in [
                store::STATE_PATH,
                "project/objectives.md",
                store::INSTRUCTIONS_PATH,
                store::MEMORY_CORE_PATH,
                store::EPISODIC_PATH,
            ] {
                let text = read_optional(&store::root(&project).join(rel))?;
                if !text.trim().is_empty() {
                    println!("\n## {rel}\n\n{text}");
                }
            }
            println!("\nSelected-store context. Checkpoint and handoff explicitly; no lifecycle callbacks or host transcripts.");
        }
        DotAction::Checkpoint { note } => {
            if note.trim().is_empty() {
                bail!("checkpoint note must not be empty");
            }
            validate_snapshot(&project)?;
            store::append_episodic(
                &project,
                &json!({"ts": store::now_rfc3339(), "harness": "dot", "note": note, "files": []}),
            )?;
            store::stamp_handoff_activity(&project, "dot", "checkpoint");
            match roots::snap_if_changed(&project, "dot", &super::truncate(note, 160), None)? {
                roots::SnapOutcome::Created(root, _) => {
                    println!("dot checkpoint recorded; root {}", root.id)
                }
                roots::SnapOutcome::Unchanged { .. } => println!("dot checkpoint recorded"),
            }
        }
        DotAction::Handoff { input } => {
            super::handoff::write_project_only(&project, input)?;
            println!("dot handoff recorded");
        }
        DotAction::Recall { query } => {
            if query.trim().is_empty() {
                bail!("recall query must not be empty");
            }
            let mut paths = vec![
                store::MEMORY_CORE_PATH.to_string(),
                store::EPISODIC_PATH.to_string(),
            ];
            paths.extend(stateroot_core::wiki::list_pages(&project));
            for rel in paths {
                for line in read_optional(&store::root(&project).join(&rel))?.lines() {
                    if line.to_lowercase().contains(&query.to_lowercase()) {
                        println!("{rel}: {line}");
                    }
                }
            }
        }
        DotAction::Read {
            path,
            shared: use_shared,
        } => {
            let project_store = store::root(&project);
            let root = if *use_shared {
                shared
                    .as_ref()
                    .context("read --shared requires --shared-state DIR")?
            } else {
                &project_store
            };
            if path.as_os_str().is_empty()
                || path
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                bail!("read requires a relative store path without traversal");
            }
            print!(
                "{}",
                fs::read_to_string(root.join(path)).context("cannot read selected store file")?
            );
        }
        DotAction::Snap { reason } => {
            validate_snapshot(&project)?;
            let (root, _) = roots::create_root(&project, "dot", reason, None)?;
            println!(
                "root {} (coverage: {}, files: {})",
                root.id, root.coverage, root.files_pinned
            );
        }
        DotAction::Init | DotAction::Skill => unreachable!(),
    }
    Ok(())
}

fn validate_snapshot(project: &Path) -> Result<()> {
    for name in [".gitignore", ".staterootignore"] {
        validate_store_paths(&project.join(name))?;
    }
    let rules = stateroot_core::sync_engine::ignore::IgnoreRules::load(project);
    validate_snapshot_paths(project, project, &rules)
}

// Same canonical stores as native StateRoot. No host readers or runtime launches.
fn render_intelligence(root: &Path, shared: bool) -> Result<()> {
    println!("\n## Stored shared intelligence ({})", root.display());
    for rel in [
        store::SOUL_PATH,
        "soul/OVERLAY.md",
        store::USER_PROFILE_PATH,
    ] {
        let text = read_optional(&root.join(rel))?;
        if !text.trim().is_empty() {
            println!("\n### {rel}\n\n{text}");
        }
    }
    if shared {
        let text = read_optional(&root.join(store::MEMORY_CORE_PATH))?;
        if !text.trim().is_empty() {
            println!("\n### Shared memory\n\n{text}");
        }
    }
    for dir in ["rules", "learnings"] {
        for rel in catalog(root, &root.join(dir))? {
            if rel.extension().is_some_and(|e| e == "md") {
                let text = fs::read_to_string(root.join(&rel))?;
                if dir == "learnings" {
                    for learning in super::learnings_reader::parse_learnings_md(&text, "dot") {
                        if learning.status == "active" {
                            println!(
                                "- {} [{}; confidence={}]",
                                learning.statement, learning.label, learning.confidence
                            );
                        }
                    }
                } else {
                    println!("\n### {}\n\n{text}", rel.display());
                }
            }
        }
    }
    for dir in ["plans", "skills", "tools", "memories"] {
        let files = catalog(root, &root.join(dir))?;
        if !files.is_empty() {
            println!("\n### {dir} (stored catalog; runtime availability must be checked)");
            for rel in files {
                println!(
                    "- `{}` (dot read{})",
                    rel.display(),
                    if !shared {
                        ""
                    } else {
                        "; use --shared for the shared store"
                    }
                );
            }
        }
    }
    Ok(())
}

fn catalog(root: &Path, dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            files.extend(catalog(root, &path)?);
        } else if path.is_file() {
            files.push(path.strip_prefix(root)?.to_path_buf());
        }
    }
    files.sort();
    Ok(files)
}

fn read_optional(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

// Validate before invoking helpers that can recurse or write. Reject links even
// within the store: this avoids cycles, dangling links and ambiguous write roots.
// This is a local boundary check, not a hostile-filesystem race-proof sandbox.
fn validate_store_paths(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if is_linked(&metadata) {
        bail!(
            "dot store must not contain symlinks or reparse points: {}",
            path.display()
        );
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            validate_store_paths(&entry?.path())?;
        }
    }
    Ok(())
}

fn is_linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

// The existing snapshot walker follows path.is_dir/is_file. Check eligible
// paths first rather than changing its established snapshot representation.
fn validate_snapshot_paths(
    root: &Path,
    dir: &Path,
    rules: &stateroot_core::sync_engine::ignore::IgnoreRules,
) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let rel = path
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        if rules.is_ignored(&rel, path.is_dir()) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if is_linked(&metadata) {
            bail!(
                "dot snapshot cannot follow symlinks or reparse points: {}",
                path.display()
            );
        }
        if metadata.is_dir() {
            validate_snapshot_paths(root, &path, rules)?;
        }
    }
    Ok(())
}
