//! Soul federation — two-way sync between the canonical soul and
//! harness-native persona files (openclaw `IDENTITY.md` + `SOUL.md`, hermes
//! `SOUL.md`). Personality is authored wherever the owner lives; StateRoot
//! carries it outward to every harness.
//!
//! Three-way against a per-source baseline hash of the *normalized* text:
//!
//! - native changed, canonical untouched → adopt native into canonical
//!   (history-snapshotted), then push the new canonical to every other
//!   source;
//! - canonical changed, native untouched → push canonical into the source's
//!   native file(s) (backup first, synced-marker written);
//! - both changed and they differ → **conflict**: recorded and surfaced in
//!   the digest, never silently resolved (`soul sync --accept-theirs|--
//!   accept-mine <source>`);
//! - equal → converged, baseline advances.
//!
//! Normalization strips our own machinery — provenance stamps, the
//! composed-workspace comment, the synced marker, and the composed section
//! headers — so a push→compose→compare round trip is stable and never
//! ping-pongs. Bootstrap (no baseline): equal links silently; differing is
//! a conflict, so a first sync can never clobber a foreign persona.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

/// Marker written at the top of native files we push to.
pub const SYNCED_MARKER: &str = "<!-- stateroot:synced v1 -->";
const STATE_FILE: &str = "sync-state.json";
const IDENTITY_HEADER: &str = "## Identity (IDENTITY.md)";
const PERSONA_HEADER: &str = "## Persona (SOUL.md)";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SyncState {
    #[serde(default)]
    last_run: String,
    #[serde(default)]
    sources: std::collections::BTreeMap<String, Baseline>,
    #[serde(default)]
    conflicts: Vec<ConflictRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Baseline {
    hash: String,
    at: String,
}

/// A pending sync conflict (both sides changed, or bootstrap divergence).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictRecord {
    pub source: String,
    pub detail: String,
    pub at: String,
}

/// Outcome of one sync pass.
#[derive(Debug, Default)]
pub struct SyncReport {
    pub actions: Vec<String>,
    pub conflicts: Vec<ConflictRecord>,
    /// The canonical soul was replaced (caller should refresh persona caches).
    pub canonical_changed: bool,
}

// ---------------------------------------------------------------------
// normalization + hashing
// ---------------------------------------------------------------------

/// Lines that are sync machinery, never content: provenance stamps, the
/// composed-workspace comment, the synced marker, and the composed section
/// headers (re-added by openclaw composition on every read).
fn is_machinery_line(trimmed: &str) -> bool {
    trimmed.starts_with("<!-- stateroot:soul")
        || trimmed.starts_with("<!-- imported from")
        || trimmed.starts_with("<!-- composed from openclaw")
        || trimmed.starts_with("<!-- stateroot:synced")
        || trimmed == IDENTITY_HEADER
        || trimmed == PERSONA_HEADER
}

