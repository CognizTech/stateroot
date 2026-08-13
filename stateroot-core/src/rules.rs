//! Shared rules federation — product constitution plus harness rule files.
//!
//! Canonical stores:
//! - user: `~/.stateroot/rules/` (always includes shipped `product-intent`)
//! - project: `.stateroot/rules/` (imported project AGENTS.md / CLAUDE.md /
//!   `.cursor/rules`, …)
//!
//! Sync pulls live harness instruction files into the store. Product-intent
//! is owned by StateRoot and is rewritten from the embedded copy on every
//! sync. Imported files keep provenance (`origin` + `origin_path`) and are
//! never written back into the source harness (no projection loop).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::harness_install::{home_dir, BLOCK_BEGIN, BLOCK_END};
use crate::local_store;

/// Schema for `index.json`.
pub const SCHEMA_VERSION: &str = "stateroot.rules.v1";
/// Directory name under a scope root.
pub const RULES_DIR: &str = "rules";
/// Shipped default constitution (always present in the user store).
pub const PRODUCT_INTENT_SLUG: &str = "product-intent";
/// Embedded product-intent markdown.
pub const PRODUCT_INTENT_MD: &str = include_str!("../assets/product-intent.md");

const SKIP_NAMES: &[&str] = &["stateroot.mdc", "stateroot.md"];
const MIN_CHARS: usize = 1;

const GLOBAL_FILES: &[(&str, &str)] = &[
    ("claude-code", ".claude/CLAUDE.md"),
    ("codex", ".codex/AGENTS.md"),
    ("cursor", ".cursor/AGENTS.md"),
    ("gemini-cli", ".gemini/GEMINI.md"),
];
const GLOBAL_DIRS: &[(&str, &str)] = &[
    ("cursor", ".cursor/rules"),
    ("claude-code", ".claude/rules"),
];
const PROJECT_FILES: &[(&str, &str)] = &[
    ("codex", "AGENTS.md"),
    ("claude-code", "CLAUDE.md"),
    ("gemini-cli", "GEMINI.md"),
    ("github-copilot", ".github/copilot-instructions.md"),
];
const PROJECT_DIRS: &[(&str, &str)] = &[("cursor", ".cursor/rules")];

/// Errors from the rules store.
#[derive(Debug, thiserror::Error)]
pub enum RulesError {
    /// Filesystem failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Home directory unresolved.
    #[error("home directory: {0}")]
    Home(String),
}

/// One rule in the shared pool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rule {
    /// Stable slug (`product-intent`, `cursor-no-foo`, …).
    pub slug: String,
    /// First heading or first line.
    pub title: String,
    /// `user` | `project`.
    pub scope: String,
    /// `stateroot` or a harness id.
    pub origin: String,
    /// Source path (or `embedded` for product-intent).
    pub origin_path: String,
    /// True for the shipped constitution.
    #[serde(default)]
    pub product: bool,
    /// Content hash.
    pub sha256: String,
}

/// Result of a sync.
#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    /// Product-intent was written or refreshed.
    pub seeded: bool,
    /// New imported files.
    pub imported: usize,
    /// Existing imports whose source changed.
    pub updated: usize,
    /// Unchanged.
    pub unchanged: usize,
    /// Imports whose origin file disappeared.
    pub pruned: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Index {
    schema_version: String,
    rules: Vec<Rule>,
}

/// User-global rules directory (`~/.stateroot/rules`).
pub fn user_root(home: &Path) -> PathBuf {
    home.join(".stateroot").join(RULES_DIR)
}

/// Project rules directory (`.stateroot/rules`).
pub fn project_root(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(RULES_DIR)
}

fn file_for(root: &Path, rule: &Rule) -> PathBuf {
    if rule.product {
        root.join("product-intent.md")
    } else {
        root.join("imported")
            .join(&rule.origin)
            .join(format!("{}.md", sanitize(&rule.slug)))
    }
}

fn sanitize(slug: &str) -> String {
    slug.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn title_of(text: &str) -> String {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("---") {
            continue;
        }
        let heading = line.trim_start_matches('#').trim();
        if !heading.is_empty() {
            return heading.chars().take(80).collect();
        }
    }
    "untitled rule".into()
}

