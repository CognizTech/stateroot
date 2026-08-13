//! Ignore rules for the sync root (snap / lineage trees only).
//!
//! Sources:
//! - `.staterootignore` at the sync root (gitignore syntax)
//! - hardcoded: `.git/` and `.stateroot/local/` are never synced
//!
//! Root `.gitignore` is **not** unioned — that only affects local agent
//! filesystem access, not StateRoot sync payloads.

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
    /// Load rules from `<root>/.staterootignore` when present.
    pub fn load(root: &Path) -> Self {
        let mut sets = Vec::new();
        let file = root.join(STATEROOTIGNORE);
        if file.is_file() {
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

    /// Build rules from inline `.staterootignore` content (tests).
    pub fn from_contents(root: &Path, staterootignore: Option<&str>) -> Self {
        let mut sets = Vec::new();
        if let Some(content) = staterootignore {
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
    }

    #[test]
    fn staterootignore_patterns_only() {
        let rules = IgnoreRules::from_contents(Path::new("/root"), Some("secrets/\n*.pem\n"));
        assert!(rules.is_ignored("secrets", true));
        assert!(rules.is_ignored("secrets/key", false));
        assert!(rules.is_ignored("cert.pem", false));
        // Root .gitignore is not loaded — target/ is not ignored unless listed.
        assert!(!rules.is_ignored("target", true));
        assert!(!rules.is_ignored(".stateroot/outbox.jsonl", false));
        assert!(!rules.is_ignored("src/main.rs", false));
    }
}
