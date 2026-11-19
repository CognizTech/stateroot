//! Command Code taste → StateRoot learnings (PULL-ONLY).
//!
//! Command Code keeps its own durable preferences in
//! `~/.commandcode/taste/taste.md` (user) and `.commandcode/taste/taste.md`
//! plus `.commandcode/taste/<category>/taste.md` (project). They are the same
//! kind of judgment StateRoot stores as learnings, so they are imported — as
//! **candidates** in the matching scope, never silently activated, and never
//! written back. Command Code's own learning writer owns those files.
//!
//! Bullet grammar (Command Code): `- statement. Confidence: 0.90` under a
//! `# Category` heading, or a bare `- statement`. StateRoot re-renders the
//! statement in its own grammar through `learnings::append_candidate`, which
//! also dedupes by normalized statement across the scope.

use std::path::{Path, PathBuf};

use crate::harness_install::paths;
use crate::learnings::{self, Learning};

/// Harness id carrying the provenance of every imported statement.
pub const HARNESS: &str = "commandcode";

/// Outcome of one pull pass.
#[derive(Debug, Default)]
pub struct TastePullReport {
    /// Candidates written this pass.
    pub imported: usize,
    /// Statements already present in the scope (deduped).
    pub unchanged: usize,
    /// Sources read and anything skipped.
    pub notes: Vec<String>,
}

impl TastePullReport {
    /// True when nothing was written and nothing needs surfacing.
    pub fn is_quiet(&self) -> bool {
        self.imported == 0 && self.unchanged == 0 && self.notes.is_empty()
    }
}

/// Pull Command Code taste into the matching StateRoot scope.
///
/// `dry_run` parses and dedupes but writes nothing. Never writes under any
/// `.commandcode/` path.
pub fn pull(project_dir: &Path, home: &Path, dry_run: bool) -> TastePullReport {
    let mut report = TastePullReport::default();
    let sources = [
        ("user", taste_files(&paths::commandcode_root(home))),
        ("project", taste_files(&project_dir.join(".commandcode"))),
    ];
    for (scope, files) in sources {
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                report
                    .notes
                    .push(format!("{}: unreadable, skipped", file.display()));
                continue;
            };
            let parsed = parse_taste(&text);
            if parsed.is_empty() {
                continue;
            }
            report
                .notes
                .push(format!("{}: {} bullet(s)", file.display(), parsed.len()));
            for (category, statement, confidence) in parsed {
                let learning = Learning::candidate(
                    &statement,
                    &category,
                    confidence,
                    "commandcode taste",
                    scope,
                );
                if dry_run {
                    if learnings::has_statement(project_dir, home, scope, &learning.statement) {
                        report.unchanged += 1;
                    } else {
                        report.imported += 1;
                    }
                    continue;
                }
                match learnings::append_candidate(project_dir, home, scope, &learning) {
                    Ok(true) => report.imported += 1,
                    Ok(false) => report.unchanged += 1,
                    Err(err) => report
                        .notes
                        .push(format!("{}: import failed: {err}", file.display())),
                }
            }
        }
    }
    report
}

/// `taste.md` at the root plus `taste/<category>/taste.md`, sorted for
/// deterministic reports.
fn taste_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let top = root.join("taste").join("taste.md");
    if top.is_file() {
        out.push(top);
    }
    let categories = root.join("taste");
    if let Ok(entries) = std::fs::read_dir(&categories) {
        let mut nested: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .map(|dir| dir.join("taste.md"))
            .filter(|path| path.is_file())
            .collect();
        nested.sort();
        out.extend(nested);
    }
    out
}

/// Parse Command Code taste bullets: `# Category` headings scope the bullets
/// that follow; each `- statement[. Confidence: 0.90]` becomes one learning.
fn parse_taste(text: &str) -> Vec<(String, String, f64)> {
    let mut out = Vec::new();
    let mut category = "taste".to_string();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("# ") {
            let heading = heading.trim();
            if !heading.is_empty() {
                category = heading.to_string();
            }
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("- ") else {
            continue;
        };
        // StateRoot-imported bullets would carry our own metadata comment; do
        // not re-import them as fresh statements.
        if rest.contains("<!--") {
            continue;
        }
        let (statement, confidence) = split_confidence(rest);
        let statement = statement.trim().trim_end_matches('.').trim();
        if statement.is_empty() {
            continue;
        }
        out.push((category.clone(), statement.to_string(), confidence));
    }
    out
}

