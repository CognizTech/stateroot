//! Curated hot-apex memory (Hermes-style § entries).
//!
//! Three internal targets:
//! - `memory` → `.stateroot/memories/MEMORY.md` (project facts; 8000 char write cap)
//! - `global_memory` → `~/.stateroot/memories/MEMORY.md` (user-global facts;
//!   8000 char write cap)
//! - `user` → `~/.stateroot/user/USER.md` via [`crate::user_profile`] (4000 char write cap)
//!
//! Caps are write hygiene: overflow errors so the agent consolidates. Import /
//! setup / generate paths that call [`user_profile::write`] directly bypass the
//! cap. Digest still renders full bodies even when over cap.
//!
//! Migrates legacy `.stateroot/memory.md` / `~/.stateroot/memory.md` bullets
//! into `MEMORY.md` once, then stops writing those files.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::local_store;
use crate::user_profile;

/// Entry delimiter (Hermes / server HotApexBuilder).
pub const ENTRY_DELIMITER: &str = "\n§\n";

/// Write cap for project `MEMORY.md` (chars).
pub const MEMORY_CHAR_LIMIT: usize = 8000;
/// Write cap for USER.md via the memory tool (chars).
pub const USER_CHAR_LIMIT: usize = 4000;
/// User-global memory path under `~/.stateroot/`.
pub const GLOBAL_MEMORY_PATH: &str = ".stateroot/memories/MEMORY.md";

/// Private visibility marker on an entry.
pub const PRIVATE_MARKER: &str = "<!-- visibility: private -->";

/// Legacy append-log paths (retired; migrated into MEMORY.md).
pub const LEGACY_PROJECT_MEMORY: &str = "memory.md";
/// User-global legacy append log under `~/.stateroot/`.
pub const LEGACY_USER_MEMORY: &str = "memory.md";

/// Errors from hot-apex mutations.
#[derive(Debug, thiserror::Error)]
pub enum HotApexError {
    /// Filesystem failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// User-profile write failure.
    #[error("user profile: {0}")]
    UserProfile(#[from] user_profile::UserProfileError),
    /// Invalid target name.
    #[error("invalid target: {0} (expected memory|user)")]
    InvalidTarget(String),
}

/// Result of an add/replace/remove attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct MutationResult {
    /// Whether the write succeeded.
    pub success: bool,
    /// True when the entry was already present (add) or nothing changed.
    pub noop: bool,
    /// Human-readable error when `success` is false.
    pub error: Option<String>,
    /// Usage string like `71% - 5,680/8,000 chars`.
    pub usage: String,
    /// Absolute path written (when known).
    pub path: Option<PathBuf>,
    /// Current entries after a failed overflow (for agent consolidation).
    pub current_entries: Option<Vec<String>>,
}

impl MutationResult {
    fn ok(usage: String, path: PathBuf, noop: bool) -> Self {
        Self {
            success: true,
            noop,
            error: None,
            usage,
            path: Some(path),
            current_entries: None,
        }
    }

    fn err(
        usage: String,
        path: Option<PathBuf>,
        error: String,
        current_entries: Option<Vec<String>>,
    ) -> Self {
        Self {
            success: false,
            noop: false,
            error: Some(error),
            usage,
            path,
            current_entries,
        }
    }
}

/// Split body into §-delimited entries (falls back to bullet lines for legacy).
pub fn split_entries(text: &str) -> Vec<String> {
    let raw = text.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    // Strip a leading markdown title so it is not glued onto the first entry.
    let raw = {
        let mut lines = raw.lines();
        let first = lines.next().unwrap_or("").trim();
        if first.starts_with('#') {
            lines.collect::<Vec<_>>().join("\n").trim().to_string()
        } else {
            raw.to_string()
        }
    };
    if raw.is_empty() {
        return Vec::new();
    }
    if raw.contains('§') {
        return raw
            .split('§')
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty() && !is_boilerplate_line(e))
            .collect();
    }
    // Legacy bullet / paragraph form.
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || is_boilerplate_line(line) || line.starts_with('#') {
            continue;
        }
        let entry = line.strip_prefix("- ").unwrap_or(line).trim();
        if !entry.is_empty() {
            out.push(entry.to_string());
        }
    }
    out
}

