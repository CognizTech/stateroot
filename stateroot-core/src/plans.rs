//! Central plan artifacts + lifecycle — the store behind `stateroot plan`.
//!
//! StateRoot owns the plan ARTIFACT and its lifecycle (strings above the
//! runtime); each harness keeps its own plan mode. A plan is full-fidelity
//! markdown on disk (`.stateroot/plans/<id>.md`) plus a `stateroot.plan.v1`
//! sidecar (`.stateroot/plans/<id>.json`); the digest carries only a pointer
//! plus a directive, never the body (token razor).
//!
//! Lifecycle: `draft → approved → active → done`, `abandoned` from any
//! non-terminal state. At most ONE plan is active: activating demotes the
//! currently active plan to `approved` (recorded in its notes — never
//! silent). Wrong-state transitions are clear errors. Transition writes bump
//! `updated_at` and re-stamp `root_ref` from `refs/stateroot/latest`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::local_store::{self, now_rfc3339};

/// Schema tag on the sidecar.
pub const SCHEMA_PLAN_V1: &str = "stateroot.plan.v1";
/// Plans dir, relative to `.stateroot/`.
pub const PLANS_REL: &str = "plans";

/// Plan lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    /// Being authored — planner directive in the digest.
    Draft,
    /// Approved by the user/delegator — executor directive in the digest.
    Approved,
    /// The one plan being executed (at most one).
    Active,
    /// Finished (terminal).
    Done,
    /// Dropped from any non-terminal state (terminal).
    Abandoned,
}

impl PlanStatus {
    /// Stable label used in sidecars and output.
    pub fn as_str(&self) -> &'static str {
        match self {
            PlanStatus::Draft => "draft",
            PlanStatus::Approved => "approved",
            PlanStatus::Active => "active",
            PlanStatus::Done => "done",
            PlanStatus::Abandoned => "abandoned",
        }
    }

    /// Parse a sidecar status string.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "draft" => Some(PlanStatus::Draft),
            "approved" => Some(PlanStatus::Approved),
            "active" => Some(PlanStatus::Active),
            "done" => Some(PlanStatus::Done),
            "abandoned" => Some(PlanStatus::Abandoned),
            _ => None,
        }
    }

    fn terminal(&self) -> bool {
        matches!(self, PlanStatus::Done | PlanStatus::Abandoned)
    }

    /// The legal move `self → to` per the lifecycle (abandoned is reachable
    /// from any non-terminal state; same-state is NOT a legal no-op).
    fn can_transition_to(self, to: PlanStatus) -> bool {
        use PlanStatus::*;
        if self.terminal() || self == to {
            return false;
        }
        matches!(
            (self, to),
            (Draft, Approved)
                | (Approved, Active)
                | (Active, Done)
                | (Approved, Done)
                | (_, Abandoned)
        )
    }
}

/// The `stateroot.plan.v1` sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanMeta {
    pub schema_version: String,
    pub id: String,
    pub title: String,
    pub status: String,
    pub created_by_harness: String,
    pub created_at: String,
    pub updated_at: String,
    /// Provenance: `refs/stateroot/latest` at record/transition time.
    pub root_ref: Option<String>,
    /// The original file the plan was recorded from, when any.
    pub source_path: Option<String>,
    /// Lifecycle notes (demotions are recorded here).
    pub notes: String,
}

impl PlanMeta {
    /// Typed status (unknown strings are treated as draft — honest default
    /// for a hand-edited sidecar).
    pub fn status(&self) -> PlanStatus {
        PlanStatus::parse(&self.status).unwrap_or(PlanStatus::Draft)
    }
}

/// The plans directory for one project.
pub fn plans_dir(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(PLANS_REL)
}

/// A plan's markdown path.
pub fn body_path(project_dir: &Path, id: &str) -> PathBuf {
    plans_dir(project_dir).join(format!("{id}.md"))
}

fn meta_path(project_dir: &Path, id: &str) -> PathBuf {
    plans_dir(project_dir).join(format!("{id}.json"))
}

/// `plan_<ts>_<slug>` — timestamp compacted like handoff history names.
fn plan_id(title: &str) -> String {
    let ts = now_rfc3339().replace([':', '.'], "-");
    let mut slug = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').chars().take(48).collect::<String>();
    let slug = if slug.is_empty() { "plan".into() } else { slug };
    format!("plan_{ts}_{slug}")
}

