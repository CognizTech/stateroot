//! Plan federation — harness-native plans (Cursor plan mode's
//! `~/.cursor/plans/*.plan.md`, Claude's `~/.claude/plans/`, …) land in the
//! central plan store as drafts. Without this, a plan authored in a harness's
//! native plan mode never reaches the shared store, and the next harness
//! cannot see it (the cursor-plan continuity gap).
//!
//! Attribution is by construction: a harness ingests its *own* plan dir, at
//! its own session boundaries, into the project it was working on. No
//! cross-harness guessing. Dedup is by content hash; a native plan edited
//! while still a draft updates the draft in place; once a plan moves past
//! draft it is owned and never overwritten.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

/// Per-project sync cursor (local lane — never in snapshots or git).
const STATE_REL: &str = "local/plan-federation.json";

/// One harness-native plan source directory.
struct PlanSource {
    harness: &'static str,
    dir: PathBuf,
    /// File suffix that marks a plan (`*.plan.md` for cursor, `*.md` claude).
    suffix: &'static str,
}

/// Known native plan dirs for a harness id (already normalized).
fn plan_dirs(home: &Path, harness: &str) -> Vec<PlanSource> {
    match harness {
        "cursor" => vec![PlanSource {
            harness: "cursor",
            dir: home.join(".cursor/plans"),
            suffix: ".plan.md",
        }],
        "claude" | "claude-code" => vec![PlanSource {
            harness: "claude-code",
            dir: home.join(".claude/plans"),
            suffix: ".md",
        }],
        _ => Vec::new(),
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SyncState {
    #[serde(default)]
    last_run: String,
    #[serde(default)]
    files: std::collections::BTreeMap<String, SeenFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SeenFile {
    hash: String,
    plan_id: String,
}

/// Outcome of one federation pass.
#[derive(Debug, Default)]
pub struct PlanSyncReport {
    pub ingested: Vec<String>,
    pub updated: Vec<String>,
    pub notes: Vec<String>,
}

impl PlanSyncReport {
    pub fn is_quiet(&self) -> bool {
        self.ingested.is_empty() && self.updated.is_empty() && self.notes.is_empty()
    }
}

fn state_path(project_dir: &Path) -> PathBuf {
    crate::local_store::root(project_dir).join(STATE_REL)
}

fn load_state(project_dir: &Path) -> SyncState {
    let Ok(text) = std::fs::read_to_string(state_path(project_dir)) else {
        return SyncState::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn save_state(project_dir: &Path, state: &SyncState) {
    let path = state_path(project_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(path, format!("{json}\n"));
    }
}

fn hash(text: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Plan title: frontmatter `name:`, else the first heading, else the file stem.
fn plan_title(file: &Path, text: &str) -> String {
    let mut in_frontmatter = false;
    let mut frontmatter_done = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "---" && !frontmatter_done {
            if in_frontmatter {
                frontmatter_done = true;
            } else {
                in_frontmatter = true;
            }
            continue;
        }
        if in_frontmatter {
            if let Some(name) = trimmed.strip_prefix("name:") {
                let name = name.trim();
                if !name.is_empty() {
                    return name.to_string();
                }
            }
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix("# ") {
            let heading = heading.trim();
            if !heading.is_empty() {
                return heading.to_string();
            }
        }
    }
    file.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.trim_end_matches(".plan").to_string())
        .unwrap_or_else(|| "harness plan".to_string())
}

/// One pass over a harness's native plan dirs into this project's store.
pub fn sync_from(home: &Path, project_dir: &Path, harness: &str) -> PlanSyncReport {
    let mut report = PlanSyncReport::default();
    let sources = plan_dirs(home, harness);
    let mut state = load_state(project_dir);
    for source in sources {
        if !source.dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&source.dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(source.suffix) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            let file_hash = hash(&text);
            let key = path.to_string_lossy().to_string();
            match state.files.get(&key) {
                None => {
                    let title = plan_title(&path, &text);
                    match crate::plans::record(
                        project_dir,
                        &title,
                        source.harness,
                        Some(&key),
                        &text,
                    ) {
                        Ok(meta) => {
                            report
                                .ingested
                                .push(format!("{} → {} (draft)", name, meta.id));
                            state.files.insert(
                                key,
                                SeenFile {
                                    hash: file_hash,
                                    plan_id: meta.id,
                                },
                            );
                        }
                        Err(err) => report.notes.push(format!("{name}: record failed: {err}")),
                    }
                }
                Some(seen) if seen.hash == file_hash => {}
                Some(seen) => {
                    // Native plan edited under us: refresh only while draft.
                    match crate::plans::update_draft_body(
                        project_dir,
                        &seen.plan_id,
                        &text,
                        "native plan edited — body refreshed",
                    ) {
                        Ok(_) => {
                            report.updated.push(format!("{} → {}", name, seen.plan_id));
                            state.files.insert(
                                key,
                                SeenFile {
                                    hash: file_hash,
                                    plan_id: seen.plan_id.clone(),
                                },
                            );
                        }
                        Err(err) => report
                            .notes
                            .push(format!("{name}: {err} (left alone — it is owned now)")),
                    }
                }
            }
        }
    }
    state.last_run = chrono::Utc::now().to_rfc3339();
    save_state(project_dir, &state);
    report
}

/// Hook path: at session boundaries, each harness pulls its own native plans
/// into the project it was working on — at most once per `interval_mins`.
/// Returns the report when a pass actually ran (None when the harness has no
/// known plan dir, or the interval has not passed).
pub fn maybe_auto(
    home: &Path,
    project_dir: &Path,
    harness: &str,
    interval_mins: i64,
) -> Option<PlanSyncReport> {
    if plan_dirs(home, harness).is_empty() {
        return None;
    }
    let state = load_state(project_dir);
    if !state.last_run.is_empty() {
        if let Ok(at) = chrono::DateTime::parse_from_rfc3339(&state.last_run) {
            let mins = (chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_minutes();
            if mins < interval_mins.max(1) {
                return None;
            }
        }
    }
    Some(sync_from(home, project_dir, harness))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("project");
        crate::local_store::init_skeleton(tmp.path(), "p", "n", "default").expect("init");
        tmp
    }

    fn write_plan(home: &Path, name: &str, body: &str) -> PathBuf {
        let dir = home.join(".cursor/plans");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write");
        path
    }

    const PLAN_A: &str =
        "---\nname: Conductor UI\noverview: test\n---\n\n# Conductor UI\n\nBody A.\n";

    #[test]
    fn native_plan_lands_as_draft_with_provenance() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        write_plan(home.path(), "conductor_a1b2c3d4.plan.md", PLAN_A);
        let report = sync_from(home.path(), project.path(), "cursor");
        assert_eq!(report.ingested.len(), 1, "{report:?}");
        let (meta, _) = crate::plans::current(project.path()).expect("a plan");
        assert_eq!(meta.title, "Conductor UI");
        assert_eq!(meta.status, "draft");
        assert_eq!(meta.created_by_harness, "cursor");
        assert!(meta.source_path.unwrap().contains(".cursor/plans"));
        // Second pass: no duplicate.
        let report = sync_from(home.path(), project.path(), "cursor");
        assert!(report.is_quiet(), "{report:?}");
    }

    #[test]
    fn native_edit_updates_draft_but_never_past_draft() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        let path = write_plan(home.path(), "conductor_a1b2c3d4.plan.md", PLAN_A);
        let _ = sync_from(home.path(), project.path(), "cursor");
        // The only plan in the store.
        let (meta, _) = crate::plans::current(project.path()).expect("plan");

        // Edit while draft → body refreshes.
        std::fs::write(&path, PLAN_A.replace("Body A.", "Body B.")).expect("edit");
        let report = sync_from(home.path(), project.path(), "cursor");
        assert_eq!(report.updated.len(), 1, "{report:?}");
        let (_, body_path) = crate::plans::load(project.path(), &meta.id).expect("load");
        let body = std::fs::read_to_string(body_path).expect("body");
        assert!(body.contains("Body B."));

        // Approve it → native edits stop syncing (owned).
        crate::plans::transition(project.path(), &meta.id, crate::plans::PlanStatus::Approved)
            .expect("approve");
        std::fs::write(&path, PLAN_A.replace("Body A.", "Body C.")).expect("edit2");
        let report = sync_from(home.path(), project.path(), "cursor");
        assert!(report.updated.is_empty(), "{report:?}");
        assert!(
            report.notes.iter().any(|n| n.contains("owned")),
            "{report:?}"
        );
        let (_, body_path) = crate::plans::load(project.path(), &meta.id).expect("load");
        let body = std::fs::read_to_string(body_path).expect("body");
        assert!(body.contains("Body B."), "owned plan untouched");
    }

    #[test]
    fn harnesses_without_plan_dirs_stay_silent() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        assert!(maybe_auto(home.path(), project.path(), "kimi-code", 15).is_none());
        // A harness with a known dir ingests; another project is untouched.
        write_plan(home.path(), "conductor_a1b2c3d4.plan.md", PLAN_A);
        let report = maybe_auto(home.path(), project.path(), "cursor", 15).expect("pass");
        assert_eq!(report.ingested.len(), 1);
        // Interval gate: an immediate second pass is silent.
        assert!(maybe_auto(home.path(), project.path(), "cursor", 15).is_none());
    }
}
