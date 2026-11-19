//! Plan federation — harness-native plans (Cursor plan mode's
//! `~/.cursor/plans/*.plan.md`, Claude's plan directories, Kimi's session
//! plans, and CodeBuddy's session-linked `~/.codebuddy/plans/`) land in the
//! central plan store as drafts. Without this, a plan authored in a harness's
//! native plan mode never reaches the shared store, and the next harness
//! cannot see it (the cursor-plan continuity gap).
//!
//! Attribution is explicit per source, never guessed from the harness name:
//! a *structural* source proves its project by its own path (a project-local
//! directory, or a store keyed by session metadata); a *session-window*
//! source is a user-global pile, so a file belongs to this project only when
//! one of this project's transcripts was active near the file's mtime.
//!
//! Claude resolves up to three sources — its documented `plansDirectory`
//! setting (project-local, then shared project, then user settings), the
//! default `<config root>/plans`, and `<project>/.claude/plans` — through
//! `paths::claude_root`, so a relocated `CLAUDE_CONFIG_DIR` is honored.
//!
//! Dedup is by content hash; a native plan edited while still a draft updates
//! the draft in place; once a plan moves past draft it is owned and never
//! overwritten.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest as _;

/// Per-project sync cursor (local lane — never in snapshots or git).
const STATE_REL: &str = "local/plan-federation.json";

/// How a source directory's plan files are attributed to this project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attribution {
    /// The path itself proves the project: a project-local directory, or a
    /// store whose session metadata names the workspace.
    Structural,
    /// A user-global pile: a plan belongs to this project only when one of
    /// this project's own transcripts was active near the file's mtime.
    SessionWindow,
}

/// One harness-native plan source directory.
struct PlanSource {
    harness: &'static str,
    dir: PathBuf,
    /// File suffix that marks a plan (`*.plan.md` for cursor, `*.md` claude).
    suffix: &'static str,
    attribution: Attribution,
}

/// Known native plan dirs for a harness id (already normalized).
fn plan_dirs(home: &Path, project_dir: &Path, harness: &str) -> Vec<PlanSource> {
    match harness {
        "cursor" => vec![PlanSource {
            harness: "cursor",
            dir: home.join(".cursor/plans"),
            suffix: ".plan.md",
            attribution: Attribution::SessionWindow,
        }],
        "codebuddy" => vec![PlanSource {
            harness: "codebuddy",
            dir: crate::harness_install::paths::codebuddy_root(home).join("plans"),
            suffix: ".md",
            attribution: Attribution::Structural,
        }],
        "claude" | "claude-code" => claude_plan_sources(home, project_dir),
        // `~/.commandcode/plans/` is a global pile with no project binding —
        // attribution is by this project's own session activity (below).
        "commandcode" => vec![PlanSource {
            harness: "commandcode",
            dir: crate::harness_install::paths::commandcode_root(home).join("plans"),
            suffix: ".md",
            attribution: Attribution::SessionWindow,
        }],
        _ => Vec::new(),
    }
}

/// Claude's plan sources, deduped: the resolved `plansDirectory`, the default
/// config-root `plans` dir, and the project-local `.claude/plans`.
///
/// A directory inside this project is structural; anything under the Claude
/// config root (or a configured absolute path elsewhere) is a user-global
/// pile and needs session-window attribution.
fn claude_plan_sources(home: &Path, project_dir: &Path) -> Vec<PlanSource> {
    let root = crate::harness_install::paths::claude_root(home);
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = claude_plans_directory(home, project_dir) {
        candidates.push(dir);
    }
    candidates.push(root.join("plans"));
    candidates.push(project_dir.join(".claude/plans"));

    let project_key = crate::path_identity::normalize_host_path(&project_dir.to_string_lossy());
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for dir in candidates {
        let key = crate::path_identity::normalize_host_path(&dir.to_string_lossy());
        if !seen.insert(key.clone()) {
            continue;
        }
        let attribution = if key == project_key || key.starts_with(&format!("{project_key}/")) {
            Attribution::Structural
        } else {
            Attribution::SessionWindow
        };
        out.push(PlanSource {
            harness: "claude-code",
            dir,
            suffix: ".md",
            attribution,
        });
    }
    out
}

