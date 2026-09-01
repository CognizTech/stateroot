//! Todo federation — harness todo lists as first-class state.
//!
//! Plan-bound todos live inside a Cursor plan file's frontmatter `todos:`
//! and drive deterministic plan completion. Standalone todos are session
//! working lists (Claude `TodoWrite`, Kimi `TodoList`, Codex `update_plan`,
//! Cursor `TodoWrite` not in a plan file) — visibility only, never plan
//! status. No heuristic linking: `plan_id` is set only when the harness
//! structurally binds the list to a plan.
//!
//! One record per (harness, session) at `.stateroot/todos/<harness>/<session>.json`.
//! Last-list-wins per session: kimi/codex replace; cursor merge-form merges
//! by `id`. Jsonl sources replay only a tail, never the whole transcript.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::local_store::{self, now_rfc3339};
use crate::path_identity;
use crate::plans::{self, PlanStatus};

/// Schema tag on a todo record.
pub const SCHEMA_TODO_V1: &str = "stateroot.todo.v1";
/// Todos dir, relative to `.stateroot/`.
pub const TODOS_REL: &str = "todos";
/// Tail window for jsonl replay (bytes).
const JSONL_TAIL: usize = 512 * 1024;
/// Head window for session meta / cwd (bytes).
const JSONL_HEAD: usize = 16 * 1024;

/// One item in a federated todo list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    /// Harness-native key, verbatim (cursor `id`, kimi `title`, claude
    /// `content`, codex `step`).
    pub key: String,
    /// Display text.
    pub content: String,
    /// `pending`, `in_progress`, or `completed`.
    pub status: String,
}

/// One persisted (harness, session) record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoRecord {
    /// Schema tag.
    #[serde(default = "todo_schema")]
    pub schema_version: String,
    /// Canonical harness id.
    pub harness: String,
    /// Session (or plan-file) key.
    pub session_id: String,
    /// Set only when structurally bound to a plan (cursor frontmatter).
    pub plan_id: Option<String>,
    /// Current items.
    pub items: Vec<TodoItem>,
    /// Last write.
    pub updated_at: String,
    /// `observed · <source path>`.
    pub provenance: String,
}

fn todo_schema() -> String {
    SCHEMA_TODO_V1.to_string()
}

/// Outcome of one standalone federation pass.
#[derive(Debug, Default)]
pub struct TodoSyncReport {
    /// Records written or replaced.
    pub written: Vec<String>,
    /// Notes (skipped, parse gaps).
    pub notes: Vec<String>,
}

impl TodoSyncReport {
    /// True when nothing happened.
    pub fn is_quiet(&self) -> bool {
        self.written.is_empty() && self.notes.is_empty()
    }
}

fn todos_dir(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(TODOS_REL)
}

fn record_path(project_dir: &Path, harness: &str, session_key: &str) -> PathBuf {
    todos_dir(project_dir)
        .join(sanitize_key(harness))
        .join(format!("{}.json", sanitize_key(session_key)))
}

/// Filesystem-safe key (Windows-hostile `:` `*` etc. folded).
pub fn sanitize_key(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped.trim_matches('-');
    if trimmed.is_empty() {
        "session".to_string()
    } else {
        trimmed.chars().take(120).collect()
    }
}

fn normalize_status(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "completed" | "complete" | "done" => "completed".into(),
        "in_progress" | "in-progress" | "inprogress" => "in_progress".into(),
        "pending" | "todo" | "not_started" => "pending".into(),
        "" => "pending".into(),
        other => other.to_string(),
    }
}

/// True when every item is `completed` and the list is non-empty.
pub fn all_completed(items: &[TodoItem]) -> bool {
    !items.is_empty() && items.iter().all(|item| item.status == "completed")
}

/// `(completed, total)` for a plan-bound record, when any.
pub fn plan_todo_progress(project_dir: &Path, plan_id: &str) -> Option<(usize, usize)> {
    let record = load_plan_bound(project_dir, plan_id)?;
    if record.items.is_empty() {
        return None;
    }
    let done = record
        .items
        .iter()
        .filter(|item| item.status == "completed")
        .count();
    Some((done, record.items.len()))
}

fn load_plan_bound(project_dir: &Path, plan_id: &str) -> Option<TodoRecord> {
    let dir = todos_dir(project_dir);
    let Ok(harnesses) = std::fs::read_dir(&dir) else {
        return None;
    };
    for harness in harnesses.flatten() {
        let Ok(files) = std::fs::read_dir(harness.path()) else {
            continue;
        };
        for file in files.flatten() {
            let Ok(text) = std::fs::read_to_string(file.path()) else {
                continue;
            };
            let Ok(record) = serde_json::from_str::<TodoRecord>(&text) else {
                continue;
            };
            if record.plan_id.as_deref() == Some(plan_id) {
                return Some(record);
            }
        }
    }
    None
}

