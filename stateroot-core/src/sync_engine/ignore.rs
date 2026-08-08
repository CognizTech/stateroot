//! Ignore rules for the sync root.
//!
//! Sources (same syntax, later files win within their own order):
//! - `.gitignore` at the sync root (standard gitignore semantics via the
//!   `ignore` crate)
//! - `.staterootignore` at the sync root (same syntax)
//! - hardcoded: `.git/` is never synced, anywhere in the tree.

use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// Name of the extra stateroot ignore file.
pub const STATEROOTIGNORE: &str = ".staterootignore";

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

    /// Empty ruleset (only `.git/` is ignored).
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
        // Doctrine: `.stateroot/` itself ALWAYS syncs — it is our state, not
        // user content; ignore rules never apply to it.
        if rel == ".stateroot" || rel.starts_with(".stateroot/") {
            return false;
        }
        // Hardcoded: .git/ is never synced (any depth).
        if rel.split('/').any(|component| component == ".git") {
            return true;
        }
        for set in &self.sets {
            // Gitignore matching expects candidate paths under its root; use
            // the parents-aware variant so `target/` also matches files below it.
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

// RuleSet is shared via Arc; no per-instance Clone needed.

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
    fn gitignore_patterns() {
        let rules = IgnoreRules::from_contents(
            Path::new("/root"),
            Some("target/\n*.log\nnode_modules\n"),
            Some(".stateroot/outbox.jsonl\n"),
        );
        assert!(rules.is_ignored("target", true));
        assert!(rules.is_ignored("target/debug/a", false));
        assert!(rules.is_ignored("app.log", false));
        assert!(rules.is_ignored("node_modules", true));
        // Doctrine: `.stateroot/` always syncs — ignore rules never exclude
        // it (it is our state, not user content), even when a rule matches.
        assert!(!rules.is_ignored(".stateroot/outbox.jsonl", false));
        assert!(!rules.is_ignored(".stateroot/manifest.json", false));
        assert!(!rules.is_ignored(".stateroot", true));
        assert!(!rules.is_ignored("src/main.rs", false));
    }
}
