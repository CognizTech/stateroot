//! Read-only delegation status — the ONE interpretation shared by the CLI
//! (`delegate list` / `delegate status`, digest Recent Delegations) and
//! continuity (attention items, plan directives). M6: previously the CLI
//! honored recorded process namespaces while continuity probed pids
//! locally, so the two readers could disagree (foreign live pid read as
//! "running", foreign dead-pid coincidence read as "lost"/failed); both
//! readers also silently dropped unreadable or malformed records.
//!
//! Contract:
//! - A terminal outcome recorded by the worker takes precedence over any
//!   process evidence.
//! - A pid is meaningful only inside the namespace that recorded it:
//!   foreign, legacy (absent) or unverifiable namespace evidence is
//!   UNKNOWN — never "running" and never "lost". Missing pid evidence is
//!   unknown too, never a death proof.
//! - Liveness is probed only for an owned namespace with a recorded pid —
//!   a bounded read (`safe_io::pid_alive`); reads launch no worker, spawn
//!   no process and never rewrite, reclassify or "reap" a record.
//! - Malformed, unreadable, unsupported or id-less records are PRESERVED
//!   and reported as concise degraded observations — never silently
//!   equated with absence.
//! - Process status is never semantic plan acceptance: closing a plan
//!   stays with `plan done --evidence` and source-bound receipts.

use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::local_store;

pub const SCHEMA_V1: &str = "stateroot.delegation.v1";
pub const SCHEMA_V2: &str = "stateroot.delegation.v2";

/// The recorded PID-namespace identity of this host — the WRITER's format,
/// moved verbatim from the CLI delegate module. Windows and WSL can read
/// the same project store but cannot interpret each other's process IDs,
/// so this identity is what makes a pid comparable at all. Do not "unify"
/// this with `safe_io::host_namespace`: that identity serves the service
/// heartbeat pair and differs on Windows (MachineGuid vs COMPUTERNAME) —
/// changing either hashing silently invalidates existing records.
pub fn process_namespace() -> Option<String> {
    static ID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        #[cfg(target_os = "linux")]
        let identity = {
            let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
            let namespace = std::fs::read_link("/proc/self/ns/pid").ok()?;
            format!("linux:{}:{}", boot.trim(), namespace.display())
        };
        #[cfg(windows)]
        let identity = format!("windows:{}", std::env::var("COMPUTERNAME").ok()?);
        #[cfg(not(any(target_os = "linux", windows)))]
        let identity = {
            let host = std::process::Command::new("hostname").output().ok()?;
            let boot = std::process::Command::new("sysctl")
                .args(["-n", "kern.boottime"])
                .output()
                .ok()?;
            if !host.status.success() || !boot.status.success() {
                return None;
            }
            format!(
                "{}:{:?}:{:?}",
                std::env::consts::OS,
                host.stdout,
                boot.stdout
            )
        };
        Some(format!("sha256:{:x}", Sha256::digest(identity.as_bytes())))
    })
    .clone()
}

/// True when the record's namespace matches this host's current identity —
/// the only condition under which a recorded pid may be probed locally.
pub fn owns_namespace(record: &Value) -> bool {
    match (
        record.get("process_namespace").and_then(Value::as_str),
        process_namespace(),
    ) {
        (Some(recorded), Some(current)) => recorded == current,
        _ => false,
    }
}

/// The shared status vocabulary. `Terminal` carries the worker-recorded
/// outcome verbatim (completed / failed / lost / timed_out / cancelling /
/// cancelled_with_root / …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationStatus {
    Terminal(String),
    Running,
    Lost,
    Unknown,
}

impl DelegationStatus {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Terminal(outcome) => outcome,
            Self::Running => "running",
            Self::Lost => "lost",
            Self::Unknown => "unknown",
        }
    }

    /// Proven failure: a terminal failure outcome recorded by the worker,
    /// or an owned worker pid observed dead. Unknown is NEVER failure.
    pub fn is_failed(&self) -> bool {
        match self {
            Self::Terminal(outcome) => {
                matches!(outcome.as_str(), "failed" | "lost" | "timed_out")
            }
            Self::Lost => true,
            _ => false,
        }
    }

    /// Proven completion (terminal success only — never inferred).
    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Terminal(outcome) if outcome == "completed")
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
}

/// Liveness as an OBSERVED input. Separating observation from the decision
/// keeps every case that does not strictly require a probe (terminal,
/// foreign, legacy, starting, pid-less) free of any process access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// The caller did not probe (not required, or declined): decide honestly.
    NotProbed,
    Alive,
    Dead,
}

