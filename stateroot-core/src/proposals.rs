//! Local proposals — the shared approval gate (M3).
//!
//! Every evolution of identity, learnings, skills, or memory passes through a
//! proposal file: `.stateroot/proposals/<id>.json`. Nothing activates until
//! `stateroot proposals approve` (or `learnings accept`, which is the user's
//! own approval). Rejection keeps the file for audit (append-only history).

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::local_store;

/// Proposals directory under `.stateroot/`.
pub const PROPOSALS_DIR: &str = "proposals";
/// Proposal schema id.
pub const PROPOSAL_SCHEMA: &str = "stateroot.proposal.local.v1";

/// Pending → approved|rejected.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Proposal {
    /// Schema id.
    #[serde(default)]
    pub schema_version: String,
    /// Proposal id (uuid v7 — time-ordered).
    #[serde(default)]
    pub id: String,
    /// `soul` | `learning` | `memory` | `skill`.
    #[serde(default)]
    pub kind: String,
    /// One-line title.
    #[serde(default)]
    pub title: String,
    /// Why this change (classification route, distiller evidence).
    #[serde(default)]
    pub rationale: String,
    /// Kind-specific payload (soul content, learning record, …).
    #[serde(default)]
    pub payload: Value,
    /// `pending` | `approved` | `rejected`.
    #[serde(default = "pending_str")]
    pub status: String,
    /// Creation timestamp.
    #[serde(default)]
    pub created_at: String,
    /// Decision timestamp.
    #[serde(default)]
    pub decided_at: String,
    /// Who decided (harness / "cli").
    #[serde(default)]
    pub decided_by: String,
    /// Activation note (what approve did, or why it is deferred).
    #[serde(default)]
    pub evidence: Value,
}

fn pending_str() -> String {
    "pending".into()
}

/// Errors from the proposals store.
#[derive(Debug, thiserror::Error)]
pub enum ProposalError {
    /// Local filesystem failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialization failure.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Named proposal does not exist.
    #[error("{0}")]
    NotFound(String),
    /// Proposal already decided (approve/reject is one-shot).
    #[error("proposal already {0}")]
    AlreadyDecided(String),
}

fn dir(project_dir: &Path) -> std::path::PathBuf {
    local_store::root(project_dir).join(PROPOSALS_DIR)
}

fn write(proposal: &Proposal, project_dir: &Path) -> Result<(), ProposalError> {
    let path = dir(project_dir).join(format!("{}.json", proposal.id));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let pretty = serde_json::to_string_pretty(proposal)?;
    std::fs::write(path, format!("{pretty}\n"))?;
    Ok(())
}

/// Create a pending proposal and persist it.
pub fn create(
    project_dir: &Path,
    kind: &str,
    title: &str,
    rationale: &str,
    payload: Value,
    evidence: Value,
) -> Result<Proposal, ProposalError> {
    let proposal = Proposal {
        schema_version: PROPOSAL_SCHEMA.into(),
        id: uuid::Uuid::now_v7().to_string(),
        kind: kind.into(),
        title: title.into(),
        rationale: rationale.into(),
        payload,
        status: "pending".into(),
        created_at: local_store::now_rfc3339(),
        evidence,
        ..Default::default()
    };
    write(&proposal, project_dir)?;
    Ok(proposal)
}

/// List proposals, newest first; optional status filter.
pub fn list(project_dir: &Path, status: Option<&str>) -> Result<Vec<Proposal>, ProposalError> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir(project_dir)) else {
        return Ok(out);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(proposal) = serde_json::from_str::<Proposal>(&text) {
            if status.map(|s| proposal.status == s).unwrap_or(true) {
                out.push(proposal);
            }
        }
    }
    out.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(out)
}

/// Load one proposal by id prefix.
pub fn get(project_dir: &Path, id_prefix: &str) -> Result<Proposal, ProposalError> {
    let all = list(project_dir, None)?;
    let matches: Vec<&Proposal> = all.iter().filter(|p| p.id.starts_with(id_prefix)).collect();
    match matches.len() {
        0 => Err(ProposalError::NotFound(format!(
            "no proposal matching '{id_prefix}'"
        ))),
        1 => Ok(matches[0].clone()),
        _ => Err(ProposalError::NotFound(format!(
            "ambiguous proposal prefix '{id_prefix}'"
        ))),
    }
}

/// Record a decision (one-shot). Returns the updated proposal.
pub fn decide(
    project_dir: &Path,
    id_prefix: &str,
    approve: bool,
    by: &str,
    edit: Option<Value>,
) -> Result<Proposal, ProposalError> {
    let mut proposal = get(project_dir, id_prefix)?;
    if proposal.status != "pending" {
        return Err(ProposalError::AlreadyDecided(proposal.status));
    }
    if let Some(edited) = edit {
        proposal.payload = edited;
    }
    proposal.status = if approve { "approved" } else { "rejected" }.into();
    proposal.decided_at = local_store::now_rfc3339();
    proposal.decided_by = by.into();
    write(&proposal, project_dir)?;
    Ok(proposal)
}

/// Activate an approved proposal (idempotent per kind). Returns a human note
/// of what happened. Unknown/deferred kinds record an honest note.
pub fn activate(project_dir: &Path, home: &Path, proposal: &Proposal) -> String {
    match proposal.kind.as_str() {
        "soul" => {
            let content = proposal
                .payload
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if content.trim().is_empty() {
                return "soul proposal had no content — nothing applied".into();
            }
            match crate::soul::write_canonical(home, content, Some(&proposal.title)) {
                Ok(note) => note,
                Err(err) => format!("soul apply failed: {err}"),
            }
        }
        "learning" => {
            let id = proposal
                .payload
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let scope = proposal
                .payload
                .get("scope")
                .and_then(|v| v.as_str())
                .unwrap_or("project");
            match crate::learnings::promote(project_dir, home, scope, id) {
                Ok(true) => format!("learning {id} promoted to active ({scope})"),
                Ok(false) => format!("learning {id} not found in candidates — nothing promoted"),
                Err(err) => format!("learning promote failed: {err}"),
            }
        }
        "memory" => {
            let content = proposal
                .payload
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let scope = proposal
                .payload
                .get("scope")
                .and_then(|v| v.as_str())
                .unwrap_or("project");
            match crate::learnings::append_memory_note(project_dir, home, scope, content) {
                Ok(path) => format!("memory note appended ({})", path.display()),
                Err(err) => format!("memory append failed: {err}"),
            }
        }
        _ => format!(
            "activation for kind '{}' lands in M4 — recorded as approved intent",
            proposal.kind
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn create_list_decide_one_shot() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join(".stateroot")).unwrap();
        let p = create(
            dir,
            "soul",
            "tighten tone",
            "distiller note",
            json!({"content": "x"}),
            json!({}),
        )
        .expect("create");
        assert_eq!(p.status, "pending");
        assert_eq!(list(dir, Some("pending")).unwrap().len(), 1);
        let decided = decide(dir, &p.id[..8], true, "cli", None).expect("approve");
        assert_eq!(decided.status, "approved");
        assert!(decide(dir, &p.id, true, "cli", None).is_err(), "one-shot");
        assert_eq!(list(dir, Some("pending")).unwrap().len(), 0);
        assert_eq!(list(dir, Some("approved")).unwrap().len(), 1);
    }
}