fn write_record(project_dir: &Path, record: &TodoRecord) -> Result<PathBuf, String> {
    let path = record_path(project_dir, &record.harness, &record.session_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create todos dir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(record).map_err(|e| e.to_string())?;
    std::fs::write(&path, format!("{json}\n")).map_err(|e| format!("write todo record: {e}"))?;
    Ok(path)
}

fn load_record(project_dir: &Path, harness: &str, session_key: &str) -> Option<TodoRecord> {
    let text = std::fs::read_to_string(record_path(project_dir, harness, session_key)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Replace the session list (kimi / codex / cursor full rewrite).
pub fn replace_list(
    project_dir: &Path,
    harness: &str,
    session_id: &str,
    items: Vec<TodoItem>,
    provenance: &str,
    plan_id: Option<String>,
) -> Result<TodoRecord, String> {
    let record = TodoRecord {
        schema_version: SCHEMA_TODO_V1.into(),
        harness: harness.to_string(),
        session_id: session_id.to_string(),
        plan_id,
        items,
        updated_at: now_rfc3339(),
        provenance: format!("observed · {provenance}"),
    };
    write_record(project_dir, &record)?;
    Ok(record)
}

/// Merge incoming items by `key` (cursor merge-form). Existing content is
/// kept when the incoming item omits it.
pub fn merge_by_id(
    project_dir: &Path,
    harness: &str,
    session_id: &str,
    incoming: Vec<TodoItem>,
    provenance: &str,
) -> Result<TodoRecord, String> {
    let mut record = load_record(project_dir, harness, session_id).unwrap_or(TodoRecord {
        schema_version: SCHEMA_TODO_V1.into(),
        harness: harness.to_string(),
        session_id: session_id.to_string(),
        plan_id: None,
        items: Vec::new(),
        updated_at: now_rfc3339(),
        provenance: String::new(),
    });
    for item in incoming {
        if let Some(existing) = record.items.iter_mut().find(|row| row.key == item.key) {
            existing.status = item.status;
            if !item.content.is_empty() {
                existing.content = item.content;
            }
        } else {
            record.items.push(item);
        }
    }
    record.updated_at = now_rfc3339();
    record.provenance = format!("observed · {provenance}");
    write_record(project_dir, &record)?;
    Ok(record)
}

/// Write a plan-bound record from Cursor frontmatter (replace, structural bind).
pub fn upsert_plan_bound(
    project_dir: &Path,
    session_key: &str,
    plan_id: &str,
    items: Vec<TodoItem>,
    source: &Path,
) -> Result<TodoRecord, String> {
    replace_list(
        project_dir,
        "cursor",
        session_key,
        items,
        &source.to_string_lossy(),
        Some(plan_id.to_string()),
    )
}

/// Draft → approved → done, or approved/active → done. Terminal no-op.
/// Notes get `auto: all todos completed`.
pub fn complete_plan(project_dir: &Path, plan_id: &str) -> Result<bool, String> {
    let Some((meta, _)) = plans::load(project_dir, plan_id) else {
        return Ok(false);
    };
    match meta.status() {
        PlanStatus::Done | PlanStatus::Abandoned => Ok(false),
        PlanStatus::Draft => {
            plans::transition(project_dir, plan_id, PlanStatus::Approved)?;
            plans::transition(project_dir, plan_id, PlanStatus::Done)?;
            plans::append_notes(project_dir, plan_id, "auto: all todos completed")?;
            Ok(true)
        }
        PlanStatus::Approved | PlanStatus::Active => {
            plans::transition(project_dir, plan_id, PlanStatus::Done)?;
            plans::append_notes(project_dir, plan_id, "auto: all todos completed")?;
            Ok(true)
        }
    }
}

/// Parse Cursor plan frontmatter `todos:` (real shape includes `isProject`).
pub fn parse_frontmatter_todos(text: &str) -> Vec<TodoItem> {
    let Some(yaml) = frontmatter_yaml(text) else {
        return Vec::new();
    };
    let Ok(value) = serde_yaml::from_str::<Value>(&yaml) else {
        return Vec::new();
    };
    let Some(todos) = value.get("todos").and_then(Value::as_array) else {
        return Vec::new();
    };
    todos.iter().filter_map(item_from_cursor_yaml).collect()
}

fn frontmatter_yaml(text: &str) -> Option<String> {
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut body = String::new();
    for line in lines {
        if line.trim() == "---" {
            return Some(body);
        }
        body.push_str(line);
        body.push('\n');
    }
    None
}

fn item_from_cursor_yaml(value: &Value) -> Option<TodoItem> {
    let key = value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let content = value
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let status = normalize_status(value.get("status").and_then(Value::as_str).unwrap_or(""));
    if key.is_empty() && content.is_empty() {
        return None;
    }
    Some(TodoItem {
        key: if key.is_empty() { content.clone() } else { key },
        content,
        status,
    })
}

fn items_from_cursor_input(input: &Value) -> Vec<TodoItem> {
    let Some(todos) = input.get("todos").and_then(Value::as_array) else {
        return Vec::new();
    };
    todos
        .iter()
        .filter_map(|value| {
            let key = value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let content = value
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let status =
                normalize_status(value.get("status").and_then(Value::as_str).unwrap_or(""));
            if key.is_empty() && content.is_empty() {
                return None;
            }
            Some(TodoItem {
                key: if key.is_empty() { content.clone() } else { key },
                content,
                status,
            })
        })
        .collect()
}

fn cursor_merge_flag(input: &Value) -> bool {
    input.get("merge").and_then(Value::as_bool).unwrap_or(false)
}

/// True when this harness has a standalone todo source.
pub fn harness_has_sources(harness: &str) -> bool {
    matches!(
        harness,
        "cursor" | "claude" | "claude-code" | "kimi" | "kimi-code" | "codex"
    )
}

/// Standalone pass for one harness (plan-bound Cursor todos are applied
/// from `plan_federation::sync_from`).
pub fn sync_from(home: &Path, project_dir: &Path, harness: &str) -> TodoSyncReport {
    match harness {
        "cursor" => sync_cursor_standalone(home, project_dir),
        "claude" | "claude-code" => sync_claude(home, project_dir),
        "kimi" | "kimi-code" => sync_kimi(home, project_dir),
        "codex" => sync_codex(home, project_dir),
        _ => TodoSyncReport::default(),
    }
}

fn sync_cursor_standalone(home: &Path, project_dir: &Path) -> TodoSyncReport {
    let mut report = TodoSyncReport::default();
    let project_key = path_identity::equivalent_project_key(project_dir);
    let projects = home.join(".cursor/projects");
    let Ok(workspaces) = std::fs::read_dir(&projects) else {
        return report;
    };
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
                if !transcript_head_belongs(&path, &project_key) {
                    continue;
                }
                let Some(items) = cursor_todowrite_state(&path) else {
                    continue;
                };
                let session_id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("session");
                let result = replace_list(
                    project_dir,
                    "cursor",
                    session_id,
                    items,
                    &path.to_string_lossy(),
                    None,
                );
                match result {
                    Ok(record) => report.written.push(format!(
                        "cursor/{} ({} items)",
                        record.session_id,
                        record.items.len()
                    )),
                    Err(err) => report.notes.push(err),
                }
            }
        }
    }
    report
}

