//! Ignore rules for the sync root (snap / lineage trees).
//!
//! Sources (same syntax; a path is excluded if either file matches):
//! - `.gitignore` at the sync root
//! - `.staterootignore` at the sync root (extra patterns for things git
//!   still tracks that StateRoot should not)
//! - hardcoded: `.git/` and `.stateroot/local/` are never synced
//!
//! Nested `.gitignore` files in subfolders are not loaded.

use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// Name of the extra stateroot ignore file.
pub const STATEROOTIGNORE: &str = ".staterootignore";
/// An optional, separate declaration for file-tool payloads kept local.
pub const CAPTURE_IGNORE: &str = ".staterootcaptureignore";

/// Whether an existing file named by a tool input has a local-only capture
/// declaration. This does not interpret shell commands or infer where their
/// output originated; callers must label that limitation honestly.
pub fn capture_file_excluded(project: &Path, file: &Path) -> Result<bool, String> {
    let policy = project.join(CAPTURE_IGNORE);
    let metadata = match std::fs::metadata(&policy) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("capture policy unreadable: {error}")),
    };
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err("capture policy is not a bounded regular file".into());
    }
    let (rules, error) = Gitignore::new(&policy);
    if let Some(error) = error {
        return Err(format!("capture policy invalid: {error}"));
    }
    let canonical_project = project
        .canonicalize()
        .map_err(|error| format!("project path unavailable: {error}"))?;
    let canonical_file = file
        .canonicalize()
        .map_err(|error| format!("tool file path unavailable: {error}"))?;
    let Ok(relative) = canonical_file.strip_prefix(&canonical_project) else {
        return Ok(false);
    };
    // The ignore matcher is rooted at the original project path. On Windows,
    // canonical paths gain a `\\?\` prefix; rejoin the scope-checked relative
    // path under the matcher's own root before asking it to match.
    Ok(rules
        .matched_path_or_any_parents(project.join(relative), false)
        .is_ignore())
}

/// One compiled gitignore with the root its patterns are relative to.
struct RuleSet {
    root: PathBuf,
    gitignore: Gitignore,
}

/// Compiled ignore rules for a sync root.
#[derive(Clone)]
pub struct IgnoreRules {
    sets: Vec<std::sync::Arc<RuleSet>>,
}

impl IgnoreRules {
    /// Load rules from `<root>/.gitignore` and `<root>/.staterootignore` when present.
    pub fn load(root: &Path) -> Self {
        let mut sets = Vec::new();
        for name in [".gitignore", STATEROOTIGNORE] {
            let file = root.join(name);
            if !file.is_file() {
                continue;
            }
            let (gitignore, err) = Gitignore::new(&file);
            if let Some(err) = err {
                tracing::warn!("failed to parse {}: {err}", file.display());
            }
            sets.push(std::sync::Arc::new(RuleSet {
                root: root.to_path_buf(),
                gitignore,
            }));
        }
        Self { sets }
    }

    /// Build rules from inline content (tests).
    pub fn from_contents(
        root: &Path,
        gitignore: Option<&str>,
        staterootignore: Option<&str>,
    ) -> Self {
        let mut sets = Vec::new();
        for content in [gitignore, staterootignore].into_iter().flatten() {
            let mut builder = GitignoreBuilder::new(root);
            for line in content.lines() {
                let _ = builder.add_line(None, line);
            }
            if let Ok(gitignore) = builder.build() {
                sets.push(std::sync::Arc::new(RuleSet {
                    root: root.to_path_buf(),
                    gitignore,
                }));
            }
        }
        Self { sets }
    }

    /// Empty ruleset (only hardcoded paths are ignored).
    pub fn none() -> Self {
        Self { sets: Vec::new() }
    }

