//! Compiled long-term wiki (LLM-wiki style): catalog in, corpus out.
//! The bundle under `.stateroot/wiki/` is an OKF v0.2 bundle
//! (<https://github.com/GoogleCloudPlatform/knowledge-catalog>, okf/SPEC.md):
//!
//! - `wiki/SCHEMA.md` — page conventions (`type: Reference` frontmatter)
//! - `wiki/index.md` — one line per page (injected into digest); root
//!   frontmatter carries `okf_version`
//! - `wiki/log.md` — date-grouped update log, newest first (OKF §9)
//! - `wiki/pages/**/*.md` — compiled entity/concept pages with OKF
//!   frontmatter (never dumped into the digest)
//! - `wiki/pages/_inbox.md` — deterministic distill floor (`status: draft`)
//!
//! Provenance rule: `generated`/`sources` are written only when honestly
//! known. Unknown provenance stays absent — never fabricated timestamps.

use std::fs;
use std::path::{Path, PathBuf};

use serde_yaml::{Mapping, Value};

use crate::local_store;

/// Wiki directory relative to `.stateroot/` (the OKF bundle root).
pub const WIKI_DIR: &str = "wiki";
/// Compiled pages directory relative to `.stateroot/` (inside the bundle).
pub const PAGES_DIR: &str = "wiki/pages";
/// Pre-OKF pages location; migrated into `wiki/pages/` on first touch.
pub const LEGACY_PAGES_DIR: &str = "memories/pages";
/// Inbox page name.
pub const INBOX_PAGE: &str = "_inbox.md";
/// Index file (reserved OKF filename).
pub const INDEX_FILE: &str = "index.md";
/// Log file (reserved OKF filename).
pub const LOG_FILE: &str = "log.md";
/// Schema file.
pub const SCHEMA_FILE: &str = "SCHEMA.md";
/// OKF version this bundle targets.
pub const OKF_VERSION: &str = "0.2";
/// How many leading log lines (newest first) to inject into the digest.
pub const DIGEST_LOG_LINES: usize = 30;

const LOG_HEADER: &str = "# Directory Update Log";

const DEFAULT_SCHEMA: &str = r#"---
type: Reference
---

# StateRoot Wiki Schema

Compiled project knowledge. This directory is an OKF v0.2 bundle: every page
carries YAML frontmatter with at least a `type` key. Page bodies are pulled on
demand (`stateroot wiki show` or `memory_recall`) — only `index.md` and recent
`log.md` lines enter the session digest.

## Conventions

- One page per entity or concept under `.stateroot/wiki/pages/<slug>.md`
- Page frontmatter: `type` (required), `title`, `description`, and — only when
  honestly known — `generated` / `sources` / `status`. Unknown provenance stays
  absent; StateRoot never fabricates timestamps.
- `index.md` lists every page as: `- [path](path) - summary (kind)` and carries
  the bundle-root `okf_version` frontmatter
- `log.md` is date-grouped, newest first (OKF §9)
- `_inbox.md` holds deterministic distill bullets until an agentic compile
  files them (`status: draft`)
- Do not put taste/judgment here — those are learnings (`learn record`)
- Do not put identity here — that is soul / USER.md
- `stateroot wiki lint` verifies OKF conformance (frontmatter, `type`,
  `okf_version`, reserved filenames)
"#;

const DEFAULT_INDEX: &str = "---\nokf_version: \"0.2\"\n---\n\n# StateRoot Wiki Index\n\n";

const DEFAULT_INBOX: &str = "---\ntype: Inbox\nstatus: draft\n---\n\n# Inbox\n\nDeterministic distill floor — unique mined notes awaiting compile.\n";

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

// ---------------------------------------------------------------------------
// Frontmatter helpers
// ---------------------------------------------------------------------------

/// Split a document into (frontmatter YAML, body). The frontmatter block is a
/// leading `---` line closed by the next `---` line. Our own frontmatter is
/// flat and never contains a `---` line inside a value.
fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return (None, text);
    };
    let Some(end) = rest.find("\n---") else {
        return (None, text);
    };
    let fm = &rest[..end];
    let after = &rest[end + 4..];
    let body = after.strip_prefix('\n').unwrap_or(after);
    (Some(fm), body)
}