/// Claude's documented `plansDirectory` setting, highest scope first:
/// project-local settings, shared project settings, then user settings.
fn claude_plans_directory(home: &Path, project_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        project_dir.join(".claude/settings.local.json"),
        project_dir.join(".claude/settings.json"),
        crate::harness_install::paths::claude_root(home).join("settings.json"),
    ];
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(doc) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(raw) = doc.get("plansDirectory").and_then(Value::as_str) else {
            continue;
        };
        if let Some(dir) = resolve_plans_directory(project_dir, raw) {
            return Some(dir);
        }
    }
    None
}

/// Resolve a `plansDirectory` value: absolute as written, relative against the
/// project root, and a foreign-OS path fails closed to the default instead of
/// joining a meaningless directory.
fn resolve_plans_directory(project_dir: &Path, raw: &str) -> Option<PathBuf> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    let windows_shaped = value.len() > 1
        && value.as_bytes()[1] == b':'
        && matches!(value.as_bytes().get(2), Some(b'\\') | Some(b'/'));
    if windows_shaped {
        return None;
    }
    Some(project_dir.join(path))
}

/// Kimi stores its plan history below each session rather than in a shared
/// plan directory. A session's `state.json` identifies its workspace; the
/// newest native plan for that project is the only eligible artifact.
///
/// Unlike Cursor and Claude's dedicated plan directories, scanning every
/// Kimi session plan would resurrect historical drafts whenever federation
/// first runs. The newest file is the plan Kimi just authored at the session
/// boundary; subsequent edits refresh that same artifact through `SyncState`.
fn kimi_plan_paths(home: &Path, project_dir: &Path) -> Vec<PathBuf> {
    let sessions = home.join(".kimi-code/sessions");
    let Ok(workspace_dirs) = std::fs::read_dir(sessions) else {
        return Vec::new();
    };
    let project_key = crate::path_identity::equivalent_project_key(project_dir);
    let mut paths = Vec::new();
    for workspace in workspace_dirs.flatten() {
        let Ok(session_dirs) = std::fs::read_dir(workspace.path()) else {
            continue;
        };
        for session in session_dirs.flatten() {
            let state_path = session.path().join("state.json");
            let Ok(state) = std::fs::read_to_string(state_path) else {
                continue;
            };
            let Ok(state) = serde_json::from_str::<serde_json::Value>(&state) else {
                continue;
            };
            let Some(cwd) = state.get("cwd").and_then(|value| value.as_str()) else {
                continue;
            };
            if crate::path_identity::equivalent_project_key(Path::new(cwd)) != project_key {
                continue;
            }
            let plans = session.path().join("agents/main/plans");
            let Ok(entries) = std::fs::read_dir(plans) else {
                continue;
            };
            paths.extend(entries.flatten().map(|entry| entry.path()).filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".md"))
            }));
        }
    }
    paths.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
    });
    paths.into_iter().rev().take(1).collect()
}

/// DSH plan mode does not write plan files: `exit_plan_mode` carries the
/// complete markdown plan in its tool arguments, and the session log is the
/// durable artifact. Read those calls back for sessions bound to this project.
fn dsh_plan_candidates(home: &Path, project_dir: &Path) -> Vec<(String, String)> {
    let (plain, zstd) = crate::transcripts::dsh::session_files(home);
    let mut out = Vec::new();
    for file in plain.iter().chain(zstd.iter()) {
        let Some(raw) = crate::transcripts::dsh::parse_session_file(file) else {
            continue;
        };
        let session_id = raw
            .header
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("session");
        let cwd = raw
            .header
            .get("cwd")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if !crate::transcripts::cwd_matches(cwd, project_dir) {
            continue;
        }
        for event in &raw.events {
            if event.get("type").and_then(|value| value.as_str()) != Some("tool/call") {
                continue;
            }
            let Some(data) = event.get("data") else {
                continue;
            };
            if data.get("name").and_then(|value| value.as_str()) != Some("exit_plan_mode") {
                continue;
            }
            let Some(arguments) = data.get("arguments").and_then(|value| value.as_str()) else {
                continue;
            };
            let Ok(arguments) = serde_json::from_str::<Value>(arguments) else {
                continue;
            };
            let Some(plan) = arguments.get("plan").and_then(|value| value.as_str()) else {
                continue;
            };
            if plan.trim().is_empty() {
                continue;
            }
            let call = data
                .get("callId")
                .and_then(|value| value.as_str())
                .unwrap_or("plan");
            out.push((format!("dsh:{session_id}#{call}"), plan.to_string()));
        }
    }
    out
}

