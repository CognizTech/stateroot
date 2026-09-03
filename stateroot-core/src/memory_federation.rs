//! Memory federation — pull harness-native memories into the StateRoot pool
//! as `observed` tier, and push a curated state-of-the-work brief back into
//! harness-native memory files.
//!
//! Pull readers:
//! - Claude Code: `~/.claude/projects/<slug>/memory/*.md` (slug decodes to a
//!   cwd; matched against `project_dir` with walk-up/walk-down tolerance).
//! - Codex: `~/.codex/memories/*.md` (flat, consolidated by codex's pipeline —
//!   we read only the `.md`, never the sqlite).
//! - OpenClaw: `~/.openclaw/workspace/memory/*.md` (daily logs → episodic).
//!
//! Every imported artifact carries a provenance header and is `observed`. The
//! import ledger `.stateroot/memories/federation.json` is the dedup source of
//! truth: dedup is by content hash, conflicts (same title, different content)
//! are preserved alongside, never overwritten.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{hot_apex, local_store, plans, wiki};

/// Import ledger path relative to `.stateroot/`.
pub const FEDERATION_PATH: &str = "memories/federation.json";
/// Import ledger schema version.
pub const FEDERATION_SCHEMA: &str = "stateroot.memory_federation.v1";
/// Marker that identifies a StateRoot-managed push target.
pub const MANAGED_MARKER: &str = "<!-- stateroot:managed v1 -->";
/// Extension frontmatter key carrying import provenance on imported pages.
pub const IMPORT_KEY: &str = "stateroot_import";
/// Cap on the pushed brief body (chars, truncate at a line boundary).
pub const PUSH_CAP: usize = 4000;

/// Harness ids understood by this module (stable, lowercase).
pub const HARNESSES: [&str; 3] = ["claude", "codex", "openclaw"];

/// One harness memory note read from a harness-native store.
#[derive(Debug, Clone, PartialEq)]
pub struct HarnessMemoryNote {
    /// Stable harness id.
    pub harness: &'static str,
    /// Absolute source path (provenance only).
    pub source_path: String,
    /// Display title (stem, or `index` for claude's MEMORY.md).
    pub title: String,
    /// Verbatim body (trimmed).
    pub text: String,
    /// First 16 hex of sha256 over normalized text (dedup key).
    pub hash: String,
}

/// Aggregated pull result per source harness.
#[derive(Debug, Default, Clone)]
pub struct SourceReport {
    /// Harness id.
    pub harness: String,
    /// Notes discovered on disk.
    pub found: usize,
    /// Notes newly imported this run.
    pub imported: usize,
    /// Notes already in the ledger (skipped).
    pub duplicates: usize,
    /// Notes preserved alongside an existing title (different content).
    pub conflicts: usize,
}

/// Aggregate pull report across all scanned sources.
#[derive(Debug, Default)]
pub struct SyncReport {
    /// Per-source reports, in scan order.
    pub sources: Vec<SourceReport>,
}

/// Outcome of one push target.
#[derive(Debug, Clone)]
pub struct PushResult {
    /// Harness id.
    pub harness: String,
    /// Absolute target path.
    pub target: PathBuf,
    /// `written` | `updated` | `conflict` | `no-home` | `dry-run`.
    pub status: String,
    /// Brief size in bytes (what would be / was written).
    pub bytes: usize,
}

/// Errors from memory federation.
#[derive(Debug, thiserror::Error)]
pub enum MemoryFederationError {
    /// Filesystem failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Any other failure (surfaced as a message).
    #[error("{0}")]
    Other(String),
}

impl From<wiki::WikiError> for MemoryFederationError {
    fn from(e: wiki::WikiError) -> Self {
        MemoryFederationError::Other(e.to_string())
    }
}

