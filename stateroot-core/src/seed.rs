//! Deterministic init seed extraction (no LLM, no network).
//!
//! Reads only what the repo already declares — README/TODO.md via the
//! observed [`ContextPack`], plus best-effort git facts via git2. Every
//! extractor stays empty when its source is absent: empty stays empty.

use std::path::Path;

use crate::context_pack::ContextPack;

const MAX_OBJECTIVE_CHARS: usize = 500;
const MAX_NEXT_ACTIONS: usize = 8;
const MAX_NEXT_ACTION_CHARS: usize = 200;
const MAX_RECENT_COMMITS: usize = 5;
const MAX_LAYOUT_ENTRIES: usize = 12;

/// What the repo declares about itself, distilled for init seeding.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeedDraft {
    /// README title + first paragraph.
    pub objective: Option<String>,
    /// Short digest of observed docs and top-level layout.
    pub context_summary: Option<String>,
    /// TODO.md unchecked boxes / next-steps bullets.
    pub next_actions: Vec<String>,
    /// Layout, docs present, git remote, recent commits.
    pub memory_facts: Vec<String>,
}

impl SeedDraft {
    /// True when nothing could be observed (nothing to seed).
    pub fn is_empty(&self) -> bool {
        self.objective.is_none()
            && self.context_summary.is_none()
            && self.next_actions.is_empty()
            && self.memory_facts.is_empty()
    }
}

/// Extract the deterministic seed from the repo directory and its pack.
pub fn extract(project_dir: &Path, pack: &ContextPack) -> SeedDraft {
    SeedDraft {
        objective: extract_objective(pack),
        context_summary: extract_context_summary(pack),
        next_actions: extract_next_actions(pack),
        memory_facts: extract_memory_facts(project_dir, pack),
    }
}

fn section_body<'a>(pack: &'a ContextPack, name: &str) -> Option<&'a str> {
    pack.sections
        .iter()
        .find(|s| s.title == format!("Repo: {name} (observed)"))
        .map(|s| s.content.as_str())
}

fn observed_doc_names(pack: &ContextPack) -> Vec<String> {
    pack.sections
        .iter()
        .filter_map(|s| {
            s.title
                .strip_prefix("Repo: ")
                .and_then(|t| t.strip_suffix(" (observed)"))
                .map(str::to_string)
        })
        .collect()
}

/// A line that carries no prose — badges, link rows, raw HTML blocks.
fn is_badge_or_link_only(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty()
        && line.split_whitespace().all(|token| {
            token.starts_with('[') || token.starts_with("![") || token.starts_with('<')
        })
}

fn cap_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn extract_objective(pack: &ContextPack) -> Option<String> {
    let body = section_body(pack, "README.md").or_else(|| section_body(pack, "README"))?;
    let mut title: Option<String> = None;
    let mut paragraph: Vec<String> = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if title.is_none() {
            if let Some(heading) = line.strip_prefix("# ") {
                title = Some(heading.trim().to_string());
                continue;
            }
        }
        if line.starts_with('#') || is_badge_or_link_only(line) {
            continue;
        }
        paragraph.push(line.to_string());
    }
    let paragraph = paragraph.join(" ");
    let objective = match (title, paragraph.is_empty()) {
        (Some(t), false) => format!("{t} — {paragraph}"),
        (Some(t), true) => t,
        (None, false) => paragraph,
        (None, true) => return None,
    };
    let objective = objective.trim().to_string();
    (!objective.is_empty()).then(|| cap_chars(&objective, MAX_OBJECTIVE_CHARS))
}

fn extract_context_summary(pack: &ContextPack) -> Option<String> {
    if pack.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    let docs = observed_doc_names(pack);
    if !docs.is_empty() {
        parts.push(format!("observed docs: {}", docs.join(", ")));
    }
    if !pack.top_level.is_empty() {
        let shown: Vec<&str> = pack
            .top_level
            .iter()
            .take(MAX_LAYOUT_ENTRIES)
            .map(String::as_str)
            .collect();
        let suffix = if pack.top_level.len() > MAX_LAYOUT_ENTRIES {
            format!(" ({} entries)", pack.top_level.len())
        } else {
            String::new()
        };
        parts.push(format!("top level: {}{suffix}", shown.join(", ")));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

fn bullet_text(line: &str) -> Option<String> {
    let line = line.trim();
    let rest = line
        .strip_prefix("- [ ]")
        .or_else(|| line.strip_prefix("* [ ]"))?;
    let text = rest.trim();
    (!text.is_empty()).then(|| cap_chars(text, MAX_NEXT_ACTION_CHARS))
}

fn plain_bullet_text(line: &str) -> Option<String> {
    let line = line.trim();
    let rest = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))?;
    let text = rest.trim();
    if text.starts_with("[ ]") || text.starts_with("[x]") || text.starts_with("[X]") {
        return None;
    }
    (!text.is_empty()).then(|| cap_chars(text, MAX_NEXT_ACTION_CHARS))
}

fn is_next_steps_heading(line: &str) -> bool {
    let line = line.trim();
    if !line.starts_with('#') {
        return false;
    }
    let heading = line.trim_start_matches('#').trim().to_ascii_lowercase();
    ["next", "todo", "roadmap"]
        .iter()
        .any(|key| heading.contains(key))
}