fn transcript_head_belongs(path: &Path, project_key: &str) -> bool {
    let Ok(head) = read_head(path, JSONL_HEAD) else {
        return false;
    };
    for token in head.split(|c: char| c == '"' || c == '\'' || c.is_whitespace()) {
        let t = token.trim_matches(|c| ",{}[]()".contains(c));
        let looks_absolute = t.starts_with('/')
            || (t.len() > 2
                && t.as_bytes()[1] == b':'
                && (t.as_bytes()[2] == b'\\' || t.as_bytes()[2] == b'/'));
        if !looks_absolute {
            continue;
        }
        if let Some(root) = local_store::find_project_root(Path::new(t)) {
            if path_identity::equivalent_project_key(&root) == project_key {
                return true;
            }
        }
    }
    false
}

/// Replay one transcript's todo state: the last full TodoWrite (merge=false)
/// is the base list; every merge call after it updates statuses by id with
/// content preserved from the base. Reading only the LAST call leaves
/// merge-only items contentless — the blank `[x]` bug: a transcript that
/// ends on a merge call would push id+status items with no text at all.
fn cursor_todowrite_state(path: &Path) -> Option<Vec<TodoItem>> {
    let tail = read_tail(path, JSONL_TAIL).ok()?;
    let mut base: Option<Vec<TodoItem>> = None;
    let mut merges: Vec<Vec<TodoItem>> = Vec::new();
    for line in tail.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(input) = cursor_todowrite_input(&value) else {
            continue;
        };
        let items = items_from_cursor_input(input);
        if items.is_empty() {
            continue;
        }
        if cursor_merge_flag(input) {
            if base.is_some() {
                merges.push(items);
            } else {
                // A merge with no full write before it: treat as the base —
                // nothing earlier exists to borrow content from.
                base = Some(items);
                merges.clear();
            }
        } else {
            base = Some(items);
            merges.clear();
        }
    }
    let mut items = base?;
    for merge_items in merges {
        for item in merge_items {
            if let Some(row) = items.iter_mut().find(|r| r.key == item.key) {
                row.status = item.status;
                if !item.content.is_empty() {
                    row.content = item.content;
                }
            } else {
                items.push(item);
            }
        }
    }
    Some(items)
}

