//! Durable, federated obligations — explicit future work items shared across
//! harnesses (campaign reviews, plan-closure receipts, scheduled follow-ups).
//!
//! Storage (shared project store, synced):
//! - `.stateroot/obligations/<id>.json` — current definition/state, schema
//!   `stateroot.obligation.v1`.
//! - `.stateroot/obligations/events.jsonl` — append-only lifecycle journal,
//!   schema `stateroot.obligation-event.v1` (merge=union like every jsonl).
//!
//! Corrupt event lines are preserved and counted, never deleted: evidence
//! stores stay immutable. Ids are UUIDv7 (time-ordered); every mutation
//! carries an idempotent operation id so retries never double-apply.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::local_store::{self, now_rfc3339};
use crate::safe_io::{self, ResourceLock};

pub const OBLIGATIONS_REL: &str = "obligations";
pub const EVENTS_REL: &str = "obligations/events.jsonl";
pub const LOCK_REL: &str = "local/locks/obligations.lock";
pub const SCHEMA_OBLIGATION_V1: &str = "stateroot.obligation.v1";
pub const SCHEMA_OBLIGATION_EVENT_V1: &str = "stateroot.obligation-event.v1";

/// Lifecycle states. `Snoozed` with a lapsed `snoozed_until` is effectively
/// open again — snooze suppresses, it never cancels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObligationState {
    Open,
    Snoozed,
    Done,
    Cancelled,
}

impl ObligationState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Snoozed => "snoozed",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Self::Open),
            "snoozed" => Some(Self::Snoozed),
            "done" => Some(Self::Done),
            "cancelled" | "canceled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }
}

/// The `stateroot.obligation.v1` definition document (current state).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Obligation {
    pub schema_version: String,
    /// UUIDv7 — time-ordered unique id.
    pub id: String,
    /// What must happen, in one imperative sentence.
    pub task: String,
    /// RFC3339 UTC due instant (first release: one-shot due dates only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
    /// Harness the work is routed to (None = any harness).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assign: Option<String>,
    /// Plan this obligation is bound to, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    /// Idempotency key of the creating operation (retries reuse it).
    pub operation_id: String,
    pub state: String,
    pub created_at: String,
    pub created_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_by: Option<String>,
    /// Why this is done — the evidence bearing the completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_reason: Option<String>,
}

impl Obligation {
    pub fn state(&self) -> ObligationState {
        // Honest default for a hand-edited file: an unknown state is work
        // that still needs a decision, so it surfaces as open.
        ObligationState::parse(&self.state).unwrap_or(ObligationState::Open)
    }

    /// True when the obligation is actionable now: open, or snoozed with a
    /// lapsed snooze horizon. Timestamps are normalized UTC `Z` seconds, so
    /// lexicographic comparison is chronological.
    pub fn effective_open(&self, now: &str) -> bool {
        match self.state() {
            ObligationState::Open => true,
            ObligationState::Snoozed => match &self.snoozed_until {
                Some(until) => until.as_str() <= now,
                None => true,
            },
            _ => false,
        }
    }

    /// True when actionable and past its due instant.
    pub fn due(&self, now: &str) -> bool {
        self.effective_open(now) && self.due_at.as_deref().is_some_and(|d| d <= now)
    }
}

pub fn obligations_dir(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(OBLIGATIONS_REL)
}

fn definition_path(project_dir: &Path, id: &str) -> PathBuf {
    obligations_dir(project_dir).join(format!("{id}.json"))
}

fn events_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(EVENTS_REL)
}

fn lock_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(LOCK_REL)
}

fn report_path(project_dir: &Path, abs: &Path) {
    if let Ok(rel) = abs.strip_prefix(local_store::root(project_dir)) {
        local_store::report_written(project_dir, &rel.to_string_lossy());
    }
}