fn is_boilerplate_line(text: &str) -> bool {
    let t = text.trim();
    t == "# Project Memory"
        || t == "# Global Memory"
        || t == "Curated long-term memory for this project."
        || t == "Curated long-term memory for this scope."
        || t.eq_ignore_ascii_case("curated long-term memory for this project.")
        || t.eq_ignore_ascii_case("curated long-term memory for this scope.")
}

/// Join entries with the § delimiter.
pub fn join_entries(entries: &[String]) -> String {
    entries
        .iter()
        .map(|e| e.trim())
        .filter(|e| !e.is_empty())
        .collect::<Vec<_>>()
        .join(ENTRY_DELIMITER)
}

/// Usage string for digests and tool responses.
pub fn usage(chars: usize, limit: usize) -> String {
    let pct = if limit == 0 {
        100
    } else {
        ((chars as f64 / limit as f64) * 100.0).round() as usize
    };
    format!("{pct}% - {chars}/{limit} chars")
}

/// Capacity header for digest injection.
pub fn capacity_header(label: &str, text: &str, limit: usize) -> String {
    format!("{label} [{}]", usage(text.len(), limit))
}

/// Char limit for a target.
pub fn limit_for(target: &str) -> Result<usize, HotApexError> {
    match target {
        "memory" | "global_memory" => Ok(MEMORY_CHAR_LIMIT),
        "user" => Ok(USER_CHAR_LIMIT),
        other => Err(HotApexError::InvalidTarget(other.into())),
    }
}

/// Absolute path for a target.
pub fn path_for(project_dir: &Path, home: &Path, target: &str) -> Result<PathBuf, HotApexError> {
    match target {
        "memory" => Ok(local_store::root(project_dir).join(local_store::MEMORY_CORE_PATH)),
        "global_memory" => Ok(home.join(GLOBAL_MEMORY_PATH)),
        "user" => Ok(user_profile::path(home)),
        other => Err(HotApexError::InvalidTarget(other.into())),
    }
}

/// Read raw text for a target (empty if missing).
pub fn read_text(project_dir: &Path, home: &Path, target: &str) -> Result<String, HotApexError> {
    let path = path_for(project_dir, home, target)?;
    Ok(fs::read_to_string(path).unwrap_or_default())
}

/// Render a digest block with capacity header (full body; no truncation).
pub fn render_for_digest(project_dir: &Path, home: &Path, target: &str) -> Option<String> {
    let limit = limit_for(target).ok()?;
    let text = read_text(project_dir, home, target).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty()
        || (matches!(target, "memory" | "global_memory") && is_only_skeleton(trimmed))
    {
        return None;
    }
    let label = match target {
        "user" => "USER PROFILE",
        "global_memory" => "GLOBAL MEMORY (curated facts)",
        _ => "MEMORY (curated facts)",
    };
    Some(format!(
        "{}\n{}",
        capacity_header(label, trimmed, limit),
        trimmed
    ))
}

fn is_only_skeleton(text: &str) -> bool {
    split_entries(text).is_empty()
}

