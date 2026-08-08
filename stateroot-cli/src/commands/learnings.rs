//! `stateroot learnings` — list/accept/reject/edit project learnings, plus
//! the shared parser for the canonical `.stateroot/learnings/*.md` files
//! (synced from the server into the local project tree).
//!
//! Canonical doc format (one bullet per learning, metadata in an HTML
//! comment):
//! `- **<statement>** … <!-- id: …; confidence: 0.75; label: observed; sources: …; scope: project; status: active -->`

use std::path::Path;

use serde_json::Value;

use super::{ensure_auth, note, truncate, Ctx};

/// Confidence threshold for default surfacing (below it needs `--all`).
pub const SURFACE_THRESHOLD: f64 = 0.4;

/// One learning (parsed or server-shaped).
#[derive(Debug, Clone)]
pub struct Learning {
    /// Stable id.
    pub id: String,
    /// The statement.
    pub statement: String,
    /// Category (from the file name or server field).
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
        for mut learning in parse_learnings_md(&text, &category) {
            if let Some(status) = status_override {
                if learning.status == "active" && dir.ends_with("_candidates") {
                    learning.status = status.to_string();
                }
            }
            out.push(learning);
        }
    }
    out
}

/// Render one grouped list of learnings (shared by `list` and resume).
pub fn render_grouped(learnings: &[Learning], show_below_threshold: bool) -> String {
    let mut out = String::new();
    let mut categories: Vec<&str> = learnings.iter().map(|l| l.category.as_str()).collect();
    categories.sort();
    categories.dedup();
    for category in categories {
        let rows: Vec<&Learning> = learnings
            .iter()
            .filter(|l| l.category == category)
            .collect();
        let mut section = String::new();
        for learning in rows {
            let below = learning.confidence < SURFACE_THRESHOLD;
            if below && !show_below_threshold {
                continue;
            }
            let marker = if below { " (below threshold)" } else { "" };
            let lifecycle = if learning.status == "active" {
                String::new()
            } else {
                format!(", {}", learning.status)
            };
            section.push_str(&format!(
                "- {} ({:.2}, {}{}{})\n",
                truncate(&learning.statement, 200),
                learning.confidence,
                learning.label,
                lifecycle,
                marker
            ));
        }
        if !section.is_empty() {
            out.push_str(&format!("### {category}\n\n{section}\n"));
        }
    }
    out
}

/// `stateroot learnings list [--all] [--scope] [--status]`.
pub async fn list(
    ctx: &Ctx,
    all: bool,
    scope: Option<&str>,
    status: Option<&str>,
) -> anyhow::Result<()> {
    let project = ctx.require_project()?;
    let cred = ctx.try_credential().await?;
    if let Some(token) = cred {
        let client = ctx.stateroot_client(Some(token))?;
        match client
            .list_learnings(&project.project_id, scope, status)
            .await
        {
            Ok(rows) => {
                let learnings: Vec<Learning> = rows.iter().map(learning_from_row).collect();
                let rendered = render_grouped(&learnings, all);
                if rendered.is_empty() {
                    println!("no learnings above the surface threshold ({SURFACE_THRESHOLD})");
                    if !all {
                        println!("(try --all to see below-threshold learnings)");
                    }
                } else {
                    print!("{rendered}");
                }
                return Ok(());
            }
            Err(err) => note!("warning: server learnings unavailable ({err}); using local copy"),
        }
    }
    // Offline fallback: the synced local files.
    let mut learnings = read_local_learnings(&ctx.cwd);
    if let Some(scope) = scope {
        learnings.retain(|l| l.scope == scope);
    }
    if let Some(status) = status {
        learnings.retain(|l| l.status == status);
    }
    let rendered = render_grouped(&learnings, all);
    if rendered.is_empty() {
        println!("no learnings found (local copy)");
    } else {
        println!("(local copy)");
        print!("{rendered}");
    }
    Ok(())
}

/// `stateroot learnings accept|reject|edit` — feedback on one learning.
pub async fn feedback(
    ctx: &Ctx,
    learning_id: &str,
    action: &str,
    edit_text: Option<&str>,
) -> anyhow::Result<()> {
    let project = ctx.require_project()?;
    let cred = ensure_auth(ctx).await?;
    let client = ctx.stateroot_client(Some(cred))?;
    let old_confidence = client
        .list_learnings(&project.project_id, None, None)
        .await
        .ok()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("id").and_then(|v| v.as_str()) == Some(learning_id))
                .and_then(|row| row.get("confidence").and_then(|v| v.as_f64()))
        });
    let updated = client
        .learning_feedback(&project.project_id, learning_id, action, edit_text)
        .await?;
    let statement = updated
        .get("statement")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let new_confidence = updated
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    match old_confidence {
        Some(old) => println!(
            "{action}: {} ({old:.2} → {new_confidence:.2})",
            truncate(statement, 120)
        ),
        None => println!(
            "{action}: {} (confidence {new_confidence:.2})",
            truncate(statement, 120)
        ),
    }
    Ok(())
}

