//! Compiled long-term wiki (LLM-wiki style): catalog in, corpus out.
//!
//! Layout under `.stateroot/`:
//! - `wiki/SCHEMA.md` — page conventions
//! - `wiki/index.md` — one line per page (injected into digest)
//! - `wiki/log.md` — append-only compile/ingest/lint log (last N lines in digest)
//! - `memories/pages/*.md` — compiled entity/concept pages (never dumped into digest)
//! - `memories/pages/_inbox.md` — deterministic distill floor

use std::fs;
use std::path::{Path, PathBuf};

use crate::local_store;

/// Wiki directory relative to `.stateroot/`.
pub const WIKI_DIR: &str = "wiki";
/// Compiled pages directory relative to `.stateroot/`.
pub const PAGES_DIR: &str = "memories/pages";
/// Inbox page name.
pub const INBOX_PAGE: &str = "_inbox.md";
/// Index file.
pub const INDEX_FILE: &str = "index.md";
/// Log file.
pub const LOG_FILE: &str = "log.md";
/// Schema file.
pub const SCHEMA_FILE: &str = "SCHEMA.md";
/// How many trailing log lines to inject into the digest.
pub const DIGEST_LOG_LINES: usize = 30;

const DEFAULT_SCHEMA: &str = r#"# StateRoot Wiki Schema

Compiled project knowledge. Page bodies are pulled on demand (`stateroot wiki show`
or `memory_recall`) — only `index.md` and recent `log.md` lines enter the session digest.

## Conventions

- One page per entity or concept under `.stateroot/memories/pages/<slug>.md`
- `index.md` lists every page as: `- [path](path) - summary (kind)`
- `log.md` is append-only (ingest / lint / compile)
- `_inbox.md` holds deterministic distill bullets until an agentic compile files them
- Do not put taste/judgment here — those are learnings (`learn record`)
- Do not put identity here — that is soul / USER.md
"#;

const DEFAULT_INDEX: &str = "# StateRoot Wiki Index\n\n";

/// Errors from wiki IO.
#[derive(Debug, thiserror::Error)]
pub enum WikiError {
    /// Filesystem failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// One lint finding.
#[derive(Debug, Clone, PartialEq)]
pub struct LintFinding {
    /// Short code.
    pub code: String,
    /// Human message.
    pub message: String,
    /// Related path when known.
    pub path: Option<String>,
}

/// Ensure wiki skeleton exists.
pub fn ensure_layout(project_dir: &Path) -> Result<(), WikiError> {
    let root = local_store::root(project_dir);
    let wiki = root.join(WIKI_DIR);
    let pages = root.join(PAGES_DIR);
    fs::create_dir_all(&wiki)?;
    fs::create_dir_all(&pages)?;
    let schema = wiki.join(SCHEMA_FILE);
    if !schema.exists() {
        fs::write(&schema, DEFAULT_SCHEMA)?;
    }
    let index = wiki.join(INDEX_FILE);
    if !index.exists() {
        fs::write(&index, DEFAULT_INDEX)?;
    }
    let log = wiki.join(LOG_FILE);
    if !log.exists() {
        fs::write(&log, "")?;
    }
    let inbox = pages.join(INBOX_PAGE);
    if !inbox.exists() {
        fs::write(
            &inbox,
            "# Inbox\n\nDeterministic distill floor — unique mined notes awaiting compile.\n",
        )?;
        upsert_index(
            project_dir,
            &format!("{PAGES_DIR}/{INBOX_PAGE}"),
            "deterministic distill inbox",
            "inbox",
        )?;
    }
    Ok(())
}

fn wiki_path(project_dir: &Path, file: &str) -> PathBuf {
    local_store::root(project_dir).join(WIKI_DIR).join(file)
}

fn pages_dir(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(PAGES_DIR)
}

/// Render an index bullet (server `render_index_entry` shape).
pub fn render_index_entry(path: &str, summary: &str, kind: &str) -> String {
    format!(
        "- [{path}]({path}) - {summary} ({kind})",
        path = path.trim(),
        summary = summary.trim(),
        kind = kind.trim()
    )
}

/// Upsert one index line by path.
pub fn upsert_index(
    project_dir: &Path,
    path: &str,
    summary: &str,
    kind: &str,
) -> Result<(), WikiError> {
    ensure_layout(project_dir)?;
    let index_path = wiki_path(project_dir, INDEX_FILE);
    let bullet = render_index_entry(path, summary, kind);
    let existing = fs::read_to_string(&index_path).unwrap_or_else(|_| DEFAULT_INDEX.to_string());
    let mut lines: Vec<String> = existing.lines().map(|l| l.to_string()).collect();
    let mut replaced = false;
    for line in &mut lines {
        if line_path(line).as_deref() == Some(path) {
            *line = bullet.clone();
            replaced = true;
            break;
        }
    }
    if !replaced {
        if let Some(last) = lines.last() {
            if !last.trim().is_empty() {
                lines.push(String::new());
            }
        }
        lines.push(bullet);
    }
    let mut body = lines.join("\n");
    if !body.ends_with('\n') {
        body.push('\n');
    }
    fs::write(index_path, body)?;
    Ok(())
}

fn line_path(line: &str) -> Option<String> {
    let line = line.trim();
    if !line.starts_with("- [") {
        return None;
    }
    let rest = line.strip_prefix("- [")?;
    let end = rest.find(']')?;
    Some(rest[..end].to_string())
}

/// Append one log line.
pub fn append_log(project_dir: &Path, summary: &str) -> Result<(), WikiError> {
    ensure_layout(project_dir)?;
    let path = wiki_path(project_dir, LOG_FILE);
    let mut body = fs::read_to_string(&path).unwrap_or_default();
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&format!(
        "- {} — {}\n",
        local_store::now_rfc3339(),
        summary.trim()
    ));
    fs::write(path, body)?;
    Ok(())
}