/// Parse frontmatter YAML into a mapping (empty mapping on parse failure).
fn parse_frontmatter(yaml: &str) -> Mapping {
    serde_yaml::from_str::<Mapping>(yaml).unwrap_or_default()
}

fn yaml_str(key: &str) -> Value {
    Value::String(key.to_string())
}

fn get_str(map: &Mapping, key: &str) -> Option<String> {
    map.get(yaml_str(key))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
}

/// OKF `type` for one of our internal kinds. Free-form but descriptive.
fn page_type_for_kind(kind: &str) -> String {
    let k = kind.trim();
    if k.is_empty() {
        return "Concept".to_string();
    }
    if k.eq_ignore_ascii_case("harness") {
        return "Harness Note".to_string();
    }
    if k.eq_ignore_ascii_case("inbox") {
        return "Inbox".to_string();
    }
    let mut chars = k.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => "Concept".to_string(),
    }
}

/// Render frontmatter mapping + body into a page document.
fn render_page(fm: &Mapping, body: &str) -> String {
    let yaml = if fm.is_empty() {
        String::new()
    } else {
        serde_yaml::to_string(fm).unwrap_or_default()
    };
    if yaml.trim().is_empty() {
        format!("{}\n", body.trim())
    } else {
        format!("---\n{yaml}---\n\n{}\n", body.trim())
    }
}

/// Build or refresh a page's OKF frontmatter. Unknown keys from an existing
/// page are preserved (OKF SHOULD). `extra` carries caller-supplied families
/// (`sources`, extension keys) and wins over preserved values on conflict.
/// `generated.at` is refreshed only when the body actually changed; when the
/// body is unchanged and a `generated` block exists, it is kept verbatim.
pub fn conform_page(
    existing: Option<&str>,
    kind: &str,
    title: &str,
    summary: &str,
    actor: Option<&str>,
    extra: &Mapping,
    body: &str,
) -> String {
    let (mut map, body_changed) = match existing {
        Some(text) => {
            let (fm, old_body) = split_frontmatter(text);
            let map = fm.map(parse_frontmatter).unwrap_or_default();
            let changed = old_body.trim() != body.trim();
            (map, changed)
        }
        None => (Mapping::new(), true),
    };

    // Canonical keys in OKF order; preserve unknown keys by only setting ours.
    let mut ordered = Mapping::new();
    ordered.insert(yaml_str("type"), Value::String(page_type_for_kind(kind)));
    let title = if title.trim().is_empty() {
        get_str(&map, "title")
    } else {
        Some(title.trim().to_string())
    };
    if let Some(t) = title {
        ordered.insert(yaml_str("title"), Value::String(t));
    }
    let description = if summary.trim().is_empty() {
        get_str(&map, "description")
    } else {
        Some(summary.trim().to_string())
    };
    if let Some(d) = description {
        ordered.insert(yaml_str("description"), Value::String(d));
    }
    // Lifecycle/provenance families pass through from the existing page.
    for key in ["status", "stale_after"] {
        if let Some(v) = map.get(yaml_str(key)) {
            ordered.insert(yaml_str(key), v.clone());
        }
    }
    let existing_generated = map.get(yaml_str("generated")).cloned();
    if !body_changed {
        if let Some(g) = existing_generated {
            ordered.insert(yaml_str("generated"), g);
        }
    } else if let Some(actor) = actor {
        let mut g = Mapping::new();
        g.insert(yaml_str("by"), Value::String(actor.to_string()));
        g.insert(yaml_str("at"), Value::String(local_store::now_rfc3339()));
        ordered.insert(yaml_str("generated"), Value::Mapping(g));
    }
    let sources = extra
        .get(yaml_str("sources"))
        .or_else(|| map.get(yaml_str("sources")));
    if let Some(sources) = sources {
        ordered.insert(yaml_str("sources"), sources.clone());
    }
    // Unknown/extension keys ride along untouched; caller-supplied `extra`
    // keys win on conflict (they are the caller's current truth).
    for (k, v) in &map {
        if let Some(key) = k.as_str() {
            if matches!(
                key,
                "type"
                    | "title"
                    | "description"
                    | "status"
                    | "stale_after"
                    | "generated"
                    | "sources"
            ) {
                continue;
            }
        }
        ordered.insert(k.clone(), v.clone());
    }
    for (k, v) in extra {
        if let Some(key) = k.as_str() {
            if matches!(
                key,
                "type"
                    | "title"
                    | "description"
                    | "status"
                    | "stale_after"
                    | "generated"
                    | "sources"
            ) {
                continue;
            }
        }
        ordered.insert(k.clone(), v.clone());
    }
    let _ = &mut map;
    render_page(&ordered, body)
}

