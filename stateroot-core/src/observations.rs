//! Read-only access to `.stateroot/spool/observations.jsonl` — audit/provenance only.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::local_store;

/// One spool observation with a stable line-based id.
#[derive(Debug, Clone)]
pub struct Observation {
    /// Stable id (`obs_<line>`).
    pub id: String,
    /// 1-based line number in the spool file.
    pub line_no: usize,
    /// RFC3339 timestamp when present.
    pub ts: String,
    /// Canonical hook event name.
    pub event: String,
    /// Harness id.
    pub harness: String,
    /// Captured text body.
    pub text: String,
    /// Optional kind hint from the hook.
    pub kind_hint: Option<String>,
    /// Optional tool name from harness payload.
    pub tool: Option<String>,
    /// Optional excerpt from harness payload.
    pub excerpt: Option<String>,
    /// `foreign` when evidence suggests another initialized project/worktree.
    pub scope_status: Option<String>,
}

/// Filter options for listing/searching observations.
#[derive(Debug, Clone, Default)]
pub struct ObservationFilter {
    /// Match `kind_hint` or `event` (case-insensitive substring).
    pub kind: Option<String>,
    /// Match harness id (case-insensitive exact).
    pub harness: Option<String>,
    /// Match RFC3339 prefix (inclusive lower bound).
    pub since: Option<String>,
    /// Match RFC3339 prefix (inclusive upper bound).
    pub until: Option<String>,
    /// Case-insensitive substring over text/excerpt/tool fields.
    pub query: Option<String>,
    /// Maximum rows (0 = unlimited).
    pub limit: usize,
}

fn spool_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join("spool/observations.jsonl")
}

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn opt_str(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn foreign_scope_status(project_dir: &Path, text: &str) -> Option<String> {
    let current = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());
    for token in text.split_whitespace() {
        if !token.contains(".stateroot") {
            continue;
        }
        let path = token.trim_matches(|c: char| "{}[],'\"".contains(c));
        if path.is_empty() {
            continue;
        }
        let candidate = PathBuf::from(path);
        let root = if candidate.file_name().and_then(|n| n.to_str()) == Some(".stateroot") {
            candidate.parent().map(Path::to_path_buf)
        } else if candidate.ends_with(".stateroot/manifest.json") {
            candidate
                .parent()
                .and_then(|p| p.parent())
                .map(Path::to_path_buf)
        } else {
            None
        };
        let Some(root) = root else {
            continue;
        };
        let root = root.canonicalize().unwrap_or(root);
        if root != current && local_store::is_stateroot_dir(&root) {
            return Some("foreign".into());
        }
    }
    if text.contains("possible_project_mismatch") {
        return Some("possible_project_mismatch".into());
    }
    None
}

fn parse_line(project_dir: &Path, line_no: usize, line: &str) -> Option<Observation> {
    let value: Value = serde_json::from_str(line).ok()?;
    let text = str_field(&value, "text");
    let scope_status = foreign_scope_status(project_dir, &text);
    Some(Observation {
        id: format!("obs_{line_no}"),
        line_no,
        ts: str_field(&value, "ts"),
        event: str_field(&value, "event"),
        harness: str_field(&value, "harness"),
        text,
        kind_hint: opt_str(&value, "kind_hint"),
        tool: opt_str(&value, "tool"),
        excerpt: opt_str(&value, "excerpt"),
        scope_status,
    })
}

/// Load all observations from the project spool (empty vec when missing).
pub fn load_spool(project_dir: &Path) -> Vec<Observation> {
    let path = spool_path(project_dir);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            parse_line(project_dir, idx + 1, trimmed)
        })
        .collect()
}

fn matches_filter(obs: &Observation, filter: &ObservationFilter) -> bool {
    if let Some(kind) = filter.kind.as_deref() {
        let kind_lower = kind.to_ascii_lowercase();
        let event_match = obs.event.to_ascii_lowercase().contains(&kind_lower);
        let hint_match = obs
            .kind_hint
            .as_deref()
            .map(|h| h.to_ascii_lowercase().contains(&kind_lower))
            .unwrap_or(false);
        if !event_match && !hint_match {
            return false;
        }
    }
    if let Some(harness) = filter.harness.as_deref() {
        if !obs.harness.eq_ignore_ascii_case(harness) {
            return false;
        }
    }
    if let Some(since) = filter.since.as_deref() {
        if obs.ts.as_str() < since {
            return false;
        }
    }
    if let Some(until) = filter.until.as_deref() {
        if !obs.ts.is_empty() && obs.ts.as_str() > until {
            return false;
        }
    }
    if let Some(query) = filter.query.as_deref() {
        let q = query.to_ascii_lowercase();
        let hay = format!(
            "{} {} {} {}",
            obs.text,
            obs.excerpt.as_deref().unwrap_or(""),
            obs.tool.as_deref().unwrap_or(""),
            obs.event
        )
        .to_ascii_lowercase();
        if !hay.contains(&q) {
            return false;
        }
    }
    true
}

/// Filter observations from the spool.
pub fn filter_spool(project_dir: &Path, filter: &ObservationFilter) -> Vec<Observation> {
    let mut rows: Vec<Observation> = load_spool(project_dir)
        .into_iter()
        .filter(|obs| matches_filter(obs, filter))
        .collect();
    if filter.limit > 0 && rows.len() > filter.limit {
        rows.truncate(filter.limit);
    }
    rows
}

/// Find one observation by id (`obs_<line>` or line number).
pub fn get_observation(project_dir: &Path, id: &str) -> Option<Observation> {
    let line_no = id
        .strip_prefix("obs_")
        .and_then(|n| n.parse::<usize>().ok())?;
    load_spool(project_dir)
        .into_iter()
        .find(|obs| obs.line_no == line_no)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn load_and_filter_spool_rows() {
        let dir = tempfile::tempdir().expect("tmpdir");
        local_store::init_skeleton(dir.path(), "p1", "demo", "default").unwrap();
        let spool = spool_path(dir.path());
        std::fs::create_dir_all(spool.parent().unwrap()).unwrap();
        std::fs::write(
            &spool,
            format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "ts": "2026-08-17T10:00:00Z",
                    "event": "user_prompt_submit",
                    "harness": "cursor",
                    "text": "fix the importer",
                    "kind_hint": "correction",
                }))
                .unwrap()
            ),
        )
        .unwrap();
        let rows = filter_spool(
            dir.path(),
            &ObservationFilter {
                kind: Some("correction".into()),
                harness: Some("cursor".into()),
                limit: 10,
                ..ObservationFilter::default()
            },
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "obs_1");
        assert!(get_observation(dir.path(), "obs_1").is_some());
    }
}