/// `stateroot learnings propose <id> [--scope] [--rationale]`.
pub async fn propose(
    ctx: &Ctx,
    learning_id: &str,
    scope: Option<&str>,
    rationale: Option<&str>,
) -> anyhow::Result<()> {
    let project = ctx.require_project()?;
    let cred = ensure_auth(ctx).await?;
    let client = ctx.stateroot_client(Some(cred))?;
    let result = client
        .propose_learning(&project.project_id, learning_id, scope, rationale)
        .await?;
    let proposal_id = result
        .get("proposal")
        .and_then(|p| p.get("proposal_id").or_else(|| p.get("id")))
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    println!("proposed learning {learning_id} → proposal {proposal_id} (approve to activate)");
    Ok(())
}

fn learning_from_row(row: &Value) -> Learning {
    Learning {
        id: row
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        statement: row
            .get("statement")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        category: row
            .get("category")
            .and_then(|v| v.as_str())
            .unwrap_or("general")
            .to_string(),
        confidence: row
            .get("confidence")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        label: row
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("observed")
            .to_string(),
        sources: row
            .get("sources")
            .map(|v| match v {
                Value::Array(arr) => arr
                    .iter()
                    .filter_map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                other => other.to_string(),
            })
            .unwrap_or_default(),
        scope: row
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("project")
            .to_string(),
        status: row
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("active")
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_reads_valid_bullets_and_skips_malformed() {
        let text = r#"# Workflow

- **Commit early, commit often** evidence: 4 observation(s) <!-- id: lrn-1; confidence: 0.75; label: observed; sources: codex:s-1, claude:s-2 -->
- **Prefer rebase over merge** evidence: 2 observation(s) <!-- id: lrn-2; confidence: 0.35; label: inferred; sources: codex:s-1 -->
- **unterminated comment** evidence: 1 observation(s) <!-- id: lrn-3; confidence: 0.9; label: observed
- not a bullet at all
- **missing id** <!-- confidence: 0.5; label: observed -->
"#;
        let learnings = parse_learnings_md(text, "workflow");
        assert_eq!(learnings.len(), 2, "learnings: {learnings:?}");
        assert_eq!(learnings[0].id, "lrn-1");
        assert_eq!(learnings[0].statement, "Commit early, commit often");
        assert_eq!(learnings[0].category, "workflow");
        assert!((learnings[0].confidence - 0.75).abs() < f64::EPSILON);
        assert_eq!(learnings[0].label, "observed");
        assert_eq!(learnings[0].sources, "codex:s-1, claude:s-2");
        assert_eq!(learnings[1].id, "lrn-2");
        assert!((learnings[1].confidence - 0.35).abs() < f64::EPSILON);
        assert_eq!(learnings[0].scope, "project");
        assert_eq!(learnings[0].status, "active");
    }

    #[test]
    fn parser_reads_w2_scope_status_and_defaults_legacy() {
        let text = r#"
- **Legacy one** <!-- id: legacy; confidence: 0.80; label: observed; sources: a -->
- **Scoped one** <!-- id: scoped; confidence: 0.70; label: observed; sources: b; scope: user; status: candidate -->
"#;
        let learnings = parse_learnings_md(text, "workflow");
        assert_eq!(learnings.len(), 2);
        assert_eq!(learnings[0].scope, "project");
        assert_eq!(learnings[0].status, "active");
        assert_eq!(learnings[1].scope, "user");
        assert_eq!(learnings[1].status, "candidate");
    }

    #[test]
    fn grouped_render_marks_below_threshold_only_with_all() {
        let learnings = vec![
            Learning {
                id: "a".into(),
                statement: "high one".into(),
                category: "tools".into(),
                confidence: 0.8,
                label: "observed".into(),
                sources: String::new(),
                scope: "project".into(),
                status: "active".into(),
            },
            Learning {
                id: "b".into(),
                statement: "low one".into(),
                category: "tools".into(),
                confidence: 0.2,
                label: "inferred".into(),
                sources: String::new(),
                scope: "project".into(),
                status: "active".into(),
            },
        ];
        let default = render_grouped(&learnings, false);
        assert!(default.contains("high one"));
        assert!(!default.contains("low one"));
        let all = render_grouped(&learnings, true);
        assert!(all.contains("low one"));
        assert!(all.contains("(below threshold)"), "all: {all}");
    }
}