/// Remove StateRoot managed blocks so we import only the harness's own text.
pub fn strip_managed_blocks(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(BLOCK_BEGIN) {
        out.push_str(&rest[..start]);
        if let Some(end_rel) = rest[start..].find(BLOCK_END) {
            let after = start + end_rel + BLOCK_END.len();
            rest = rest[after..].trim_start_matches(['\n', '\r']);
        } else {
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn skip_name(name: &str) -> bool {
    SKIP_NAMES
        .iter()
        .any(|skip| name.eq_ignore_ascii_case(skip))
}

/// Seed or refresh the shipped product-intent constitution.
pub fn ensure_product_intent(home: &Path) -> Result<bool, RulesError> {
    let root = user_root(home);
    std::fs::create_dir_all(&root)?;
    let path = root.join("product-intent.md");
    let bytes = PRODUCT_INTENT_MD.as_bytes();
    let changed = std::fs::read(&path).ok().as_deref() != Some(bytes);
    if changed {
        std::fs::write(&path, bytes)?;
    }
    let rule = Rule {
        slug: PRODUCT_INTENT_SLUG.into(),
        title: title_of(PRODUCT_INTENT_MD),
        scope: "user".into(),
        origin: "stateroot".into(),
        origin_path: "embedded".into(),
        product: true,
        sha256: sha256_hex(bytes),
    };
    upsert_index(&root, rule)?;
    Ok(changed)
}

fn read_index(root: &Path) -> Index {
    let path = root.join("index.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Index {
            schema_version: SCHEMA_VERSION.into(),
            rules: Vec::new(),
        };
    };
    serde_json::from_str(&text).unwrap_or(Index {
        schema_version: SCHEMA_VERSION.into(),
        rules: Vec::new(),
    })
}

fn write_index(root: &Path, index: &Index) -> Result<(), RulesError> {
    std::fs::create_dir_all(root)?;
    let pretty = serde_json::to_string_pretty(index).unwrap_or_else(|_| "{}".into());
    std::fs::write(root.join("index.json"), format!("{pretty}\n"))?;
    Ok(())
}

fn upsert_index(root: &Path, rule: Rule) -> Result<(), RulesError> {
    let mut index = read_index(root);
    index.schema_version = SCHEMA_VERSION.into();
    if let Some(existing) = index.rules.iter_mut().find(|r| r.slug == rule.slug) {
        *existing = rule;
    } else {
        index.rules.push(rule);
    }
    write_index(root, &index)
}

/// List rules for one scope (product-intent first).
pub fn list_scope(project_dir: &Path, home: &Path, scope: &str) -> Vec<Rule> {
    let root = if scope == "user" {
        user_root(home)
    } else {
        project_root(project_dir)
    };
    let mut rules = read_index(&root).rules;
    rules.sort_by(|a, b| match (a.product, b.product) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.slug.cmp(&b.slug),
    });
    rules
}

/// Union of user + project rules (product-intent first).
pub fn list_all(project_dir: &Path, home: &Path) -> Vec<Rule> {
    let mut out = list_scope(project_dir, home, "user");
    let mut seen: std::collections::BTreeSet<String> = out.iter().map(|r| r.slug.clone()).collect();
    for rule in list_scope(project_dir, home, "project") {
        if seen.insert(rule.slug.clone()) {
            out.push(rule);
        }
    }
    out
}

/// Load the markdown body for a slug.
pub fn show(project_dir: &Path, home: &Path, slug: &str) -> Option<(Rule, String)> {
    for scope in ["user", "project"] {
        let root = if scope == "user" {
            user_root(home)
        } else {
            project_root(project_dir)
        };
        let index = read_index(&root);
        if let Some(rule) = index.rules.into_iter().find(|r| r.slug == slug) {
            let path = file_for(&root, &rule);
            if let Ok(text) = std::fs::read_to_string(path) {
                return Some((rule, text));
            }
        }
    }
    None
}

/// Full digest section: product-intent body plus every imported rule body.
pub fn compose_section(project_dir: &Path, home: &Path) -> String {
    let rules = list_all(project_dir, home);
    let mut out = String::from("## Shared Rules\n\n");
    if rules.is_empty() {
        out.push_str(
            "Empty. Run `stateroot rules sync` (product-intent is included by default).\n",
        );
        return out;
    }
    for rule in &rules {
        let body = show(project_dir, home, &rule.slug)
            .map(|(_, text)| text)
            .unwrap_or_default();
        out.push_str(&format!(
            "### {} [{} / {}]\n\n",
            rule.slug, rule.origin, rule.scope
        ));
        if !rule.title.is_empty() && rule.title != rule.slug {
            out.push_str(&format!("{}\n\n", rule.title));
        }
        out.push_str(body.trim());
        out.push_str("\n\n");
    }
    out.push_str("`stateroot rules list` / `stateroot rules sync`\n");
    out
}

struct Candidate {
    harness: String,
    origin_path: PathBuf,
    scope: &'static str,
    body: String,
}

fn collect_file(path: &Path, harness: &str, scope: &'static str, out: &mut Vec<Candidate>) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let body = strip_managed_blocks(&raw);
    if body.chars().count() < MIN_CHARS {
        return;
    }
    out.push(Candidate {
        harness: harness.into(),
        origin_path: path.to_path_buf(),
        scope,
        body,
    });
}

fn collect_dir(dir: &Path, harness: &str, scope: &'static str, out: &mut Vec<Candidate>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if skip_name(name) {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "md" | "mdc" | "txt") {
            continue;
        }
        collect_file(&path, harness, scope, out);
    }
}