/// The pure decision inputs — no I/O below this line.
#[derive(Debug)]
pub struct StatusInput<'a> {
    pub outcome: Option<&'a str>,
    pub status: Option<&'a str>,
    pub pid: u32,
    pub record_namespace: Option<&'a str>,
    pub current_namespace: Option<&'a str>,
    pub liveness: Liveness,
}

/// The one status decision, shared verbatim by every product view:
/// terminal precedence → namespace ownership → observed liveness.
pub fn decide(input: &StatusInput) -> DelegationStatus {
    if let Some(outcome) = input.outcome {
        return DelegationStatus::Terminal(outcome.to_string());
    }
    // `starting` (a reservation whose spawn may never have landed), an
    // absent status and unrecognized values all carry no interpretable
    // liveness evidence — unknown, never a death proof.
    if input.status != Some("running") {
        return DelegationStatus::Unknown;
    }
    let owned = match (input.record_namespace, input.current_namespace) {
        (Some(recorded), Some(current)) => recorded == current,
        _ => false,
    };
    if !owned || input.pid == 0 {
        return DelegationStatus::Unknown;
    }
    match input.liveness {
        Liveness::Alive => DelegationStatus::Running,
        Liveness::Dead => DelegationStatus::Lost,
        Liveness::NotProbed => DelegationStatus::Unknown,
    }
}

/// Interpret one parsed record. Probes liveness ONLY when the decision
/// strictly requires it: no outcome, status "running", a recorded pid and
/// an owned namespace.
pub fn interpret(record: &Value) -> DelegationStatus {
    let outcome = record.get("outcome").and_then(Value::as_str);
    let status = record.get("status").and_then(Value::as_str);
    let pid = record.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32;
    let record_namespace = record.get("process_namespace").and_then(Value::as_str);
    let current = process_namespace();
    let owned = match (record_namespace, current.as_deref()) {
        (Some(recorded), Some(current)) => recorded == current,
        _ => false,
    };
    let probe = outcome.is_none() && status == Some("running") && pid != 0 && owned;
    let liveness = if probe {
        if crate::safe_io::pid_alive(pid) {
            Liveness::Alive
        } else {
            Liveness::Dead
        }
    } else {
        Liveness::NotProbed
    };
    decide(&StatusInput {
        outcome,
        status,
        pid,
        record_namespace,
        current_namespace: current.as_deref(),
        liveness,
    })
}

/// One delegation-store entry, exactly as found. Reads never rewrite,
/// reclassify or delete anything.
#[derive(Debug)]
pub enum DelegationEntry {
    /// A parsed delegation record (any supported schema, or a legacy record
    /// carrying no schema marker at all).
    Record(PathBuf, Value),
    /// Present but not interpretable as a delegation record. Surfaced as a
    /// concise degraded observation instead of being silently omitted.
    Degraded { path: PathBuf, reason: String },
}

impl DelegationEntry {
    pub fn path(&self) -> &Path {
        match self {
            Self::Record(path, _) => path,
            Self::Degraded { path, .. } => path,
        }
    }
}

/// Why a store entry is degraded (concise, stable vocabulary).
fn degraded_reason(path: &Path) -> Option<String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return Some("unreadable file".into()),
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return Some("unparseable JSON".into()),
    };
    match value.get("schema_version").and_then(Value::as_str) {
        Some(SCHEMA_V1) | Some(SCHEMA_V2) | None => {}
        Some(other) => return Some(format!("unsupported schema_version {other}")),
    }
    match value.get("id").and_then(Value::as_str) {
        Some(id) if !id.is_empty() => None,
        _ => Some("record without id".into()),
    }
}

/// Read every entry of the project's delegation store, records newest first
/// by `ts`, degraded entries preserved after them (they carry no
/// comparable timestamp). Byte-preserving: this is a pure read.
pub fn read_entries(project_dir: &Path) -> Vec<DelegationEntry> {
    let dir = local_store::root(project_dir).join("delegations");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut records: Vec<(PathBuf, Value)> = Vec::new();
    let mut degraded: Vec<DelegationEntry> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        match degraded_reason(&path) {
            None => {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                records.push((path, value));
            }
            Some(reason) => degraded.push(DelegationEntry::Degraded { path, reason }),
        }
    }
    records.sort_by(|a, b| {
        let ts_of = |r: &Value| {
            r.get("ts")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        ts_of(&b.1).cmp(&ts_of(&a.1))
    });
    records
        .into_iter()
        .map(|(path, value)| DelegationEntry::Record(path, value))
        .chain(degraded)
        .collect()
}