/// Split a trailing `Confidence: 0.90` marker (also accepts `(0.9)`).
fn split_confidence(text: &str) -> (String, f64) {
    if let Some((head, tail)) = text.rsplit_once("Confidence:") {
        let head = head.trim().trim_end_matches('.').trim();
        let value = tail
            .split_whitespace()
            .next()
            .and_then(|raw| {
                raw.trim_end_matches(&[',', ';', '.'][..])
                    .parse::<f64>()
                    .ok()
            })
            .filter(|value| (0.0..=1.0).contains(value))
            .unwrap_or(0.85);
        return (head.to_string(), value);
    }
    (text.trim().to_string(), 0.85)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, body).expect("write");
    }

    #[test]
    fn parses_categories_and_confidence() {
        let parsed = parse_taste(
            "# JavaScript\n\n- Use const instead of let. Confidence: 0.90\n\n# Testing\n\n- Always run tests before committing\n- **Bold statement** Confidence: 0.7\n",
        );
        assert_eq!(parsed.len(), 3, "{parsed:?}");
        assert_eq!(parsed[0].0, "JavaScript");
        assert_eq!(parsed[0].1, "Use const instead of let");
        assert!((parsed[0].2 - 0.9).abs() < f64::EPSILON);
        assert_eq!(parsed[1].0, "Testing");
        assert_eq!(parsed[1].1, "Always run tests before committing");
        assert!((parsed[1].2 - 0.85).abs() < f64::EPSILON);
        assert_eq!(parsed[2].1, "**Bold statement**");
    }

    #[test]
    fn pull_imports_user_and_project_taste_as_candidates_without_writing_taste() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        crate::local_store::init_skeleton(project.path(), "p", "n", "default").expect("init");

        let user_taste = home.path().join(".commandcode/taste/taste.md");
        write(
            &user_taste,
            "# Global\n\n- Prefer Rust for CLIs. Confidence: 0.8\n",
        );
        let project_taste = project.path().join(".commandcode/taste/taste.md");
        write(
            &project_taste,
            "# Project\n\n- Run cargo fmt before commits. Confidence: 0.9\n",
        );
        let nested = project.path().join(".commandcode/taste/testing/taste.md");
        write(
            &nested,
            "# Testing\n\n- Test the real path. Confidence: 0.7\n",
        );
        let before: Vec<String> = [user_taste.clone(), project_taste.clone(), nested.clone()]
            .iter()
            .map(|path| std::fs::read_to_string(path).expect("read"))
            .collect();

        // Dry run reports what WOULD be imported and writes no candidates.
        let dry = pull(project.path(), home.path(), true);
        assert_eq!(dry.imported, 3, "{dry:?}");
        assert_eq!(dry.unchanged, 0, "{dry:?}");
        assert!(
            dry.notes.iter().any(|note| note.contains("bullet")),
            "{dry:?}"
        );
        assert!(learnings::read_scope(project.path(), home.path(), "project").is_empty());

        let report = pull(project.path(), home.path(), false);
        assert_eq!(report.imported, 3, "{report:?}");
        let project_scope = learnings::read_scope(project.path(), home.path(), "project");
        let statements: Vec<&str> = project_scope
            .iter()
            .map(|learning| learning.statement.as_str())
            .collect();
        assert!(
            statements.contains(&"Run cargo fmt before commits"),
            "{statements:?}"
        );
        assert!(statements.contains(&"Test the real path"), "{statements:?}");
        assert_eq!(
            project_scope[0].status, "candidate",
            "imported taste is reviewable, never silently active"
        );
        // User taste lands in the user scope, not the project store.
        let user_scope = learnings::read_scope(project.path(), home.path(), "user");
        assert!(user_scope
            .iter()
            .any(|learning| learning.statement == "Prefer Rust for CLIs"));

        // Second pass dedupes; taste files are untouched.
        let again = pull(project.path(), home.path(), false);
        assert_eq!(again.imported, 0, "{again:?}");
        assert_eq!(again.unchanged, 3, "{again:?}");
        let after: Vec<String> = [user_taste, project_taste, nested]
            .iter()
            .map(|path| std::fs::read_to_string(path).expect("read"))
            .collect();
        assert_eq!(before, after, "pull must never write .commandcode/taste");
    }
}