fn extract_next_actions(pack: &ContextPack) -> Vec<String> {
    let mut actions = Vec::new();
    for section in &pack.sections {
        if !section.title.starts_with("Repo: ") {
            continue;
        }
        // Unchecked checkboxes anywhere in observed repo docs.
        for line in section.content.lines() {
            if let Some(text) = bullet_text(line) {
                actions.push(text);
            }
        }
        // Fallback: bullets under a next/todo/roadmap heading.
        if actions.is_empty() {
            let mut in_section = false;
            for line in section.content.lines() {
                if line.trim().starts_with('#') {
                    in_section = is_next_steps_heading(line);
                    continue;
                }
                if in_section {
                    if let Some(text) = plain_bullet_text(line) {
                        actions.push(text);
                    }
                }
            }
        }
    }
    actions.truncate(MAX_NEXT_ACTIONS);
    actions
}

fn extract_memory_facts(project_dir: &Path, pack: &ContextPack) -> Vec<String> {
    let mut facts = Vec::new();
    if !pack.top_level.is_empty() {
        let shown: Vec<&str> = pack
            .top_level
            .iter()
            .take(MAX_LAYOUT_ENTRIES)
            .map(String::as_str)
            .collect();
        facts.push(format!(
            "Top-level layout at init: {} ({} entries)",
            shown.join(", "),
            pack.top_level.len()
        ));
    }
    let docs = observed_doc_names(pack);
    if !docs.is_empty() {
        facts.push(format!("Docs observed at init: {}", docs.join(", ")));
    }
    if let Ok(repo) = git2::Repository::open(project_dir) {
        if let Ok(remote) = repo.find_remote("origin") {
            if let Some(url) = remote.url() {
                facts.push(format!("Git origin remote: {url}"));
            }
        }
        if let Ok(mut walk) = repo.revwalk() {
            if walk.push_head().is_ok() {
                let subjects: Vec<String> = walk
                    .flatten()
                    .filter_map(|oid| repo.find_commit(oid).ok())
                    .filter_map(|commit| commit.summary().map(str::to_string))
                    .take(MAX_RECENT_COMMITS)
                    .collect();
                if !subjects.is_empty() {
                    facts.push(format!("Recent commits at init: {}", subjects.join("; ")));
                }
            }
        }
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_pack;
    use tempfile::tempdir;

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn commit_all(repo: &git2::Repository, subject: &str) {
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let parents: Vec<git2::Commit> = repo
            .head()
            .ok()
            .and_then(|h| h.target())
            .and_then(|oid| repo.find_commit(oid).ok())
            .into_iter()
            .collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, subject, &tree, &parent_refs)
            .unwrap();
    }

    #[test]
    fn readme_title_and_paragraph_become_the_objective() {
        let tmp = tempdir().unwrap();
        write(
            tmp.path(),
            "README.md",
            "# SiderAgents\n\n[![badge](x)](y)\nLive upgrade target.\nMore prose.\n\n## Install\n",
        );
        let pack = context_pack::build(tmp.path());
        let draft = extract(tmp.path(), &pack);
        assert_eq!(
            draft.objective.as_deref(),
            Some("SiderAgents — Live upgrade target. More prose.")
        );
    }

    #[test]
    fn todo_checkboxes_become_next_actions() {
        let tmp = tempdir().unwrap();
        write(
            tmp.path(),
            "TODO.md",
            "# Todo\n\n- [ ] wire the parser\n- [x] done already\n- [ ] ship it\n",
        );
        let pack = context_pack::build(tmp.path());
        let draft = extract(tmp.path(), &pack);
        assert_eq!(draft.next_actions, ["wire the parser", "ship it"]);
    }

    #[test]
    fn roadmap_bullets_are_the_next_action_fallback() {
        let tmp = tempdir().unwrap();
        write(
            tmp.path(),
            "README.md",
            "# Proj\n\nGoal.\n\n## Roadmap\n\n- first thing\n- second thing\n\n## Other\n\n- not next\n",
        );
        let pack = context_pack::build(tmp.path());
        let draft = extract(tmp.path(), &pack);
        assert_eq!(draft.next_actions, ["first thing", "second thing"]);
    }

    #[test]
    fn git_facts_are_best_effort() {
        let tmp = tempdir().unwrap();
        write(tmp.path(), "README.md", "# P\n\nGoal.\n");
        let repo = git2::Repository::init(tmp.path()).unwrap();
        commit_all(&repo, "first commit");
        write(tmp.path(), "src/main.rs", "fn main() {}\n");
        commit_all(&repo, "second commit");
        let pack = context_pack::build(tmp.path());
        let draft = extract(tmp.path(), &pack);
        assert!(
            draft
                .memory_facts
                .iter()
                .any(|f| f.contains("second commit; first commit")),
            "{:?}",
            draft.memory_facts
        );
        assert!(
            draft
                .memory_facts
                .iter()
                .any(|f| f.starts_with("Docs observed at init: README.md")),
            "{:?}",
            draft.memory_facts
        );
    }

    #[test]
    fn empty_dir_stays_empty() {
        let tmp = tempdir().unwrap();
        let pack = context_pack::build(tmp.path());
        let draft = extract(tmp.path(), &pack);
        assert!(draft.is_empty(), "{draft:?}");
    }
}