/// Ensure wiki skeleton exists, migrating legacy layouts on the way.
pub fn ensure_layout(project_dir: &Path) -> Result<(), WikiError> {
    let root = local_store::root(project_dir);
    let wiki = root.join(WIKI_DIR);
    let pages = root.join(PAGES_DIR);
    fs::create_dir_all(&wiki)?;
    // Move the pre-OKF pages tree into the bundle before any skeleton file
    // could shadow it (notably `_inbox.md`).
    let legacy = root.join(LEGACY_PAGES_DIR);
    if legacy.is_dir() {
        move_missing(&legacy, &pages)?;
        remove_empty_dirs(&legacy);
        if legacy
            .read_dir()
            .map(|mut r| r.next().is_none())
            .unwrap_or(false)
        {
            let _ = fs::remove_dir(&legacy);
        }
    }
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
        fs::write(&inbox, DEFAULT_INBOX)?;
        upsert_index(
            project_dir,
            &format!("{PAGES_DIR}/{INBOX_PAGE}"),
            "deterministic distill inbox",
            "inbox",
        )?;
    }
    migrate_okf(project_dir)?;
    Ok(())
}

/// Recursively move files from `from` into `to`, never overwriting an
/// existing target. Directory moves are rename-first with a copy fallback.
fn move_missing(from: &Path, to: &Path) -> Result<(), WikiError> {
    let Ok(entries) = fs::read_dir(from) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let src = entry.path();
        let dest = to.join(entry.file_name());
        if src.is_dir() {
            move_missing(&src, &dest)?;
            remove_empty_dirs(&src);
            if src
                .read_dir()
                .map(|mut r| r.next().is_none())
                .unwrap_or(false)
            {
                let _ = fs::remove_dir(&src);
            }
        } else if !dest.exists() {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            if fs::rename(&src, &dest).is_err() {
                fs::copy(&src, &dest)?;
                let _ = fs::remove_file(&src);
            }
        }
    }
    Ok(())
}

fn remove_empty_dirs(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            remove_empty_dirs(&path);
        }
    }
    if dir
        .read_dir()
        .map(|mut r| r.next().is_none())
        .unwrap_or(false)
    {
        let _ = fs::remove_dir(dir);
    }
}

fn wiki_path(project_dir: &Path, file: &str) -> PathBuf {
    local_store::root(project_dir).join(WIKI_DIR).join(file)
}

fn pages_dir(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(PAGES_DIR)
}

/// Parse index body lines into path -> (summary, kind).
fn parse_index_entries(text: &str) -> std::collections::HashMap<String, (String, String)> {
    let mut out = std::collections::HashMap::new();
    for line in text.lines() {
        let Some(path) = line_path(line) else {
            continue;
        };
        // Format: `- [path](path) - summary (kind)`
        let after_link = match line.find(") - ") {
            Some(i) => &line[i + 4..],
            None => continue,
        };
        let (summary, kind) = match after_link.rfind(" (") {
            Some(i) if after_link.ends_with(')') => (
                after_link[..i].trim().to_string(),
                after_link[i + 2..after_link.len() - 1].trim().to_string(),
            ),
            _ => (after_link.trim().to_string(), String::new()),
        };
        out.insert(path, (summary, kind));
    }
    out
}