fn codebuddy_plan_path(value: &Value) -> Option<PathBuf> {
    value
        .get("planFilePath")
        .or_else(|| value.pointer("/meta/planFilePath"))
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// Native plans are eligible only when a session belonging to this project
/// explicitly names the file in its `planFilePath`. The global plan directory
/// alone is never sufficient attribution.
fn codebuddy_plan_paths(home: &Path, project_dir: &Path) -> std::collections::BTreeSet<PathBuf> {
    let plans_root = crate::harness_install::paths::codebuddy_root(home).join("plans");
    let canonical_root = std::fs::canonicalize(&plans_root).unwrap_or(plans_root);
    let mut plans = std::collections::BTreeSet::new();
    for transcript in crate::transcripts::codebuddy::session_files(home) {
        if crate::transcripts::codebuddy::session_identity(&transcript, project_dir).is_none() {
            continue;
        }
        let mut candidates = Vec::new();
        if let Ok(text) = std::fs::read_to_string(transcript.with_extension("meta.json")) {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                if let Some(path) = codebuddy_plan_path(&value) {
                    candidates.push(path);
                }
            }
        }
        if let Some(events) = crate::transcripts::codebuddy::parse_session_file(&transcript) {
            candidates.extend(events.iter().filter_map(codebuddy_plan_path));
        }
        for candidate in candidates {
            let candidate = if candidate.is_relative() {
                canonical_root.join(candidate)
            } else {
                candidate
            };
            if !candidate.is_file() {
                continue;
            }
            if let Ok(canonical) = std::fs::canonicalize(candidate) {
                if canonical.starts_with(&canonical_root) {
                    plans.insert(canonical);
                }
            }
        }
    }
    plans
}

/// How far back a cursor session may have touched a plan for it to count as
/// that session's work (plans older than this with no session are skipped).
const CURSOR_ATTRIBUTION_WINDOW: std::time::Duration =
    std::time::Duration::from_secs(7 * 24 * 3600);

/// Cursor's `~/.cursor/plans/` is a *global* pile with no project binding —
/// ingesting by file presence alone pours every project's plans into one
/// store (the historical-junk incident). A plan file is attributable to this
/// project only when one of this project's transcripts shows activity within
/// the attribution window of the plan's mtime. Returns this project's
/// transcript mtimes, attributed by the absolute paths in the transcripts'
/// own tool inputs.
fn cursor_project_transcript_times(home: &Path, project_dir: &Path) -> Vec<std::time::SystemTime> {
    let projects = home.join(".cursor/projects");
    let Ok(workspaces) = std::fs::read_dir(&projects) else {
        return Vec::new();
    };
    let project_key = crate::path_identity::equivalent_project_key(project_dir);
    let mut times = Vec::new();
    for workspace in workspaces.flatten() {
        let Ok(sessions) = std::fs::read_dir(workspace.path().join("agent-transcripts")) else {
            continue;
        };
        for session in sessions.flatten() {
            let Ok(files) = std::fs::read_dir(session.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Ok(head) = read_head(&path, 8192) else {
                    continue;
                };
                let belongs = transcript_project(&head)
                    .map(|root| crate::path_identity::equivalent_project_key(&root) == project_key)
                    .unwrap_or(false);
                if belongs {
                    if let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                        times.push(mtime);
                    }
                }
            }
        }
    }
    times
}

/// Command Code's `~/.commandcode/plans/` is the same kind of global pile as
/// Cursor's. Its transcripts ARE project-scoped (`projects/<slug>/`), and the
/// header carries the real cwd, so attribution reads only this project's
/// session mtimes — no content guessing.
fn commandcode_project_transcript_times(
    home: &Path,
    project_dir: &Path,
) -> Vec<std::time::SystemTime> {
    crate::transcripts::commandcode::session_files(home)
        .into_iter()
        .filter(|file| {
            crate::transcripts::commandcode::session_identity(file, project_dir).is_some()
        })
        .filter_map(|file| {
            std::fs::metadata(&file)
                .and_then(|meta| meta.modified())
                .ok()
        })
        .collect()
}