/// The latest root hash for provenance (empty/absent → None).
fn latest_root_ref(project_dir: &Path) -> Option<String> {
    crate::roots::latest_root(project_dir)
        .ok()
        .flatten()
        .filter(|r| !r.is_empty())
}

fn write_meta(project_dir: &Path, meta: &PlanMeta) -> Result<(), String> {
    let path = meta_path(project_dir, &meta.id);
    let text = serde_json::to_string_pretty(meta).map_err(|e| e.to_string())?;
    std::fs::write(&path, format!("{text}\n")).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Record a plan body as a new DRAFT (verbatim markdown + sidecar).
pub fn record(
    project_dir: &Path,
    title: &str,
    harness: &str,
    source_path: Option<&str>,
    body: &str,
) -> Result<PlanMeta, String> {
    if body.trim().is_empty() {
        return Err("plan body is empty — nothing to record".into());
    }
    let title = title.trim();
    if title.is_empty() {
        return Err("plan title is empty — pass --title or start the body with a heading".into());
    }
    std::fs::create_dir_all(plans_dir(project_dir))
        .map_err(|e| format!("create plans dir: {e}"))?;
    let mut id = plan_id(title);
    // Same title in the same second must never silently overwrite.
    for n in 2.. {
        if !meta_path(project_dir, &id).exists() {
            break;
        }
        id = format!("{}-{n}", plan_id(title));
    }
    let now = now_rfc3339();
    let meta = PlanMeta {
        schema_version: SCHEMA_PLAN_V1.into(),
        id: id.clone(),
        title: title.to_string(),
        status: PlanStatus::Draft.as_str().into(),
        created_by_harness: harness.to_string(),
        created_at: now.clone(),
        updated_at: now,
        root_ref: latest_root_ref(project_dir),
        source_path: source_path.map(str::to_string),
        notes: String::new(),
    };
    std::fs::write(body_path(project_dir, &id), body)
        .map_err(|e| format!("write plan body: {e}"))?;
    write_meta(project_dir, &meta)?;
    Ok(meta)
}

/// Every plan sidecar, oldest first.
pub fn list(project_dir: &Path) -> Vec<PlanMeta> {
    let Ok(entries) = std::fs::read_dir(plans_dir(project_dir)) else {
        return Vec::new();
    };
    let mut out: Vec<PlanMeta> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok())
        .filter(|m: &PlanMeta| m.schema_version == SCHEMA_PLAN_V1)
        .collect();
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    out
}

/// Load one plan by id (exact, or a unique prefix) — meta + markdown path.
pub fn load(project_dir: &Path, id: &str) -> Option<(PlanMeta, PathBuf)> {
    let all = list(project_dir);
    if let Some(meta) = all.iter().find(|m| m.id == id) {
        return Some((meta.clone(), body_path(project_dir, &meta.id)));
    }
    let matches: Vec<&PlanMeta> = all.iter().filter(|m| m.id.starts_with(id)).collect();
    if matches.len() == 1 {
        let meta = matches[0];
        return Some((meta.clone(), body_path(project_dir, &meta.id)));
    }
    None
}

/// The currently ACTIVE plan, when any.
pub fn active(project_dir: &Path) -> Option<(PlanMeta, PathBuf)> {
    list(project_dir)
        .into_iter()
        .find(|m| m.status() == PlanStatus::Active)
        .map(|m| {
            let path = body_path(project_dir, &m.id);
            (m, path)
        })
}

/// The active-or-approved plan (the executor-directive tier; handoff
/// `plan_ref` attaches exactly this).
pub fn active_or_approved(project_dir: &Path) -> Option<(PlanMeta, PathBuf)> {
    let all = list(project_dir);
    let pick = all
        .iter()
        .find(|m| m.status() == PlanStatus::Active)
        .or_else(|| {
            // Several approved plans: the most recently updated one leads.
            all.iter()
                .filter(|m| m.status() == PlanStatus::Approved)
                .max_by(|a, b| a.updated_at.cmp(&b.updated_at).then(a.id.cmp(&b.id)))
        })?;
    Some((pick.clone(), body_path(project_dir, &pick.id)))
}