/// True when the bundle-root index carries the `okf_version` frontmatter key.
fn index_declares_okf(raw: &str) -> bool {
    let (fm, _) = split_frontmatter(raw);
    fm.map(|f| parse_frontmatter(f).contains_key(yaml_str("okf_version")))
        .unwrap_or(false)
}

/// One-shot OKF v0.2 backfill for pre-conformance projects. Guarded by the
/// bundle-root index frontmatter: once `okf_version` is declared, this is a
/// no-op. Provenance is never fabricated — pages of unknown origin get
/// `type`/`title`/`description` only.
pub fn migrate_okf(project_dir: &Path) -> Result<(), WikiError> {
    let root = local_store::root(project_dir);
    let index_path = root.join(WIKI_DIR).join(INDEX_FILE);
    let raw = fs::read_to_string(&index_path).unwrap_or_default();
    if index_declares_okf(&raw) {
        return Ok(());
    }

    // Backfill pages lacking frontmatter (legacy move already happened in
    // ensure_layout before this runs). Legacy index lines still reference
    // `memories/pages/…`, so index both spellings for the kind/summary lookup.
    let mut entries = parse_index_entries(&raw);
    for (path, meta) in parse_index_entries(&raw) {
        if let Some(tail) = path.strip_prefix(&format!("{LEGACY_PAGES_DIR}/")) {
            entries.insert(format!("{PAGES_DIR}/{tail}"), meta);
        }
    }
    for rel in list_pages(project_dir) {
        let path = root.join(&rel);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let (fm, body) = split_frontmatter(&text);
        if fm.is_some() {
            continue;
        }
        let (summary, kind) = entries.get(&rel).cloned().unwrap_or_default();
        let title = body
            .lines()
            .find_map(|l| l.trim().strip_prefix("# ").map(str::trim))
            .map(str::to_string)
            .or_else(|| {
                Path::new(&rel)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
            })
            .unwrap_or_default();
        let kind = if rel.ends_with(INBOX_PAGE) {
            "inbox"
        } else if kind.is_empty() {
            "concept"
        } else {
            &kind
        };
        let mut fm = Mapping::new();
        fm.insert(yaml_str("type"), Value::String(page_type_for_kind(kind)));
        if !title.is_empty() {
            fm.insert(yaml_str("title"), Value::String(title));
        }
        if !summary.is_empty() {
            fm.insert(yaml_str("description"), Value::String(summary));
        }
        if rel.ends_with(INBOX_PAGE) {
            fm.insert(yaml_str("status"), Value::String("draft".to_string()));
        }
        fs::write(&path, render_page(&fm, body))?;
    }

    // SCHEMA.md is a non-reserved document inside the bundle: it needs
    // frontmatter too.
    let schema_path = root.join(WIKI_DIR).join(SCHEMA_FILE);
    if let Ok(schema) = fs::read_to_string(&schema_path) {
        let (fm, _) = split_frontmatter(&schema);
        if fm.is_none() {
            let mut fm = Mapping::new();
            fm.insert(yaml_str("type"), Value::String("Reference".to_string()));
            let (_, body) = split_frontmatter(&schema);
            fs::write(&schema_path, render_page(&fm, body))?;
        }
    }

    // Declare the bundle version on the root index, rewriting legacy
    // `memories/pages/…` entry paths to their in-bundle location.
    let (_, body) = split_frontmatter(&raw);
    let body = body.replace(&format!("{LEGACY_PAGES_DIR}/"), &format!("{PAGES_DIR}/"));
    let upgraded = format!(
        "---\nokf_version: \"{OKF_VERSION}\"\n---\n\n{}",
        body.trim_start()
    );
    fs::write(index_path, upgraded)?;
    Ok(())
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

/// Upsert one index line by path. Frontmatter and the header survive:
/// non-bullet lines are never reordered and new bullets append at the end.
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

/// Append one log entry, OKF §9 shape: date-grouped, newest first. The entry
/// lands under today's `## YYYY-MM-DD` heading directly below the header;
/// legacy flat lines remain below as history.
pub fn append_log(project_dir: &Path, summary: &str) -> Result<(), WikiError> {
    ensure_layout(project_dir)?;
    let path = wiki_path(project_dir, LOG_FILE);
    let body = fs::read_to_string(&path).unwrap_or_default();
    let now = local_store::now_rfc3339();
    let today = now.get(..10).unwrap_or(now.as_str()).to_string();
    let entry = format!("* **Update**: {}", summary.trim());
    let today_heading = format!("## {today}");

    let history = body
        .strip_prefix(LOG_HEADER)
        .unwrap_or(&body)
        .trim_start_matches('\n');
    let mut out = String::from(LOG_HEADER);
    out.push('\n');
    if let Some(rest) = history.strip_prefix(today_heading.as_str()) {
        out.push('\n');
        out.push_str(&today_heading);
        out.push('\n');
        out.push_str(&entry);
        out.push('\n');
        out.push_str(rest.trim_start_matches('\n'));
    } else {
        out.push('\n');
        out.push_str(&today_heading);
        out.push('\n');
        out.push_str(&entry);
        out.push('\n');
        if !history.trim().is_empty() {
            out.push('\n');
            out.push_str(history.trim_end());
            out.push('\n');
        }
    }
    fs::write(path, out)?;
    Ok(())
}

/// Read the index catalog text (frontmatter stripped — never leak the
/// `okf_version` block into digests or synthesis packs).
pub fn read_index(project_dir: &Path) -> String {
    let text = fs::read_to_string(wiki_path(project_dir, INDEX_FILE)).unwrap_or_default();
    split_frontmatter(&text).1.to_string()
}

/// First N log lines (newest first), skipping the document header.
pub fn recent_log(project_dir: &Path, n: usize) -> String {
    let text = fs::read_to_string(wiki_path(project_dir, LOG_FILE)).unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty() && l.trim() != LOG_HEADER)
        .collect();
    let take = n.min(lines.len());
    lines[..take].join("\n")
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

/// Resolve a page path (accepts `wiki/pages/foo.md`, legacy
/// `memories/pages/foo.md`, `foo.md`, or `foo`).
pub fn resolve_page_path(project_dir: &Path, rel: &str) -> PathBuf {
    let rel = rel.trim().trim_start_matches('/');
    let root = local_store::root(project_dir);
    if let Some(legacy) = rel.strip_prefix(&format!("{LEGACY_PAGES_DIR}/")) {
        // Pre-OKF path spelling: the page now lives inside the bundle.
        return root.join(PAGES_DIR).join(legacy);
    }
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

/// Read a page document (frontmatter included — it is part of the page).
pub fn show(project_dir: &Path, rel: &str) -> Result<String, WikiError> {
    let path = resolve_page_path(project_dir, rel);
    Ok(fs::read_to_string(path)?)
}

/// Write/overwrite a page body and refresh the index. The page carries OKF
/// frontmatter; `actor` records who honestly produced this content (a model
/// id for synthesis, a harness for imports, `stateroot/<version>` for system
/// writes) and is only stamped when the body changed.
pub fn write_page(
    project_dir: &Path,
    slug: &str,
    body: &str,
    summary: &str,
    kind: &str,
    actor: Option<&str>,
) -> Result<PathBuf, WikiError> {
    ensure_layout(project_dir)?;
    let slug = slug.trim().trim_end_matches(".md");
    let file = format!("{slug}.md");
    let path = pages_dir(project_dir).join(&file);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = if body.trim().starts_with('#') {
        body.trim().to_string()
    } else {
        format!("# {slug}\n\n{}", body.trim())
    };
    let existing = fs::read_to_string(&path).ok();
    let actor = actor
        .map(str::to_string)
        .unwrap_or_else(|| format!("stateroot/{}", env!("CARGO_PKG_VERSION")));
    let title = body
        .lines()
        .find_map(|l| l.trim().strip_prefix("# ").map(str::trim))
        .unwrap_or(slug)
        .to_string();
    let text = conform_page(
        existing.as_deref(),
        kind,
        &title,
        summary,
        Some(&actor),
        &Mapping::new(),
        &body,
    );
    fs::write(&path, text)?;
    let rel = format!("{PAGES_DIR}/{file}");
    upsert_index(project_dir, &rel, summary, kind)?;
    Ok(path)
}

/// Append unique bullets into `_inbox.md` (deterministic distill floor).
/// Returns how many new bullets were added. Frontmatter stays on top.
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

/// List page paths under wiki/pages (relative to `.stateroot/`).
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

/// Walk markdown files under the bundle root, yielding paths relative to it
/// (no prefix — unlike `walk_pages`, which is anchored at the pages dir).
fn walk_bundle(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    items.sort();
    for path in items {
        if path.is_dir() {
            walk_bundle(base, &path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Ok(rel) = path.strip_prefix(base) {
                let rel: String = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push(rel);
            }
        }
    }
}

/// Lint: pages missing from index, index orphans, duplicate titles, and OKF
/// conformance (frontmatter, `type`, root `okf_version`, reserved filenames).
pub fn lint(project_dir: &Path) -> Result<Vec<LintFinding>, WikiError> {
    ensure_layout(project_dir)?;
    let mut findings = Vec::new();
    let root = local_store::root(project_dir);
    let raw_index = fs::read_to_string(wiki_path(project_dir, INDEX_FILE)).unwrap_or_default();
    if !index_declares_okf(&raw_index) {
        findings.push(LintFinding {
            code: "okf_index_no_version".into(),
            message: "bundle-root index.md lacks okf_version frontmatter".into(),
            path: Some(format!("{WIKI_DIR}/{INDEX_FILE}")),
        });
    }
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
            let abs = root.join(&p);
            if !abs.is_file() {
                findings.push(LintFinding {
                    code: "orphan_index".into(),
                    message: format!("index lists missing page {p}"),
                    path: Some(p.clone()),
                });
            }
            if let Ok(text) = fs::read_to_string(&abs) {
                let (_, body) = split_frontmatter(&text);
                if let Some(title) = body.lines().next().map(|l| l.trim().to_string()) {
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

    // OKF conformance over every markdown file inside the bundle.
    let bundle = root.join(WIKI_DIR);
    let mut bundle_files = Vec::new();
    walk_bundle(&bundle, &bundle, &mut bundle_files);
    for rel in bundle_files {
        let store_rel = format!("{WIKI_DIR}/{rel}");
        let name = Path::new(&rel)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let is_root_reserved = matches!(rel.as_str(), "index.md" | "log.md");
        if !is_root_reserved && matches!(name.as_str(), "index.md" | "log.md") {
            findings.push(LintFinding {
                code: "okf_reserved_misuse".into(),
                message: format!(
                    "{store_rel}: index.md/log.md are reserved and must not be concepts"
                ),
                path: Some(store_rel.clone()),
            });
            continue;
        }
        if is_root_reserved {
            continue;
        }
        let Ok(text) = fs::read_to_string(bundle.join(&rel)) else {
            continue;
        };
        let (fm, _) = split_frontmatter(&text);
        let Some(fm) = fm else {
            findings.push(LintFinding {
                code: "okf_no_frontmatter".into(),
                message: format!("{store_rel}: no YAML frontmatter block"),
                path: Some(store_rel.clone()),
            });
            continue;
        };
        if get_str(&parse_frontmatter(fm), "type").is_none() {
            findings.push(LintFinding {
                code: "okf_no_type".into(),
                message: format!("{store_rel}: frontmatter has no non-empty `type`"),
                path: Some(store_rel),
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
            None,
        )
        .unwrap();
        // Drop index entries artificially (keep the okf_version frontmatter).
        fs::write(
            wiki_path(p.path(), INDEX_FILE),
            "---\nokf_version: \"0.2\"\n---\n\n# StateRoot Wiki Index\n\n",
        )
        .unwrap();
        let findings = lint(p.path()).unwrap();
        assert!(findings.iter().any(|f| f.code == "missing_index"));
        assert!(!findings.iter().any(|f| f.code.starts_with("okf_")));
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
            None,
        )
        .unwrap();
        let section = compose_digest_section(p.path());
        assert!(section.contains("deploy.md"));
        assert!(!section.contains("SECRET_DEPLOY_DETAIL_SHOULD_NOT_APPEAR"));
    }

    #[test]
    fn write_page_emits_okf_frontmatter() {
        let p = project();
        write_page(
            p.path(),
            "auth",
            "JWT tokens live in crates/auth",
            "auth module",
            "entity",
            Some("kimi/k2"),
        )
        .unwrap();
        let text = show(p.path(), "auth").unwrap();
        let (fm, body) = split_frontmatter(&text);
        let fm = parse_frontmatter(fm.expect("frontmatter"));
        assert_eq!(get_str(&fm, "type").as_deref(), Some("Entity"));
        assert_eq!(get_str(&fm, "title").as_deref(), Some("auth"));
        assert_eq!(get_str(&fm, "description").as_deref(), Some("auth module"));
        let generated = fm.get(yaml_str("generated")).expect("generated");
        assert_eq!(
            generated.get(yaml_str("by")).and_then(|v| v.as_str()),
            Some("kimi/k2")
        );
        assert!(generated
            .get(yaml_str("at"))
            .and_then(|v| v.as_str())
            .is_some());
        assert!(body.contains("JWT tokens live in crates/auth"));
    }

    #[test]
    fn overwrite_preserves_unknown_keys_and_generated_when_unchanged() {
        let p = project();
        write_page(p.path(), "x", "body one", "s", "concept", Some("kimi/k2")).unwrap();
        let path = resolve_page_path(p.path(), "x");
        // Inject an extension key as a foreign producer would.
        let text = fs::read_to_string(&path).unwrap();
        let injected = text.replace("---\n\n# x", "custom_key: keepme\n---\n\n# x");
        fs::write(&path, injected).unwrap();
        // Same body again: generated.at must NOT move.
        let before = fs::read_to_string(&path).unwrap();
        write_page(
            p.path(),
            "x",
            "# x\n\nbody one",
            "s",
            "concept",
            Some("claude/x"),
        )
        .unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains("custom_key: keepme"));
        let fm_before = parse_frontmatter(split_frontmatter(&before).0.unwrap());
        let fm_after = parse_frontmatter(split_frontmatter(&after).0.unwrap());
        assert_eq!(
            fm_before.get(yaml_str("generated")),
            fm_after.get(yaml_str("generated")),
            "unchanged body keeps the original generated stamp"
        );
        // Changed body: generated.by refreshes.
        write_page(p.path(), "x", "body two", "s", "concept", Some("claude/x")).unwrap();
        let changed = fs::read_to_string(&path).unwrap();
        let fm_changed = parse_frontmatter(split_frontmatter(&changed).0.unwrap());
        let g = fm_changed.get(yaml_str("generated")).unwrap();
        assert_eq!(
            g.get(yaml_str("by")).and_then(|v| v.as_str()),
            Some("claude/x")
        );
    }

    #[test]
    fn migrate_moves_legacy_pages_and_backfills_honestly() {
        let p = project();
        let root = local_store::root(p.path());
        // Legacy pre-OKF layout: pages outside the bundle, plain index.
        let legacy = root.join(LEGACY_PAGES_DIR);
        fs::create_dir_all(&legacy).unwrap();
        fs::write(
            legacy.join("auth.md"),
            "# Auth\n\nJWT handling lives here.\n",
        )
        .unwrap();
        fs::create_dir_all(root.join(WIKI_DIR)).unwrap();
        fs::write(
            root.join(WIKI_DIR).join(INDEX_FILE),
            "# StateRoot Wiki Index\n\n- [memories/pages/auth.md](memories/pages/auth.md) - auth module (entity)\n",
        )
        .unwrap();
        // Fresh ensure_layout must move the page and conform it.
        ensure_layout(p.path()).unwrap();
        let moved = root.join(PAGES_DIR).join("auth.md");
        assert!(moved.is_file(), "page moved into the bundle");
        assert!(!legacy.join("auth.md").exists(), "legacy page gone");
        let text = fs::read_to_string(&moved).unwrap();
        let (fm, _) = split_frontmatter(&text);
        let fm = parse_frontmatter(fm.expect("frontmatter after migration"));
        assert_eq!(get_str(&fm, "type").as_deref(), Some("Entity"));
        assert_eq!(get_str(&fm, "description").as_deref(), Some("auth module"));
        assert!(
            fm.get(yaml_str("generated")).is_none(),
            "unknown provenance stays absent — never fabricated"
        );
        let raw_index = fs::read_to_string(wiki_path(p.path(), INDEX_FILE)).unwrap();
        assert!(index_declares_okf(&raw_index), "index declares okf_version");
        assert!(
            raw_index.contains("wiki/pages/auth.md"),
            "index entry rewritten to the in-bundle path: {raw_index}"
        );
        assert!(!raw_index.contains("memories/pages"), "{raw_index}");
        // Idempotent: a second pass changes nothing.
        let before = fs::read_to_string(&moved).unwrap();
        ensure_layout(p.path()).unwrap();
        assert_eq!(before, fs::read_to_string(&moved).unwrap());
    }

    #[test]
    fn digest_section_strips_index_frontmatter() {
        let p = project();
        ensure_layout(p.path()).unwrap();
        let section = compose_digest_section(p.path());
        assert!(!section.contains("okf_version"), "{section}");
        assert!(!section.contains("---"), "{section}");
    }

    #[test]
    fn append_log_is_date_grouped_newest_first() {
        let p = project();
        ensure_layout(p.path()).unwrap();
        append_log(p.path(), "first entry").unwrap();
        append_log(p.path(), "second entry").unwrap();
        let text = fs::read_to_string(wiki_path(p.path(), LOG_FILE)).unwrap();
        assert!(text.starts_with(LOG_HEADER));
        let today = local_store::now_rfc3339();
        let today = today.get(..10).unwrap();
        assert!(text.contains(&format!("## {today}")));
        let first_pos = text.find("first entry").unwrap();
        let second_pos = text.find("second entry").unwrap();
        assert!(second_pos < first_pos, "newest entry first: {text}");
        let recent = recent_log(p.path(), 10);
        assert!(recent.contains("second entry"));
    }

    #[test]
    fn lint_flags_nonconformant_pages() {
        let p = project();
        ensure_layout(p.path()).unwrap();
        // Hand-written page with no frontmatter at all.
        let bad = pages_dir(p.path()).join("rogue.md");
        fs::write(&bad, "# Rogue\n\nno frontmatter here\n").unwrap();
        let findings = lint(p.path()).unwrap();
        assert!(
            findings.iter().any(|f| f.code == "okf_no_frontmatter"
                && f.path.as_deref() == Some("wiki/pages/rogue.md")),
            "{findings:?}"
        );
        // Page with frontmatter but no type.
        fs::write(&bad, "---\ntitle: Rogue\n---\n\n# Rogue\n").unwrap();
        let findings = lint(p.path()).unwrap();
        assert!(
            findings.iter().any(|f| f.code == "okf_no_type"),
            "{findings:?}"
        );
    }

    #[test]
    fn legacy_page_path_still_resolves() {
        let p = project();
        write_page(p.path(), "auth", "body", "s", "concept", None).unwrap();
        let via_legacy = resolve_page_path(p.path(), "memories/pages/auth.md");
        assert!(via_legacy.is_file(), "{via_legacy:?}");
        assert_eq!(
            show(p.path(), "memories/pages/auth.md").unwrap(),
            show(p.path(), "auth").unwrap()
        );
    }
}