impl From<local_store::LocalStoreError> for MemoryFederationError {
    fn from(e: local_store::LocalStoreError) -> Self {
        MemoryFederationError::Other(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Hashing + normalization
// ---------------------------------------------------------------------------

/// Normalize text for hashing: CRLF→LF, lone CR→LF, trim.
fn normalized_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_string()
}

/// First 16 hex of sha256 over normalized text.
fn content_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(normalized_text(text).as_bytes());
    let full = format!("{:x}", hasher.finalize());
    full.chars().take(16).collect()
}

/// Normalize a path string for comparison (Windows ↔ WSL, slash form).
fn normalize_for_match(raw: &str) -> String {
    crate::path_identity::normalize_host_path(raw)
}

/// True when two normalized paths are equal or one is a strict ancestor of the
/// other (boundary-safe: `/foo/bar2` never matches `/foo/bar`).
fn paths_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if a.len() > b.len() && a.starts_with(b) && a.as_bytes()[b.len()] == b'/' {
        return true;
    }
    if b.len() > a.len() && b.starts_with(a) && b.as_bytes()[a.len()] == b'/' {
        return true;
    }
    false
}

/// Decode a claude project slug back to its cwd: leading `/` became a leading
/// `-`, every `/` became `-`. Inherently ambiguous for directory names that
/// contain `-`; matching is tolerant (walk-up/walk-down overlap), never exact.
fn claude_slug_to_cwd(slug: &str) -> String {
    let rest = slug.strip_prefix('-').unwrap_or(slug);
    format!("/{}", rest.replace('-', "/"))
}

/// Encode an absolute path as a claude project slug (best-effort fallback when
/// no existing slug dir matches).
fn claude_slug_encode(path: &str) -> String {
    let replaced = normalize_for_match(path).replace('/', "-");
    if replaced.starts_with('-') {
        replaced
    } else {
        format!("-{replaced}")
    }
}

/// Sanitize a title into a filesystem-safe page stem.
fn sanitize_title(title: &str) -> String {
    let mut out: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if out.is_empty() {
        out = "note".to_string();
    }
    out
}

/// Truncate to `max` chars with an ellipsis.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

// ---------------------------------------------------------------------------
// Readers
// ---------------------------------------------------------------------------

/// Read `*.md` (non-recursive) from a memory dir.
fn read_memory_dir(
    harness: &'static str,
    dir: &Path,
    memory_as_index: bool,
) -> Vec<HarnessMemoryNote> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let text = text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let title = if memory_as_index && stem == "MEMORY" {
            "index".to_string()
        } else {
            stem
        };
        out.push(HarnessMemoryNote {
            harness,
            source_path: path.to_string_lossy().to_string(),
            title,
            hash: content_hash(&text),
            text,
        });
    }
    out
}

/// Read claude memory for the project: every slug dir under
/// `~/.claude/projects/` whose decoded cwd overlaps `project_dir` (exact,
/// walk-up, or walk-down), reading its `memory/*.md`.
pub fn read_claude(home: &Path, project_dir: &Path) -> Vec<HarnessMemoryNote> {
    let projects = home.join(".claude").join("projects");
    let Ok(entries) = std::fs::read_dir(&projects) else {
        return Vec::new();
    };
    let project_norm = normalize_for_match(&project_dir.to_string_lossy());
    let mut notes = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let slug = entry.file_name().to_string_lossy().to_string();
        if !paths_overlap(
            &normalize_for_match(&claude_slug_to_cwd(&slug)),
            &project_norm,
        ) {
            continue;
        }
        notes.extend(read_memory_dir("claude", &path.join("memory"), true));
    }
    notes
}

/// Read codex consolidated memory: every `*.md` directly under
/// `~/.codex/memories/` (no recursion; the sqlite is pipeline state, not read).
pub fn read_codex(home: &Path, _project_dir: &Path) -> Vec<HarnessMemoryNote> {
    read_memory_dir("codex", &home.join(".codex").join("memories"), false)
}

/// Read openclaw daily session logs: every `*.md` under
/// `~/.openclaw/workspace/memory/` (maps to the episodic tier).
pub fn read_openclaw(home: &Path, _project_dir: &Path) -> Vec<HarnessMemoryNote> {
    read_memory_dir(
        "openclaw",
        &home.join(".openclaw").join("workspace").join("memory"),
        false,
    )
}