/// The plan the digest points at: active, else newest approved, else the
/// newest draft (planner directive). Terminal plans never surface.
pub fn current(project_dir: &Path) -> Option<(PlanMeta, PathBuf)> {
    if let Some(found) = active_or_approved(project_dir) {
        return Some(found);
    }
    let all = list(project_dir);
    let draft = all
        .iter()
        .filter(|m| m.status() == PlanStatus::Draft)
        .max_by(|a, b| a.updated_at.cmp(&b.updated_at).then(a.id.cmp(&b.id)))?;
    Some((draft.clone(), body_path(project_dir, &draft.id)))
}

/// Update a plan's body while it is still a draft (plan federation refresh:
/// the native file changed under us). Refuses past draft — once a human or
/// harness approves, the plan is owned, not synced. Returns the meta.
pub fn update_draft_body(
    project_dir: &Path,
    id: &str,
    body: &str,
    note: &str,
) -> Result<PlanMeta, String> {
    let Some((mut meta, path)) = load(project_dir, id) else {
        return Err(format!("unknown plan `{id}`"));
    };
    if meta.status() != PlanStatus::Draft {
        return Err(format!(
            "plan {} is {} — not syncing native edits past draft",
            meta.id, meta.status
        ));
    }
    if body.trim().is_empty() {
        return Err("plan body is empty — refusing to blank a draft".into());
    }
    std::fs::write(&path, body).map_err(|e| format!("write plan body: {e}"))?;
    meta.updated_at = now_rfc3339();
    if !note.is_empty() {
        meta.notes = note.to_string();
    }
    write_meta(project_dir, &meta)?;
    Ok(meta)
}