fn cursor_todowrite_input(value: &Value) -> Option<&Value> {
    walk_tool_use(value, "TodoWrite")
}

fn walk_tool_use<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    if value.get("name").and_then(Value::as_str) == Some(name) {
        if let Some(input) = value.get("input") {
            return Some(input);
        }
        if let Some(input) = value.get("arguments") {
            return Some(input);
        }
    }
    match value {
        Value::Object(map) => map.values().find_map(|inner| walk_tool_use(inner, name)),
        Value::Array(arr) => arr.iter().find_map(|inner| walk_tool_use(inner, name)),
        _ => None,
    }
}

fn sync_claude(home: &Path, project_dir: &Path) -> TodoSyncReport {
    let mut report = TodoSyncReport::default();
    let projects = home.join(".claude/projects");
    let Ok(entries) = std::fs::read_dir(&projects) else {
        return report;
    };
    let project_norm = path_identity::normalize_host_path(&project_dir.to_string_lossy());
    for entry in entries.flatten() {
        let slug = entry.file_name().to_string_lossy().to_string();
        if !claude_slug_overlaps(&slug, &project_norm) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(entry.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(items) = cursor_todowrite_state(&path) else {
                continue;
            };
            if items.is_empty() {
                continue;
            }
            let session_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("session");
            let result = replace_list(
                project_dir,
                "claude-code",
                session_id,
                items,
                &path.to_string_lossy(),
                None,
            );
            match result {
                Ok(record) => report.written.push(format!(
                    "claude-code/{} ({} items)",
                    record.session_id,
                    record.items.len()
                )),
                Err(err) => report.notes.push(err),
            }
        }
    }
    report
}

fn claude_slug_overlaps(slug: &str, project_norm: &str) -> bool {
    let rest = slug.strip_prefix('-').unwrap_or(slug);
    let decoded = path_identity::normalize_host_path(&format!("/{}", rest.replace('-', "/")));
    decoded == *project_norm
        || (decoded.len() > project_norm.len()
            && decoded.starts_with(project_norm)
            && decoded.as_bytes()[project_norm.len()] == b'/')
        || (project_norm.len() > decoded.len()
            && project_norm.starts_with(&decoded)
            && project_norm.as_bytes()[decoded.len()] == b'/')
}

fn sync_kimi(home: &Path, project_dir: &Path) -> TodoSyncReport {
    let mut report = TodoSyncReport::default();
    let sessions = home.join(".kimi-code/sessions");
    let Ok(workspace_dirs) = std::fs::read_dir(&sessions) else {
        return report;
    };
    let project_key = path_identity::equivalent_project_key(project_dir);
    for workspace in workspace_dirs.flatten() {
        let Ok(session_dirs) = std::fs::read_dir(workspace.path()) else {
            continue;
        };
        for session in session_dirs.flatten() {
            let session_dir = session.path();
            let state_path = session_dir.join("state.json");
            let Ok(state) = std::fs::read_to_string(&state_path) else {
                continue;
            };
            let Ok(state) = serde_json::from_str::<Value>(&state) else {
                continue;
            };
            let Some(cwd) = state.get("cwd").and_then(Value::as_str) else {
                continue;
            };
            if path_identity::equivalent_project_key(Path::new(cwd)) != project_key {
                continue;
            }
            let wire = session_dir.join("agents/main/wire.jsonl");
            let Some(items) = last_kimi_todolist(&wire) else {
                continue;
            };
            let session_id = session_dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("session");
            match replace_list(
                project_dir,
                "kimi-code",
                session_id,
                items,
                &wire.to_string_lossy(),
                None,
            ) {
                Ok(record) => report.written.push(format!(
                    "kimi-code/{} ({} items)",
                    record.session_id,
                    record.items.len()
                )),
                Err(err) => report.notes.push(err),
            }
        }
    }
    report
}