fn discover(project_dir: &Path, home: &Path) -> Vec<Candidate> {
    let mut out = Vec::new();
    for (harness, rel) in GLOBAL_FILES {
        collect_file(&home.join(rel), harness, "user", &mut out);
    }
    for (harness, rel) in GLOBAL_DIRS {
        collect_dir(&home.join(rel), harness, "user", &mut out);
    }
    if local_store::is_stateroot_dir(project_dir) || project_dir.exists() {
        for (harness, rel) in PROJECT_FILES {
            collect_file(&project_dir.join(rel), harness, "project", &mut out);
        }
        for (harness, rel) in PROJECT_DIRS {
            collect_dir(&project_dir.join(rel), harness, "project", &mut out);
        }
    }
    out
}

fn slug_for(harness: &str, path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("rule")
        .to_lowercase();
    format!("{harness}-{}", sanitize(&stem))
}

fn import_one(
    project_dir: &Path,
    home: &Path,
    candidate: Candidate,
    report: &mut SyncReport,
) -> Result<(), RulesError> {
    let slug = slug_for(&candidate.harness, &candidate.origin_path);
    let hash = sha256_hex(candidate.body.as_bytes());
    let root = if candidate.scope == "user" {
        user_root(home)
    } else {
        project_root(project_dir)
    };
    let rule = Rule {
        slug: slug.clone(),
        title: title_of(&candidate.body),
        scope: candidate.scope.into(),
        origin: candidate.harness.clone(),
        origin_path: candidate.origin_path.display().to_string(),
        product: false,
        sha256: hash,
    };
    let dest = file_for(&root, &rule);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(&dest).ok();
    if existing.as_deref() == Some(candidate.body.as_str()) {
        report.unchanged += 1;
    } else if existing.is_some() {
        std::fs::write(&dest, candidate.body.as_bytes())?;
        report.updated += 1;
    } else {
        std::fs::write(&dest, candidate.body.as_bytes())?;
        report.imported += 1;
    }
    upsert_index(&root, rule)?;
    Ok(())
}

fn prune_missing(
    project_dir: &Path,
    home: &Path,
    live_paths: &std::collections::BTreeSet<String>,
    report: &mut SyncReport,
) -> Result<(), RulesError> {
    for scope in ["user", "project"] {
        let root = if scope == "user" {
            user_root(home)
        } else {
            project_root(project_dir)
        };
        let mut index = read_index(&root);
        let mut kept = Vec::new();
        for rule in index.rules.drain(..) {
            if rule.product || live_paths.contains(&rule.origin_path) {
                kept.push(rule);
                continue;
            }
            let dest = file_for(&root, &rule);
            let _ = std::fs::remove_file(dest);
            report.pruned += 1;
        }
        index.rules = kept;
        write_index(&root, &index)?;
    }
    Ok(())
}

