//! Local `.stateroot/` directory management.
//!
//! The on-disk layout mirrors the canonical schema in
//! `technical/stateroot_canonical_schema.md`:
//!
//! ```text
//! .stateroot/
//! ├── manifest.json            # stateroot.manifest.v1
//! ├── project/state.json       # stateroot.project_state.v1
//! ├── project/objectives.md
//! ├── handoffs/current.json    # stateroot.handoff.v1
//! ├── handoffs/history/<ts>-<harness>.json
//! ├── memories/episodic.jsonl  # one JSON record per line
//! ├── memories/MEMORY.md
//! ├── soul/SOUL.md
//! ├── instructions/AGENTS.md
//! ├── user/USER.md
//! └── outbox.jsonl             # CLI-local offline op queue
//! ```

use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;

/// Name of the per-project marker directory.
pub const STROOT_DIR_NAME: &str = ".stateroot";

/// Manifest path relative to `.stateroot/`.
pub const MANIFEST_PATH: &str = "manifest.json";
/// Project state path relative to `.stateroot/`.
pub const STATE_PATH: &str = "project/state.json";
/// Episodic JSONL path relative to `.stateroot/`.
pub const EPISODIC_PATH: &str = "memories/episodic.jsonl";
/// Current handoff path relative to `.stateroot/`.
pub const HANDOFF_CURRENT_PATH: &str = "handoffs/current.json";
/// Handoff history directory relative to `.stateroot/`.
pub const HANDOFF_HISTORY_DIR: &str = "handoffs/history";
/// Soul document path relative to `.stateroot/`.
pub const SOUL_PATH: &str = "soul/SOUL.md";
/// Shared instructions path relative to `.stateroot/`.
pub const INSTRUCTIONS_PATH: &str = "instructions/AGENTS.md";
/// Hot-apex user profile path relative to `.stateroot/`.
pub const USER_PROFILE_PATH: &str = "user/USER.md";
/// Hot-apex core memory path relative to `.stateroot/`.
pub const MEMORY_CORE_PATH: &str = "memories/MEMORY.md";
/// Offline outbox path relative to `.stateroot/`.
pub const OUTBOX_PATH: &str = "outbox.jsonl";

/// Schema version strings (canonical; must not drift).
pub const SCHEMA_MANIFEST_V1: &str = "stateroot.manifest.v1";
/// Project state schema version.
pub const SCHEMA_PROJECT_STATE_V1: &str = "stateroot.project_state.v1";
/// Handoff packet schema version.
pub const SCHEMA_HANDOFF_V1: &str = "stateroot.handoff.v1";

/// Errors from local `.stateroot` IO.
#[derive(Debug, Error)]
pub enum LocalStoreError {
    /// Filesystem failure.
    #[error("io error on {path}: {source}")]
    Io {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// JSON (de)serialization failure.
    #[error("json error on {path}: {source}")]
    Json {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying error.
        source: serde_json::Error,
    },
}

fn io_err(path: &Path) -> impl Fn(std::io::Error) -> LocalStoreError + '_ {
    move |source| LocalStoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn json_err(path: &Path) -> impl Fn(serde_json::Error) -> LocalStoreError + '_ {
    move |source| LocalStoreError::Json {
        path: path.to_path_buf(),
        source,
    }
}

/// The `.stateroot` root inside a project directory.
pub fn root(project_dir: &Path) -> PathBuf {
    project_dir.join(STROOT_DIR_NAME)
}

/// True when `project_dir` is a stateroot project (marker: `.stateroot/manifest.json`).
pub fn is_stateroot_dir(project_dir: &Path) -> bool {
    root(project_dir).join(MANIFEST_PATH).is_file()
}

/// Read and parse the manifest, or `None` when absent.
pub fn read_manifest(project_dir: &Path) -> Result<Option<Value>, LocalStoreError> {
    let path = root(project_dir).join(MANIFEST_PATH);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let value = serde_json::from_str(&text).map_err(json_err(&path))?;
            Ok(Some(value))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io_err(&path)(err)),
    }
}