fn last_kimi_todolist(path: &Path) -> Option<Vec<TodoItem>> {
    let value = last_jsonl_value(path, is_kimi_todolist)?;
    kimi_items(&value)
}

fn is_kimi_todolist(value: &Value) -> bool {
    kimi_items(value).is_some()
}

fn kimi_items(value: &Value) -> Option<Vec<TodoItem>> {
    let event = value.get("event").unwrap_or(value);
    if event.get("name").and_then(Value::as_str) != Some("TodoList") {
        return None;
    }
    let todos = event
        .get("args")
        .and_then(|args| args.get("todos"))
        .and_then(Value::as_array)?;
    let items: Vec<TodoItem> = todos
        .iter()
        .filter_map(|todo| {
            let title = todo.get("title").and_then(Value::as_str)?;
            Some(TodoItem {
                key: title.to_string(),
                content: title.to_string(),
                status: normalize_status(todo.get("status").and_then(Value::as_str).unwrap_or("")),
            })
        })
        .collect();
    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}

fn sync_codex(home: &Path, project_dir: &Path) -> TodoSyncReport {
    let mut report = TodoSyncReport::default();
    let (sessions, archived) = crate::harness_install::paths::codex_transcript_roots(home);
    let mut files = Vec::new();
    collect_rollouts(&sessions, &mut files);
    collect_rollouts(&archived, &mut files);
    let project_key = path_identity::equivalent_project_key(project_dir);
    for path in files {
        let Ok(head) = read_head(&path, JSONL_HEAD) else {
            continue;
        };
        let Some(first) = head.lines().find(|line| !line.trim().is_empty()) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<Value>(first) else {
            continue;
        };
        if meta.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        let Some(cwd) = meta.pointer("/payload/cwd").and_then(Value::as_str) else {
            continue;
        };
        if path_identity::equivalent_project_key(Path::new(cwd)) != project_key {
            continue;
        }
        let session_id = meta
            .pointer("/payload/id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("session")
                    .to_string()
            });
        let Some(items) = last_codex_plan(&path) else {
            continue;
        };
        match replace_list(
            project_dir,
            "codex",
            &session_id,
            items,
            &path.to_string_lossy(),
            None,
        ) {
            Ok(record) => report.written.push(format!(
                "codex/{} ({} items)",
                record.session_id,
                record.items.len()
            )),
            Err(err) => report.notes.push(err),
        }
    }
    report
}

fn collect_rollouts(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(walker) = std::fs::read_dir(root) else {
        return;
    };
    for entry in walker.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rollouts(&path, out);
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with("rollout-") && name.ends_with(".jsonl") {
            out.push(path);
        }
    }
}

fn last_codex_plan(path: &Path) -> Option<Vec<TodoItem>> {
    let value = last_jsonl_value(path, is_codex_update_plan)?;
    codex_items(&value)
}

fn is_codex_update_plan(value: &Value) -> bool {
    value
        .pointer("/payload/name")
        .and_then(Value::as_str)
        .is_some_and(|name| name == "update_plan")
        || value.get("name").and_then(Value::as_str) == Some("update_plan")
}

fn codex_items(value: &Value) -> Option<Vec<TodoItem>> {
    let args_raw = value
        .pointer("/payload/arguments")
        .and_then(Value::as_str)
        .or_else(|| value.get("arguments").and_then(Value::as_str))?;
    let args: Value = serde_json::from_str(args_raw).ok()?;
    let plan = args.get("plan").and_then(Value::as_array)?;
    let items: Vec<TodoItem> = plan
        .iter()
        .filter_map(|step| {
            let text = step.get("step").and_then(Value::as_str)?;
            Some(TodoItem {
                key: text.to_string(),
                content: text.to_string(),
                status: normalize_status(step.get("status").and_then(Value::as_str).unwrap_or("")),
            })
        })
        .collect();
    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}