// ---------------------------------------------------------------------------
// Import ledger
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImportRecord {
    hash: String,
    harness: String,
    source_path: String,
    title: String,
    target: String,
    imported_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConflictRecord {
    hash: String,
    harness: String,
    source_path: String,
    title: String,
    reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Ledger {
    schema_version: String,
    #[serde(default)]
    imports: Vec<ImportRecord>,
    #[serde(default)]
    conflicts: Vec<ConflictRecord>,
}

fn ledger_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(FEDERATION_PATH)
}

fn load_ledger(project_dir: &Path) -> Ledger {
    let path = ledger_path(project_dir);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ledger {
            schema_version: FEDERATION_SCHEMA.to_string(),
            ..Default::default()
        };
    };
    let mut ledger: Ledger = serde_json::from_str(&text).unwrap_or_default();
    if ledger.schema_version.is_empty() {
        ledger.schema_version = FEDERATION_SCHEMA.to_string();
    }
    ledger
}

fn save_ledger(project_dir: &Path, ledger: &Ledger) -> Result<(), MemoryFederationError> {
    let path = ledger_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(ledger)?;
    std::fs::write(path, format!("{text}\n"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Pull (M1)
// ---------------------------------------------------------------------------

/// Classify a note against the ledger.
enum Outcome {
    Imported,
    Duplicate,
    Conflict,
}

/// Pull harness memories into the pool. `harness` (optional) restricts to one
/// source; `dry_run` reports without writing pages, episodic, or the ledger.
pub fn sync_pull(
    project_dir: &Path,
    home: &Path,
    harness: Option<&str>,
    dry_run: bool,
) -> Result<SyncReport, MemoryFederationError> {
    wiki::ensure_layout(project_dir)?;
    let mut ledger = load_ledger(project_dir);
    let mut report = SyncReport::default();

    for id in HARNESSES {
        if let Some(filter) = harness {
            if filter != id {
                continue;
            }
        }
        let notes = match id {
            "claude" => read_claude(home, project_dir),
            "codex" => read_codex(home, project_dir),
            _ => read_openclaw(home, project_dir),
        };
        let mut source = SourceReport {
            harness: id.to_string(),
            found: notes.len(),
            ..Default::default()
        };
        for note in notes {
            match import_one(project_dir, &mut ledger, &note, dry_run)? {
                Outcome::Imported => source.imported += 1,
                Outcome::Duplicate => source.duplicates += 1,
                Outcome::Conflict => source.conflicts += 1,
            }
        }
        report.sources.push(source);
    }

    if !dry_run {
        save_ledger(project_dir, &ledger)?;
    }
    Ok(report)
}

fn import_one(
    project_dir: &Path,
    ledger: &mut Ledger,
    note: &HarnessMemoryNote,
    dry_run: bool,
) -> Result<Outcome, MemoryFederationError> {
    if ledger.imports.iter().any(|i| i.hash == note.hash) {
        return Ok(Outcome::Duplicate);
    }

    let imported_at = local_store::now_rfc3339();
    if note.harness == "openclaw" {
        // Append-only episodic tier; no title-conflict concept.
        let target = format!("harness-memory:openclaw:{}", note.hash);
        if !dry_run {
            let record = serde_json::json!({
                "ts": imported_at,
                "harness": "observed",
                "note": format!("[openclaw {}] {}", note.title, truncate_chars(&note.text, 300)),
                "files": [],
                "source_id": target,
            });
            local_store::append_episodic(project_dir, &record)?;
        }
        ledger.imports.push(ImportRecord {
            hash: note.hash.clone(),
            harness: note.harness.to_string(),
            source_path: note.source_path.clone(),
            title: note.title.clone(),
            target,
            imported_at,
        });
        return Ok(Outcome::Imported);
    }

    let safe_title = sanitize_title(&note.title);
    let same_title_different = ledger
        .imports
        .iter()
        .any(|i| i.harness == note.harness && i.title == note.title && i.hash != note.hash);

    let (file, outcome) = if same_title_different {
        let suffix: String = note.hash.chars().take(8).collect();
        (format!("{safe_title}__{suffix}.md"), Outcome::Conflict)
    } else {
        (format!("{safe_title}.md"), Outcome::Imported)
    };
    // OKF reserved filenames must not be used for concept pages.
    let file = match file.as_str() {
        "index.md" | "log.md" => format!("_{file}"),
        _ => file,
    };

    if !dry_run {
        write_imported_page(project_dir, note, &file)?;
    }
    let target = format!("{}/harness/{}/{}", wiki::PAGES_DIR, note.harness, file);
    ledger.imports.push(ImportRecord {
        hash: note.hash.clone(),
        harness: note.harness.to_string(),
        source_path: note.source_path.clone(),
        title: note.title.clone(),
        target,
        imported_at,
    });
    if matches!(outcome, Outcome::Conflict) {
        ledger.conflicts.push(ConflictRecord {
            hash: note.hash.clone(),
            harness: note.harness.to_string(),
            source_path: note.source_path.clone(),
            title: note.title.clone(),
            reason: "same title, different content; preserved alongside".to_string(),
        });
    }
    Ok(outcome)
}

fn write_imported_page(
    project_dir: &Path,
    note: &HarnessMemoryNote,
    file: &str,
) -> Result<(), MemoryFederationError> {
    let dir = local_store::root(project_dir)
        .join(wiki::PAGES_DIR)
        .join("harness")
        .join(note.harness);
    std::fs::create_dir_all(&dir)?;
    // Absorb the source document's own frontmatter (harness memory files carry
    // one) into the OKF frontmatter instead of leaving it as junk body text.
    let (src_fm, src_body) = wiki::split_frontmatter(note.text.trim());
    let src_fm = src_fm.map(|f| serde_yaml::from_str::<serde_yaml::Mapping>(f).unwrap_or_default());
    let fm_str = |key: &str| {
        src_fm.as_ref().and_then(|m| {
            m.get(serde_yaml::Value::String(key.into()))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .filter(|s| !s.trim().is_empty())
        })
    };
    let title = fm_str("name").unwrap_or_else(|| note.title.clone());
    let body = src_body.trim();
    let summary = fm_str("description").unwrap_or_else(|| {
        body.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or(&title)
            .chars()
            .take(70)
            .collect()
    });
    let mut extra = serde_yaml::Mapping::new();
    let mut source = serde_yaml::Mapping::new();
    source.insert(
        serde_yaml::Value::String("resource".into()),
        serde_yaml::Value::String(note.source_path.clone()),
    );
    extra.insert(
        serde_yaml::Value::String("sources".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(source)]),
    );
    let mut import = serde_yaml::Mapping::new();
    import.insert(
        serde_yaml::Value::String("harness".into()),
        serde_yaml::Value::String(note.harness.to_string()),
    );
    import.insert(
        serde_yaml::Value::String("hash".into()),
        serde_yaml::Value::String(note.hash.clone()),
    );
    extra.insert(
        serde_yaml::Value::String(IMPORT_KEY.into()),
        serde_yaml::Value::Mapping(import),
    );
    let existing = std::fs::read_to_string(dir.join(file)).ok();
    let doc = wiki::conform_page(
        existing.as_deref(),
        "harness",
        &title,
        &summary,
        Some(note.harness),
        &extra,
        body,
    );
    std::fs::write(dir.join(file), doc)?;
    let rel = format!("{}/harness/{}/{}", wiki::PAGES_DIR, note.harness, file);
    wiki::upsert_index(project_dir, &rel, &summary, "harness")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Push (M2)
// ---------------------------------------------------------------------------

/// Compute the claude project dir for push: an existing slug dir whose decoded
/// cwd overlaps `project_dir`, else the encoded slug (best-effort).
fn claude_slug_dir(home: &Path, project_dir: &Path) -> PathBuf {
    let projects = home.join(".claude").join("projects");
    let project_norm = normalize_for_match(&project_dir.to_string_lossy());
    if let Ok(entries) = std::fs::read_dir(&projects) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let slug = entry.file_name().to_string_lossy().to_string();
            if paths_overlap(
                &normalize_for_match(&claude_slug_to_cwd(&slug)),
                &project_norm,
            ) {
                return path;
            }
        }
    }
    projects.join(claude_slug_encode(&project_dir.to_string_lossy()))
}

fn harness_base(home: &Path, harness: &str) -> PathBuf {
    match harness {
        "claude" => home.join(".claude").join("projects"),
        "codex" => home.join(".codex"),
        _ => home.join(".openclaw").join("workspace"),
    }
}

/// Push targets for the harnesses whose native home exists on this machine.
fn push_targets(home: &Path, project_dir: &Path) -> Vec<(String, PathBuf)> {
    let mut targets = Vec::new();
    for id in HARNESSES {
        if !harness_base(home, id).is_dir() {
            continue;
        }
        let target = match id {
            "claude" => claude_slug_dir(home, project_dir).join("memory/stateroot.md"),
            "codex" => home.join(".codex").join("memories").join("stateroot.md"),
            _ => home
                .join(".openclaw")
                .join("workspace")
                .join("memory")
                .join("stateroot.md"),
        };
        targets.push((id.to_string(), target));
    }
    targets
}

/// Render the compact state-of-the-work brief pushed into harness memory.
pub fn render_push_brief(project_dir: &Path, home: &Path) -> String {
    let mut out = String::new();
    out.push_str(MANAGED_MARKER);
    out.push('\n');
    out.push_str("# StateRoot — project brief\n\n");

    let state_path = local_store::root(project_dir).join(local_store::STATE_PATH);
    let mut objective = String::new();
    let mut phase = String::new();
    if let Ok(text) = std::fs::read_to_string(&state_path) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            objective = value
                .get("objective")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            phase = value
                .get("current_phase")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
        }
    }
    if objective.is_empty() {
        if let Ok(Some(packet)) = local_store::read_handoff_local(project_dir) {
            objective = packet
                .get("objective")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
        }
    }
    if !objective.is_empty() {
        out.push_str(&format!("**Objective:** {objective}\n\n"));
    }
    if !phase.is_empty() {
        out.push_str(&format!("**Phase:** {phase}\n\n"));
    }

    if let Some((meta, _)) = plans::active(project_dir) {
        out.push_str("**Active plan:** ");
        out.push_str(meta.title.trim());
        if let Some(src) = &meta.source_path {
            if !src.is_empty() {
                out.push_str(&format!(" (directive: {src})"));
            }
        }
        out.push_str("\n\n");
    }

    let checkpoints = local_store::recent_episodic(project_dir, 5);
    if !checkpoints.is_empty() {
        out.push_str("## Recent checkpoints\n\n");
        for record in &checkpoints {
            let ts = record.get("ts").and_then(|v| v.as_str()).unwrap_or("");
            let note = record.get("note").and_then(|v| v.as_str()).unwrap_or("");
            if note.trim().is_empty() {
                continue;
            }
            out.push_str(&format!("- [{ts}] {}\n", truncate_chars(note.trim(), 200)));
        }
        out.push('\n');
    }

    if let Ok(memory) = hot_apex::read_text(project_dir, home, "memory") {
        let memory = memory.trim();
        if !memory.is_empty() {
            out.push_str("## Project memory (hot apex)\n\n");
            out.push_str(memory);
            out.push('\n');
        }
    }

    cap_at_line_boundary(out)
}