/// Normalize a user-supplied RFC3339 instant to UTC `Z` seconds so stored
/// timestamps compare lexicographically. Rejects unparseable input.
pub fn normalize_rfc3339(raw: &str) -> Result<String, String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(raw.trim())
        .map_err(|e| format!("invalid RFC3339 timestamp `{raw}`: {e}"))?;
    Ok(parsed
        .with_timezone(&chrono::Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Parse a relative duration like `30m`, `24h`, `7d`, `2w`, or compounds
/// (`1h30m`) into seconds. Rejects empty, zero, and unknown-unit input.
pub fn parse_duration_secs(raw: &str) -> Result<u64, String> {
    let text = raw.trim().to_ascii_lowercase();
    if text.is_empty() {
        return Err("duration is empty — use e.g. `30m`, `24h`, `7d`".into());
    }
    let mut total: u64 = 0;
    let mut digits = String::new();
    let mut parts = 0usize;
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        let unit = match ch {
            'm' => 60u64,
            'h' => 3_600,
            'd' => 86_400,
            'w' => 604_800,
            _ => {
                return Err(format!(
                    "unknown duration unit `{ch}` in `{raw}` (use m/h/d/w)"
                ))
            }
        };
        if digits.is_empty() {
            return Err(format!("missing number before `{ch}` in `{raw}`"));
        }
        let value: u64 = digits
            .parse()
            .map_err(|_| format!("duration number too large in `{raw}`"))?;
        total = total
            .checked_add(value.checked_mul(unit).ok_or("duration overflow")?)
            .ok_or("duration overflow")?;
        digits.clear();
        parts += 1;
    }
    if !digits.is_empty() {
        return Err(format!(
            "trailing number without a unit in `{raw}` (use m/h/d/w)"
        ));
    }
    if parts == 0 || total == 0 {
        return Err(format!("duration `{raw}` must be positive"));
    }
    Ok(total)
}

/// `now + duration` as a normalized RFC3339 UTC string.
pub fn in_duration(now: &str, secs: u64) -> Result<String, String> {
    let base = chrono::DateTime::parse_from_rfc3339(now)
        .map_err(|e| format!("invalid base timestamp: {e}"))?
        .with_timezone(&chrono::Utc);
    let shifted = base + chrono::Duration::seconds(secs as i64);
    Ok(shifted.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// One lifecycle event. The journal is the audit trail; the definition file
/// is the fold. Events dedupe by `operation_id` on read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObligationEvent {
    pub schema_version: String,
    pub id: String,
    pub obligation_id: String,
    /// created | snoozed | done | cancelled
    pub kind: String,
    pub at: String,
    pub actor: String,
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

fn append_event(project_dir: &Path, event: &ObligationEvent) -> Result<(), String> {
    let path = events_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create obligations dir: {e}"))?;
    }
    let mut line = serde_json::to_string(event).map_err(|e| e.to_string())?;
    line.push('\n');
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    file.write_all(line.as_bytes())
        .map_err(|e| format!("append {}: {e}", path.display()))?;
    report_path(project_dir, &path);
    Ok(())
}

/// Read the event journal. Corrupt lines are preserved on disk and counted —
/// never silently dropped from the report.
pub fn read_events(project_dir: &Path) -> (Vec<ObligationEvent>, usize) {
    let path = events_path(project_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (Vec::new(), 0);
    };
    let mut events = Vec::new();
    let mut corrupt = 0usize;
    let mut seen_ops = std::collections::HashSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<ObligationEvent>(line) {
            Ok(event) if event.schema_version == SCHEMA_OBLIGATION_EVENT_V1 => {
                if seen_ops.insert(event.operation_id.clone()) {
                    events.push(event);
                }
            }
            _ => corrupt += 1,
        }
    }
    (events, corrupt)
}

fn write_definition(project_dir: &Path, obligation: &Obligation) -> Result<(), String> {
    let path = definition_path(project_dir, &obligation.id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create obligations dir: {e}"))?;
    }
    let value = serde_json::to_value(obligation).map_err(|e| e.to_string())?;
    safe_io::atomic_replace_json(&path, &value)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    report_path(project_dir, &path);
    Ok(())
}

/// Every obligation definition, oldest first (creation order ≈ id order,
/// UUIDv7 is time-ordered). Unparseable files are skipped, never consumed.
pub fn list(project_dir: &Path) -> Vec<Obligation> {
    let Ok(entries) = std::fs::read_dir(obligations_dir(project_dir)) else {
        return Vec::new();
    };
    let mut out: Vec<Obligation> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok())
        .filter(|o: &Obligation| o.schema_version == SCHEMA_OBLIGATION_V1)
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Load one obligation by id (exact, or a unique prefix like plans).
pub fn load(project_dir: &Path, id: &str) -> Option<Obligation> {
    let all = list(project_dir);
    if let Some(found) = all.iter().find(|o| o.id == id) {
        return Some(found.clone());
    }
    let matches: Vec<&Obligation> = all.iter().filter(|o| o.id.starts_with(id)).collect();
    (matches.len() == 1).then(|| matches[0].clone())
}

/// Find an obligation by its idempotency operation id (e.g.
/// `plan-closure:<plan_id>` for closure obligations).
pub fn find_by_operation(project_dir: &Path, operation_id: &str) -> Option<Obligation> {
    list(project_dir)
        .into_iter()
        .find(|o| o.operation_id == operation_id)
}

pub struct NewObligation {
    pub task: String,
    pub due_at: Option<String>,
    pub assign: Option<String>,
    pub plan_id: Option<String>,
    pub operation_id: String,
    pub actor: String,
}

/// Create an obligation, or return the existing one when `operation_id` was
/// already consumed (retry idempotency). Returns `(obligation, created)`.
pub fn add(project_dir: &Path, new: NewObligation) -> Result<(Obligation, bool), String> {
    if new.task.trim().is_empty() {
        return Err("obligation task is empty".into());
    }
    let _guard = ResourceLock::acquire(lock_path(project_dir)).map_err(|e| e.to_string())?;
    if let Some(existing) = find_by_operation(project_dir, &new.operation_id) {
        return Ok((existing, false));
    }
    let now = now_rfc3339();
    let obligation = Obligation {
        schema_version: SCHEMA_OBLIGATION_V1.into(),
        id: uuid::Uuid::now_v7().to_string(),
        task: new.task.trim().to_string(),
        due_at: new.due_at,
        assign: new.assign,
        plan_id: new.plan_id,
        operation_id: new.operation_id.clone(),
        state: ObligationState::Open.as_str().into(),
        created_at: now.clone(),
        created_by: new.actor.clone(),
        snoozed_until: None,
        completed_at: None,
        completed_by: None,
        evidence: None,
        cancelled_at: None,
        cancel_reason: None,
    };
    write_definition(project_dir, &obligation)?;
    append_event(
        project_dir,
        &ObligationEvent {
            schema_version: SCHEMA_OBLIGATION_EVENT_V1.into(),
            id: uuid::Uuid::now_v7().to_string(),
            obligation_id: obligation.id.clone(),
            kind: "created".into(),
            at: now,
            actor: new.actor,
            operation_id: new.operation_id,
            evidence: None,
            reason: None,
            until: obligation.due_at.clone(),
        },
    )?;
    Ok((obligation, true))
}

fn mutate(
    project_dir: &Path,
    id: &str,
    kind: &str,
    operation_id: &str,
    actor: &str,
    apply: impl Fn(&mut Obligation) -> Result<(), String>,
    event_fields: impl Fn(&Obligation) -> (Option<String>, Option<String>, Option<String>),
) -> Result<Obligation, String> {
    let _guard = ResourceLock::acquire(lock_path(project_dir)).map_err(|e| e.to_string())?;
    // Idempotent retry of the same mutation operation returns current state.
    let (events, _) = read_events(project_dir);
    if events.iter().any(|e| e.operation_id == operation_id) {
        return load(project_dir, id)
            .ok_or_else(|| format!("unknown obligation `{id}` — run `stateroot obligation list`"));
    }
    let Some(mut obligation) = load(project_dir, id) else {
        return Err(format!(
            "unknown obligation `{id}` — run `stateroot obligation list`"
        ));
    };
    apply(&mut obligation)?;
    write_definition(project_dir, &obligation)?;
    let (evidence, reason, until) = event_fields(&obligation);
    append_event(
        project_dir,
        &ObligationEvent {
            schema_version: SCHEMA_OBLIGATION_EVENT_V1.into(),
            id: uuid::Uuid::now_v7().to_string(),
            obligation_id: obligation.id.clone(),
            kind: kind.into(),
            at: now_rfc3339(),
            actor: actor.to_string(),
            operation_id: operation_id.to_string(),
            evidence,
            reason,
            until,
        },
    )?;
    Ok(obligation)
}

/// Complete an obligation with explicit evidence. Terminal obligations are
/// not re-completable.
pub fn done(
    project_dir: &Path,
    id: &str,
    evidence: &str,
    actor: &str,
    operation_id: &str,
) -> Result<Obligation, String> {
    if evidence.trim().is_empty() {
        return Err("evidence is empty — say what proves this is done".into());
    }
    mutate(
        project_dir,
        id,
        "done",
        operation_id,
        actor,
        |o| {
            if o.state().is_terminal() {
                return Err(format!("obligation {} is already {}", o.id, o.state));
            }
            let now = now_rfc3339();
            o.state = ObligationState::Done.as_str().into();
            o.completed_at = Some(now);
            o.completed_by = Some(actor.to_string());
            o.evidence = Some(evidence.trim().to_string());
            Ok(())
        },
        |o| (o.evidence.clone(), None, None),
    )
}

/// Suppress an obligation until a future instant (UTC-normalized).
pub fn snooze(
    project_dir: &Path,
    id: &str,
    until: &str,
    actor: &str,
    operation_id: &str,
) -> Result<Obligation, String> {
    let until = normalize_rfc3339(until)?;
    mutate(
        project_dir,
        id,
        "snoozed",
        operation_id,
        actor,
        |o| {
            if o.state().is_terminal() {
                return Err(format!("obligation {} is already {}", o.id, o.state));
            }
            o.state = ObligationState::Snoozed.as_str().into();
            o.snoozed_until = Some(until.clone());
            Ok(())
        },
        |o| (None, None, o.snoozed_until.clone()),
    )
}

/// Cancel an obligation with a recorded reason.
pub fn cancel(
    project_dir: &Path,
    id: &str,
    reason: &str,
    actor: &str,
    operation_id: &str,
) -> Result<Obligation, String> {
    if reason.trim().is_empty() {
        return Err("cancel reason is empty — say why this work is dropped".into());
    }
    mutate(
        project_dir,
        id,
        "cancelled",
        operation_id,
        actor,
        |o| {
            if o.state().is_terminal() {
                return Err(format!("obligation {} is already {}", o.id, o.state));
            }
            o.state = ObligationState::Cancelled.as_str().into();
            o.cancelled_at = Some(now_rfc3339());
            o.cancel_reason = Some(reason.trim().to_string());
            Ok(())
        },
        |o| (None, o.cancel_reason.clone(), None),
    )
}

/// The operation id a plan-closure obligation is created with — deterministic,
/// so `ensure_plan_closure` is naturally idempotent per plan.
pub fn plan_closure_operation_id(plan_id: &str) -> String {
    format!("plan-closure:{plan_id}")
}

/// Create (or find) the explicit plan-closure obligation that replaces the
/// old "all todos completed → auto done" behavior. Returns
/// `(obligation, created)`. A user-cancelled closure obligation stays
/// cancelled — the derived continuity assessment still surfaces the plan.
pub fn ensure_plan_closure(
    project_dir: &Path,
    plan_id: &str,
    plan_title: &str,
    actor: &str,
) -> Result<(Obligation, bool), String> {
    add(
        project_dir,
        NewObligation {
            task: format!(
                "Close plan '{plan_title}' — record completion evidence (`stateroot plan done {plan_id} --evidence \"…\"`) or state concrete remaining work"
            ),
            due_at: None,
            assign: None,
            plan_id: Some(plan_id.to_string()),
            operation_id: plan_closure_operation_id(plan_id),
            actor: actor.to_string(),
        },
    )
}

/// Mark the plan-closure obligation done when the plan itself completes.
/// Absence or an already-terminal record is not an error.
pub fn resolve_plan_closure(project_dir: &Path, plan_id: &str, evidence: &str, actor: &str) {
    let operation_id = plan_closure_operation_id(plan_id);
    if let Some(existing) = find_by_operation(project_dir, &operation_id) {
        if !existing.state().is_terminal() {
            let _ = done(
                project_dir,
                &existing.id,
                evidence,
                actor,
                &format!("{operation_id}:resolved"),
            );
        }
    }
}

/// Mint a fresh operation id for ad-hoc CLI mutations.
pub fn mint_operation_id() -> String {
    uuid::Uuid::now_v7().to_string()
}