/// Newest session record per harness (current view).
pub fn current_lists(project_dir: &Path, harness: Option<&str>) -> Vec<TodoRecord> {
    let dir = todos_dir(project_dir);
    let Ok(harnesses) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut all = Vec::new();
    for entry in harnesses.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(filter) = harness {
            let want = match filter.trim().to_ascii_lowercase().as_str() {
                "kimi" | "kimi-code" => "kimi-code".to_string(),
                "claude" | "claude-code" => "claude-code".to_string(),
                "cursor" => "cursor".to_string(),
                "codex" => "codex".to_string(),
                other => sanitize_key(other),
            };
            if name != want {
                continue;
            }
        }
        let Ok(files) = std::fs::read_dir(entry.path()) else {
            continue;
        };
        for file in files.flatten() {
            let Ok(text) = std::fs::read_to_string(file.path()) else {
                continue;
            };
            if let Ok(record) = serde_json::from_str::<TodoRecord>(&text) {
                all.push(record);
            }
        }
    }
    all.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then(b.session_id.cmp(&a.session_id))
    });
    let mut seen = std::collections::HashSet::new();
    let mut current = Vec::new();
    for record in all {
        if seen.insert(record.harness.clone()) {
            current.push(record);
        }
    }
    current.sort_by(|a, b| a.harness.cmp(&b.harness));
    current
}

fn read_head(path: &Path, bytes: usize) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; bytes];
    let n = file.read(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf[..n]).to_string())
}

fn last_jsonl_value(path: &Path, pred: impl Fn(&Value) -> bool) -> Option<Value> {
    let tail = read_tail(path, JSONL_TAIL).ok()?;
    let mut last = None;
    for line in tail.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if pred(&value) {
            last = Some(value);
        }
    }
    last
}

