//! Active local continuity — the ONE deterministic continuity assessment,
//! reused by `stateroot status`, resume/hook digests, the resident service,
//! and the editor projection.
//!
//! Local StateRoot is active but NON-agentic: this module only observes
//! already-recorded state and derives stable attention items from it. No
//! keyword classifiers, no model calls, no inferred intent — every item
//! names the exact store evidence it came from. The optional synthesis pass
//! (elsewhere) may attach labeled advisory text to the projection; it can
//! never create or mutate these deterministic items.
//!
//! Output: a machine-local atomic projection at
//! `.stateroot/local/projections/continuity.v1.json` (never synced).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{ContinuityConfig, ProjectsRegistry};
use crate::local_store::{self, now_rfc3339};
use crate::plans::{self, PlanMeta, PlanStatus};
use crate::safe_io;
use crate::todo_federation;

pub const PROJECTION_REL: &str = "local/projections/continuity.v1.json";
pub const ADVISORY_REL: &str = "local/projections/continuity-advisory.v1.json";
pub const SCHEMA_CONTINUITY_V1: &str = "stateroot.continuity.v1";
pub const SCHEMA_ADVISORY_V1: &str = "stateroot.continuity-advisory.v1";

/// Attention kinds — stable vocabulary, one per evidence source.
pub const KIND_OBLIGATION_DUE: &str = "obligation_due";
pub const KIND_PLAN_RECEIPT_PENDING: &str = "plan_receipt_pending";
pub const KIND_PLAN_CLOSURE: &str = "plan_closure";
pub const KIND_HANDOFF_ROUTED: &str = "handoff_routed";
pub const KIND_DELEGATION_FAILED: &str = "delegation_failed";
pub const KIND_BOUNDARY_JOB_MANUAL: &str = "boundary_journal_manual";
pub const KIND_PLAN_UNASSIGNED: &str = "plan_unassigned";
pub const KIND_HANDOFF_STALE: &str = "handoff_stale";
pub const KIND_SERVICE_UNHEALTHY: &str = "service_unhealthy";
pub const KIND_REGISTRY_PROJECT_MISSING: &str = "registry_project_missing";

/// One derived attention item. `id` is stable (`<kind>:<entity>`) so
/// dismiss/snooze bookkeeping and hash-idempotency survive reconciles.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionItem {
    pub id: String,
    pub kind: String,
    /// Lower ranks surface first in the bounded digest section.
    pub rank: u8,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// The concrete next CLI action, when one exists.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obligation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_id: Option<String>,
}

impl AttentionItem {
    fn new(kind: &str, rank: u8, entity: &str, title: String) -> Self {
        Self {
            id: format!("{kind}:{entity}"),
            kind: kind.into(),
            rank,
            title,
            detail: String::new(),
            action: String::new(),
            plan_id: None,
            obligation_id: None,
            handoff_seq: None,
            delegation_id: None,
        }
    }
}

/// The continuity projection document (machine-local).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuityAssessment {
    pub schema_version: String,
    pub generated_at: String,
    /// sha256 of the deterministic inputs — synthesis hash-idempotency key
    /// and advisory freshness marker.
    pub inputs_hash: String,
    pub attention: Vec<AttentionItem>,
    pub open_obligations: usize,
    pub corrupt_obligation_events: usize,
    /// Active/approved plan id when one exists (decision-ready brief).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_plan_status: Option<String>,
    pub plan_directive: String,
    // Service health (machine-local view of the resident continuity service).
    pub service_registered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_kind: Option<String>,
    pub service_running: bool,
    /// verified | unknown | stale | absent. The compatibility bool above
    /// is true only with actual process identity proof, not a fresh PID.
    #[serde(default)]
    pub service_identity_status: String,
    #[serde(default)]
    pub service_identity_detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_last_beat_at: Option<String>,
}

fn projection_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(PROJECTION_REL)
}

fn advisory_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(ADVISORY_REL)
}

/// Strict RFC3339 comparison; unparseable sides never claim (mirror of the
/// digest's `ts_newer`).
pub fn ts_newer(a: &str, b: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => a > b,
        _ => false,
    }
}

/// Newest observed activity timestamp: last checkpoint vs latest root,
/// newest wins (same rule as the digest's Latest Activity section).
fn latest_activity_at(project_dir: &Path) -> Option<String> {
    let mut best: Option<String> = local_store::recent_episodic(project_dir, 1)
        .into_iter()
        .next()
        .and_then(|rec| rec.get("ts").and_then(|v| v.as_str()).map(str::to_string))
        .filter(|ts| !ts.is_empty());
    if let Ok(Some(hash)) = crate::roots::latest_root(project_dir) {
        if let Ok(manifest) = crate::roots::get_root(project_dir, &hash) {
            let candidate = manifest.created_at;
            let replace = match (&best, candidate.is_empty()) {
                (None, false) => true,
                (Some(current), false) => ts_newer(&candidate, current),
                _ => false,
            };
            if replace {
                best = Some(candidate);
            }
        }
    }
    best
}