/// Seed product-intent and pull live harness rule files into the shared store.
pub fn sync(project_dir: &Path, home: &Path) -> Result<SyncReport, RulesError> {
    let mut report = SyncReport {
        seeded: ensure_product_intent(home)?,
        ..SyncReport::default()
    };
    let discovered = discover(project_dir, home);
    let live: std::collections::BTreeSet<String> = discovered
        .iter()
        .map(|c| c.origin_path.display().to_string())
        .collect();
    for candidate in discovered {
        import_one(project_dir, home, candidate, &mut report)?;
    }
    prune_missing(project_dir, home, &live, &mut report)?;
    Ok(report)
}

/// Seed product-intent using the process home directory.
pub fn ensure_product_intent_from_env() -> Result<bool, RulesError> {
    let home = home_dir().map_err(|e| RulesError::Home(e.to_string()))?;
    ensure_product_intent(&home)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_intent_is_seeded_and_not_overwritten_by_imports() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        assert!(ensure_product_intent(home.path()).unwrap());
        assert!(!ensure_product_intent(home.path()).unwrap());
        let listed = list_scope(project.path(), home.path(), "user");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].slug, PRODUCT_INTENT_SLUG);
        assert!(listed[0].product);
        let body =
            std::fs::read_to_string(user_root(home.path()).join("product-intent.md")).unwrap();
        assert!(body.contains("Product Intent Is a Hard Constraint"));
        assert!(body.contains("Never Silently Redesign the Product"));
    }

    #[test]
    fn sync_imports_harness_rules_and_skips_stateroot_projection() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        crate::local_store::init_skeleton(project.path(), "p1", "demo", "default").unwrap();

        std::fs::create_dir_all(home.path().join(".codex")).unwrap();
        std::fs::write(
            home.path().join(".codex/AGENTS.md"),
            "# Codex house rules\n\nNever commit secrets.\n\n<!-- stateroot:begin -->\nmanaged\n<!-- stateroot:end -->\n",
        )
        .unwrap();
        std::fs::create_dir_all(project.path().join(".cursor/rules")).unwrap();
        std::fs::write(
            project.path().join(".cursor/rules/no-foo.mdc"),
            "---\ndescription: no foo\n---\n\n# No foo\n\nDo not introduce a foo layer.\n",
        )
        .unwrap();
        std::fs::write(
            project.path().join(".cursor/rules/stateroot.mdc"),
            "# stateroot projection — must not be imported\n",
        )
        .unwrap();
        std::fs::write(
            project.path().join("AGENTS.md"),
            "# Project agents\n\nPrefer the existing module layout.\n",
        )
        .unwrap();

        let report = sync(project.path(), home.path()).unwrap();
        assert!(report.seeded);
        assert!(report.imported >= 3, "{report:?}");

        let all = list_all(project.path(), home.path());
        let slugs: Vec<&str> = all.iter().map(|r| r.slug.as_str()).collect();
        assert!(slugs.contains(&"product-intent"));
        assert!(slugs.iter().any(|s| s.contains("codex")));
        assert!(slugs.iter().any(|s| s.contains("no-foo")));
        assert!(slugs.iter().any(|s| s.contains("agents")));
        assert!(!slugs.iter().any(|s| s.contains("stateroot")));

        let (_, codex) = show(project.path(), home.path(), "codex-agents").expect("codex");
        assert!(codex.contains("Never commit secrets"));
        assert!(!codex.contains("stateroot:begin"));

        let section = compose_section(project.path(), home.path());
        assert!(section.contains("product-intent"));
        assert!(
            section.contains("Preserve product intent")
                || section.contains("product intent")
                || PRODUCT_INTENT_MD
                    .lines()
                    .find(|l| l.len() > 20)
                    .is_some_and(|line| section.contains(line)),
            "digest must include product-intent body, not titles only: {section}"
        );
        assert!(section.contains("Never commit secrets") || section.contains("codex"));
    }

    #[test]
    fn strip_managed_blocks_leaves_foreign_text() {
        let text = "keep\n<!-- stateroot:begin -->\ngone\n<!-- stateroot:end -->\nalso keep\n";
        let stripped = strip_managed_blocks(text);
        assert_eq!(stripped, "keep\nalso keep");
    }
}