fn cap_at_line_boundary(body: String) -> String {
    if body.chars().count() <= PUSH_CAP {
        return body;
    }
    let mut cut = PUSH_CAP;
    while cut > 0 && !body.is_char_boundary(cut) {
        cut -= 1;
    }
    while cut > 0 && body.as_bytes()[cut - 1] != b'\n' {
        cut -= 1;
    }
    if cut == 0 {
        cut = PUSH_CAP;
    }
    format!(
        "{}<!-- brief truncated at {} chars -->\n",
        &body[..cut],
        PUSH_CAP
    )
}

/// Push the brief into managed harness memory files. Only writes when the file
/// is absent or carries the managed marker; an unmarked pre-existing file is a
/// conflict and left untouched.
pub fn sync_push(
    project_dir: &Path,
    home: &Path,
    dry_run: bool,
) -> Result<Vec<PushResult>, MemoryFederationError> {
    let brief = render_push_brief(project_dir, home);
    let bytes = brief.len();
    let mut results = Vec::new();
    for (harness, target) in push_targets(home, project_dir) {
        let status = if dry_run {
            "dry-run".to_string()
        } else if target.exists() {
            let existing = std::fs::read_to_string(&target).unwrap_or_default();
            if existing.contains(MANAGED_MARKER) {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&target, &brief)?;
                "updated".to_string()
            } else {
                "conflict".to_string()
            }
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, &brief)?;
            "written".to_string()
        };
        results.push(PushResult {
            harness,
            target,
            status,
            bytes,
        });
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        local_store::init_skeleton(dir.path(), "p", "P", "default").unwrap();
        dir
    }

    fn write(home: &Path, rel: &str, content: &str) {
        let path = home.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn claude_reader_matches_slug_and_excludes_wrong_cwd() {
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            ".claude/projects/-work-demo/memory/MEMORY.md",
            "# index body",
        );
        write(
            home.path(),
            ".claude/projects/-work-demo/memory/topic-one.md",
            "topic one body",
        );
        write(
            home.path(),
            ".claude/projects/-elsewhere/memory/x.md",
            "should not appear",
        );
        let notes = read_claude(home.path(), Path::new("/work/demo"));
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes.iter().any(|n| n.title == "index"));
        assert!(notes.iter().any(|n| n.title == "topic-one"));
        // Walk-up: a deeper project dir still matches the claude slug.
        let nested = read_claude(home.path(), Path::new("/work/demo/sub"));
        assert_eq!(nested.len(), 2);
        // Wrong cwd is excluded.
        let other = read_claude(home.path(), Path::new("/unrelated"));
        assert!(other.is_empty());
        // Windows and WSL forms of the same tree overlap.
        write(
            home.path(),
            ".claude/projects/-mnt-d-work-demo/memory/win.md",
            "wsl slug body",
        );
        let from_windows = read_claude(home.path(), Path::new(r"D:\work\demo"));
        assert!(
            from_windows.iter().any(|n| n.title == "win"),
            "{from_windows:?}"
        );
    }

    #[test]
    fn codex_and_openclaw_readers_are_flat() {
        let home = tempfile::tempdir().unwrap();
        write(home.path(), ".codex/memories/a.md", "codex a");
        write(
            home.path(),
            ".codex/memories/memory_summary.md",
            "codex summary",
        );
        write(
            home.path(),
            ".openclaw/workspace/memory/2026-08-26-1400.md",
            "openclaw log",
        );
        let codex = read_codex(home.path(), Path::new("/x"));
        assert_eq!(codex.len(), 2);
        assert!(codex.iter().any(|n| n.title == "memory_summary"));
        let openclaw = read_openclaw(home.path(), Path::new("/x"));
        assert_eq!(openclaw.len(), 1);
        assert_eq!(openclaw[0].title, "2026-08-26-1400");
    }

    #[test]
    fn pull_imports_pages_with_provenance_and_is_idempotent() {
        let p = project();
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            ".codex/memories/project-status.md",
            "unique-token-77 project status",
        );
        let first = sync_pull(p.path(), home.path(), Some("codex"), false).unwrap();
        let src = &first.sources[0];
        assert_eq!(src.found, 1);
        assert_eq!(src.imported, 1);
        let page = p
            .path()
            .join(".stateroot/wiki/pages/harness/codex/project-status.md");
        let text = std::fs::read_to_string(&page).unwrap();
        assert!(text.contains("type: Harness Note"), "{text}");
        assert!(text.contains("stateroot_import"), "{text}");
        assert!(text.contains("harness: codex"), "{text}");
        assert!(text.contains("unique-token-77"), "{text}");

        // Idempotent: second pull imports nothing, reports duplicates.
        let second = sync_pull(p.path(), home.path(), Some("codex"), false).unwrap();
        let src = &second.sources[0];
        assert_eq!(src.found, 1);
        assert_eq!(src.duplicates, 1);
        assert_eq!(src.imported, 0);

        // FTS recall finds the imported page text.
        let hits =
            crate::memory_index::search(p.path(), home.path(), "unique-token-77", 5, true).unwrap();
        assert!(
            hits.iter().any(|h| h.text.contains("unique-token-77")),
            "{hits:?}"
        );
    }

    #[test]
    fn pull_renames_okf_reserved_filenames() {
        let p = project();
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            ".codex/memories/index.md",
            "codex index note content",
        );
        let report = sync_pull(p.path(), home.path(), Some("codex"), false).unwrap();
        assert_eq!(report.sources[0].imported, 1);
        assert!(
            p.path()
                .join(".stateroot/wiki/pages/harness/codex/_index.md")
                .is_file(),
            "reserved index.md must be renamed to _index.md"
        );
    }

    #[test]
    fn pull_absorbs_source_frontmatter_into_okf() {
        let p = project();
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            ".codex/memories/topic.md",
            "---\nname: Real Title\ndescription: real source summary\n---\n\nbody token-55 content\n",
        );
        let report = sync_pull(p.path(), home.path(), Some("codex"), false).unwrap();
        assert_eq!(report.sources[0].imported, 1);
        let text = std::fs::read_to_string(
            p.path()
                .join(".stateroot/wiki/pages/harness/codex/topic.md"),
        )
        .unwrap();
        let (fm, body) = wiki::split_frontmatter(&text);
        let fm = fm.expect("frontmatter");
        assert!(fm.contains("title: Real Title"), "{text}");
        assert!(fm.contains("description: real source summary"), "{text}");
        assert!(body.contains("body token-55 content"), "{text}");
        assert!(
            !body.contains("name: Real Title"),
            "source frontmatter must not sit in the body: {text}"
        );
    }

    #[test]
    fn pull_conflict_preserves_both_and_records() {
        let p = project();
        let home = tempfile::tempdir().unwrap();
        let src_file = ".codex/memories/topic.md";
        write(home.path(), src_file, "content A");
        let first = sync_pull(p.path(), home.path(), Some("codex"), false).unwrap();
        assert_eq!(first.sources[0].imported, 1);
        assert_eq!(first.sources[0].conflicts, 0);

        write(home.path(), src_file, "content B changed");
        let second = sync_pull(p.path(), home.path(), Some("codex"), false).unwrap();
        assert_eq!(second.sources[0].imported, 0);
        assert_eq!(second.sources[0].conflicts, 1);

        let pages = p.path().join(".stateroot/wiki/pages/harness/codex");
        let mut names: Vec<String> = std::fs::read_dir(&pages)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.iter().any(|n| n == "topic.md"), "{names:?}");
        assert!(names.iter().any(|n| n.starts_with("topic__")), "{names:?}");
    }

    #[test]
    fn pull_openclaw_goes_to_episodic_not_pages() {
        let p = project();
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            ".openclaw/workspace/memory/2026-08-26-1400.md",
            "openclaw session note",
        );
        let report = sync_pull(p.path(), home.path(), Some("openclaw"), false).unwrap();
        assert_eq!(report.sources[0].imported, 1);
        let episodic =
            std::fs::read_to_string(p.path().join(".stateroot/memories/episodic.jsonl")).unwrap();
        assert!(episodic.contains("harness-memory:openclaw:"), "{episodic}");
        assert!(!p
            .path()
            .join(".stateroot/wiki/pages/harness/openclaw")
            .exists());
    }

    #[test]
    fn push_brief_is_marked_and_bounded() {
        let p = project();
        let home = tempfile::tempdir().unwrap();
        // Set an objective so the brief has real content.
        let state = p.path().join(".stateroot/project/state.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
        v["objective"] = serde_json::json!("ship memory federation");
        std::fs::write(&state, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        hot_apex::add(p.path(), home.path(), "memory", "hot apex fact", false).unwrap();

        let brief = render_push_brief(p.path(), home.path());
        assert!(brief.starts_with(MANAGED_MARKER), "{brief}");
        assert!(brief.contains("ship memory federation"), "{brief}");
        assert!(
            brief.chars().count() <= PUSH_CAP + 80,
            "{}",
            brief.chars().count()
        );
    }

    #[test]
    fn push_writes_managed_and_refuses_unmarked() {
        let p = project();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".codex/memories")).unwrap();

        let results = sync_push(p.path(), home.path(), false).unwrap();
        let codex = results.iter().find(|r| r.harness == "codex").unwrap();
        assert_eq!(codex.status, "written");
        let target = home.path().join(".codex/memories/stateroot.md");
        let text = std::fs::read_to_string(&target).unwrap();
        assert!(text.starts_with(MANAGED_MARKER), "{text}");

        // Overwrite with unmarked content → conflict, file untouched.
        std::fs::write(&target, "foreign memory").unwrap();
        let results = sync_push(p.path(), home.path(), false).unwrap();
        let codex = results.iter().find(|r| r.harness == "codex").unwrap();
        assert_eq!(codex.status, "conflict");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "foreign memory");

        // Dry-run writes nothing.
        let before = std::fs::read_to_string(&target).unwrap();
        let _ = sync_push(p.path(), home.path(), true).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), before);
    }
}