// ---------------------------------------------------------------------
// delegation records (minimal core reader — the CLI owns the writer)
// ---------------------------------------------------------------------

#[derive(Debug)]
struct DelegationView {
    id: String,
    plan_id: Option<String>,
    /// Terminal outcome when finalized (completed/failed/lost/…).
    outcome: Option<String>,
    running: bool,
    failed: bool,
}

fn delegations_dir(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join("delegations")
}

fn read_delegations(project_dir: &Path) -> Vec<DelegationView> {
    let Ok(entries) = std::fs::read_dir(delegations_dir(project_dir)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let schema = record.get("schema").and_then(|v| v.as_str()).unwrap_or("");
        if schema != "stateroot.delegation.v2" && schema != "stateroot.delegation.v1" {
            continue;
        }
        let id = record
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        let plan_id = record
            .get("plan_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let outcome = record
            .get("outcome")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let status = record.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let pid = record.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let live = pid > 0 && safe_io::pid_alive(pid);
        let running = outcome.is_none() && matches!(status, "starting" | "running") && live;
        // A record with no outcome whose worker pid is dead is lost, exactly
        // as the delegate module's live reaping would classify it on read.
        let lost =
            outcome.is_none() && matches!(status, "starting" | "running") && pid > 0 && !live;
        let failed = matches!(
            outcome.as_deref(),
            Some("failed") | Some("lost") | Some("timed_out")
        ) || lost;
        out.push(DelegationView {
            id,
            plan_id,
            outcome,
            running,
            failed,
        });
    }
    out
}

// ---------------------------------------------------------------------
// service heartbeat / registration (machine-local, config-dir scoped)
// ---------------------------------------------------------------------

pub const SERVICE_REGISTRATION_FILE: &str = "continuity-service.registration.json";
pub const SERVICE_HEARTBEAT_FILE: &str = "continuity-service.heartbeat.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceRegistration {
    pub schema_version: String,
    /// systemd-user | launchd | schtasks | wsl-schtasks | detached
    pub kind: String,
    pub installed_at: String,
    #[serde(default)]
    pub detail: String,
    /// The exact binary the manager descriptor points at (empty in
    /// pre-WS3 records). `service install` rewrites the descriptor when
    /// the current exe drifts from this (self-update rearm).
    #[serde(default)]
    pub exe: String,
    /// The config home the service was registered against.
    #[serde(default)]
    pub config_home: String,
    #[serde(default)]
    pub exe_identity: String,
    #[serde(default)]
    pub build_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceHeartbeat {
    pub schema_version: String,
    pub pid: u32,
    pub beat_at: String,
    pub version: String,
    /// The binary that wrote the beat (empty in pre-WS3 records) — stop
    /// verifies ownership against this before signalling the pid.
    #[serde(default)]
    pub exe: String,
    /// Host/PID namespace of the writer (`safe_io::host_namespace`; empty in
    /// records that predate it). A pid is only meaningful inside the
    /// namespace that wrote it — cross-runtime liveness is never assumed.
    #[serde(default)]
    pub namespace: String,
    /// Process-start token of the writer (unix: /proc starttime jiffies;
    /// Windows: creation FILETIME). Binds the recorded pid to THIS process
    /// instance — a reused pid fails the comparison. 0 in legacy records.
    #[serde(default)]
    pub pid_start: u64,
    /// Actual config selected by the writer; copied/legacy beats cannot
    /// authorize operations against a different installation.
    #[serde(default)]
    pub config_home: String,
    #[serde(default)]
    pub exe_identity: String,
    #[serde(default)]
    pub projects_scanned: usize,
}

pub fn service_registration_path(config_dir: &Path) -> PathBuf {
    config_dir.join(SERVICE_REGISTRATION_FILE)
}

pub fn service_heartbeat_path(config_dir: &Path) -> PathBuf {
    config_dir.join(SERVICE_HEARTBEAT_FILE)
}

pub fn read_service_registration(config_dir: &Path) -> Option<ServiceRegistration> {
    let text = std::fs::read_to_string(service_registration_path(config_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn read_service_heartbeat(config_dir: &Path) -> Option<ServiceHeartbeat> {
    let text = std::fs::read_to_string(service_heartbeat_path(config_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Read-only Linux observation for the core projection. No manager queries
/// or signals; unsupported platforms defer to authoritative CLI status.
fn heartbeat_identity_observed(beat: &ServiceHeartbeat, home: &Path) -> bool {
    if beat.pid == 0
        || beat.namespace.is_empty()
        || Some(beat.namespace.as_str()) != safe_io::host_namespace()
        || beat.config_home.is_empty()
        || Path::new(&beat.config_home) != home
        || beat.pid_start == 0
        || beat.exe.is_empty()
    {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        use std::io::Read as _;
        use std::os::unix::fs::MetadataExt as _;
        let bounded = |name: &str| -> Option<Vec<u8>> {
            let mut bytes = Vec::new();
            std::fs::File::open(format!("/proc/{}/{name}", beat.pid))
                .ok()?
                .take(65537)
                .read_to_end(&mut bytes)
                .ok()?;
            (bytes.len() <= 65536).then_some(bytes)
        };
        let birth = || -> Option<u64> {
            let raw = bounded("stat")?;
            let s = std::str::from_utf8(&raw).ok()?;
            s.rsplit(')')
                .next()?
                .split_whitespace()
                .nth(19)?
                .parse()
                .ok()
        };
        if birth() != Some(beat.pid_start) {
            return false;
        }
        let Some(args) = bounded("cmdline") else {
            return false;
        };
        let argv: Vec<&[u8]> = args.split(|b| *b == 0).filter(|a| !a.is_empty()).collect();
        let exe = beat.exe.strip_suffix(" (deleted)").unwrap_or(&beat.exe);
        if argv.as_slice() != [exe.as_bytes(), b"service".as_slice(), b"run".as_slice()] {
            return false;
        }
        let image_path = format!("/proc/{}/exe", beat.pid);
        let Ok(image) = std::fs::read_link(&image_path) else {
            return false;
        };
        let image = image.display().to_string();
        if image.strip_suffix(" (deleted)").unwrap_or(&image) != exe {
            return false;
        }
        let Ok(m) = std::fs::metadata(&image_path) else {
            return false;
        };
        if beat.exe_identity
            != format!(
                "{}:{}:{}:{}:{}",
                m.dev(),
                m.ino(),
                m.len(),
                m.mtime(),
                m.mtime_nsec()
            )
        {
            return false;
        }
        let Some(env) = bounded("environ") else {
            return false;
        };
        let pin = format!("STATEROOT_HOME={}", home.display());
        env.split(|b| *b == 0).any(|v| v == pin.as_bytes())
            && birth() == Some(beat.pid_start)
            && safe_io::pid_alive(beat.pid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Heartbeat is stale past three poll intervals (floor 90s) or a dead pid.
pub fn service_beat_stale(beat_at: &str, now: &str, poll_interval_seconds: u64) -> bool {
    let (Ok(beat), Ok(now)) = (
        chrono::DateTime::parse_from_rfc3339(beat_at),
        chrono::DateTime::parse_from_rfc3339(now),
    ) else {
        return true;
    };
    let grace = chrono::Duration::seconds((3 * poll_interval_seconds).max(90) as i64);
    now.signed_duration_since(beat) > grace
}

// ---------------------------------------------------------------------
// the assessment
// ---------------------------------------------------------------------

/// The state-aware plan directive for digests/handoffs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanDirective {
    /// Draft — keep planning.
    Plan,
    /// Approved, no executor — assign or claim.
    Assign,
    /// Execute the plan as written.
    Execute,
    /// Work is structurally finished — record completion evidence or state
    /// concrete remaining work; do not restart implementation.
    Close,
}

impl PlanDirective {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Assign => "assign",
            Self::Execute => "execute",
            Self::Close => "close",
        }
    }
}

/// The Telemetry-v2 closure signal: the current handoff declares no
/// remaining work AND its boundary is not older than the plan's current
/// status. The age guard matters: a project's init handoff carries empty
/// next_actions forever — a handoff that predates the plan's activation
/// cannot be declaring that plan finished.
fn handoff_declares_no_remaining_work(project_dir: &Path, status_since: &str) -> bool {
    let Ok(Some(packet)) = local_store::read_handoff_local(project_dir) else {
        return false;
    };
    let no_actions = packet
        .get("next_actions")
        .and_then(|v| v.as_array())
        .is_none_or(|a| a.is_empty());
    if !no_actions {
        return false;
    }
    let boundary = packet
        .get("written_at")
        .and_then(|v| v.as_str())
        .filter(|t| !t.is_empty())
        .or_else(|| packet.get("created_at").and_then(|v| v.as_str()))
        .unwrap_or("");
    !boundary.is_empty() && boundary >= status_since
}

/// Derive the directive for one plan from structural state only. Mirrors
/// the assessment's closure conditions: structurally completed todos, an
/// open plan-closure obligation, or the Telemetry-v2 shape (active plan,
/// handoff with no remaining actions, nothing plan-bound running).
pub fn plan_directive(project_dir: &Path, plan: &PlanMeta) -> PlanDirective {
    if plan.status() == PlanStatus::Draft {
        return PlanDirective::Plan;
    }
    let todos = todo_federation::plan_todo_progress(project_dir, &plan.id);
    let todos_structurally_done =
        matches!(todos, Some((done, total)) if total > 0 && done == total);
    let closure_obligation_open = crate::obligations::find_by_operation(
        project_dir,
        &crate::obligations::plan_closure_operation_id(plan.id.as_str()),
    )
    .is_some_and(|o| !o.state().is_terminal());
    if todos_structurally_done || closure_obligation_open {
        return PlanDirective::Close;
    }
    let delegations = read_delegations(project_dir);
    if plan.status() == PlanStatus::Active {
        let running_bound = delegations
            .iter()
            .any(|d| d.plan_id.as_deref() == Some(plan.id.as_str()) && d.running);
        let incomplete_todos = matches!(todos, Some((done, total)) if done < total);
        if !running_bound
            && !incomplete_todos
            && handoff_declares_no_remaining_work(project_dir, &plan.updated_at)
        {
            return PlanDirective::Close;
        }
    }
    if plan.status() == PlanStatus::Approved {
        let has_executor = delegations.iter().any(|d| {
            d.plan_id.as_deref() == Some(plan.id.as_str())
                && (d.running || d.outcome.as_deref() == Some("completed"))
        });
        if !has_executor {
            return PlanDirective::Assign;
        }
    }
    PlanDirective::Execute
}

/// Run the deterministic continuity assessment over one project.
pub fn assess(
    project_dir: &Path,
    config_dir: &Path,
    continuity_cfg: &ContinuityConfig,
    registry: Option<&ProjectsRegistry>,
) -> ContinuityAssessment {
    let now = now_rfc3339();
    let mut attention: Vec<AttentionItem> = Vec::new();

    // 1. Explicit obligations due or overdue.
    let obligations = crate::obligations::list(project_dir);
    let (_, corrupt_events) = crate::obligations::read_events(project_dir);
    let open_obligations = obligations
        .iter()
        .filter(|o| !o.state().is_terminal())
        .count();
    for obligation in &obligations {
        if !obligation.due(&now) {
            continue;
        }
        let mut item = AttentionItem::new(
            KIND_OBLIGATION_DUE,
            10,
            &obligation.id,
            format!("Obligation due: {}", obligation.task),
        );
        item.detail = format!(
            "due {}{}",
            obligation.due_at.as_deref().unwrap_or(""),
            obligation
                .assign
                .as_ref()
                .map(|h| format!(" · assigned to {h}"))
                .unwrap_or_default()
        );
        item.action = format!(
            "stateroot obligation done {} --evidence \"…\" (or `stateroot obligation snooze {0} --until …`)",
            obligation.id
        );
        item.obligation_id = Some(obligation.id.clone());
        item.plan_id = obligation.plan_id.clone();
        attention.push(item);
    }

    let plan_list = plans::list(project_dir);
    let delegations = read_delegations(project_dir);
    let handoff = local_store::read_handoff_local(project_dir).ok().flatten();

    // 2/3/4. Plan-derived attention.
    for plan in &plan_list {
        if !matches!(plan.status(), PlanStatus::Approved | PlanStatus::Active) {
            continue;
        }
        let todos = todo_federation::plan_todo_progress(project_dir, &plan.id);
        let todos_structurally_done =
            matches!(todos, Some((done, total)) if total > 0 && done == total);
        let closure_obligation_open = obligations.iter().any(|o| {
            o.operation_id == crate::obligations::plan_closure_operation_id(&plan.id)
                && !o.state().is_terminal()
        });

        // Structurally completed work awaiting its explicit completion
        // receipt — the plan stays active until `plan done --evidence`.
        if todos_structurally_done || closure_obligation_open {
            let mut item = AttentionItem::new(
                KIND_PLAN_RECEIPT_PENDING,
                15,
                &plan.id,
                format!("Plan '{}' awaits its completion receipt", plan.title),
            );
            item.detail = if todos_structurally_done {
                "all plan-bound todos are completed".into()
            } else {
                "a plan-closure obligation is open".into()
            };
            item.action = format!("stateroot plan done {} --evidence \"…\"", plan.id);
            item.plan_id = Some(plan.id.clone());
            attention.push(item);
        }

        if plan.status() == PlanStatus::Active {
            // The Telemetry-v2 failure shape: current handoff declares no
            // remaining actions (and is not older than the plan's current
            // status — an init handoff never speaks for a plan), no
            // plan-bound delegation is running, and no plan-bound todos are
            // incomplete — yet the plan is still active.
            let running_bound = delegations
                .iter()
                .any(|d| d.plan_id.as_deref() == Some(plan.id.as_str()) && d.running);
            let incomplete_todos = matches!(todos, Some((done, total)) if done < total);
            if !running_bound
                && !incomplete_todos
                && handoff_declares_no_remaining_work(project_dir, &plan.updated_at)
            {
                let mut item = AttentionItem::new(
                    KIND_PLAN_CLOSURE,
                    20,
                    &plan.id,
                    format!(
                        "Active plan '{}' has no remaining work recorded",
                        plan.title
                    ),
                );
                item.detail =
                    "current handoff lists no next actions; nothing plan-bound is running".into();
                item.action = format!(
                    "stateroot plan done {} --evidence \"…\" (or record concrete remaining work)",
                    plan.id
                );
                item.plan_id = Some(plan.id.clone());
                attention.push(item);
            }
        }

        if plan.status() == PlanStatus::Approved {
            let has_executor = delegations.iter().any(|d| {
                d.plan_id.as_deref() == Some(plan.id.as_str())
                    && (d.running || d.outcome.as_deref() == Some("completed"))
            });
            if !has_executor {
                let mut item = AttentionItem::new(
                    KIND_PLAN_UNASSIGNED,
                    40,
                    &plan.id,
                    format!("Approved plan '{}' has no executor", plan.title),
                );
                item.action = format!(
                    "stateroot plan activate {0} and execute, or `stateroot delegate --to <harness> --plan {0}`",
                    plan.id
                );
                item.plan_id = Some(plan.id.clone());
                attention.push(item);
            }
        }

        // Failed/lost/timed-out delegations against this still-open plan.
        for delegation in &delegations {
            if delegation.plan_id.as_deref() == Some(plan.id.as_str()) && delegation.failed {
                let mut item = AttentionItem::new(
                    KIND_DELEGATION_FAILED,
                    30,
                    &delegation.id,
                    format!(
                        "Delegation for plan '{}' ended without completion",
                        plan.title
                    ),
                );
                item.detail = format!("delegation {}", delegation.id);
                item.action = format!(
                    "stateroot delegate --to <harness> --task \"…\" (reassign), or resolve plan {}",
                    plan.id
                );
                item.plan_id = Some(plan.id.clone());
                item.delegation_id = Some(delegation.id.clone());
                attention.push(item);
            }
        }
    }

    // 5. Routed handoff awaiting acceptance / handoff stale by newer activity.
    if let Some(packet) = &handoff {
        let seq = packet.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
        let routed_to = packet
            .get("recommended_next_harness")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        if let Some(dest) = routed_to {
            let accepted = packet
                .get("accepted_by")
                .and_then(|v| v.as_array())
                .is_some_and(|list| list.iter().any(|h| h.as_str() == Some(dest)));
            if !accepted {
                let mut item = AttentionItem::new(
                    KIND_HANDOFF_ROUTED,
                    25,
                    &format!("handoff-{seq}"),
                    format!("Handoff #{seq} awaits acceptance by {dest}"),
                );
                item.action = "stateroot handoff accept".into();
                item.handoff_seq = Some(seq);
                attention.push(item);
            }
        }
        let boundary = packet
            .get("written_at")
            .and_then(|v| v.as_str())
            .filter(|t| !t.is_empty())
            .or_else(|| packet.get("created_at").and_then(|v| v.as_str()));
        if let Some(boundary) = boundary {
            if let Some(activity) = latest_activity_at(project_dir) {
                if ts_newer(&activity, boundary) {
                    let mut item = AttentionItem::new(
                        KIND_HANDOFF_STALE,
                        45,
                        &format!("handoff-{seq}"),
                        format!("Handoff #{seq} is stale — activity continued after it"),
                    );
                    item.detail = format!("latest activity {activity} postdates the handoff");
                    item.action = "stateroot handoff write (refresh the formal handoff)".into();
                    item.handoff_seq = Some(seq);
                    attention.push(item);
                }
            }
        }
    }

    // 7. Boundary-journal jobs parked for manual attention.
    for job in crate::finalize_journal::load_all(project_dir) {
        if job.state != "manual_attention" {
            continue;
        }
        let mut item = AttentionItem::new(
            KIND_BOUNDARY_JOB_MANUAL,
            35,
            &job.id,
            format!(
                "Session-boundary job for {} parked for manual attention",
                job.harness
            ),
        );
        item.detail = job.last_error.clone().unwrap_or_default();
        item.action = "stateroot doctor (inspect the boundary journal)".into();
        attention.push(item);
    }

    // 8. Unhealthy or stale continuity service (only when one is registered;
    //    never-installed is doctor's degraded-mode report, not nagging).
    let registration = read_service_registration(config_dir);
    let heartbeat = read_service_heartbeat(config_dir);
    let mut service_running = false;
    let mut service_identity_status = "absent".to_string();
    let mut service_identity_detail = "no heartbeat recorded".to_string();
    let mut service_last_beat: Option<String> = None;
    if continuity_cfg.enabled && registration.is_some() {
        let unhealthy = match &heartbeat {
            None => true,
            Some(beat) => {
                service_last_beat = Some(beat.beat_at.clone());
                let fresh =
                    !service_beat_stale(&beat.beat_at, &now, continuity_cfg.poll_interval_seconds);
                service_running = fresh && heartbeat_identity_observed(beat, config_dir);
                service_identity_status = if !fresh {
                    "stale"
                } else if service_running {
                    "verified"
                } else {
                    "unknown"
                }
                .into();
                service_identity_detail=if service_running {"read-only Linux image/args/birth/namespace/config verified"} else {"heartbeat observed; process ownership not established here; inspect stateroot service status"}.into();
                !service_running
            }
        };
        if unhealthy {
            let mut item = AttentionItem::new(
                KIND_SERVICE_UNHEALTHY,
                50,
                "continuity-service",
                "Continuity service liveness is stale or unverified".into(),
            );
            item.detail = match &heartbeat {
                None => "no heartbeat recorded yet".into(),
                Some(beat) => format!(
                    "last beat {} (pid {}); {}: {}",
                    beat.beat_at, beat.pid, service_identity_status, service_identity_detail
                ),
            };
            item.action = "stateroot service status (then `stateroot service restart`)".into();
            attention.push(item);
        }
    } else if let Some(beat) = &heartbeat {
        service_last_beat = Some(beat.beat_at.clone());
        let fresh = !service_beat_stale(&beat.beat_at, &now, continuity_cfg.poll_interval_seconds);
        service_running = fresh && heartbeat_identity_observed(beat, config_dir);
        service_identity_status = if !fresh {
            "stale"
        } else if service_running {
            "verified"
        } else {
            "unknown"
        }
        .into();
        service_identity_detail =
            "heartbeat observed; authoritative ownership/readout: stateroot service status".into();
    }

    // 9. Registered projects that vanished produce attention, not silence.
    if let Some(registry) = registry {
        let mut missing: Vec<String> = Vec::new();
        for key in registry.projects.keys() {
            if crate::path_identity::resolve_existing_dir(Path::new(key)).is_none() {
                missing.push(key.clone());
            }
        }
        if !missing.is_empty() {
            let mut item = AttentionItem::new(
                KIND_REGISTRY_PROJECT_MISSING,
                55,
                "registry",
                format!(
                    "{} registered project{} missing from disk",
                    missing.len(),
                    if missing.len() == 1 { " is" } else { "s are" }
                ),
            );
            item.detail = missing.join(", ");
            item.action = "stateroot projects --prune (after confirming the moves)".into();
            attention.push(item);
        }
    }

    attention.sort_by(|a, b| a.rank.cmp(&b.rank).then(a.id.cmp(&b.id)));

    let current_plan = plans::active_or_approved(project_dir);
    let (current_plan_id, current_plan_status, directive) = match &current_plan {
        Some((plan, _)) => (
            Some(plan.id.clone()),
            Some(plan.status().as_str().to_string()),
            plan_directive(project_dir, plan).as_str().to_string(),
        ),
        None => (None, None, String::new()),
    };

    let inputs_hash = inputs_hash(&attention, &obligations, corrupt_events);
    ContinuityAssessment {
        schema_version: SCHEMA_CONTINUITY_V1.into(),
        generated_at: now,
        inputs_hash,
        attention,
        open_obligations,
        corrupt_obligation_events: corrupt_events,
        current_plan_id,
        current_plan_status,
        plan_directive: directive,
        service_registered: registration.is_some(),
        service_kind: registration.map(|r| r.kind),
        service_running,
        service_identity_status,
        service_identity_detail,
        service_last_beat_at: service_last_beat,
    }
}

fn inputs_hash(
    attention: &[AttentionItem],
    obligations: &[crate::obligations::Obligation],
    corrupt_events: usize,
) -> String {
    let payload = serde_json::json!({
        "attention": attention,
        "obligations": obligations.iter().map(|o| serde_json::json!({
            "id": o.id, "state": o.state, "due_at": o.due_at,
            "snoozed_until": o.snoozed_until, "plan_id": o.plan_id,
        })).collect::<Vec<_>>(),
        "corrupt_events": corrupt_events,
    });
    crate::canonical::content_hash(&payload).unwrap_or_else(|_| "sha256:unknown".into())
}

/// Assess and atomically write the machine-local projection. This is the
/// single reconciliation step invoked after StateRoot mutations, at hook
/// boundaries, and by the resident service.
pub fn reconcile(
    project_dir: &Path,
    config_dir: &Path,
    continuity_cfg: &ContinuityConfig,
) -> Result<ContinuityAssessment, String> {
    let registry = crate::config::load_registry(config_dir).ok();
    let assessment = assess(project_dir, config_dir, continuity_cfg, registry.as_ref());
    let path = projection_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create projections dir: {e}"))?;
    }
    let value = serde_json::to_value(&assessment).map_err(|e| e.to_string())?;
    safe_io::atomic_replace_json(&path, &value)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(assessment)
}

/// Read the projection written by the last reconcile (None when absent or
/// unreadable — callers treat that as "not yet reconciled", never as "no
/// attention").
pub fn read_projection(project_dir: &Path) -> Option<ContinuityAssessment> {
    let text = std::fs::read_to_string(projection_path(project_dir)).ok()?;
    let value: ContinuityAssessment = serde_json::from_str(&text).ok()?;
    (value.schema_version == SCHEMA_CONTINUITY_V1).then_some(value)
}

// ---------------------------------------------------------------------
// advisory (synthesis-written, provenance-labeled, advisory-only)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuityAdvisory {
    pub schema_version: String,
    /// The assessment inputs this advisory was synthesized from — a stale
    /// advisory (hash mismatch) is never shown.
    pub inputs_hash: String,
    pub text: String,
    pub generated_at: String,
    /// Always "synthesis" today — provenance is part of the contract.
    pub source: String,
}

pub fn write_advisory(project_dir: &Path, advisory: &ContinuityAdvisory) -> Result<(), String> {
    let path = advisory_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create projections dir: {e}"))?;
    }
    let value = serde_json::to_value(advisory).map_err(|e| e.to_string())?;
    safe_io::atomic_replace_json(&path, &value)
        .map_err(|e| format!("write {}: {e}", path.display()))
}

/// The advisory, only when it labels the CURRENT assessment inputs.
pub fn current_advisory(project_dir: &Path, assessment: &ContinuityAssessment) -> Option<String> {
    let text = std::fs::read_to_string(advisory_path(project_dir)).ok()?;
    let advisory: ContinuityAdvisory = serde_json::from_str(&text).ok()?;
    if advisory.schema_version == SCHEMA_ADVISORY_V1
        && advisory.source == "synthesis"
        && advisory.inputs_hash == assessment.inputs_hash
        && !advisory.text.trim().is_empty()
    {
        Some(advisory.text)
    } else {
        None
    }
}

// ---------------------------------------------------------------------
// digest rendering (shared by resume, hooks, status)
// ---------------------------------------------------------------------

pub const DIGEST_ATTENTION_MAX: usize = 5;

/// "## Needs Attention" — the bounded push section, placed before the plan
/// section in injected digests. Highest-priority five items plus an overflow
/// count; absent when nothing needs attention (empty stays empty). A
/// hash-current synthesis advisory renders as one labeled trailing line.
pub fn needs_attention_markdown(
    assessment: &ContinuityAssessment,
    advisory: Option<&str>,
) -> Option<String> {
    if assessment.attention.is_empty() && advisory.is_none() {
        return None;
    }
    let mut out = String::from("## Needs Attention\n\n");
    // Presentation-only grouping (WS3): identical rows (same kind, title and
    // action — e.g. one row per parked boundary job of the same class)
    // collapse to a counted row. Every underlying item keeps its stable id
    // in the projection; nothing is dropped from the stored assessment.
    let mut groups: Vec<(&AttentionItem, usize)> = Vec::new();
    for item in &assessment.attention {
        if let Some((_, count)) = groups.iter_mut().find(|(first, _)| {
            first.kind == item.kind && first.title == item.title && first.action == item.action
        }) {
            *count += 1;
        } else {
            groups.push((item, 1));
        }
    }
    let mut rendered = 0usize;
    for (item, count) in groups.iter() {
        if rendered >= DIGEST_ATTENTION_MAX {
            break;
        }
        rendered += 1;
        out.push_str(&format!("- **{}**", item.title));
        if *count > 1 {
            out.push_str(&format!(" ×{count}"));
        }
        if !item.detail.is_empty() {
            let capped: String = item.detail.chars().take(160).collect();
            out.push_str(&format!(" — {capped}"));
        }
        if !item.action.is_empty() {
            out.push_str(&format!(" → `{}`", item.action));
        }
        out.push('\n');
    }
    let overflow = groups.len().saturating_sub(rendered);
    if overflow > 0 {
        out.push_str(&format!("- … +{overflow} more (`stateroot status`)\n"));
    }
    if let Some(text) = advisory {
        let capped: String = text.chars().take(400).collect();
        out.push_str(&format!(
            "- advisory (synthesized, not instructions): {capped}\n"
        ));
    }
    out.push('\n');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_heartbeat_without_process_identity_stays_explicitly_unknown() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(service_registration_path(home.path()),serde_json::to_vec(&serde_json::json!({"schema_version":"stateroot.continuity-registration.v1","kind":"detached","installed_at":now_rfc3339(),"config_home":home.path()})).unwrap()).unwrap();
        std::fs::write(service_heartbeat_path(home.path()),serde_json::to_vec(&serde_json::json!({"schema_version":"stateroot.continuity-heartbeat.v1","pid":std::process::id(),"beat_at":now_rfc3339(),"version":"fixture","config_home":home.path()})).unwrap()).unwrap();
        let result = assess(
            project.path(),
            home.path(),
            &ContinuityConfig::default(),
            None,
        );
        assert!(!result.service_running);
        assert_eq!(result.service_identity_status, "unknown");
        assert!(result
            .service_identity_detail
            .contains("heartbeat observed"));
        let item = result
            .attention
            .iter()
            .find(|item| item.kind == KIND_SERVICE_UNHEALTHY)
            .unwrap();
        assert!(!item.title.contains("not heartbeating"));
        assert!(item.detail.contains("unknown"));
        assert!(item.action.contains("service status"));
    }

    fn item(kind: &str, entity: &str, title: &str, action: &str) -> AttentionItem {
        let mut item = AttentionItem {
            id: format!("{kind}:{entity}"),
            kind: kind.into(),
            rank: 5,
            title: title.into(),
            detail: String::new(),
            action: action.into(),
            plan_id: None,
            obligation_id: None,
            handoff_seq: None,
            delegation_id: None,
        };
        item.detail = String::new();
        item
    }

    fn assessment(attention: Vec<AttentionItem>) -> ContinuityAssessment {
        ContinuityAssessment {
            schema_version: "test".into(),
            generated_at: "2026-10-09T00:00:00Z".into(),
            inputs_hash: "sha256:test".into(),
            attention,
            open_obligations: 0,
            corrupt_obligation_events: 0,
            current_plan_id: None,
            current_plan_status: None,
            plan_directive: String::new(),
            service_registered: false,
            service_kind: None,
            service_running: false,
            service_identity_status: "absent".into(),
            service_identity_detail: "no heartbeat recorded".into(),
            service_last_beat_at: None,
        }
    }

    #[test]
    fn needs_attention_groups_identical_rows_presentation_only() {
        // 62 identical parked-job rows + 2 distinct: the digest stays
        // bounded and counted, ids survive in the stored assessment.
        let mut attention: Vec<AttentionItem> = (0..62)
            .map(|i| {
                item(
                    "boundary_job",
                    &format!("job-{i}"),
                    "Parked boundary job",
                    "stateroot status",
                )
            })
            .collect();
        attention.push(item(
            "handoff_stale",
            "h1",
            "Handoff is stale",
            "stateroot handoff accept",
        ));
        attention.push(item(
            "plan_closure",
            "p1",
            "Plan awaits closure",
            "stateroot plan done",
        ));
        let a = assessment(attention);
        let md = needs_attention_markdown(&a, None).expect("section");
        assert!(md.contains("**Parked boundary job** ×62"), "{md}");
        assert!(md.contains("**Handoff is stale**"), "{md}");
        assert!(md.contains("**Plan awaits closure**"), "{md}");
        // Three grouped rows fit under the cap without an overflow line.
        assert!(!md.contains("+"), "{md}");
        // The stored assessment keeps every item — grouping is display-only.
        assert_eq!(a.attention.len(), 64);
    }

    #[test]
    fn needs_attention_overflow_counts_distinct_groups() {
        let mut attention: Vec<AttentionItem> = (0..4)
            .map(|i| item("boundary_job", &format!("job-{i}"), "Parked job", "x"))
            .collect();
        for i in 0..5 {
            attention.push(item("kind", &format!("e{i}"), &format!("Distinct {i}"), ""));
        }
        let a = assessment(attention);
        let md = needs_attention_markdown(&a, None).expect("section");
        assert!(md.contains("×4"), "{md}");
        // 6 groups > 5 cap → one overflow line naming the remaining groups.
        assert!(md.contains("+1 more"), "{md}");
    }
}