fn read_tail(path: &Path, cap: usize) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(String::new());
    }
    let take = u64::try_from(cap).unwrap_or(u64::MAX).min(len);
    let start = len.saturating_sub(take);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    if start > 0 {
        if let Some(idx) = buf.iter().position(|&b| b == b'\n') {
            buf = buf.split_off(idx + 1);
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("project");
        crate::local_store::init_skeleton(tmp.path(), "p", "n", "default").expect("init");
        tmp
    }

    const FRONTMATTER: &str = r#"---
name: cli-autoinstall
overview: Add a confirmed stable-channel CLI bootstrap
todos:
  - id: installer
    content: Implement platform detection
    status: completed
  - id: cli-wrapper
    content: Route every CLI action
    status: completed
  - id: surface
    content: Add install command
    status: completed
  - id: verify
    content: Compile, package, and smoke
    status: completed
isProject: false
---

# Confirmed CLI auto-install
"#;

    #[test]
    fn frontmatter_parse_includes_is_project_shape() {
        let items = parse_frontmatter_todos(FRONTMATTER);
        assert_eq!(items.len(), 4, "{items:?}");
        assert_eq!(items[0].key, "installer");
        assert_eq!(items[0].status, "completed");
        assert!(all_completed(&items));
    }

    #[test]
    fn kimi_full_list_replace() {
        let dir = project();
        let first = vec![
            TodoItem {
                key: "a".into(),
                content: "a".into(),
                status: "in_progress".into(),
            },
            TodoItem {
                key: "b".into(),
                content: "b".into(),
                status: "pending".into(),
            },
        ];
        replace_list(dir.path(), "kimi-code", "sess-1", first, "wire", None).expect("write");
        let second = vec![TodoItem {
            key: "b".into(),
            content: "b".into(),
            status: "completed".into(),
        }];
        let record =
            replace_list(dir.path(), "kimi-code", "sess-1", second, "wire", None).expect("replace");
        assert_eq!(record.items.len(), 1);
        assert_eq!(record.items[0].key, "b");
        assert!(record.plan_id.is_none());
    }

    #[test]
    fn cursor_merge_by_id_keeps_content() {
        let dir = project();
        replace_list(
            dir.path(),
            "cursor",
            "abc",
            vec![TodoItem {
                key: "audit".into(),
                content: "Audit the seams".into(),
                status: "in_progress".into(),
            }],
            "t.jsonl",
            None,
        )
        .expect("seed");
        let record = merge_by_id(
            dir.path(),
            "cursor",
            "abc",
            vec![TodoItem {
                key: "audit".into(),
                content: String::new(),
                status: "completed".into(),
            }],
            "t.jsonl",
        )
        .expect("merge");
        assert_eq!(record.items.len(), 1);
        assert_eq!(record.items[0].content, "Audit the seams");
        assert_eq!(record.items[0].status, "completed");
    }

    #[test]
    fn todowrite_state_replays_full_write_then_merges() {
        let tmp = tempfile::tempdir().expect("tmp");
        let transcript = tmp.path().join("s1.jsonl");
        let full = r#"{"role":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"merge":false,"todos":[{"id":"trace","content":"Trace the path","status":"in_progress"},{"id":"cause","content":"Name the cause","status":"pending"}]}}]}}"#;
        let merge = r#"{"role":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"merge":true,"todos":[{"id":"trace","status":"completed"},{"id":"fix","status":"completed"}]}}]}}"#;
        std::fs::write(&transcript, format!("{full}\n{merge}\n")).expect("write");
        let items = cursor_todowrite_state(&transcript).expect("state");
        let trace = items.iter().find(|i| i.key == "trace").expect("trace");
        assert_eq!(trace.content, "Trace the path");
        assert_eq!(trace.status, "completed");
        let cause = items.iter().find(|i| i.key == "cause").expect("cause");
        assert_eq!(cause.content, "Name the cause");
        assert_eq!(cause.status, "pending");
        // A merge-only id is added (contentless — nothing to borrow from).
        let fix = items.iter().find(|i| i.key == "fix").expect("fix");
        assert_eq!(fix.status, "completed");
        // Crucially: the merge must not blank any content-bearing row.
        assert!(items.iter().filter(|i| !i.content.is_empty()).count() >= 2);

        // A merge before any full write becomes the base — no blanks panic.
        std::fs::write(&transcript, format!("{merge}\n")).expect("rewrite");
        let items = cursor_todowrite_state(&transcript).expect("merge-only");
        assert!(items
            .iter()
            .any(|i| i.key == "trace" && i.status == "completed"));
    }

    #[test]
    fn plan_bound_all_complete_from_draft_approved_active() {
        let dir = project();
        for start in [PlanStatus::Draft, PlanStatus::Approved, PlanStatus::Active] {
            let meta = plans::record(
                dir.path(),
                &format!("Plan {start:?}"),
                "cursor",
                None,
                "# body\n\nDo it.\n",
            )
            .expect("record");
            match start {
                PlanStatus::Approved => {
                    plans::transition(dir.path(), &meta.id, PlanStatus::Approved).expect("ap");
                }
                PlanStatus::Active => {
                    plans::transition(dir.path(), &meta.id, PlanStatus::Approved).expect("ap");
                    plans::transition(dir.path(), &meta.id, PlanStatus::Active).expect("act");
                }
                PlanStatus::Draft => {}
                _ => unreachable!(),
            }
            assert!(complete_plan(dir.path(), &meta.id).expect("complete"));
            let (done, _) = plans::load(dir.path(), &meta.id).expect("load");
            assert_eq!(done.status(), PlanStatus::Done);
            assert!(done.notes.contains("auto: all todos completed"));
        }
    }

    #[test]
    fn abandoned_and_zero_todo_untouched() {
        let dir = project();
        let abandoned =
            plans::record(dir.path(), "Drop", "cursor", None, "# Drop\n\nno.\n").expect("record");
        plans::transition(dir.path(), &abandoned.id, PlanStatus::Abandoned).expect("abandon");
        assert!(!complete_plan(dir.path(), &abandoned.id).expect("noop"));
        let (meta, _) = plans::load(dir.path(), &abandoned.id).expect("load");
        assert_eq!(meta.status(), PlanStatus::Abandoned);

        assert!(!all_completed(&[]));
        let open = plans::record(dir.path(), "Open", "cursor", None, "# Open\n\nbody.\n")
            .expect("record2");
        // zero-todo: we never call complete_plan from federation
        assert_eq!(plan_todo_progress(dir.path(), &open.id), None);
        let (meta, _) = plans::load(dir.path(), &open.id).expect("load2");
        assert_eq!(meta.status(), PlanStatus::Draft);
    }

    #[test]
    fn standalone_never_transitions_plans() {
        let dir = project();
        let meta = plans::record(dir.path(), "Stay", "kimi-code", None, "# Stay\n\nbody.\n")
            .expect("record");
        replace_list(
            dir.path(),
            "kimi-code",
            "s1",
            vec![TodoItem {
                key: "done".into(),
                content: "done".into(),
                status: "completed".into(),
            }],
            "wire",
            None,
        )
        .expect("todos");
        let (still, _) = plans::load(dir.path(), &meta.id).expect("load");
        assert_eq!(still.status(), PlanStatus::Draft);
        assert!(plan_todo_progress(dir.path(), &meta.id).is_none());
    }

    #[test]
    fn last_list_wins_and_session_freeze() {
        let dir = project();
        replace_list(
            dir.path(),
            "kimi-code",
            "old",
            vec![TodoItem {
                key: "old".into(),
                content: "old".into(),
                status: "completed".into(),
            }],
            "a",
            None,
        )
        .expect("old");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        replace_list(
            dir.path(),
            "kimi-code",
            "new",
            vec![TodoItem {
                key: "new".into(),
                content: "new".into(),
                status: "pending".into(),
            }],
            "b",
            None,
        )
        .expect("new");
        let current = current_lists(dir.path(), Some("kimi-code"));
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].session_id, "new");
        let old = load_record(dir.path(), "kimi-code", "old").expect("frozen");
        assert_eq!(old.items[0].key, "old");
    }

    #[test]
    fn windows_path_rules_canonicalize_both_sides() {
        let dir = project();
        let unix = dir.path().to_string_lossy().replace('\\', "/");
        let key_a = path_identity::equivalent_project_key(Path::new(&unix));
        let key_b = path_identity::equivalent_project_key(dir.path());
        assert_eq!(key_a, key_b);
        let folded = path_identity::normalize_host_path(r"D:\siderai\skillsAgent\stateroot");
        let wsl = path_identity::normalize_host_path("/mnt/d/siderai/skillsAgent/stateroot");
        assert_eq!(folded, wsl, "{folded} vs {wsl}");
    }

    #[test]
    fn cursor_plan_frontmatter_completes_via_plan_federation() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        let session = home.path().join(".cursor/projects/ws/agent-transcripts/s1");
        std::fs::create_dir_all(&session).expect("mkdir");
        let cwd = project.path().display().to_string().replace('\\', "/");
        std::fs::write(
            session.join("s1.jsonl"),
            format!(
                "{{\"role\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"name\":\"Read\",\"input\":{{\"path\":\"{cwd}/src/lib.rs\"}}}}]}}}}"
            ),
        )
        .expect("transcript");
        let plans = home.path().join(".cursor/plans");
        std::fs::create_dir_all(&plans).expect("plans");
        std::fs::write(plans.join("auto_abcd1234.plan.md"), FRONTMATTER).expect("plan");

        let report = crate::plan_federation::sync_from(home.path(), project.path(), "cursor");
        assert_eq!(report.completed.len(), 1, "{report:?}");
        let (meta, _) = crate::plans::list(project.path())
            .into_iter()
            .find(|m| m.title == "cli-autoinstall")
            .map(|m| crate::plans::load(project.path(), &m.id).expect("load"))
            .expect("plan");
        assert_eq!(meta.status(), PlanStatus::Done);
        let (done, total) = plan_todo_progress(project.path(), &meta.id).expect("progress");
        assert_eq!((done, total), (4, 4));
    }

    #[test]
    fn kimi_wire_tail_replace_and_codex_update_plan() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        let session = home.path().join(".kimi-code/sessions/wd/session-abc");
        std::fs::create_dir_all(session.join("agents/main")).expect("mkdir");
        std::fs::write(
            session.join("state.json"),
            format!(
                r#"{{"cwd":{}}}"#,
                serde_json::to_string(&crate::transcripts::path_for_json(project.path())).unwrap()
            ),
        )
        .expect("state");
        std::fs::write(
            session.join("agents/main/wire.jsonl"),
            r#"{"type":"context.append_loop_event","event":{"type":"tool.call","name":"TodoList","args":{"todos":[{"status":"pending","title":"one"},{"status":"in_progress","title":"two"}]}}}
{"type":"context.append_loop_event","event":{"type":"tool.call","name":"TodoList","args":{"todos":[{"status":"done","title":"one"}]}}}
"#,
        )
        .expect("wire");
        let report = sync_from(home.path(), project.path(), "kimi-code");
        assert_eq!(report.written.len(), 1, "{report:?}");
        let rec = load_record(project.path(), "kimi-code", "session-abc").expect("rec");
        assert_eq!(rec.items.len(), 1);
        assert_eq!(rec.items[0].status, "completed");

        let roll = home
            .path()
            .join(".codex/sessions/2026/08/31/rollout-x.jsonl");
        std::fs::create_dir_all(roll.parent().unwrap()).expect("codex dir");
        let cwd = crate::transcripts::path_for_json(project.path());
        std::fs::write(
            &roll,
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"sess-c\",\"cwd\":{cwd}}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call\",\"name\":\"update_plan\",\"arguments\":\"{{\\\"plan\\\":[{{\\\"step\\\":\\\"A\\\",\\\"status\\\":\\\"completed\\\"}}]}}\"}}}}\n",
                cwd = serde_json::to_string(&cwd).unwrap()
            ),
        )
        .expect("rollout");
        // harness_install paths may point at default home; write under both
        // common roots used by the helper.
        let report = sync_from(home.path(), project.path(), "codex");
        assert!(
            report.written.iter().any(|line| line.contains("codex")),
            "{report:?}"
        );
    }
}
