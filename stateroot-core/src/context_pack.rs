//! Local context pack for resume / hooks (no server).
//!
//! Observed project documents at the repo root plus `.stateroot/` project
//! docs. Missing files skip silently. Empty stays empty.

use std::fs;
use std::path::{Path, PathBuf};

use crate::local_store;
use crate::sync_engine::ignore::IgnoreRules;

/// Per-file cap (product direction §4.8 `repo_doc_char_cap`).
pub const REPO_DOC_CHAR_CAP: usize = 8_000;
/// Total budget across repo-doc sections, in pack order: a doc that fits the
/// remaining budget inlines whole (per-doc cap applied); a doc past the
/// budget is title-listed, never cut (`capped — N more docs on disk`).
pub const REPO_DOCS_TOTAL_CAP: usize = 16_000;
/// Canonical repo-root names, in pack order.
pub const CANONICAL_REPO_DOCS: &[&str] = &[
    "README.md",
    "README",
    "PROGRESS.md",
    "ARCHITECTURE.md",
    "TODO.md",
];
const TRUNCATION_MARKER: &str = "\n\n[truncated]";
const MAX_EXTRA_ROOT_DOCS: usize = 8;
const MAX_TOP_LEVEL: usize = 80;

const PLACEHOLDER_OBJECTIVES: &str =
    "# Objectives\n\nDescribe the project goal and success criteria.";
const PLACEHOLDER_INSTRUCTIONS: &str = "# Agent Instructions\n\nShared instructions for all harnesses attached to this StateRoot project.\nHarness-specific guidance lives in `instructions/{harness}.md`.";

/// One observed section of the local pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackSection {
    /// Heading (`Repo: README.md (observed)`).
    pub title: String,
    /// Body, possibly truncated.
    pub content: String,
}

/// Deterministic local pack.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContextPack {
    /// Document sections in display order.
    pub sections: Vec<PackSection>,
    /// Top-level names (ignored paths omitted).
    pub top_level: Vec<String>,
}