/// Read index.md body (empty string if missing).
pub fn read_index(project_dir: &Path) -> String {
    fs::read_to_string(wiki_path(project_dir, INDEX_FILE)).unwrap_or_default()
}

/// Last N log lines.
pub fn recent_log(project_dir: &Path, n: usize) -> String {
    let text = fs::read_to_string(wiki_path(project_dir, LOG_FILE)).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Digest section: index + recent log (no page bodies).
pub fn compose_digest_section(project_dir: &Path) -> String {
    let _ = ensure_layout(project_dir);
    let index = read_index(project_dir);
    let log = recent_log(project_dir, DIGEST_LOG_LINES);
    let mut out = String::from("## Wiki (catalog)\n\n");
    let index_trim = index.trim();
    if index_trim.is_empty() || index_trim == "# StateRoot Wiki Index" {
        out.push_str("(empty index — pages appear after distill/compile)\n");
    } else {
        out.push_str(index_trim);
        out.push('\n');
    }
    if !log.trim().is_empty() {
        out.push_str("\n### Recent wiki log\n\n");
        out.push_str(log.trim());
        out.push('\n');
    }
    out
}

/// Resolve a page path (accepts `memories/pages/foo.md`, `foo.md`, or `foo`).
pub fn resolve_page_path(project_dir: &Path, rel: &str) -> PathBuf {
    let rel = rel.trim().trim_start_matches('/');
    let root = local_store::root(project_dir);
    if rel.starts_with("memories/") || rel.starts_with("wiki/") {
        return root.join(rel);
    }
    let name = if rel.ends_with(".md") {
        rel.to_string()
    } else {
        format!("{rel}.md")
    };
    pages_dir(project_dir).join(name)
}

/// Read a page body.
pub fn show(project_dir: &Path, rel: &str) -> Result<String, WikiError> {
    let path = resolve_page_path(project_dir, rel);
    Ok(fs::read_to_string(path)?)
}

/// Write/overwrite a page body and refresh the index.
pub fn write_page(
    project_dir: &Path,
    slug: &str,
    body: &str,
    summary: &str,
    kind: &str,
) -> Result<PathBuf, WikiError> {
    ensure_layout(project_dir)?;
    let slug = slug.trim().trim_end_matches(".md");
    let file = format!("{slug}.md");
    let path = pages_dir(project_dir).join(&file);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = if body.trim().starts_with('#') {
        format!("{}\n", body.trim())
    } else {
        format!("# {slug}\n\n{}\n", body.trim())
    };
    fs::write(&path, text)?;
    let rel = format!("{PAGES_DIR}/{file}");
    upsert_index(project_dir, &rel, summary, kind)?;
    Ok(path)
}

/// Append unique bullets into `_inbox.md` (deterministic distill floor).
/// Returns how many new bullets were added.
pub fn append_inbox_bullets(project_dir: &Path, bullets: &[String]) -> Result<usize, WikiError> {
    ensure_layout(project_dir)?;
    let path = pages_dir(project_dir).join(INBOX_PAGE);
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut seen: std::collections::BTreeSet<String> = existing
        .lines()
        .filter_map(|l| {
            let l = l.trim().strip_prefix("- ")?.trim();
            (!l.is_empty()).then(|| normalize(l))
        })
        .collect();
    let mut added = 0usize;
    let mut body = existing;
    if !body.ends_with('\n') && !body.is_empty() {
        body.push('\n');
    }
    for bullet in bullets {
        let bullet = bullet.trim();
        if bullet.len() < 8 {
            continue;
        }
        let key = normalize(bullet);
        if !seen.insert(key) {
            continue;
        }
        body.push_str(&format!("- {bullet}\n"));
        added += 1;
    }
    if added > 0 {
        fs::write(&path, body)?;
        upsert_index(
            project_dir,
            &format!("{PAGES_DIR}/{INBOX_PAGE}"),
            "deterministic distill inbox",
            "inbox",
        )?;
    }
    Ok(added)
}

fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// List page paths under memories/pages (relative to `.stateroot/`).
/// Recursive — imported harness pages live in `harness/<harness>/` subdirs.
pub fn list_pages(project_dir: &Path) -> Vec<String> {
    let dir = pages_dir(project_dir);
    let mut out = Vec::new();
    walk_pages(&dir, &dir, &mut out);
    out.sort();
    out
}

fn walk_pages(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    items.sort();
    for path in items {
        if path.is_dir() {
            walk_pages(base, &path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Ok(rel) = path.strip_prefix(base) {
                let rel: String = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push(format!("{PAGES_DIR}/{rel}"));
            }
        }
    }
}

/// Lint: pages missing from index, index orphans, duplicate titles.
pub fn lint(project_dir: &Path) -> Result<Vec<LintFinding>, WikiError> {
    ensure_layout(project_dir)?;
    let mut findings = Vec::new();
    let index = read_index(project_dir);
    let mut index_paths: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut titles: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for line in index.lines() {
        if let Some(p) = line_path(line) {
            if !index_paths.insert(p.clone()) {
                findings.push(LintFinding {
                    code: "duplicate_index".into(),
                    message: format!("duplicate index entry for {p}"),
                    path: Some(p.clone()),
                });
            }
            let abs = local_store::root(project_dir).join(&p);
            if !abs.is_file() {
                findings.push(LintFinding {
                    code: "orphan_index".into(),
                    message: format!("index lists missing page {p}"),
                    path: Some(p.clone()),
                });
            }
            if let Ok(text) = fs::read_to_string(&abs) {
                if let Some(title) = text.lines().next().map(|l| l.trim().to_string()) {
                    titles.entry(title).or_default().push(p);
                }
            }
        }
    }
    for page in list_pages(project_dir) {
        if !index_paths.contains(&page) {
            findings.push(LintFinding {
                code: "missing_index".into(),
                message: format!("page {page} not listed in index.md"),
                path: Some(page),
            });
        }
    }
    for (title, paths) in titles {
        if paths.len() > 1 {
            findings.push(LintFinding {
                code: "duplicate_title".into(),
                message: format!("title {title:?} used by {}", paths.join(", ")),
                path: None,
            });
        }
    }
    Ok(findings)
}

/// Content hash of inbox + episodic + spool for skip-if-unchanged.
pub fn compile_input_hash(project_dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    let root = local_store::root(project_dir);
    let mut hasher = Sha256::new();
    for rel in [
        local_store::EPISODIC_PATH,
        "spool/observations.jsonl",
        &format!("{PAGES_DIR}/{INBOX_PAGE}"),
    ] {
        let text = fs::read_to_string(root.join(rel)).unwrap_or_default();
        hasher.update(rel.as_bytes());
        hasher.update(text.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        local_store::init_skeleton(dir.path(), "p", "P", "default").unwrap();
        dir
    }

    #[test]
    fn inbox_dedupes_and_indexes() {
        let p = project();
        ensure_layout(p.path()).unwrap();
        let n = append_inbox_bullets(
            p.path(),
            &[
                "always re-run clippy after edits".into(),
                "always re-run clippy after edits".into(),
                "prefer small diffs".into(),
            ],
        )
        .unwrap();
        assert_eq!(n, 2);
        let n2 = append_inbox_bullets(p.path(), &["prefer small diffs".into()]).unwrap();
        assert_eq!(n2, 0);
        let index = read_index(p.path());
        assert!(index.contains("_inbox.md"));
    }

    #[test]
    fn lint_finds_missing_index() {
        let p = project();
        ensure_layout(p.path()).unwrap();
        write_page(
            p.path(),
            "auth",
            "Auth lives in crates/auth",
            "auth module",
            "entity",
        )
        .unwrap();
        // Drop index entry artificially
        fs::write(
            wiki_path(p.path(), INDEX_FILE),
            "# StateRoot Wiki Index\n\n",
        )
        .unwrap();
        let findings = lint(p.path()).unwrap();
        assert!(findings.iter().any(|f| f.code == "missing_index"));
    }

    #[test]
    fn digest_section_has_no_page_body() {
        let p = project();
        write_page(
            p.path(),
            "deploy",
            "SECRET_DEPLOY_DETAIL_SHOULD_NOT_APPEAR",
            "deploy notes",
            "concept",
        )
        .unwrap();
        let section = compose_digest_section(p.path());
        assert!(section.contains("deploy.md"));
        assert!(!section.contains("SECRET_DEPLOY_DETAIL_SHOULD_NOT_APPEAR"));
    }
}