fn write_text_if_absent(
    path: &Path,
    content: &str,
    created: &mut Vec<String>,
) -> Result<(), LocalStoreError> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    std::fs::write(path, content).map_err(io_err(path))?;
    created.push(path.to_string_lossy().to_string());
    Ok(())
}

fn write_json_if_absent(
    path: &Path,
    value: &Value,
    created: &mut Vec<String>,
) -> Result<(), LocalStoreError> {
    let text = serde_json::to_string_pretty(value).map_err(json_err(path))?;
    write_text_if_absent(path, &format!("{text}\n"), created)
}

/// Create the `.stateroot/` skeleton in `project_dir`.
///
/// Idempotent: existing files are left untouched. Returns the list of files
/// that were created.
pub fn init_skeleton(
    project_dir: &Path,
    project_id: &str,
    name: &str,
    owner_user_id: &str,
) -> Result<Vec<String>, LocalStoreError> {
    let root = root(project_dir);
    let now = now_rfc3339();
    let mut created = Vec::new();

    let manifest = serde_json::json!({
        "schema_version": SCHEMA_MANIFEST_V1,
        "project_id": project_id,
        "name": name,
        "created_at": now,
        "owner_user_id": owner_user_id,
        "tier": "free",
        "harness_registry": [],
        "stateroot_layout_version": 1,
    });
    write_json_if_absent(&root.join(MANIFEST_PATH), &manifest, &mut created)?;

    let state = serde_json::json!({
        "schema_version": SCHEMA_PROJECT_STATE_V1,
        "project_id": project_id,
        "objective": "",
        "current_phase": "init",
        "status": "active",
        "last_harness": null,
        "recommended_next_harness": null,
        "task_tree_ref": "project/task-tree.json",
        "updated_at": now,
        "updated_by_harness": "skillsagent",
    });
    write_json_if_absent(&root.join(STATE_PATH), &state, &mut created)?;

    write_text_if_absent(
        &root.join("project/objectives.md"),
        "# Objectives\n\nDescribe the project goal and success criteria.\n",
        &mut created,
    )?;
    write_text_if_absent(
        &root.join(SOUL_PATH),
        "# Soul\n\nProject tone, values, and non-negotiable constraints.\n",
        &mut created,
    )?;
    write_text_if_absent(
        &root.join(INSTRUCTIONS_PATH),
        "# Agent Instructions\n\nShared instructions for all harnesses attached to this StateRoot project.\nHarness-specific guidance lives in `instructions/{harness}.md`.\n",
        &mut created,
    )?;
    write_text_if_absent(
        &root.join(USER_PROFILE_PATH),
        "# User Profile\n\nStable facts about the user that help agents collaborate.\n",
        &mut created,
    )?;
    write_text_if_absent(
        &root.join(MEMORY_CORE_PATH),
        "# Project Memory\n\nCurated long-term memory for this project.\n",
        &mut created,
    )?;
    write_text_if_absent(&root.join(EPISODIC_PATH), "", &mut created)?;

    // History directory starts empty but must exist for later writes.
    let history_dir = root.join(HANDOFF_HISTORY_DIR);
    if !history_dir.exists() {
        std::fs::create_dir_all(&history_dir).map_err(io_err(&history_dir))?;
        created.push(history_dir.to_string_lossy().to_string());
    }

    Ok(created)
}