/// Canonical-comparable form: machinery lines dropped, trailing whitespace
/// trimmed, blank runs collapsed to one, ends trimmed.
pub fn normalize(text: &str) -> String {
    let mut out = String::new();
    let mut blank = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if is_machinery_line(trimmed) {
            continue;
        }
        if trimmed.is_empty() {
            if blank {
                continue;
            }
            blank = true;
        } else {
            blank = false;
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.trim().to_string()
}

fn hash(text: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(normalize(text).as_bytes());
    format!("{:x}", hasher.finalize())
}

// ---------------------------------------------------------------------
// state
// ---------------------------------------------------------------------

fn state_path(home: &Path) -> PathBuf {
    home.join(crate::soul::SOUL_DIR).join(STATE_FILE)
}

fn load_state(home: &Path) -> SyncState {
    let Ok(text) = std::fs::read_to_string(state_path(home)) else {
        return SyncState::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn save_state(home: &Path, state: &SyncState) {
    let path = state_path(home);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(path, format!("{json}\n"));
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------
// sources
// ---------------------------------------------------------------------

struct Source {
    id: &'static str,
    /// Native text in canonical-comparable shape (None = source absent).
    text: String,
}

/// Discover present sources: openclaw (first identity pack with a persona)
/// and hermes (`SOUL.md`).
fn discover_sources(home: &Path) -> Vec<Source> {
    let mut out = Vec::new();
    if let Some(pack) = crate::openclaw_identity::discover_openclaw_identities(home)
        .into_iter()
        .find(|p| !p.persona_markdown.trim().is_empty())
    {
        out.push(Source {
            id: "openclaw",
            text: pack.persona_markdown,
        });
    }
    let hermes = crate::soul::hermes_home(home).join("SOUL.md");
    if let Ok(text) = std::fs::read_to_string(&hermes) {
        if !text.trim().is_empty() {
            out.push(Source {
                id: "hermes",
                text: text.trim().to_string(),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------
// native writes
// ---------------------------------------------------------------------

fn backup_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("SOUL.md");
    path.with_file_name(format!("{name}.stateroot-bak"))
}

/// Write one native file (backup first), unless already in sync.
fn write_native_file(path: &Path, body: &str, dry_run: bool, actions: &mut Vec<String>) {
    let body = body.trim();
    let current = std::fs::read_to_string(path).unwrap_or_default();
    if !current.trim().is_empty() && normalize(&current) == normalize(body) {
        return; // already in sync
    }
    if dry_run {
        actions.push(format!("would push → {}", path.display()));
        return;
    }
    if !current.trim().is_empty() {
        let _ = std::fs::write(backup_path(path), &current);
    }
    match std::fs::write(path, format!("{SYNCED_MARKER}\n\n{body}\n")) {
        Ok(()) => actions.push(format!("pushed → {}", path.display())),
        Err(err) => actions.push(format!("error writing {}: {err}", path.display())),
    }
}

/// Empty out a managed native file whose section no longer exists in the
/// canonical (marker-only body composes to nothing, so compares stay
/// stable). Foreign content is never emptied — that is a conflict instead.
fn empty_managed_file(path: &Path, dry_run: bool, actions: &mut Vec<String>) {
    let current = std::fs::read_to_string(path).unwrap_or_default();
    if normalize(&current).is_empty() {
        return;
    }
    if dry_run {
        actions.push(format!("would empty (managed) {}", path.display()));
        return;
    }
    let _ = std::fs::write(backup_path(path), &current);
    let _ = std::fs::write(path, format!("{SYNCED_MARKER}\n"));
    actions.push(format!("emptied (managed) {}", path.display()));
}

/// Split the canonical into openclaw's two files: the identity section
/// (between the composed headers) and everything else.
fn split_for_openclaw(canonical: &str) -> (Option<String>, String) {
    let mut identity: Option<String> = None;
    let mut persona_lines: Vec<&str> = Vec::new();
    let mut in_identity = false;
    let mut in_persona = false;
    let mut h1_dropped = false;
    for line in canonical.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("<!--") {
            continue; // stamps/comments are machinery
        }
        if trimmed == IDENTITY_HEADER {
            in_identity = true;
            in_persona = false;
            identity = Some(String::new());
            continue;
        }
        if trimmed == PERSONA_HEADER {
            in_persona = true;
            in_identity = false;
            continue;
        }
        // Drop the composed doc's own "# Soul" wrapper (first H1).
        if !h1_dropped && trimmed.starts_with("# ") && !trimmed.starts_with("## ") {
            h1_dropped = true;
            continue;
        }
        if in_identity {
            if let Some(buf) = identity.as_mut() {
                buf.push_str(line);
                buf.push('\n');
            }
        } else {
            let _ = in_persona;
            persona_lines.push(line);
        }
    }
    (
        identity
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        persona_lines.join("\n").trim().to_string(),
    )
}

/// Push the canonical into one source's native files.
fn push_source(
    home: &Path,
    source_id: &str,
    canonical: &str,
    dry_run: bool,
    actions: &mut Vec<String>,
) {
    match source_id {
        "openclaw" => {
            let Some(pack) = crate::openclaw_identity::discover_openclaw_identities(home)
                .into_iter()
                .find(|p| !p.persona_markdown.trim().is_empty())
            else {
                return;
            };
            let (identity, persona) = split_for_openclaw(canonical);
            match identity {
                Some(body) => {
                    write_native_file(&pack.workspace.join("IDENTITY.md"), &body, dry_run, actions)
                }
                None => empty_managed_file(&pack.workspace.join("IDENTITY.md"), dry_run, actions),
            }
            write_native_file(&pack.workspace.join("SOUL.md"), &persona, dry_run, actions);
        }
        "hermes" => {
            let path = crate::soul::hermes_home(home).join("SOUL.md");
            write_native_file(&path, &normalize(canonical), dry_run, actions);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------
// sync
// ---------------------------------------------------------------------

/// One sync pass over every discovered source. `dry_run` reports without
/// writing (conflicts are still surfaced; state is not persisted).
pub fn sync(home: &Path, dry_run: bool) -> SyncReport {
    let mut report = SyncReport::default();
    let Some(canonical) = crate::soul::read_canonical(home) else {
        report
            .actions
            .push("no canonical soul — nothing to sync".to_string());
        return report;
    };
    let sources = discover_sources(home);
    if sources.is_empty() {
        report
            .actions
            .push("no harness-native persona sources found".to_string());
        return report;
    }
    let mut state = load_state(home);
    let canonical_hash = hash(&canonical);
    // Conflict sources are skipped by the automatic pass until resolved.
    let conflicted: std::collections::BTreeSet<String> =
        state.conflicts.iter().map(|c| c.source.clone()).collect();

    for source in &sources {
        if conflicted.contains(source.id) {
            report.actions.push(format!(
                "{}: conflict pending — skipping (resolve with `soul sync --accept-theirs|--accept-mine {0}`)",
                source.id
            ));
            continue;
        }
        let native_hash = hash(&source.text);
        match state.sources.get(source.id) {
            None => {
                // Bootstrap: equal links silently; differing is a conflict —
                // a first sync never clobbers a foreign persona.
                if native_hash == canonical_hash {
                    state.sources.insert(
                        source.id.to_string(),
                        Baseline {
                            hash: canonical_hash.clone(),
                            at: now(),
                        },
                    );
                    report
                        .actions
                        .push(format!("{}: linked (already in sync)", source.id));
                } else {
                    let record = ConflictRecord {
                        source: source.id.to_string(),
                        detail: "bootstrap: native and canonical personas differ".to_string(),
                        at: now(),
                    };
                    report.conflicts.push(record.clone());
                    state.conflicts.push(record);
                }
            }
            Some(baseline) => {
                let base = baseline.hash.clone();
                let native_changed = native_hash != base;
                let canonical_changed = canonical_hash != base;
                match (native_changed, canonical_changed) {
                    (false, false) => {}
                    (true, false) => {
                        // Adopt native → canonical, then push outward.
                        if dry_run {
                            report.actions.push(format!(
                                "{}: would adopt native edit into canonical",
                                source.id
                            ));
                        } else {
                            match crate::soul::write_canonical(
                                home,
                                &source.text,
                                Some(&format!("sync:{}", source.id)),
                            ) {
                                Ok(note) => {
                                    report.actions.push(format!(
                                        "{}: adopted native edit into canonical",
                                        source.id
                                    ));
                                    report.canonical_changed = true;
                                    let _ = note;
                                }
                                Err(err) => {
                                    report
                                        .actions
                                        .push(format!("{}: adopt failed: {err}", source.id));
                                    continue;
                                }
                            }
                            // Carry the new canonical to every other source.
                            let new_canonical = crate::soul::read_canonical(home)
                                .unwrap_or_else(|| source.text.clone());
                            let new_hash = hash(&new_canonical);
                            for other in &sources {
                                if other.id == source.id || conflicted.contains(other.id) {
                                    continue;
                                }
                                push_source(
                                    home,
                                    other.id,
                                    &new_canonical,
                                    dry_run,
                                    &mut report.actions,
                                );
                                state.sources.insert(
                                    other.id.to_string(),
                                    Baseline {
                                        hash: new_hash.clone(),
                                        at: now(),
                                    },
                                );
                            }
                            state.sources.insert(
                                source.id.to_string(),
                                Baseline {
                                    hash: new_hash,
                                    at: now(),
                                },
                            );
                        }
                    }
                    (false, true) => {
                        if dry_run {
                            report.actions.push(format!(
                                "{}: would push canonical edit into native files",
                                source.id
                            ));
                        } else {
                            push_source(home, source.id, &canonical, dry_run, &mut report.actions);
                            state.sources.insert(
                                source.id.to_string(),
                                Baseline {
                                    hash: canonical_hash.clone(),
                                    at: now(),
                                },
                            );
                        }
                    }
                    (true, true) => {
                        if native_hash == canonical_hash {
                            // Converged independently — just advance the baseline.
                            state.sources.insert(
                                source.id.to_string(),
                                Baseline {
                                    hash: canonical_hash.clone(),
                                    at: now(),
                                },
                            );
                        } else {
                            let record = ConflictRecord {
                                source: source.id.to_string(),
                                detail: "native and canonical personas both changed".to_string(),
                                at: now(),
                            };
                            report.conflicts.push(record.clone());
                            state.conflicts.push(record);
                        }
                    }
                }
            }
        }
    }

    if !dry_run {
        state.last_run = now();
        save_state(home, &state);
    }
    report
}

/// Resolve a pending conflict: `theirs` adopts the native copy into the
/// canonical (then pushes outward); `mine` pushes the canonical over the
/// source's native files.
pub fn accept(home: &Path, source_id: &str, theirs: bool) -> SyncReport {
    let mut report = SyncReport::default();
    let mut state = load_state(home);
    state.conflicts.retain(|c| c.source != source_id);
    if theirs {
        let sources = discover_sources(home);
        if let Some(source) = sources.iter().find(|s| s.id == source_id) {
            match crate::soul::write_canonical(
                home,
                &source.text,
                Some(&format!("sync:{source_id}")),
            ) {
                Ok(_) => {
                    report
                        .actions
                        .push(format!("{source_id}: adopted native copy into canonical"));
                    report.canonical_changed = true;
                }
                Err(err) => {
                    report
                        .actions
                        .push(format!("{source_id}: adopt failed: {err}"));
                }
            }
        } else {
            report
                .actions
                .push(format!("{source_id}: source not present"));
        }
    }
    if let Some(canonical) = crate::soul::read_canonical(home) {
        let canonical_hash = hash(&canonical);
        push_source(home, source_id, &canonical, false, &mut report.actions);
        state.sources.insert(
            source_id.to_string(),
            Baseline {
                hash: canonical_hash.clone(),
                at: now(),
            },
        );
        // An accept-theirs also carries the new canonical to every other source.
        if theirs {
            for other in discover_sources(home) {
                if other.id == source_id {
                    continue;
                }
                push_source(home, other.id, &canonical, false, &mut report.actions);
                state.sources.insert(
                    other.id.to_string(),
                    Baseline {
                        hash: canonical_hash.clone(),
                        at: now(),
                    },
                );
            }
        }
    }
    state.last_run = now();
    save_state(home, &state);
    report
}

/// Pending conflicts (digest surface; empty when clean).
pub fn pending_conflicts(home: &Path) -> Vec<ConflictRecord> {
    load_state(home).conflicts
}

/// Hook path: sync at most once per `interval_hours` of agent activity.
/// Returns the report when a pass actually ran.
pub fn maybe_auto(home: &Path, interval_hours: i64) -> Option<SyncReport> {
    let state = load_state(home);
    if !state.last_run.is_empty() {
        if let Ok(at) = chrono::DateTime::parse_from_rfc3339(&state.last_run) {
            if (chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_hours()
                < interval_hours.max(1)
            {
                return None;
            }
        }
    }
    crate::soul::read_canonical(home)?;
    let report = sync(home, false);
    Some(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn home_with_canonical(canonical: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().expect("home");
        crate::soul::write_canonical(home.path(), canonical, Some("test")).expect("write");
        home
    }

    fn openclaw_ws(home: &Path) -> PathBuf {
        home.join(".openclaw/workspace")
    }

    fn seed_openclaw(home: &Path, identity: &str, soul: &str) {
        let ws = openclaw_ws(home);
        fs::create_dir_all(&ws).expect("ws");
        fs::write(ws.join("IDENTITY.md"), identity).expect("id");
        fs::write(ws.join("SOUL.md"), soul).expect("soul");
    }

    const ID: &str = "# Identity\n\n- Name: Marid\n";
    const SOUL: &str = "# Soul\n\nI am Marid, jinn of the lamp.\n";

    #[test]
    fn normalize_strips_machinery_and_collapses_blanks() {
        let text = "<!-- stateroot:soul origin=x; at=y -->\n# Soul\n\n<!-- composed from openclaw workspace /p -->\n\n## Identity (IDENTITY.md)\n\nA\n\n\n\n## Persona (SOUL.md)\n\nB\n<!-- stateroot:synced v1 -->\n";
        assert_eq!(normalize(text), "# Soul\n\nA\n\nB");
    }

    #[test]
    fn bootstrap_equal_links_silently() {
        let composed = crate::openclaw_identity::discover_openclaw_identities;
        let home = tempfile::tempdir().expect("home");
        seed_openclaw(home.path(), ID, SOUL);
        let pack = composed(home.path()).pop().expect("pack");
        crate::soul::write_canonical(home.path(), &pack.persona_markdown, Some("openclaw"))
            .expect("write");
        let report = sync(home.path(), false);
        assert!(report.conflicts.is_empty(), "{report:?}");
        assert!(
            report.actions.iter().any(|a| a.contains("linked")),
            "{report:?}"
        );
        // Second pass: fully converged, no actions.
        let report = sync(home.path(), false);
        assert!(report.actions.is_empty(), "{report:?}");
    }

    #[test]
    fn bootstrap_differ_is_a_conflict_and_writes_nothing() {
        let home = home_with_canonical("# Soul\n\nCanonical persona\n");
        seed_openclaw(home.path(), ID, SOUL);
        let report = sync(home.path(), false);
        assert_eq!(report.conflicts.len(), 1, "{report:?}");
        assert!(!report.canonical_changed);
        let native = fs::read_to_string(openclaw_ws(home.path()).join("SOUL.md")).expect("native");
        assert!(native.contains("jinn of the lamp"), "native untouched");
        // The conflict persists and skips automatic passes.
        let report = sync(home.path(), false);
        assert!(
            report
                .actions
                .iter()
                .any(|a| a.contains("conflict pending")),
            "{report:?}"
        );
    }

    #[test]
    fn native_edit_is_adopted_and_pushed_outward() {
        let home = tempfile::tempdir().expect("home");
        seed_openclaw(home.path(), ID, SOUL);
        let pack = crate::openclaw_identity::discover_openclaw_identities(home.path())
            .pop()
            .expect("pack");
        crate::soul::write_canonical(home.path(), &pack.persona_markdown, Some("openclaw"))
            .expect("write");
        let report = sync(home.path(), false);
        assert!(report.actions.iter().any(|a| a.contains("linked")));

        // The owner edits their openclaw persona.
        fs::write(
            openclaw_ws(home.path()).join("SOUL.md"),
            "# Soul\n\nI am Marid, jinn of the lamp — now formal.\n",
        )
        .expect("edit");
        let report = sync(home.path(), false);
        assert!(report.canonical_changed, "{report:?}");
        assert!(
            report.actions.iter().any(|a| a.contains("adopted")),
            "{report:?}"
        );
        let canonical = crate::soul::read_canonical(home.path()).expect("canonical");
        assert!(canonical.contains("now formal"));
        // Converged: the next pass is silent.
        let report = sync(home.path(), false);
        assert!(report.actions.is_empty(), "{report:?}");
    }

    #[test]
    fn canonical_edit_is_pushed_to_native_files_with_backup() {
        let home = tempfile::tempdir().expect("home");
        seed_openclaw(home.path(), ID, SOUL);
        let pack = crate::openclaw_identity::discover_openclaw_identities(home.path())
            .pop()
            .expect("pack");
        crate::soul::write_canonical(home.path(), &pack.persona_markdown, Some("openclaw"))
            .expect("write");
        let _ = sync(home.path(), false);

        // Canonical edited via the canonical store (any harness, `soul propose`).
        let composed = pack
            .persona_markdown
            .replace("jinn of the lamp", "jinn of the brass lamp");
        crate::soul::write_canonical(home.path(), &composed, Some("propose")).expect("edit");
        let report = sync(home.path(), false);
        assert!(
            report.actions.iter().any(|a| a.contains("pushed")),
            "{report:?}"
        );
        let native = fs::read_to_string(openclaw_ws(home.path()).join("SOUL.md")).expect("native");
        assert!(native.contains("brass lamp"));
        assert!(native.contains(SYNCED_MARKER));
        let bak = fs::read_to_string(openclaw_ws(home.path()).join("SOUL.md.stateroot-bak"))
            .expect("bak");
        assert!(bak.contains("jinn of the lamp"), "backup preserves the old");
        let report = sync(home.path(), false);
        assert!(report.actions.is_empty(), "{report:?}");
    }

    #[test]
    fn conflict_accept_theirs_and_accept_mine() {
        let home = tempfile::tempdir().expect("home");
        seed_openclaw(home.path(), ID, SOUL);
        let pack = crate::openclaw_identity::discover_openclaw_identities(home.path())
            .pop()
            .expect("pack");
        crate::soul::write_canonical(home.path(), &pack.persona_markdown, Some("openclaw"))
            .expect("write");
        let _ = sync(home.path(), false);

        // Both sides move.
        fs::write(
            openclaw_ws(home.path()).join("SOUL.md"),
            "# Soul\n\nnative side\n",
        )
        .expect("edit");
        crate::soul::write_canonical(home.path(), "# Soul\n\ncanonical side\n", Some("propose"))
            .expect("edit");
        let report = sync(home.path(), false);
        assert_eq!(report.conflicts.len(), 1, "{report:?}");

        // accept-theirs: native wins, canonical replaced, converged.
        let report = accept(home.path(), "openclaw", true);
        assert!(report.canonical_changed, "{report:?}");
        assert!(pending_conflicts(home.path()).is_empty());
        let canonical = crate::soul::read_canonical(home.path()).expect("canonical");
        assert!(canonical.contains("native side"));
        let report = sync(home.path(), false);
        assert!(
            report.conflicts.is_empty() && report.actions.is_empty(),
            "{report:?}"
        );

        // Diverge again, then accept-mine: canonical wins, native rewritten.
        fs::write(
            openclaw_ws(home.path()).join("SOUL.md"),
            "# Soul\n\nnative again\n",
        )
        .expect("edit");
        crate::soul::write_canonical(home.path(), "# Soul\n\ncanonical again\n", Some("propose"))
            .expect("edit");
        let _ = sync(home.path(), false);
        let _ = accept(home.path(), "openclaw", false);
        let native = fs::read_to_string(openclaw_ws(home.path()).join("SOUL.md")).expect("native");
        assert!(native.contains("canonical again"));
        assert!(pending_conflicts(home.path()).is_empty());
    }

    #[test]
    fn hermes_round_trip() {
        let home = tempfile::tempdir().expect("home");
        let hermes = home.path().join(".hermes");
        fs::create_dir_all(&hermes).expect("hermes");
        fs::write(hermes.join("SOUL.md"), "# Soul\n\nhermes voice\n").expect("soul");
        crate::soul::write_canonical(
            home.path(),
            "# Soul\n\nhermes voice\n",
            Some("hermes-agent"),
        )
        .expect("write");
        let report = sync(home.path(), false);
        assert!(
            report.actions.iter().any(|a| a.contains("linked")),
            "{report:?}"
        );

        // Canonical edit → hermes receives it.
        crate::soul::write_canonical(home.path(), "# Soul\n\nshared voice\n", Some("propose"))
            .expect("edit");
        let report = sync(home.path(), false);
        assert!(
            report.actions.iter().any(|a| a.contains("pushed")),
            "{report:?}"
        );
        let native = fs::read_to_string(hermes.join("SOUL.md")).expect("native");
        assert!(native.contains("shared voice"));

        // Hermes-native edit → adopted back.
        fs::write(hermes.join("SOUL.md"), "# Soul\n\nhermes again\n").expect("edit");
        let report = sync(home.path(), false);
        assert!(report.canonical_changed, "{report:?}");
        let canonical = crate::soul::read_canonical(home.path()).expect("canonical");
        assert!(canonical.contains("hermes again"));
    }

    #[test]
    fn no_canonical_is_a_note_not_an_error() {
        let home = tempfile::tempdir().expect("home");
        seed_openclaw(home.path(), ID, SOUL);
        let report = sync(home.path(), false);
        assert!(report.actions.iter().any(|a| a.contains("no canonical")));
        assert!(report.conflicts.is_empty());
    }

    #[test]
    fn dry_run_writes_nothing() {
        let home = tempfile::tempdir().expect("home");
        seed_openclaw(home.path(), ID, SOUL);
        let pack = crate::openclaw_identity::discover_openclaw_identities(home.path())
            .pop()
            .expect("pack");
        crate::soul::write_canonical(home.path(), &pack.persona_markdown, Some("openclaw"))
            .expect("write");
        let _ = sync(home.path(), false);
        crate::soul::write_canonical(home.path(), "# Soul\n\nchanged\n", Some("propose"))
            .expect("edit");
        let report = sync(home.path(), true);
        assert!(
            report.actions.iter().any(|a| a.contains("would push")),
            "{report:?}"
        );
        let native = fs::read_to_string(openclaw_ws(home.path()).join("SOUL.md")).expect("native");
        assert!(native.contains("jinn of the lamp"), "dry run never writes");
    }
}