impl ContextPack {
    /// True when there is nothing to inject.
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty() && self.top_level.is_empty()
    }

    /// Markdown for resume / hook injection.
    pub fn render_markdown(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut out = String::from("## Context pack (observed)\n\n");
        if !self.top_level.is_empty() {
            out.push_str("### Project tree (top-level, observed)\n\n");
            for name in &self.top_level {
                out.push_str(&format!("- `{name}`\n"));
            }
            out.push('\n');
        }
        for section in &self.sections {
            out.push_str(&format!("### {}\n\n{}\n\n", section.title, section.content));
        }
        out
    }

    /// JSON payload for the optional LLM compiler (no invention source).
    pub fn to_synth_value(&self) -> serde_json::Value {
        serde_json::json!({
            "schema_version": "stateroot.local_context_pack.v1",
            "top_level": self.top_level,
            "sections": self.sections.iter().map(|s| serde_json::json!({
                "title": s.title,
                "content": s.content,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Build the observed pack for `project_dir`.
pub fn build(project_dir: &Path) -> ContextPack {
    let rules = IgnoreRules::load(project_dir);
    let mut sections = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    // Repo-doc sections share REPO_DOCS_TOTAL_CAP in pack order; a doc that
    // does not fit whole is title-listed (never cut mid-content).
    let mut budget_left = REPO_DOCS_TOTAL_CAP;
    let mut not_inlined: Vec<String> = Vec::new();
    let push_repo_doc = |section: PackSection,
                         sections: &mut Vec<PackSection>,
                         budget_left: &mut usize,
                         not_inlined: &mut Vec<String>| {
        let len = section.content.chars().count();
        if len <= *budget_left {
            *budget_left -= len;
            sections.push(section);
        } else {
            not_inlined.push(section.title);
        }
    };

    for name in CANONICAL_REPO_DOCS {
        if !seen.insert((*name).to_string()) {
            continue;
        }
        if let Some(section) = read_repo_doc(project_dir, name) {
            push_repo_doc(section, &mut sections, &mut budget_left, &mut not_inlined);
        }
    }
    for name in extra_root_doc_names(project_dir) {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(section) = read_repo_doc(project_dir, &name) {
            push_repo_doc(section, &mut sections, &mut budget_left, &mut not_inlined);
        }
    }
    if !not_inlined.is_empty() {
        sections.push(PackSection {
            title: "Repo docs not inlined (budget)".into(),
            content: format!(
                "{} (capped — {} more docs on disk)",
                not_inlined.join(" · "),
                not_inlined.len()
            ),
        });
    }

    if let Some(section) =
        read_stateroot_doc(project_dir, "project/objectives.md", PLACEHOLDER_OBJECTIVES)
    {
        sections.push(section);
    }
    if let Some(section) = read_stateroot_doc(
        project_dir,
        local_store::INSTRUCTIONS_PATH,
        PLACEHOLDER_INSTRUCTIONS,
    ) {
        sections.push(section);
    }

    ContextPack {
        sections,
        top_level: top_level_names(project_dir, &rules),
    }
}

fn extra_root_doc_names(project_dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(project_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|name| is_extra_product_doc(name))
        .collect();
    names.sort();
    names.dedup();
    names.truncate(MAX_EXTRA_ROOT_DOCS);
    names
}

fn is_extra_product_doc(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if !lower.ends_with(".md") {
        return false;
    }
    if CANONICAL_REPO_DOCS
        .iter()
        .any(|c| c.eq_ignore_ascii_case(name))
    {
        return false;
    }
    lower.contains("overview") || lower.contains("usecases") || lower.contains("use_cases")
}

fn read_repo_doc(project_dir: &Path, name: &str) -> Option<PackSection> {
    let path = project_dir.join(name);
    let content = read_capped(&path)?;
    Some(PackSection {
        title: format!("Repo: {name} (observed)"),
        content,
    })
}

fn read_stateroot_doc(project_dir: &Path, rel: &str, placeholder: &str) -> Option<PackSection> {
    let path = local_store::root(project_dir).join(rel);
    let content = read_capped(&path)?;
    if content.trim() == placeholder.trim() {
        return None;
    }
    Some(PackSection {
        title: format!("StateRoot: {rel} (observed)"),
        content,
    })
}

fn read_capped(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate_doc(trimmed))
}

fn truncate_doc(text: &str) -> String {
    if text.len() <= REPO_DOC_CHAR_CAP {
        return text.to_string();
    }
    let mut end = REPO_DOC_CHAR_CAP;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATION_MARKER}", &text[..end])
}

fn top_level_names(project_dir: &Path, rules: &IgnoreRules) -> Vec<String> {
    let Ok(entries) = fs::read_dir(project_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path: PathBuf = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name == ".git" || name == ".stateroot" {
            continue;
        }
        let is_dir = path.is_dir();
        if rules.is_ignored(&name, is_dir) {
            continue;
        }
        if is_dir {
            names.push(format!("{name}/"));
        } else {
            names.push(name);
        }
    }
    names.sort();
    names.truncate(MAX_TOP_LEVEL);
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    #[test]
    fn repo_docs_share_a_total_budget_and_title_list_the_rest() {
        let tmp = tempdir().unwrap();
        // 3 × ~8760 chars (per-doc cap cuts each to ~8000): the first inlines
        // whole; the rest exceed the remaining 16000 budget and are
        // title-listed. Canonical order inlines README first.
        let big = |name: &str| format!("# {name}\n\n{}\n", "content line for {name}. ".repeat(350));
        write(tmp.path(), "README.md", &big("README.md"));
        write(tmp.path(), "TODO.md", &big("TODO.md"));
        write(tmp.path(), "ARCHITECTURE.md", &big("ARCHITECTURE.md"));

        let pack = build(tmp.path());
        let readme = pack
            .sections
            .iter()
            .find(|s| s.title == "Repo: README.md (observed)")
            .expect("readme inlined");
        assert!(readme.content.chars().count() <= REPO_DOC_CHAR_CAP + 20);
        for title in [
            "Repo: TODO.md (observed)",
            "Repo: ARCHITECTURE.md (observed)",
        ] {
            assert!(
                !pack.sections.iter().any(|s| s.title == title),
                "past-budget doc must not inline: {title}"
            );
        }
        let marker = pack
            .sections
            .iter()
            .find(|s| s.title == "Repo docs not inlined (budget)")
            .expect("marker section");
        assert!(marker.content.contains("Repo: TODO.md (observed)"));
        assert!(marker.content.contains("Repo: ARCHITECTURE.md (observed)"));
        assert!(
            marker.content.contains("(capped — 2 more docs on disk)"),
            "marker: {}",
            marker.content
        );
        // The repo-doc block stays within budget + marker overhead.
        let total: usize = pack
            .sections
            .iter()
            .map(|s| s.content.chars().count())
            .sum();
        assert!(total <= REPO_DOCS_TOTAL_CAP + 300, "total: {total}");

        // Small docs are untouched by the budget (identity).
        let small = tempdir().unwrap();
        write(small.path(), "README.md", "# Demo\n\nSmall doc.\n");
        let pack = build(small.path());
        assert!(
            !pack
                .sections
                .iter()
                .any(|s| s.title == "Repo docs not inlined (budget)"),
            "no marker when everything fits"
        );
    }

    #[test]
    fn missing_docs_skip_and_readme_is_observed() {
        let tmp = tempdir().unwrap();
        write(
            tmp.path(),
            "README.md",
            "# SiderAgents\n\nLive upgrade target.\n",
        );
        write(tmp.path(), "src/main.rs", "fn main() {}\n");
        let pack = build(tmp.path());
        assert!(
            pack.sections
                .iter()
                .any(|s| s.title == "Repo: README.md (observed)"
                    && s.content.contains("Live upgrade target")),
            "{:?}",
            pack.sections
        );
        assert!(!pack
            .sections
            .iter()
            .any(|s| s.title.contains("PROGRESS.md")));
        let md = pack.render_markdown();
        assert!(md.contains("## Context pack (observed)"));
        assert!(md.contains("`README.md`") || md.contains("`src/`"));
    }

    #[test]
    fn overview_and_usecases_are_picked_up() {
        let tmp = tempdir().unwrap();
        write(
            tmp.path(),
            "YAgents_OVERVIEW.md",
            "# Overview\n\nOld product.\n",
        );
        write(
            tmp.path(),
            "MAgentsUseCases.md",
            "# Use cases\n\nDefunct.\n",
        );
        let pack = build(tmp.path());
        let titles: Vec<_> = pack.sections.iter().map(|s| s.title.as_str()).collect();
        assert!(
            titles.iter().any(|t| t.contains("YAgents_OVERVIEW.md")),
            "{titles:?}"
        );
        assert!(
            titles.iter().any(|t| t.contains("MAgentsUseCases.md")),
            "{titles:?}"
        );
    }

    #[test]
    fn placeholder_stateroot_docs_are_omitted() {
        let tmp = tempdir().unwrap();
        local_store::init_skeleton(tmp.path(), "p", "n", "default").unwrap();
        let pack = build(tmp.path());
        assert!(
            !pack
                .sections
                .iter()
                .any(|s| s.title.contains("objectives.md")),
            "{:?}",
            pack.sections
        );
    }

    #[test]
    fn gitignore_hides_venv_from_top_level() {
        let tmp = tempdir().unwrap();
        write(tmp.path(), ".gitignore", ".venv/\n.env\n");
        write(tmp.path(), ".venv/lib/x.py", "x");
        write(tmp.path(), ".env", "SECRET=1\n");
        write(tmp.path(), "app/main.py", "print(1)\n");
        let pack = build(tmp.path());
        assert!(
            !pack.top_level.iter().any(|n| n.starts_with(".venv")),
            "{:?}",
            pack.top_level
        );
        assert!(
            !pack.top_level.iter().any(|n| n == ".env"),
            "{:?}",
            pack.top_level
        );
        assert!(
            pack.top_level.iter().any(|n| n == "app/"),
            "{:?}",
            pack.top_level
        );
    }

    #[test]
    fn long_docs_are_capped() {
        let tmp = tempdir().unwrap();
        write(
            tmp.path(),
            "ARCHITECTURE.md",
            &"a".repeat(REPO_DOC_CHAR_CAP + 50),
        );
        let pack = build(tmp.path());
        let section = pack
            .sections
            .iter()
            .find(|s| s.title.contains("ARCHITECTURE.md"))
            .unwrap();
        assert!(section.content.contains("[truncated]"));
        assert!(section.content.len() < REPO_DOC_CHAR_CAP + 40);
    }
}