/// Append one JSON record to `memories/episodic.jsonl`.
pub fn append_episodic(project_dir: &Path, record: &Value) -> Result<(), LocalStoreError> {
    let path = root(project_dir).join(EPISODIC_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    let mut line = serde_json::to_string(record).map_err(json_err(&path))?;
    line.push('\n');
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(io_err(&path))?;
    file.write_all(line.as_bytes()).map_err(io_err(&path))?;
    Ok(())
}

/// Persist a handoff packet locally: `handoffs/current.json` plus an immutable
/// copy in `handoffs/history/<ts>-<harness>.json`.
pub fn write_handoff_local(project_dir: &Path, packet: &Value) -> Result<(), LocalStoreError> {
    let root = root(project_dir);
    let current = root.join(HANDOFF_CURRENT_PATH);
    if let Some(parent) = current.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    let text = serde_json::to_string_pretty(packet).map_err(json_err(&current))?;
    std::fs::write(&current, format!("{text}\n")).map_err(io_err(&current))?;

    let ts = packet
        .get("created_at")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .replace([':', '.'], "-");
    let harness = packet
        .get("created_by_harness")
        .and_then(|v| v.as_str())
        .unwrap_or("cli");
    let history_dir = root.join(HANDOFF_HISTORY_DIR);
    std::fs::create_dir_all(&history_dir).map_err(io_err(&history_dir))?;
    let history = history_dir.join(format!("{ts}-{harness}.json"));
    std::fs::write(&history, format!("{text}\n")).map_err(io_err(&history))?;
    Ok(())
}

/// Read `handoffs/current.json`, or `None` when absent.
pub fn read_handoff_local(project_dir: &Path) -> Result<Option<Value>, LocalStoreError> {
    let path = root(project_dir).join(HANDOFF_CURRENT_PATH);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let value = serde_json::from_str(&text).map_err(json_err(&path))?;
            Ok(Some(value))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io_err(&path)(err)),
    }
}

/// Rewrite `handoffs/current.json` in place via a mutation closure — used for
/// in-place updates (acceptance marks) that must NOT create a history entry.
/// Returns true when the closure changed the document.
pub fn update_handoff_current(
    project_dir: &Path,
    mutate: impl FnOnce(&mut Value) -> bool,
) -> Result<bool, LocalStoreError> {
    let Some(mut packet) = read_handoff_local(project_dir)? else {
        return Ok(false);
    };
    if !mutate(&mut packet) {
        return Ok(false);
    }
    let path = root(project_dir).join(HANDOFF_CURRENT_PATH);
    let text = serde_json::to_string_pretty(&packet).map_err(json_err(&path))?;
    std::fs::write(&path, format!("{text}\n")).map_err(io_err(&path))?;
    Ok(true)
}

/// List local handoff history packets, oldest first.
pub fn list_handoffs_local(project_dir: &Path) -> Result<Vec<Value>, LocalStoreError> {
    let dir = root(project_dir).join(HANDOFF_HISTORY_DIR);
    let mut packets = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(packets),
        Err(err) => return Err(io_err(&dir)(err)),
    };
    let mut names: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io_err(&dir))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            names.push(path);
        }
    }
    names.sort();
    for path in names {
        let text = std::fs::read_to_string(&path).map_err(io_err(&path))?;
        let value = serde_json::from_str(&text).map_err(json_err(&path))?;
        packets.push(value);
    }
    Ok(packets)
}

/// Append an offline operation to `.stateroot/outbox.jsonl`.
pub fn outbox_append(project_dir: &Path, op: &Value) -> Result<(), LocalStoreError> {
    let path = root(project_dir).join(OUTBOX_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    let mut line = serde_json::to_string(op).map_err(json_err(&path))?;
    line.push('\n');
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(io_err(&path))?;
    file.write_all(line.as_bytes()).map_err(io_err(&path))?;
    Ok(())
}

/// Read all pending outbox operations without removing them.
pub fn outbox_pending(project_dir: &Path) -> Result<Vec<Value>, LocalStoreError> {
    let path = root(project_dir).join(OUTBOX_PATH);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let mut ops = Vec::new();
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                ops.push(serde_json::from_str(line).map_err(json_err(&path))?);
            }
            Ok(ops)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(io_err(&path)(err)),
    }
}