/// Convenience: parsed records only (callers that cannot act on degraded
/// entries, e.g. id-keyed lookups).
pub fn read_records(project_dir: &Path) -> Vec<(PathBuf, Value)> {
    read_entries(project_dir)
        .into_iter()
        .filter_map(|entry| match entry {
            DelegationEntry::Record(path, value) => Some((path, value)),
            DelegationEntry::Degraded { .. } => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS_LOCAL: &str = "sha256:local-fixture";
    const NS_FOREIGN: &str = "sha256:foreign-fixture";
    const LIVE: Liveness = Liveness::Alive;
    const DEAD: Liveness = Liveness::Dead;
    const UNPROBED: Liveness = Liveness::NotProbed;

    fn input<'a>(
        outcome: Option<&'a str>,
        status: Option<&'a str>,
        pid: u32,
        record_namespace: Option<&'a str>,
        liveness: Liveness,
    ) -> StatusInput<'a> {
        StatusInput {
            outcome,
            status,
            pid,
            record_namespace,
            current_namespace: Some(NS_LOCAL),
            liveness,
        }
    }

    /// The accepted contract matrix: one row per case, read top to bottom
    /// as the product's status law for BOTH consumers.
    #[test]
    fn status_contract_matrix() {
        let cases: Vec<(StatusInput, DelegationStatus)> = vec![
            // Terminal outcome takes precedence over any process evidence,
            // including foreign live/dead pid coincidences and pid reuse.
            (
                input(Some("completed"), None, 0, None, UNPROBED),
                DelegationStatus::Terminal("completed".into()),
            ),
            (
                input(Some("failed"), Some("running"), 1, Some(NS_FOREIGN), LIVE),
                DelegationStatus::Terminal("failed".into()),
            ),
            (
                input(Some("lost"), Some("running"), 1, Some(NS_LOCAL), LIVE),
                DelegationStatus::Terminal("lost".into()),
            ),
            (
                input(Some("cancelling"), None, 1, Some(NS_LOCAL), DEAD),
                DelegationStatus::Terminal("cancelling".into()),
            ),
            (
                input(Some("cancelled_with_root"), None, 0, None, UNPROBED),
                DelegationStatus::Terminal("cancelled_with_root".into()),
            ),
            // Owned namespace: observed liveness decides.
            (
                input(None, Some("running"), 101, Some(NS_LOCAL), LIVE),
                DelegationStatus::Running,
            ),
            (
                input(None, Some("running"), 101, Some(NS_LOCAL), DEAD),
                DelegationStatus::Lost,
            ),
            // Foreign namespace: unknown on EITHER pid coincidence.
            (
                input(None, Some("running"), 101, Some(NS_FOREIGN), LIVE),
                DelegationStatus::Unknown,
            ),
            (
                input(None, Some("running"), 101, Some(NS_FOREIGN), DEAD),
                DelegationStatus::Unknown,
            ),
            // Absent/legacy namespace, missing pid, missing current
            // identity: unknown, never death.
            (
                input(None, Some("running"), 101, None, UNPROBED),
                DelegationStatus::Unknown,
            ),
            (
                input(None, Some("running"), 0, Some(NS_LOCAL), UNPROBED),
                DelegationStatus::Unknown,
            ),
            // Starting reservations and status-less records: unknown.
            (
                input(None, Some("starting"), 0, Some(NS_LOCAL), UNPROBED),
                DelegationStatus::Unknown,
            ),
            (
                input(None, Some("starting"), 101, Some(NS_LOCAL), DEAD),
                DelegationStatus::Unknown,
            ),
            (
                input(None, None, 0, None, UNPROBED),
                DelegationStatus::Unknown,
            ),
        ];
        for (index, (case, expected)) in cases.into_iter().enumerate() {
            assert_eq!(decide(&case), expected, "matrix case {index}: {case:?}");
        }
    }

    #[test]
    fn failed_and_completed_flags_never_fire_on_unknown() {
        for (status, failed, completed) in [
            (DelegationStatus::Terminal("completed".into()), false, true),
            (DelegationStatus::Terminal("failed".into()), true, false),
            (DelegationStatus::Terminal("lost".into()), true, false),
            (DelegationStatus::Terminal("timed_out".into()), true, false),
            (
                DelegationStatus::Terminal("cancelling".into()),
                false,
                false,
            ),
            (
                DelegationStatus::Terminal("cancelled_with_root".into()),
                false,
                false,
            ),
            (DelegationStatus::Running, false, false),
            (DelegationStatus::Lost, true, false),
            (DelegationStatus::Unknown, false, false),
        ] {
            assert_eq!(status.is_failed(), failed, "{status:?}");
            assert_eq!(status.is_completed(), completed, "{status:?}");
        }
    }

    #[test]
    fn interpret_probes_only_owned_running_records() {
        // Owned + running + live pid (this test process) → Running.
        let live = serde_json::json!({
            "status": "running",
            "pid": std::process::id(),
            "process_namespace": process_namespace(),
        });
        assert_eq!(interpret(&live), DelegationStatus::Running);
        // Owned + running + a pid that cannot be alive → Lost.
        let dead = serde_json::json!({
            "status": "running",
            "pid": 4_000_000u32,
            "process_namespace": process_namespace(),
        });
        assert_eq!(interpret(&dead), DelegationStatus::Lost);
        // Foreign + a live pid coincidence → Unknown (no probe interpretation).
        let foreign = serde_json::json!({
            "status": "running",
            "pid": std::process::id(),
            "process_namespace": NS_FOREIGN,
        });
        assert_eq!(interpret(&foreign), DelegationStatus::Unknown);
        // Terminal outcomes short-circuit before any namespace/liveness read.
        let terminal = serde_json::json!({"outcome": "completed", "status": "running", "pid": 1});
        assert_eq!(
            interpret(&terminal),
            DelegationStatus::Terminal("completed".into())
        );
    }

    #[test]
    fn read_entries_preserves_records_and_reports_degraded() {
        let project = tempfile::tempdir().expect("project");
        let dir = local_store::root(project.path()).join("delegations");
        std::fs::create_dir_all(&dir).expect("dir");
        let good = serde_json::json!({
            "schema_version": SCHEMA_V2,
            "id": "good",
            "ts": "2026-10-22T00:00:00Z",
        });
        let legacy = serde_json::json!({
            "schema_version": SCHEMA_V1,
            "id": "legacy",
            "ts": "2026-10-21T00:00:00Z",
        });
        let unmarked = serde_json::json!({"id": "unmarked", "ts": "2026-10-20T00:00:00Z"});
        std::fs::write(dir.join("good.json"), good.to_string()).expect("good");
        std::fs::write(dir.join("legacy.json"), legacy.to_string()).expect("legacy");
        std::fs::write(dir.join("unmarked.json"), unmarked.to_string()).expect("unmarked");
        std::fs::write(dir.join("broken.json"), b"{not json".as_slice()).expect("broken");
        std::fs::write(
            dir.join("future.json"),
            serde_json::json!({"schema_version": "stateroot.delegation.v9", "id": "future"})
                .to_string(),
        )
        .expect("future");
        std::fs::write(
            dir.join("noname.json"),
            serde_json::json!({"schema_version": SCHEMA_V2, "status": "running"}).to_string(),
        )
        .expect("noname");

        let entries = read_entries(project.path());
        let records: Vec<&Value> = entries
            .iter()
            .filter_map(|entry| match entry {
                DelegationEntry::Record(_, value) => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(records.len(), 3, "v1, v2 and unmarked legacy records");
        assert_eq!(records[0]["id"], "good", "newest first");
        assert_eq!(records[2]["id"], "unmarked");
        let degraded: Vec<&DelegationEntry> = entries
            .iter()
            .filter(|entry| matches!(entry, DelegationEntry::Degraded { .. }))
            .collect();
        assert_eq!(degraded.len(), 3, "{degraded:?}");
        for expected in [
            "unparseable JSON",
            "unsupported schema_version",
            "record without id",
        ] {
            assert!(
                degraded.iter().any(|entry| match entry {
                    DelegationEntry::Degraded { reason, .. } => reason.contains(expected),
                    _ => false,
                }),
                "missing degraded reason {expected}: {degraded:?}"
            );
        }
        // Byte-preserving reads.
        assert_eq!(
            std::fs::read(dir.join("broken.json")).expect("bytes"),
            b"{not json".as_slice()
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("good.json")).expect("bytes"),
            good.to_string()
        );
    }
}
