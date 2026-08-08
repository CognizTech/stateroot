//! Local learnings + goals readers (lifted from the monorepo's learnings/goal
//! commands — the server-backed learnings/goals commands are not part of M1;
//! only the local file readers resume needs are kept).

use std::path::Path;

use serde_json::Value;

/// Confidence floor for surfacing durable preferences in the digest.
pub const SURFACE_THRESHOLD: f64 = 0.4;

/// One learning (parsed from a category markdown file). All fields
/// are the on-disk contract; resume reads only some today.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Learning {
    /// Stable id.
    pub id: String,
    /// The statement.
    pub statement: String,
    /// Category (from the file name).
    pub category: String,
    /// Confidence 0..1.
    pub confidence: f64,
    /// `observed` | `inferred`.
    pub label: String,
    /// Source references (free-form, joined).
    pub sources: String,
    /// `user|workspace|project|domain|session_candidate` (legacy → project).
    pub scope: String,
    /// Lifecycle status (legacy → active).
    pub status: String,
}

/// Parse one canonical learnings markdown file. Malformed bullets are
/// skipped (never fatal — a hand-edited file must not break resume).
pub fn parse_learnings_md(text: &str, category: &str) -> Vec<Learning> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(learning) = parse_bullet(line, category) else {
            continue;
        };
        out.push(learning);
    }
    out
}

fn parse_bullet(line: &str, category: &str) -> Option<Learning> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("- **")?;
    let (statement, rest) = rest.split_once("**")?;
    let statement = statement.trim();
    if statement.is_empty() {
        return None;
    }
    let comment_start = rest.find("<!--")?;
    let comment_end = rest[comment_start..].find("-->")?;
    let comment = &rest[comment_start + 4..comment_start + comment_end];

    let mut id = String::new();
    let mut confidence = 0.0f64;
    let mut label = String::new();
    let mut sources = String::new();
    let mut scope = String::from("project");
    let mut status = String::from("active");
    for part in comment.split(';') {
        let Some((key, value)) = part.trim().split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "id" => id = value.to_string(),
            "confidence" => confidence = value.parse().unwrap_or(0.0),
            "label" => label = value.to_string(),
            "sources" => sources = value.to_string(),
            "scope" if !value.is_empty() => scope = value.to_string(),
            "status" if !value.is_empty() => status = value.to_string(),
            _ => {}
        }
    }
    if id.is_empty() {
        return None;
    }
    Some(Learning {
        id,
        statement: statement.to_string(),
        category: category.to_string(),
        confidence,
        label,
        sources,
        scope,
        status,
    })
}

/// Read all local learnings from `.stateroot/learnings/*.md` (any `*.md`
/// file's stem becomes its category). Also reads `_candidates/`.
pub fn read_local_learnings(project_dir: &Path) -> Vec<Learning> {
    let dir = stateroot_core::local_store::root(project_dir).join("learnings");
    let mut out = read_learnings_dir(&dir, None);
    out.extend(read_learnings_dir(
        &dir.join("_candidates"),
        Some("candidate"),
    ));
    out.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

fn read_learnings_dir(dir: &Path, status_override: Option<&str>) -> Vec<Learning> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let category = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("general")
            .to_string();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut learnings = parse_learnings_md(&text, &category);
        if let Some(status) = status_override {
            for learning in &mut learnings {
                learning.status = status.to_string();
            }
        }
        out.extend(learnings);
    }
    out
}

/// Read local goal docs from `.stateroot/goals/*.json`.
pub fn read_local_goals(project_dir: &std::path::Path) -> Vec<Value> {
    let dir = stateroot_core::local_store::root(project_dir).join("goals");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(goal) =
            serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap_or_default())
        {
            out.push(goal);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn read_local_goals_finds_active() {
        let project = tempfile::tempdir().expect("p");
        let dir = project.path().join(".stateroot/goals");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("g1.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "id": "g1", "lifecycle": "active", "objective": "x"
            }))
            .unwrap(),
        )
        .unwrap();
        let goals = super::read_local_goals(project.path());
        assert_eq!(goals.len(), 1, "goals: {goals:?}");
        let active = goals
            .into_iter()
            .find(|g| g.get("lifecycle").and_then(|v| v.as_str()) == Some("active"));
        assert!(active.is_some());
    }
}