/// Read all pending outbox operations and clear the queue.
pub fn outbox_drain(project_dir: &Path) -> Result<Vec<Value>, LocalStoreError> {
    let ops = outbox_pending(project_dir)?;
    let path = root(project_dir).join(OUTBOX_PATH);
    if path.exists() {
        std::fs::remove_file(&path).map_err(io_err(&path))?;
    }
    Ok(ops)
}

/// Directory holding canonical skill copies inside `.stateroot/`.
pub const SKILLS_DIR: &str = "skills";

/// A locally available skill discovered under `.stateroot/skills/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSkill {
    /// Directory slug.
    pub slug: String,
    /// `name:` from the SKILL.md frontmatter (falls back to slug).
    pub name: String,
    /// `description:` from the SKILL.md frontmatter (may be empty).
    pub description: String,
}

fn skip_leading_html_comments(text: &str) -> &str {
    let mut rest = text.trim_start();
    while rest.starts_with("<!--") {
        match rest.find("-->") {
            Some(end) => rest = rest[end + 3..].trim_start(),
            None => break,
        }
    }
    rest
}

/// Parse the YAML-ish frontmatter of a SKILL.md (`---` fenced `key: value`).
/// Tolerant by design: unknown lines are ignored, multiline values are not
/// supported (fine for name/description). Leading HTML provenance comments
/// (W7 `stateroot:skill`) are skipped so projected skills still parse.
fn parse_frontmatter(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut lines = skip_leading_html_comments(text).lines();
    if lines.next().map(str::trim) != Some("---") {
        return pairs;
    }
    for line in lines {
        let line = line.trim_end();
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_string();
            let value = value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            if !key.is_empty() {
                pairs.push((key, value));
            }
        }
    }
    pairs
}

/// Scan `.stateroot/skills/*/SKILL.md` for available skills.
pub fn list_local_skills(project_dir: &Path) -> Vec<LocalSkill> {
    let skills_root = root(project_dir).join(SKILLS_DIR);
    let mut skills = Vec::new();
    let entries = match std::fs::read_dir(&skills_root) {
        Ok(entries) => entries,
        Err(_) => return skills,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let slug = entry.file_name().to_string_lossy().to_string();
        let skill_md = path.join("SKILL.md");
        let Ok(text) = std::fs::read_to_string(&skill_md) else {
            continue;
        };
        let pairs = parse_frontmatter(&text);
        let get = |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let name = {
            let name = get("name");
            if name.is_empty() {
                slug.clone()
            } else {
                name
            }
        };
        skills.push(LocalSkill {
            slug,
            name,
            description: get("description"),
        });
    }
    skills.sort_by(|a, b| a.slug.cmp(&b.slug));
    skills
}

/// Read one local skill's SKILL.md by slug.
pub fn read_local_skill(project_dir: &Path, slug: &str) -> Option<String> {
    let path = root(project_dir)
        .join(SKILLS_DIR)
        .join(slug)
        .join("SKILL.md");
    std::fs::read_to_string(path).ok()
}