    /// True when `rel_path` (workspace-relative, leading `/` tolerated) must
    /// not sync. Directories should pass `is_dir = true`.
    pub fn is_ignored(&self, rel_path: &str, is_dir: bool) -> bool {
        let rel = rel_path.trim_start_matches('/');
        if rel.is_empty() {
            return false;
        }
        // Hardcoded: .git/ is never synced (any depth).
        if rel.split('/').any(|component| component == ".git") {
            return true;
        }
        // Hardcoded: .stateroot/local/ stays machine-local.
        if rel == ".stateroot/local" || rel.starts_with(".stateroot/local/") {
            return true;
        }
        // Capture's existing locks live beside its durable segments, not in
        // the evidence itself. Never pin/transport ownership tokens.
        if matches!(
            rel,
            ".stateroot/spool/current.lock" | ".stateroot/spool/current.lock.guard"
        ) || (rel.starts_with(".stateroot/spool/segments/")
            && (rel.ends_with(".lock") || rel.ends_with(".lock.guard")))
        {
            return true;
        }
        // Hardcoded: .stateroot/worktrees/ stays out of trees — a fork
        // materialized inside the project is a machine-local checkout;
        // pinning it would snapshot the fork's whole tree into the trunk
        // (recursively, on every snap).
        if rel == ".stateroot/worktrees" || rel.starts_with(".stateroot/worktrees/") {
            return true;
        }
        // Doctrine: other `.stateroot/` paths ALWAYS sync — they are our state.
        if rel == ".stateroot" || rel.starts_with(".stateroot/") {
            return false;
        }
        for set in &self.sets {
            if set
                .gitignore
                .matched_path_or_any_parents(set.root.join(rel), is_dir)
                .is_ignore()
            {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_dir_always_ignored() {
        let rules = IgnoreRules::none();
        assert!(rules.is_ignored(".git", true));
        assert!(rules.is_ignored(".git/config", false));
        assert!(rules.is_ignored("/sub/.git/HEAD", false));
        assert!(!rules.is_ignored("src/.gitkeep", false));
    }

    #[test]
    fn stateroot_local_always_ignored() {
        let rules = IgnoreRules::none();
        assert!(rules.is_ignored(".stateroot/local", true));
        assert!(rules.is_ignored(".stateroot/local/cache", false));
        assert!(!rules.is_ignored(".stateroot/manifest.json", false));
        for lock in [
            ".stateroot/spool/current.lock.guard",
            ".stateroot/spool/segments/codex__session.lock.guard",
        ] {
            assert!(
                rules.is_ignored(lock, false),
                "kernel mutation guards stay with their checkout"
            );
        }
        assert!(!rules.is_ignored(".stateroot/spool/segments/codex__session.jsonl", false));
    }

    #[test]
    fn separate_capture_declaration_matches_only_named_files() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join(CAPTURE_IGNORE), "private/\n").unwrap();
        std::fs::create_dir(project.path().join("private")).unwrap();
        let private = project.path().join("private/ignored.txt");
        let public = project.path().join("public.txt");
        std::fs::write(&private, "private fixture").unwrap();
        std::fs::write(&public, "public fixture").unwrap();
        assert!(capture_file_excluded(project.path(), &private).unwrap());
        assert!(!capture_file_excluded(project.path(), &public).unwrap());
        assert!(!IgnoreRules::load(project.path()).is_ignored("private/ignored.txt", false));
    }

    #[test]
    fn gitignore_and_staterootignore_are_unioned() {
        let rules = IgnoreRules::from_contents(
            Path::new("/root"),
            Some("target/\n*.log\nnode_modules\n"),
            Some("secrets/\n*.pem\n"),
        );
        assert!(rules.is_ignored("target", true));
        assert!(rules.is_ignored("target/debug/a", false));
        assert!(rules.is_ignored("app.log", false));
        assert!(rules.is_ignored("node_modules", true));
        assert!(rules.is_ignored("secrets", true));
        assert!(rules.is_ignored("secrets/key", false));
        assert!(rules.is_ignored("cert.pem", false));
        // `.stateroot/` always syncs — ignore rules never exclude it.
        assert!(!rules.is_ignored(".stateroot/outbox.jsonl", false));
        assert!(!rules.is_ignored("src/main.rs", false));
    }
}