/// Move one plan to a new status. Returns the updated meta plus the id of a
/// plan that was demoted by this transition (activate demoting the previous
/// active). Wrong-state transitions are clear errors.
pub fn transition(
    project_dir: &Path,
    id: &str,
    to: PlanStatus,
) -> Result<(PlanMeta, Option<String>), String> {
    let Some((mut meta, _)) = load(project_dir, id) else {
        return Err(format!("unknown plan `{id}` — run `stateroot plan list`"));
    };
    let from = meta.status();
    if !from.can_transition_to(to) {
        return Err(format!(
            "plan {} is {} — cannot move to {} (lifecycle: draft → approved → active → done; approved → done for plans finished while approved; abandoned from any open state)",
            meta.id,
            from.as_str(),
            to.as_str()
        ));
    }
    let mut demoted = None;
    if to == PlanStatus::Active {
        if let Some((current, _)) = active(project_dir) {
            if current.id != meta.id {
                // One active at most: the previous active drops to approved,
                // recorded in its own notes — never silent.
                let mut previous = current;
                previous.status = PlanStatus::Approved.as_str().into();
                previous.updated_at = now_rfc3339();
                previous.root_ref = latest_root_ref(project_dir);
                previous.notes = format!(
                    "{}\ndemoted to approved when {} was activated",
                    previous.notes.trim_end(),
                    meta.id
                )
                .trim()
                .to_string();
                write_meta(project_dir, &previous)?;
                demoted = Some(previous.id);
            }
        }
    }
    meta.status = to.as_str().into();
    meta.updated_at = now_rfc3339();
    meta.root_ref = latest_root_ref(project_dir);
    write_meta(project_dir, &meta)?;
    Ok((meta, demoted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_plan(dir: &Path, title: &str) -> PlanMeta {
        record(
            dir,
            title,
            "claude",
            None,
            &format!("# {title}\n\nDo the thing.\n"),
        )
        .expect("record")
    }

    #[test]
    fn lifecycle_walk_and_wrong_state_errors() {
        let dir = tempfile::tempdir().expect("dir");
        let plan = record_plan(dir.path(), "Ship the parser");
        assert!(plan.id.starts_with("plan_"));
        assert_eq!(plan.status(), PlanStatus::Draft);
        assert_eq!(plan.created_by_harness, "claude");
        assert!(body_path(dir.path(), &plan.id).is_file());

        // draft → done is illegal; approve first.
        let err = transition(dir.path(), &plan.id, PlanStatus::Done).unwrap_err();
        assert!(err.contains("cannot move to done"), "{err}");
        // Same-state is a clear error too, not a silent no-op.
        let err = transition(dir.path(), &plan.id, PlanStatus::Draft).unwrap_err();
        assert!(err.contains("cannot move to draft"), "{err}");

        let (plan, demoted) =
            transition(dir.path(), &plan.id, PlanStatus::Approved).expect("approve");
        assert_eq!(plan.status(), PlanStatus::Approved);
        assert!(demoted.is_none());
        let (plan, _) = transition(dir.path(), &plan.id, PlanStatus::Active).expect("activate");
        assert_eq!(plan.status(), PlanStatus::Active);
        // approved → done is legal: a plan finished while approved completes
        // without a pointless activate-then-done dance.
        let second = record_plan(dir.path(), "Second plan");
        transition(dir.path(), &second.id, PlanStatus::Approved).expect("approve 2");
        let (second, _) = transition(dir.path(), &second.id, PlanStatus::Done).expect("done 2");
        assert_eq!(second.status(), PlanStatus::Done);
        let (plan, _) = transition(dir.path(), &plan.id, PlanStatus::Done).expect("done");
        assert_eq!(plan.status(), PlanStatus::Done);
        // Terminal states reject everything, including abandon.
        let err = transition(dir.path(), &plan.id, PlanStatus::Abandoned).unwrap_err();
        assert!(err.contains("cannot move to abandoned"), "{err}");

        // Unknown id errors.
        assert!(transition(dir.path(), "nope", PlanStatus::Approved).is_err());
    }

    #[test]
    fn at_most_one_active_with_recorded_demotion() {
        let dir = tempfile::tempdir().expect("dir");
        let first = record_plan(dir.path(), "First");
        let second = record_plan(dir.path(), "Second");
        for plan in [&first, &second] {
            transition(dir.path(), &plan.id, PlanStatus::Approved).expect("approve");
        }
        transition(dir.path(), &first.id, PlanStatus::Active).expect("activate 1");
        let (_, demoted) =
            transition(dir.path(), &second.id, PlanStatus::Active).expect("activate 2");
        assert_eq!(demoted.as_deref(), Some(first.id.as_str()));

        let (first_meta, _) = load(dir.path(), &first.id).expect("load first");
        assert_eq!(first_meta.status(), PlanStatus::Approved);
        assert!(
            first_meta.notes.contains("demoted to approved"),
            "notes: {}",
            first_meta.notes
        );
        let (current, _) = active(dir.path()).expect("an active plan");
        assert_eq!(current.id, second.id);

        // Prefix load resolves uniquely.
        let prefix = &second.id[..second.id.len() - 2];
        assert!(load(dir.path(), prefix).is_some());
    }

    #[test]
    fn current_prefers_active_then_approved_then_draft() {
        let dir = tempfile::tempdir().expect("dir");
        assert!(active(dir.path()).is_none());
        assert!(current(dir.path()).is_none());

        let draft = record_plan(dir.path(), "Only a draft");
        let (current_meta, _) = current(dir.path()).expect("draft surfaces");
        assert_eq!(current_meta.id, draft.id);
        assert!(active_or_approved(dir.path()).is_none());

        transition(dir.path(), &draft.id, PlanStatus::Approved).expect("approve");
        let (meta, _) = active_or_approved(dir.path()).expect("approved surfaces");
        assert_eq!(meta.status(), PlanStatus::Approved);

        // Done/abandoned never surface.
        transition(dir.path(), &draft.id, PlanStatus::Active).expect("activate");
        transition(dir.path(), &draft.id, PlanStatus::Done).expect("done");
        assert!(current(dir.path()).is_none());
        let other = record_plan(dir.path(), "Abandoned one");
        transition(dir.path(), &other.id, PlanStatus::Abandoned).expect("abandon");
        assert!(current(dir.path()).is_none());
    }

    #[test]
    fn record_rejects_empty_body_and_title() {
        let dir = tempfile::tempdir().expect("dir");
        assert!(record(dir.path(), "T", "cli", None, "  \n").is_err());
        assert!(record(dir.path(), "  ", "cli", None, "# T\n").is_err());
    }
}