/// RFC 3339 UTC timestamp (seconds precision) — lifted verbatim from the
/// monorepo's `auth::token` helper to keep local_store free of the auth module.
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_skills_scan_and_read() {
        let tmp = tempfile::tempdir().expect("tmp");
        let skill_dir = root(tmp.path()).join(SKILLS_DIR).join("demo");
        std::fs::create_dir_all(&skill_dir).expect("mkdir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "<!-- stateroot:skill origin=learning_review harness=skillsagent version=1 -->\n---\nname: demo-skill\ndescription: Does demo things\n---\n\n# Demo\n",
        )
        .expect("write");
        // A directory without SKILL.md is skipped.
        std::fs::create_dir_all(root(tmp.path()).join(SKILLS_DIR).join("broken")).expect("mkdir2");

        let skills = list_local_skills(tmp.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].slug, "demo");
        assert_eq!(skills[0].name, "demo-skill");
        assert_eq!(skills[0].description, "Does demo things");
        assert!(read_local_skill(tmp.path(), "demo")
            .expect("read")
            .contains("# Demo"));
        assert!(read_local_skill(tmp.path(), "missing").is_none());
    }

    #[test]
    fn skeleton_matches_canonical_layout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let created = init_skeleton(tmp.path(), "proj-1", "demo", "default").expect("init");
        assert!(!created.is_empty());
        assert!(is_stateroot_dir(tmp.path()));

        let root = root(tmp.path());
        for rel in [
            MANIFEST_PATH,
            STATE_PATH,
            "project/objectives.md",
            SOUL_PATH,
            INSTRUCTIONS_PATH,
            USER_PROFILE_PATH,
            MEMORY_CORE_PATH,
            EPISODIC_PATH,
        ] {
            assert!(root.join(rel).is_file(), "missing {rel}");
        }
        assert!(root.join(HANDOFF_HISTORY_DIR).is_dir());

        let manifest = read_manifest(tmp.path()).expect("read").expect("manifest");
        assert_eq!(manifest["schema_version"], SCHEMA_MANIFEST_V1);
        assert_eq!(manifest["project_id"], "proj-1");
        assert_eq!(manifest["name"], "demo");
        assert_eq!(manifest["stateroot_layout_version"], 1);

        let state: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join(STATE_PATH)).expect("state file"),
        )
        .expect("state json");
        assert_eq!(state["schema_version"], SCHEMA_PROJECT_STATE_V1);
        assert_eq!(state["status"], "active");

        // Idempotent: second run creates nothing and keeps content.
        let created_again = init_skeleton(tmp.path(), "proj-1", "demo", "default").expect("init2");
        assert!(created_again.is_empty());
    }

    #[test]
    fn episodic_append_writes_jsonl() {
        let tmp = tempfile::tempdir().expect("tempdir");
        init_skeleton(tmp.path(), "p", "n", "default").expect("init");
        append_episodic(
            tmp.path(),
            &serde_json::json!({"ts": "t1", "note": "first"}),
        )
        .expect("append 1");
        append_episodic(
            tmp.path(),
            &serde_json::json!({"ts": "t2", "note": "second"}),
        )
        .expect("append 2");
        let text = std::fs::read_to_string(root(tmp.path()).join(EPISODIC_PATH)).expect("read");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_str(lines[0]).expect("json");
        assert_eq!(first["note"], "first");
    }

    #[test]
    fn handoff_write_read_and_history() {
        let tmp = tempfile::tempdir().expect("tempdir");
        init_skeleton(tmp.path(), "p", "n", "default").expect("init");
        let packet = serde_json::json!({
            "schema_version": SCHEMA_HANDOFF_V1,
            "project_id": "p",
            "seq": 1,
            "created_at": "2026-07-18T12:00:00Z",
            "created_by_harness": "skillsagent",
        });
        write_handoff_local(tmp.path(), &packet).expect("write");
        let read = read_handoff_local(tmp.path())
            .expect("read")
            .expect("packet");
        assert_eq!(read["seq"], 1);
        let history = list_handoffs_local(tmp.path()).expect("history");
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn outbox_append_pending_drain() {
        let tmp = tempfile::tempdir().expect("tempdir");
        init_skeleton(tmp.path(), "p", "n", "default").expect("init");
        assert!(outbox_pending(tmp.path()).expect("pending").is_empty());
        outbox_append(
            tmp.path(),
            &serde_json::json!({"kind": "checkpoint", "note": "n1"}),
        )
        .expect("append");
        outbox_append(tmp.path(), &serde_json::json!({"kind": "handoff"})).expect("append");
        let pending = outbox_pending(tmp.path()).expect("pending");
        assert_eq!(pending.len(), 2);
        let drained = outbox_drain(tmp.path()).expect("drain");
        assert_eq!(drained.len(), 2);
        assert!(outbox_pending(tmp.path())
            .expect("pending after")
            .is_empty());
    }
}