/// Claude's home plan directory (`<config root>/plans`) is the same kind of
/// user-global pile as Cursor's. Claude transcripts are project-scoped by
/// slug and every record carries `cwd`, so a bounded head read attributes a
/// session to this project without parsing whole files.
fn claude_project_transcript_times(home: &Path, project_dir: &Path) -> Vec<std::time::SystemTime> {
    crate::transcripts::claude::session_files(home)
        .into_iter()
        .filter(|file| {
            read_head(file, 8192)
                .ok()
                .and_then(|head| claude_transcript_cwd(&head))
                .is_some_and(|cwd| crate::transcripts::cwd_matches(&cwd, project_dir))
        })
        .filter_map(|file| std::fs::metadata(&file).and_then(|m| m.modified()).ok())
        .collect()
}

/// First `cwd` in a Claude session head (every record type carries it).
fn claude_transcript_cwd(head: &str) -> Option<String> {
    for line in head.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(cwd) = value.get("cwd").and_then(Value::as_str) {
            if !cwd.trim().is_empty() {
                return Some(cwd.to_string());
            }
        }
    }
    None
}

/// The first absolute path in the transcript head that resolves (walking up)
/// to a stateroot project root. Tool inputs carry the workspace explicitly.
fn transcript_project(head: &str) -> Option<PathBuf> {
    for token in head.split(|c: char| c == '"' || c == '\'' || c.is_whitespace()) {
        let t = token.trim_matches(|c| ",{}[]()".contains(c));
        let looks_absolute = t.starts_with('/')
            || (t.len() > 2
                && t.as_bytes()[1] == b':'
                && (t.as_bytes()[2] == b'\\' || t.as_bytes()[2] == b'/'));
        if !looks_absolute {
            continue;
        }
        if let Some(root) = crate::local_store::find_project_root(Path::new(t)) {
            return Some(root);
        }
    }
    None
}

fn read_head(path: &Path, bytes: usize) -> std::io::Result<String> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; bytes];
    let n = file.read(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf[..n]).to_string())
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
    /// Plan ids whose all-completed todos opened a plan-closure obligation.
    /// Plans are never auto-completed — completion requires explicit evidence
    /// (`stateroot plan done <id> --evidence`).
    pub closure_obligations: Vec<String>,
}