/// Atomic write via temp + rename (best-effort drift guard between harnesses).
fn atomic_write_locked(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut out = File::create(&tmp)?;
        out.write_all(content.as_bytes())?;
        out.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn write_memory_body(path: &Path, entries: &[String], global: bool) -> std::io::Result<()> {
    let title = if global {
        "Global Memory"
    } else {
        "Project Memory"
    };
    let body = if entries.is_empty() {
        format!("# {title}\n\nCurated long-term memory for this scope.\n")
    } else {
        format!("# {title}\n\n{}\n", join_entries(entries))
    };
    atomic_write_locked(path, &body)
}

fn normalize_entry(content: &str, private: bool) -> String {
    let mut entry = content.trim().to_string();
    if private && !entry.contains(PRIVATE_MARKER) {
        entry = format!("{entry} {PRIVATE_MARKER}");
    }
    entry
}

/// Add an entry. Duplicate (exact match after trim) is a noop.
pub fn add(
    project_dir: &Path,
    home: &Path,
    target: &str,
    content: &str,
    private: bool,
) -> Result<MutationResult, HotApexError> {
    let limit = limit_for(target)?;
    let path = path_for(project_dir, home, target)?;
    let entry = normalize_entry(content, private);
    if entry.is_empty() {
        return Ok(MutationResult::err(
            usage(0, limit),
            Some(path),
            "content is required".into(),
            None,
        ));
    }

    if target == "user" {
        return add_user(home, &entry, limit);
    }

    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut entries = split_entries(&existing);
    if entries.iter().any(|e| e.trim() == entry.trim()) {
        return Ok(MutationResult::ok(
            usage(existing.trim().len(), limit),
            path,
            true,
        ));
    }
    entries.push(entry.clone());
    let candidate = join_entries(&entries);
    if candidate.len() > limit {
        // Pain-driven compaction: demote the oldest entries into the tier-2
        // wiki archive, then retry the add once. The cap errors only when
        // nothing more can be freed — never as the first answer.
        let needed = candidate.len() - limit;
        if let Ok(report) = compact_for_capacity(project_dir, home, target, needed, false) {
            if report.freed_chars >= needed {
                let after = fs::read_to_string(&path).unwrap_or_default();
                let mut entries = split_entries(&after);
                entries.push(entry);
                let candidate = join_entries(&entries);
                if candidate.len() <= limit {
                    write_memory_body(&path, &entries, target == "global_memory")?;
                    return Ok(MutationResult::ok(
                        usage(candidate.len(), limit),
                        path,
                        false,
                    ));
                }
            }
        }
        let current = split_entries(&existing);
        return Ok(MutationResult::err(
            usage(existing.trim().len(), limit),
            Some(path),
            format!(
                "would exceed cap at {} — consolidate with replace/remove first",
                usage(candidate.len(), limit)
            ),
            Some(current),
        ));
    }
    write_memory_body(&path, &entries, target == "global_memory")?;
    Ok(MutationResult::ok(
        usage(candidate.len(), limit),
        path,
        false,
    ))
}

fn add_user(home: &Path, entry: &str, limit: usize) -> Result<MutationResult, HotApexError> {
    let path = user_profile::path(home);
    let existing = user_profile::read(home).unwrap_or_default();
    let mut entries = split_entries(&existing);
    if entries.iter().any(|e| e.trim() == entry.trim()) {
        return Ok(MutationResult::ok(usage(existing.len(), limit), path, true));
    }
    // Prefer appending as a bullet under existing prose when the file is not
    // already §-structured (imported USER.md). Cap still applies to final body.
    let candidate = if existing.trim().is_empty() {
        entry.to_string()
    } else if existing.contains('§') || !existing.lines().any(|l| l.trim().starts_with('-')) {
        // Keep prose + append § entry block.
        let mut parts = split_entries(&existing);
        if parts.is_empty() && !existing.trim().is_empty() {
            parts.push(existing.trim().to_string());
        }
        parts.push(entry.to_string());
        join_entries(&parts)
    } else {
        entries.push(entry.to_string());
        format!("{}\n- {}\n", existing.trim_end(), entry)
    };
    if candidate.len() > limit {
        return Ok(MutationResult::err(
            usage(existing.len(), limit),
            Some(path),
            format!(
                "would exceed cap at {} — consolidate with replace/remove first",
                usage(candidate.len(), limit)
            ),
            Some(split_entries(&existing)),
        ));
    }
    user_profile::write(home, &candidate, Some("memory-tool"))?;
    Ok(MutationResult::ok(
        usage(candidate.len(), limit),
        user_profile::path(home),
        false,
    ))
}

// ---------------------------------------------------------------------
// Pain-driven hot-apex compaction (owner directive 2026-09-08)
// ---------------------------------------------------------------------

/// Wiki archive slug for demoted hot-apex entries (tier-2 store).
pub const APEX_ARCHIVE_SLUG: &str = "memory/apex-archive";
/// Prefix of the pointer entry left in the hot apex for the archive.
const POINTER_PREFIX: &str = "[apex archive]";
/// Headroom kept beyond the incoming entry so the next add does not
/// immediately re-trigger compaction.
const COMPACT_HEADROOM: usize = 512;

/// Outcome of a compaction run.
#[derive(Debug, Clone)]
pub struct CompactReport {
    /// Entries demoted into the archive (verbatim, oldest first).
    pub demoted_entries: Vec<String>,
    /// Chars freed in the hot apex (before the pointer line).
    pub freed_chars: usize,
    /// The archive page (tier-2 wiki).
    pub archive: PathBuf,
    /// True when nothing was written (read-only count).
    pub dry_run: bool,
    /// Human note (e.g. "dry-run", "nothing to demote").
    pub note: String,
}

/// Parse the running total from a pointer entry.
fn pointer_total(entry: &str) -> usize {
    entry
        .split_whitespace()
        .nth(2)
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Pain-driven hot-apex compaction: demote the OLDEST entries (verbatim,
/// order preserved) into the tier-2 wiki archive, leave a one-line pointer
/// in the hot apex, and record a tier-3 episodic note. Deterministic floor
/// always — summarization is a CLI-side, flag-gated addition, never part of
/// the demotion itself. Write order is crash-safe: archive first, episodic
/// second, the hot-apex rewrite last, so an interruption can only duplicate
/// (never lose) entries, and the archive dedups on the next run.
pub fn compact_for_capacity(
    project_dir: &Path,
    home: &Path,
    target: &str,
    needed_chars: usize,
    dry_run: bool,
) -> Result<CompactReport, HotApexError> {
    if !matches!(target, "memory" | "global_memory") {
        return Err(HotApexError::InvalidTarget(target.into()));
    }
    let limit = limit_for(target)?;
    let path = path_for(project_dir, home, target)?;
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let all = split_entries(&existing);
    let (pointers, mut entries): (Vec<String>, Vec<String>) = all
        .into_iter()
        .partition(|e| e.trim_start().starts_with(POINTER_PREFIX));
    let prior_total = pointers.first().map(|p| pointer_total(p)).unwrap_or(0);

    // Demote oldest-first until survivors fit the budget.
    let budget = limit.saturating_sub(needed_chars + COMPACT_HEADROOM);
    let mut demoted: Vec<String> = Vec::new();
    let mut survivor_body = join_entries(&entries);
    while survivor_body.len() > budget && !entries.is_empty() {
        demoted.push(entries.remove(0));
        survivor_body = join_entries(&entries);
    }
    let freed_chars = existing.trim().len().saturating_sub(survivor_body.len());
    let ts = local_store::now_rfc3339();
    let archive = crate::wiki::pages_dir(project_dir).join(format!("{APEX_ARCHIVE_SLUG}.md"));

    if demoted.is_empty() {
        return Ok(CompactReport {
            demoted_entries: Vec::new(),
            freed_chars,
            archive,
            dry_run,
            note: "nothing to demote (already within budget)".into(),
        });
    }
    if dry_run {
        return Ok(CompactReport {
            demoted_entries: demoted,
            freed_chars,
            archive,
            dry_run: true,
            note: "dry-run (read-only count; nothing written)".into(),
        });
    }

    // 1. Archive (tier-2 wiki): deduped bullets with a batch marker.
    let archive_text = fs::read_to_string(&archive).unwrap_or_default();
    let mut seen: std::collections::BTreeSet<String> = archive_text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("- "))
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    let mut body = archive_text.trim_end().to_string();
    if !body.is_empty() {
        body.push_str("\n\n");
    }
    body.push_str(&format!(
        "<!-- stateroot:demoted target={target} at={ts} count={} -->\n",
        demoted.len()
    ));
    body.push_str(&format!(
        "- [demoted {ts} · {target}] {} hot-apex entries archived\n",
        demoted.len()
    ));
    let mut added = 0usize;
    for entry in &demoted {
        let key = entry.split_whitespace().collect::<Vec<_>>().join(" ");
        if !seen.insert(key) {
            continue;
        }
        body.push_str(&format!("- {entry}\n"));
        added += 1;
    }
    crate::wiki::write_page(
        project_dir,
        APEX_ARCHIVE_SLUG,
        &body,
        "hot-apex demotions archive (pain-driven compaction)",
        "memory",
        None,
    )
    .map_err(|e| HotApexError::Io(std::io::Error::other(e.to_string())))?;

    // 2. Tier-3 episodic record (recall surface).
    local_store::append_episodic(
        project_dir,
        &serde_json::json!({
            "ts": ts,
            "harness": "cli",
            "kind": "apex_compact",
            "target": target,
            "demoted": added,
            "note": format!("apex compaction: {added} entries demoted from {target} → {APEX_ARCHIVE_SLUG} (pain-driven)"),
        }),
    )
    .map_err(|e| HotApexError::Io(std::io::Error::other(e.to_string())))?;

    // 3. Hot-apex rewrite: pointer first, survivors after.
    let pointer = format!(
        "{POINTER_PREFIX} {} entries demoted (latest {ts}) → {APEX_ARCHIVE_SLUG} — recall: stateroot memory recall \"<topic>\"",
        prior_total + added
    );
    let mut rewritten = vec![pointer];
    rewritten.extend(entries);
    write_memory_body(&path, &rewritten, target == "global_memory")?;

    Ok(CompactReport {
        demoted_entries: demoted,
        freed_chars,
        archive,
        dry_run: false,
        note: format!("{added} entries archived"),
    })
}

/// Append a synthesized summary of a demoted batch to the archive page
/// (flag-gated; labeled `synthesized` per the truth contract).
pub fn append_synthesized_summary(project_dir: &Path, summary: &str) -> Result<(), HotApexError> {
    let archive = crate::wiki::pages_dir(project_dir).join(format!("{APEX_ARCHIVE_SLUG}.md"));
    let existing = fs::read_to_string(&archive).unwrap_or_default();
    let ts = local_store::now_rfc3339();
    let body = format!(
        "{}\n\n<!-- stateroot:synthesized at={ts} -->\n- [synthesized summary {ts}] {}\n",
        existing.trim_end(),
        summary.trim()
    );
    crate::wiki::write_page(
        project_dir,
        APEX_ARCHIVE_SLUG,
        &body,
        "hot-apex demotions archive (pain-driven compaction)",
        "memory",
        None,
    )
    .map_err(|e| HotApexError::Io(std::io::Error::other(e.to_string())))?;
    Ok(())
}

/// Replace the first entry (or substring) matching `old_text`.
pub fn replace(
    project_dir: &Path,
    home: &Path,
    target: &str,
    old_text: &str,
    content: &str,
    private: bool,
) -> Result<MutationResult, HotApexError> {
    let limit = limit_for(target)?;
    let path = path_for(project_dir, home, target)?;
    let needle = old_text.trim();
    let replacement = normalize_entry(content, private);
    if needle.is_empty() || replacement.is_empty() {
        return Ok(MutationResult::err(
            usage(0, limit),
            Some(path),
            "old_text and content are required".into(),
            None,
        ));
    }

    if target == "user" {
        let existing = user_profile::read(home).unwrap_or_default();
        if !existing.contains(needle) {
            return Ok(MutationResult::err(
                usage(existing.len(), limit),
                Some(path),
                "old_text not found".into(),
                None,
            ));
        }
        let updated = existing.replacen(needle, &replacement, 1);
        if updated.len() > limit {
            return Ok(MutationResult::err(
                usage(existing.len(), limit),
                Some(path),
                format!(
                    "would exceed cap at {} — consolidate first",
                    usage(updated.len(), limit)
                ),
                Some(split_entries(&existing)),
            ));
        }
        user_profile::write(home, &updated, Some("memory-tool"))?;
        return Ok(MutationResult::ok(usage(updated.len(), limit), path, false));
    }

    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut entries = split_entries(&existing);
    if let Some(idx) = entries.iter().position(|e| e.contains(needle)) {
        entries[idx] = if entries[idx].trim() == needle {
            replacement
        } else {
            entries[idx].replacen(needle, &replacement, 1)
        };
    } else if existing.contains(needle) {
        let updated = existing.replacen(needle, &replacement, 1);
        if updated.len() > limit {
            return Ok(MutationResult::err(
                usage(existing.trim().len(), limit),
                Some(path),
                format!(
                    "would exceed cap at {} — consolidate first",
                    usage(updated.len(), limit)
                ),
                Some(split_entries(&existing)),
            ));
        }
        atomic_write_locked(&path, &updated)?;
        return Ok(MutationResult::ok(usage(updated.len(), limit), path, false));
    } else {
        return Ok(MutationResult::err(
            usage(existing.trim().len(), limit),
            Some(path),
            "old_text not found".into(),
            None,
        ));
    }
    let candidate = join_entries(&entries);
    if candidate.len() > limit {
        return Ok(MutationResult::err(
            usage(existing.trim().len(), limit),
            Some(path),
            format!(
                "would exceed cap at {} — consolidate first",
                usage(candidate.len(), limit)
            ),
            Some(split_entries(&existing)),
        ));
    }
    write_memory_body(&path, &entries, target == "global_memory")?;
    Ok(MutationResult::ok(
        usage(candidate.len(), limit),
        path,
        false,
    ))
}

/// Remove the first entry (or substring) matching `old_text`.
pub fn remove(
    project_dir: &Path,
    home: &Path,
    target: &str,
    old_text: &str,
) -> Result<MutationResult, HotApexError> {
    let limit = limit_for(target)?;
    let path = path_for(project_dir, home, target)?;
    let needle = old_text.trim();
    if needle.is_empty() {
        return Ok(MutationResult::err(
            usage(0, limit),
            Some(path),
            "old_text is required".into(),
            None,
        ));
    }

    if target == "user" {
        let existing = user_profile::read(home).unwrap_or_default();
        let mut entries = split_entries(&existing);
        let before = entries.len();
        entries.retain(|e| e.trim() != needle && !e.contains(needle));
        let updated = if entries.len() < before {
            join_entries(&entries)
        } else if existing.contains(needle) {
            let mut t = existing.replacen(needle, "", 1);
            while t.contains("\n\n\n") {
                t = t.replace("\n\n\n", "\n\n");
            }
            t.trim().to_string()
        } else {
            return Ok(MutationResult::err(
                usage(existing.len(), limit),
                Some(path),
                "old_text not found".into(),
                None,
            ));
        };
        user_profile::write(home, &updated, Some("memory-tool"))?;
        return Ok(MutationResult::ok(usage(updated.len(), limit), path, false));
    }

    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut entries = split_entries(&existing);
    let before = entries.len();
    entries.retain(|e| e.trim() != needle && !e.contains(needle));
    if entries.len() == before {
        if existing.contains(needle) {
            let mut updated = existing.replacen(needle, "", 1);
            while updated.contains("\n\n\n") {
                updated = updated.replace("\n\n\n", "\n\n");
            }
            atomic_write_locked(&path, updated.trim())?;
            return Ok(MutationResult::ok(
                usage(updated.trim().len(), limit),
                path,
                false,
            ));
        }
        return Ok(MutationResult::err(
            usage(existing.trim().len(), limit),
            Some(path),
            "old_text not found".into(),
            None,
        ));
    }
    write_memory_body(&path, &entries, target == "global_memory")?;
    let candidate = join_entries(&entries);
    Ok(MutationResult::ok(
        usage(candidate.len(), limit),
        path,
        false,
    ))
}

/// Migrate legacy `memory.md` bullets into `memories/MEMORY.md` once.
///
/// Project: `.stateroot/memory.md` → `.stateroot/memories/MEMORY.md`
/// User: `~/.stateroot/memory.md` → `~/.stateroot/memories/MEMORY.md`
///
/// Plan: copy unique bullets into MEMORY.md; do not merge into soul/USER.
pub fn migrate_legacy(project_dir: &Path, home: &Path) -> Result<usize, HotApexError> {
    let mut moved = 0usize;
    moved += migrate_one_file(
        &local_store::root(project_dir).join(LEGACY_PROJECT_MEMORY),
        project_dir,
        home,
        "memory",
    )?;
    moved += migrate_one_file(
        &home.join(".stateroot").join(LEGACY_USER_MEMORY),
        project_dir,
        home,
        "global_memory",
    )?;
    Ok(moved)
}

fn migrate_one_file(
    legacy: &Path,
    project_dir: &Path,
    home: &Path,
    target: &str,
) -> Result<usize, HotApexError> {
    if !legacy.is_file() {
        return Ok(0);
    }
    let text = fs::read_to_string(legacy).unwrap_or_default();
    let bullets = split_entries(&text);
    if bullets.is_empty() {
        let _ = fs::remove_file(legacy);
        return Ok(0);
    }
    let mut count = 0usize;
    for bullet in bullets {
        let result = add(
            project_dir,
            home,
            target,
            &bullet,
            bullet.contains(PRIVATE_MARKER),
        )?;
        if result.success && !result.noop {
            count += 1;
        }
    }
    // Rename aside so we never write it again; keep for audit.
    let bak = legacy.with_extension("md.migrated");
    let _ = fs::rename(legacy, &bak);
    Ok(count)
}

/// Convenience: ensure legacy migration ran (idempotent).
pub fn ensure_migrated(project_dir: &Path, home: &Path) {
    let _ = migrate_legacy(project_dir, home);
}

/// Show entries for a target.
pub fn show(project_dir: &Path, home: &Path, target: &str) -> Result<String, HotApexError> {
    let limit = limit_for(target)?;
    let text = read_text(project_dir, home, target)?;
    let entries = split_entries(&text);
    let mut out = format!(
        "{} — {} entries\n",
        usage(text.trim().len(), limit),
        entries.len()
    );
    for (i, e) in entries.iter().enumerate() {
        out.push_str(&format!("{}. {e}\n", i + 1));
    }
    if entries.is_empty() {
        out.push_str("(empty)\n");
    }
    Ok(out)
}

/// Entry is marked private.
pub fn is_private(entry: &str) -> bool {
    entry.contains(PRIVATE_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> (tempfile::TempDir, tempfile::TempDir) {
        let project = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "proj", "Test", "default").unwrap();
        let home = tempfile::tempdir().unwrap();
        (project, home)
    }

    #[test]
    fn add_replace_remove_roundtrip() {
        let (project, home) = dirs();
        let r = add(project.path(), home.path(), "memory", "port is 8080", false).unwrap();
        assert!(r.success && !r.noop);
        let r2 = add(project.path(), home.path(), "memory", "port is 8080", false).unwrap();
        assert!(r2.noop);
        let r3 = replace(
            project.path(),
            home.path(),
            "memory",
            "8080",
            "port is 9090",
            false,
        )
        .unwrap();
        assert!(r3.success);
        let body = read_text(project.path(), home.path(), "memory").unwrap();
        assert!(body.contains("9090"));
        assert!(!body.contains("8080"));
        let r4 = remove(project.path(), home.path(), "memory", "9090").unwrap();
        assert!(r4.success);
        assert!(
            split_entries(&read_text(project.path(), home.path(), "memory").unwrap()).is_empty()
        );
    }

    #[test]
    fn global_memory_is_separate_from_project_and_user_profile() {
        let (project, home) = dirs();
        let result = add(
            project.path(),
            home.path(),
            "global_memory",
            "standard template lives here",
            true,
        )
        .unwrap();
        assert!(result.success);
        assert_eq!(
            result.path.as_deref(),
            Some(home.path().join(GLOBAL_MEMORY_PATH).as_path())
        );
        let global = read_text(project.path(), home.path(), "global_memory").unwrap();
        assert!(global.contains("standard template lives here"));
        assert!(global.contains(PRIVATE_MARKER));
        assert!(
            split_entries(&read_text(project.path(), home.path(), "memory").unwrap()).is_empty()
        );
        assert!(user_profile::read(home.path()).is_none());
    }

    #[test]
    fn overflow_compacts_instead_of_erroring() {
        let (project, home) = dirs();
        let big = "x".repeat(MEMORY_CHAR_LIMIT - 10);
        add(project.path(), home.path(), "memory", &big, false).unwrap();
        let r = add(
            project.path(),
            home.path(),
            "memory",
            "another fact that is long enough",
            false,
        )
        .unwrap();
        // The cap is now answered by pain-driven compaction, not an error.
        assert!(r.success, "overflow compacts: {:?}", r.error);
        // The big entry demoted into the archive, never dropped.
        let archive = std::fs::read_to_string(
            crate::wiki::pages_dir(project.path()).join("memory/apex-archive.md"),
        )
        .expect("archive");
        assert!(archive.contains(&"x".repeat(200)), "big entry archived");
        let body = read_text(project.path(), home.path(), "memory").unwrap();
        assert!(body.contains("another fact that is long enough"));
        assert!(body.contains("[apex archive]"), "{body}");
    }

    #[test]
    fn migrate_legacy_memory_md() {
        let (project, home) = dirs();
        let legacy = local_store::root(project.path()).join("memory.md");
        fs::write(&legacy, "- alpha fact\n- beta fact\n").unwrap();
        let n = migrate_legacy(project.path(), home.path()).unwrap();
        assert_eq!(n, 2);
        let body = read_text(project.path(), home.path(), "memory").unwrap();
        assert!(body.contains("alpha fact"));
        assert!(body.contains("beta fact"));
        assert!(!legacy.exists());
        assert!(legacy.with_extension("md.migrated").exists());
    }

    #[test]
    fn migrate_user_legacy_memory_to_global_scope() {
        let (project, home) = dirs();
        let legacy = home.path().join(".stateroot/memory.md");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, "- global fact\n").unwrap();
        let n = migrate_legacy(project.path(), home.path()).unwrap();
        assert_eq!(n, 1);
        let global = read_text(project.path(), home.path(), "global_memory").unwrap();
        assert!(global.contains("global fact"));
        assert!(
            split_entries(&read_text(project.path(), home.path(), "memory").unwrap()).is_empty()
        );
    }

    #[test]
    fn capacity_header_format() {
        let h = capacity_header("MEMORY", "abcd", 100);
        assert!(h.contains("MEMORY"));
        assert!(h.contains("4/100"));
    }

    fn fill_near_cap(project: &Path, home: &Path, count: usize, width: usize) {
        let path = path_for(project, home, "memory").unwrap();
        let entries: Vec<String> = (0..count)
            .map(|i| format!("entry {i}: {}", "x".repeat(width)))
            .collect();
        write_memory_body(&path, &entries, false).unwrap();
    }

    #[test]
    fn compact_demotes_oldest_into_archive_and_leaves_pointer() {
        let (project, home) = dirs();
        fill_near_cap(project.path(), home.path(), 10, 600);
        let report = compact_for_capacity(project.path(), home.path(), "memory", 1500, false)
            .expect("compact");
        assert!(!report.demoted_entries.is_empty());
        assert!(report.freed_chars > 0);
        // Oldest first, verbatim.
        assert!(report.demoted_entries[0].starts_with("entry 0:"));

        let archive = std::fs::read_to_string(&report.archive).expect("archive");
        assert!(archive.contains("stateroot:demoted"), "{archive}");
        assert!(archive.contains("entry 0:"), "{archive}");
        // Survivors keep their order.
        let body = read_text(project.path(), home.path(), "memory").unwrap();
        assert!(body.contains("[apex archive]"), "{body}");
        let entries = split_entries(&body);
        assert!(entries[0].starts_with("[apex archive]"), "{entries:?}");
        let total: usize = entries[0]
            .split_whitespace()
            .nth(2)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(total, report.demoted_entries.len());
        // Tier-3 episodic record exists.
        let episodic =
            std::fs::read_to_string(project.path().join(".stateroot/memories/episodic.jsonl"))
                .expect("episodic");
        assert!(episodic.contains("apex_compact"), "{episodic}");
    }

    #[test]
    fn add_over_capacity_triggers_compaction_and_succeeds() {
        let (project, home) = dirs();
        fill_near_cap(project.path(), home.path(), 12, 620);
        let before = read_text(project.path(), home.path(), "memory").unwrap();
        assert!(
            before.trim().len() > 7000,
            "near-cap fixture: {}",
            before.len()
        );
        let r = add(
            project.path(),
            home.path(),
            "memory",
            &format!("the pain-trigger fact: {}", "y".repeat(400)),
            false,
        )
        .unwrap();
        assert!(
            r.success,
            "add should succeed after compaction: {:?}",
            r.error
        );
        let body = read_text(project.path(), home.path(), "memory").unwrap();
        assert!(body.contains("the pain-trigger fact"), "{body}");
        assert!(body.contains("[apex archive]"), "{body}");
        assert!(
            body.trim().len() <= MEMORY_CHAR_LIMIT,
            "over cap: {}",
            body.len()
        );
        let archive = std::fs::read_to_string(
            crate::wiki::pages_dir(project.path()).join("memory/apex-archive.md"),
        )
        .expect("archive");
        assert!(archive.contains("entry 0:"), "{archive}");
    }

    #[test]
    fn compact_dry_run_writes_nothing_and_rerun_idempotent() {
        let (project, home) = dirs();
        fill_near_cap(project.path(), home.path(), 10, 600);
        let before = read_text(project.path(), home.path(), "memory").unwrap();
        let report = compact_for_capacity(project.path(), home.path(), "memory", 1500, true)
            .expect("dry-run");
        assert!(report.dry_run);
        let after = read_text(project.path(), home.path(), "memory").unwrap();
        assert_eq!(before, after, "dry-run must not write");
        assert!(
            !crate::wiki::pages_dir(project.path())
                .join("memory/apex-archive.md")
                .exists(),
            "dry-run must not create the archive"
        );
        // Two real runs: no duplicated archive bullets.
        compact_for_capacity(project.path(), home.path(), "memory", 1500, false).expect("run 1");
        compact_for_capacity(project.path(), home.path(), "memory", 1500, false).expect("run 2");
        let archive = std::fs::read_to_string(
            crate::wiki::pages_dir(project.path()).join("memory/apex-archive.md"),
        )
        .expect("archive");
        let e0 = archive.matches("entry 0:").count();
        assert_eq!(e0, 1, "no duplicated bullets: {archive}");
    }

    #[test]
    fn add_single_entry_bigger_than_cap_still_errors_honestly() {
        let (project, home) = dirs();
        fill_near_cap(project.path(), home.path(), 6, 600);
        let huge = "z".repeat(MEMORY_CHAR_LIMIT + 500);
        let r = add(project.path(), home.path(), "memory", &huge, false).unwrap();
        assert!(!r.success, "a too-large entry must keep the honest error");
        assert!(r.error.unwrap_or_default().contains("would exceed cap"));
    }
}