impl PlanSyncReport {
    pub fn is_quiet(&self) -> bool {
        self.ingested.is_empty()
            && self.updated.is_empty()
            && self.notes.is_empty()
            && self.closure_obligations.is_empty()
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

/// Plan title: frontmatter `name:`, else the first heading, else `fallback`.
fn title_from_text(text: &str, fallback: &str) -> String {
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
    fallback.to_string()
}

/// Plan title for a native plan file.
fn plan_title(file: &Path, text: &str) -> String {
    let fallback = file
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.trim_end_matches(".plan").to_string())
        .unwrap_or_else(|| "harness plan".to_string());
    title_from_text(text, &fallback)
}

/// One pass over a harness's native plan dirs into this project's store.
pub fn sync_from(home: &Path, project_dir: &Path, harness: &str) -> PlanSyncReport {
    let mut report = PlanSyncReport::default();
    let sources = plan_dirs(home, project_dir, harness);
    let mut state = load_state(project_dir);
    for source in sources {
        if !source.dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&source.dir) else {
            continue;
        };
        let session_times = if source.attribution == Attribution::SessionWindow {
            match source.harness {
                "cursor" => cursor_project_transcript_times(home, project_dir),
                "commandcode" => commandcode_project_transcript_times(home, project_dir),
                "claude-code" => claude_project_transcript_times(home, project_dir),
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let codebuddy_plans = if source.harness == "codebuddy" {
            codebuddy_plan_paths(home, project_dir)
        } else {
            std::collections::BTreeSet::new()
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(source.suffix) {
                continue;
            }
            if source.harness == "codebuddy" {
                let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                if !codebuddy_plans.contains(&canonical) {
                    report.notes.push(format!(
                        "{name}: skipped — no native session planFilePath for this project"
                    ));
                    continue;
                }
            }
            if source.attribution == Attribution::SessionWindow {
                // Attribute by session activity: a plan is this project's
                // only if one of this project's transcripts was active
                // within the window of the plan's mtime.
                let attributable = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|plan_mtime| plan_mtime.checked_sub(CURSOR_ATTRIBUTION_WINDOW))
                    .map(|window_start| session_times.iter().any(|t| *t >= window_start))
                    .unwrap_or(false);
                if !attributable {
                    report.notes.push(format!(
                        "{name}: skipped — no {} session activity for this project near it",
                        source.harness
                    ));
                    continue;
                }
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
                                key.clone(),
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
                                key.clone(),
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
            if source.harness == "cursor" {
                if let Some(seen) = state.files.get(&key) {
                    apply_cursor_plan_todos(project_dir, &path, &text, &seen.plan_id, &mut report);
                }
            }
        }
    }
    if matches!(harness, "kimi" | "kimi-code") {
        for path in kimi_plan_paths(home, project_dir) {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("plan");
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
                    match crate::plans::record(project_dir, &title, "kimi-code", Some(&key), &text)
                    {
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
                Some(seen) => match crate::plans::update_draft_body(
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
                },
            }
        }
    }
    if harness == "dsh" {
        for (key, text) in dsh_plan_candidates(home, project_dir) {
            let file_hash = hash(&text);
            let title = title_from_text(&text, "dsh plan");
            match state.files.get(&key) {
                None => match crate::plans::record(project_dir, &title, "dsh", Some(&key), &text) {
                    Ok(meta) => {
                        report
                            .ingested
                            .push(format!("{title} → {} (draft)", meta.id));
                        state.files.insert(
                            key,
                            SeenFile {
                                hash: file_hash,
                                plan_id: meta.id,
                            },
                        );
                    }
                    Err(err) => report.notes.push(format!("{title}: record failed: {err}")),
                },
                Some(seen) if seen.hash == file_hash => {}
                Some(seen) => match crate::plans::update_draft_body(
                    project_dir,
                    &seen.plan_id,
                    &text,
                    "native plan edited — body refreshed",
                ) {
                    Ok(_) => {
                        report.updated.push(format!("{title} → {}", seen.plan_id));
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
                        .push(format!("{title}: {err} (left alone — it is owned now)")),
                },
            }
        }
    }
    state.last_run = chrono::Utc::now().to_rfc3339();
    save_state(project_dir, &state);
    let todos = crate::todo_federation::sync_from(home, project_dir, harness);
    for line in todos.written {
        report.notes.push(format!("todos {line}"));
    }
    for line in todos.notes {
        report.notes.push(format!("todos {line}"));
    }
    report
}

fn apply_cursor_plan_todos(
    project_dir: &Path,
    path: &Path,
    text: &str,
    plan_id: &str,
    report: &mut PlanSyncReport,
) {
    let items = crate::todo_federation::parse_frontmatter_todos(text);
    if items.is_empty() {
        return;
    }
    let session_key = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(plan_id);
    let complete = crate::todo_federation::all_completed(&items);
    if let Err(err) =
        crate::todo_federation::upsert_plan_bound(project_dir, session_key, plan_id, items, path)
    {
        report.notes.push(err);
        return;
    }
    if !complete {
        return;
    }
    // Never auto-complete a plan: structurally completed todos open a
    // plan-closure obligation; completion requires explicit evidence via
    // `stateroot plan done <id> --evidence "…"`.
    match crate::plans::load(project_dir, plan_id) {
        Some((meta, _)) => {
            match crate::obligations::ensure_plan_closure(
                project_dir,
                plan_id,
                &meta.title,
                "plan-federation",
            ) {
                Ok((_, true)) => report.closure_obligations.push(plan_id.to_string()),
                Ok((_, false)) => {}
                Err(err) => report.notes.push(err),
            }
        }
        None => report
            .notes
            .push(format!("plan {plan_id} not found for closure obligation")),
    }
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
    if plan_dirs(home, project_dir, harness).is_empty()
        && !matches!(harness, "kimi" | "kimi-code" | "dsh")
        && !crate::todo_federation::harness_has_sources(harness)
    {
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

    /// Write a plan into the Claude config-root plans dir (the global pile).
    fn write_claude_plan(home: &Path, name: &str, body: &str) -> PathBuf {
        let dir = crate::harness_install::paths::claude_root(home).join("plans");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write");
        path
    }

    /// Seed a Claude session file whose records carry this project's `cwd` —
    /// the session-activity evidence the attribution gate requires.
    fn seed_claude_transcript(home: &Path, project_dir: &Path) {
        let dir = crate::harness_install::paths::claude_projects_root(home).join("test-slug");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let record = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "plan this"},
            "cwd": project_dir.to_string_lossy(),
            "sessionId": "s1",
            "timestamp": "2026-11-18T00:00:00Z",
        });
        std::fs::write(dir.join("s1.jsonl"), format!("{record}\n")).expect("transcript");
    }

    /// Seed a cursor transcript whose tool inputs name `project_dir` — the
    /// session-activity evidence the attribution check requires.
    fn seed_cursor_transcript(home: &Path, project_dir: &Path) {
        let session = home.join(".cursor/projects/ws/agent-transcripts/s1");
        std::fs::create_dir_all(&session).expect("mkdir");
        let cwd = project_dir.display().to_string().replace('\\', "/");
        std::fs::write(
            session.join("s1.jsonl"),
            format!(
                "{{\"role\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"name\":\"Read\",\"input\":{{\"path\":\"{cwd}/src/lib.rs\"}}}}]}}}}"
            ),
        )
        .expect("transcript");
    }

    const PLAN_A: &str =
        "---\nname: Conductor UI\noverview: test\n---\n\n# Conductor UI\n\nBody A.\n";

    #[test]
    fn codebuddy_plan_requires_native_session_attribution() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        let plans = home.path().join(".codebuddy/plans");
        std::fs::create_dir_all(&plans).expect("plan dir");
        let native_plan = plans.join("native-plan.md");
        std::fs::write(&native_plan, PLAN_A).expect("native plan");
        std::fs::write(plans.join("foreign.md"), PLAN_A).expect("foreign plan");
        let transcripts = home.path().join(".codebuddy/projects/native");
        std::fs::create_dir_all(&transcripts).expect("transcript dir");
        let event = serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "plan this"}],
            "sessionId": "s1",
            "cwd": project.path().to_string_lossy(),
            "timestamp": 1i64,
        });
        std::fs::write(
            transcripts.join("s1.jsonl"),
            serde_json::to_string(&event).unwrap(),
        )
        .expect("transcript");
        std::fs::write(
            transcripts.join("s1.meta.json"),
            serde_json::to_string(&serde_json::json!({"planFilePath": native_plan})).unwrap(),
        )
        .expect("session metadata");

        let report = sync_from(home.path(), project.path(), "codebuddy");
        assert_eq!(report.ingested.len(), 1, "{report:?}");
        assert!(
            report.notes.iter().any(|note| note.contains("foreign.md")),
            "{report:?}"
        );
        let (meta, _) = crate::plans::current(project.path()).expect("plan");
        assert_eq!(meta.created_by_harness, "codebuddy");
        let second = sync_from(home.path(), project.path(), "codebuddy");
        assert!(second.ingested.is_empty(), "{second:?}");
    }

    #[test]
    fn commandcode_plans_require_this_projects_session_activity() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        let plans = home.path().join(".commandcode/plans");
        std::fs::create_dir_all(&plans).expect("plan dir");
        let plan = plans.join("integration-plan.md");
        std::fs::write(&plan, PLAN_A).expect("plan");
        let sessions = home
            .path()
            .join(".commandcode/projects")
            .join(project.path().file_name().expect("slug"));
        std::fs::create_dir_all(&sessions).expect("session dir");
        std::fs::write(
            sessions.join("s1.jsonl"),
            format!(
                "{}\n",
                serde_json::json!({
                    "type": "session",
                    "version": 3,
                    "id": "s1",
                    "timestamp": "2026-11-18T10:00:00.000Z",
                    "cwd": project.path().to_string_lossy(),
                })
            ),
        )
        .expect("transcript");

        let report = sync_from(home.path(), project.path(), "commandcode");
        assert_eq!(report.ingested.len(), 1, "{report:?}");
        let (meta, _) = crate::plans::current(project.path()).expect("plan");
        assert_eq!(meta.created_by_harness, "commandcode");
        assert_eq!(meta.status, "draft");
        // Second pass: no duplicate.
        assert!(sync_from(home.path(), project.path(), "commandcode").is_quiet());

        // A different project (no matching session cwd) must not inherit it.
        let other = tempfile::tempdir().expect("other");
        crate::local_store::init_skeleton(other.path(), "p2", "n2", "default").expect("init");
        let other_report = sync_from(home.path(), other.path(), "commandcode");
        assert!(other_report.ingested.is_empty(), "{other_report:?}");
        assert!(
            other_report.notes.iter().any(|n| n.contains("skipped")),
            "{other_report:?}"
        );
    }

    #[test]
    fn native_plan_lands_as_draft_with_provenance() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        seed_cursor_transcript(home.path(), project.path());
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
    fn cursor_plan_without_session_activity_is_skipped() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        // No transcript naming this project — the global pile must not leak.
        write_plan(home.path(), "foreign_a1b2c3d4.plan.md", PLAN_A);
        let report = sync_from(home.path(), project.path(), "cursor");
        assert!(report.ingested.is_empty(), "{report:?}");
        assert!(
            report.notes.iter().any(|n| n.contains("skipped")),
            "{report:?}"
        );
        assert!(crate::plans::current(project.path()).is_none());
    }

    #[test]
    fn claude_home_plan_needs_this_projects_session_activity() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        write_claude_plan(home.path(), "parity-plan.md", PLAN_A);

        // No transcript for this project → the global pile must not leak.
        let report = sync_from(home.path(), project.path(), "claude-code");
        assert!(report.ingested.is_empty(), "{report:?}");
        assert!(
            report.notes.iter().any(|n| n.contains("skipped")),
            "{report:?}"
        );

        // With this project's activity near the plan's mtime → a draft.
        seed_claude_transcript(home.path(), project.path());
        let report = sync_from(home.path(), project.path(), "claude-code");
        assert_eq!(report.ingested.len(), 1, "{report:?}");
        let (meta, _) = crate::plans::current(project.path()).expect("plan");
        assert_eq!(meta.status, "draft");
        assert_eq!(meta.created_by_harness, "claude-code");
        assert!(meta.source_path.unwrap().contains(".claude/plans"));
        // Second pass: no duplicate.
        assert!(sync_from(home.path(), project.path(), "claude-code").is_quiet());
    }

    #[test]
    fn claude_global_plan_does_not_leak_into_another_project() {
        let home = tempfile::tempdir().expect("home");
        let project_a = project();
        let project_b = project();
        write_claude_plan(home.path(), "parity-plan.md", PLAN_A);
        seed_claude_transcript(home.path(), project_a.path());

        let report_a = sync_from(home.path(), project_a.path(), "claude-code");
        assert_eq!(report_a.ingested.len(), 1, "{report_a:?}");
        // Project B has no Claude activity: the same global file is not its
        // plan, even though it fed the identical sync pass.
        let report_b = sync_from(home.path(), project_b.path(), "claude-code");
        assert!(report_b.ingested.is_empty(), "{report_b:?}");
        assert!(crate::plans::current(project_b.path()).is_none());
    }

    #[test]
    fn claude_project_plans_dir_is_structural() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        let dir = project.path().join(".claude/plans");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("local-plan.md"), PLAN_A).expect("write");

        // Inside the project the path itself proves ownership: no transcript
        // activity required.
        let report = sync_from(home.path(), project.path(), "claude-code");
        assert_eq!(report.ingested.len(), 1, "{report:?}");
        let (meta, _) = crate::plans::current(project.path()).expect("plan");
        assert_eq!(meta.created_by_harness, "claude-code");
    }

    #[test]
    fn claude_plans_directory_setting_resolves_against_the_project() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        std::fs::create_dir_all(project.path().join(".claude")).expect("mkdir");
        std::fs::write(
            project.path().join(".claude/settings.json"),
            serde_json::json!({"plansDirectory": ".claude/custom-plans"}).to_string(),
        )
        .expect("settings");
        let dir = project.path().join(".claude/custom-plans");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("custom-plan.md"), PLAN_A).expect("write");

        let report = sync_from(home.path(), project.path(), "claude-code");
        assert_eq!(report.ingested.len(), 1, "{report:?}");
        let (meta, _) = crate::plans::current(project.path()).expect("plan");
        assert!(
            meta.source_path.unwrap().contains(".claude/custom-plans"),
            "the configured directory is the source"
        );
    }

    #[test]
    fn claude_plans_directory_foreign_path_fails_closed() {
        // A Windows-shaped value is not project-relative on this host, and
        // must not become a bogus directory inside the project.
        assert_eq!(
            resolve_plans_directory(Path::new("/work/demo"), "C:\\Users\\a\\plans"),
            None
        );
        assert_eq!(
            resolve_plans_directory(Path::new("/work/demo"), "notes/plans"),
            Some(PathBuf::from("/work/demo/notes/plans"))
        );
        assert_eq!(resolve_plans_directory(Path::new("/work/demo"), "  "), None);
    }

    #[test]
    fn native_edit_updates_draft_but_never_past_draft() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        seed_cursor_transcript(home.path(), project.path());
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
        assert!(maybe_auto(home.path(), project.path(), "openclaw", 15).is_none());
        // A harness with a known dir ingests; another project is untouched.
        seed_cursor_transcript(home.path(), project.path());
        write_plan(home.path(), "conductor_a1b2c3d4.plan.md", PLAN_A);
        let report = maybe_auto(home.path(), project.path(), "cursor", 15).expect("pass");
        assert_eq!(report.ingested.len(), 1);
        // Interval gate: an immediate second pass is silent.
        assert!(maybe_auto(home.path(), project.path(), "cursor", 15).is_none());
    }

    fn seed_dsh_plan(home: &Path, cwd: &Path, id: &str, plan: &str) -> PathBuf {
        let dir = home.join(format!(".dsh/sessions/--tmp-dsh--/{id}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("session.jsonl");
        let header = serde_json::json!({
            "type": "session",
            "version": 4,
            "id": id,
            "createdAt": 1,
            "cwd": cwd.to_string_lossy(),
        });
        let arguments = serde_json::json!({ "plan": plan }).to_string();
        let call = serde_json::json!({
            "type": "tool/call",
            "seq": 0,
            "time": 1,
            "data": {
                "turn": 1,
                "step": 1,
                "callId": "c1",
                "name": "exit_plan_mode",
                "arguments": arguments,
            },
        });
        std::fs::write(&path, format!("{header}\n{call}\n")).expect("write");
        path
    }

    #[test]
    fn dsh_exit_plan_mode_lands_as_draft_and_refreshes() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        let other = tempfile::tempdir().expect("other");
        seed_dsh_plan(
            home.path(),
            other.path(),
            "foreign",
            "# Foreign\n\nnot ours\n",
        );
        seed_dsh_plan(home.path(), project.path(), "s1", "# DSH Plan\n\nBody A\n");

        let report = sync_from(home.path(), project.path(), "dsh");
        assert_eq!(report.ingested.len(), 1, "only this project's session");
        let (meta, _) = crate::plans::current(project.path()).expect("draft plan");
        assert_eq!(meta.created_by_harness, "dsh");
        assert_eq!(meta.status, "draft");

        let again = sync_from(home.path(), project.path(), "dsh");
        assert!(again.ingested.is_empty() && again.updated.is_empty());

        seed_dsh_plan(home.path(), project.path(), "s1", "# DSH Plan\n\nBody B\n");
        let edited = sync_from(home.path(), project.path(), "dsh");
        assert_eq!(edited.updated.len(), 1);
        let (_, body_path) = crate::plans::load(project.path(), &meta.id).expect("load");
        let body = std::fs::read_to_string(body_path).expect("body");
        assert!(body.contains("Body B"));
    }

    #[test]
    fn kimi_session_plan_lands_as_draft_for_matching_project() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        let session = home.path().join(".kimi-code/sessions/wd_project/session-1");
        std::fs::create_dir_all(session.join("agents/main/plans")).expect("mkdir");
        std::fs::write(
            session.join("state.json"),
            format!(
                r#"{{"cwd":{}}}"#,
                serde_json::to_string(&crate::transcripts::path_for_json(project.path())).unwrap()
            ),
        )
        .expect("state");
        std::fs::write(
            session.join("agents/main/plans/historical.md"),
            "# Historical plan\n\nBody.\n",
        )
        .expect("historical plan");
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(
            session.join("agents/main/plans/honest-smoke.md"),
            "# Honest smoke fix\n\nBody.\n",
        )
        .expect("current plan");

        let report = sync_from(home.path(), project.path(), "kimi-code");
        assert_eq!(report.ingested.len(), 1, "{report:?}");
        let (meta, _) = crate::plans::current(project.path()).expect("plan");
        assert_eq!(meta.title, "Honest smoke fix");
        assert_eq!(meta.created_by_harness, "kimi-code");
        assert!(meta.source_path.unwrap().contains(".kimi-code/sessions"));
    }
}
