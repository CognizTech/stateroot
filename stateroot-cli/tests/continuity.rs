//! Active-continuity acceptance: the Telemetry-v2 repro, evidence-gated
//! completion, deterministic reconciliation for every attention kind, and
//! the human/JSON status + digest surfaces.

// Each test binary includes this module but uses a subset of its helpers.
#[path = "support/mod.rs"]
mod support;

use std::path::Path;

use assert_cmd::assert::Assert;
use support::{homes, init_project, stateroot};

fn stdout_of(out: Assert) -> String {
    String::from_utf8(out.get_output().stdout.clone()).expect("utf8")
}

fn record_plan(config_home: &Path, user_home: &Path, project: &Path, title: &str) -> String {
    let out = stateroot(config_home, user_home, project)
        .args(["plan", "record", "--stdin", "--title", title])
        .write_stdin(format!("# {title}\n\nDo the work.\n"))
        .assert()
        .success();
    let stdout = stdout_of(out);
    let id = stdout
        .split_whitespace()
        .find(|w| w.starts_with("plan_"))
        .expect("plan id in output");
    id.to_string()
}

fn projection(project: &Path) -> serde_json::Value {
    let path = project.join(".stateroot/local/projections/continuity.v1.json");
    let text = std::fs::read_to_string(&path).expect("projection exists");
    serde_json::from_str(&text).expect("projection json")
}

fn attention_kinds(project: &Path) -> Vec<String> {
    projection(project)["attention"]
        .as_array()
        .expect("attention array")
        .iter()
        .filter_map(|i| i["kind"].as_str().map(str::to_string))
        .collect()
}

fn status_json(config_home: &Path, user_home: &Path, project: &Path) -> serde_json::Value {
    let out = stateroot(config_home, user_home, project)
        .args(["status", "--json"])
        .assert()
        .success();
    serde_json::from_str(&stdout_of(out)).expect("status json")
}

/// The Telemetry-v2 failure shape: an active plan, a current handoff with
/// NO next actions, nothing plan-bound running. The continuity layer must
/// derive a closure obligation (attention) and must NOT tell another agent
/// to execute the plan again.
#[test]
fn released_active_plan_with_empty_handoff_produces_closure_attention() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let id = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Ship v2",
    );
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "approve", &id])
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "activate", &id])
        .assert()
        .success();
    // Handoff with no next actions — "the work is done" in prose only.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--objective",
            "Telemetry v2 shipped",
            "--context-summary",
            "released and smoke-verified",
        ])
        .assert()
        .success();

    // The assessment flags plan_closure (handoff carries no remaining work).
    let kinds = attention_kinds(project.path());
    assert!(
        kinds.contains(&"plan_closure".to_string()),
        "kinds: {kinds:?}"
    );

    // Status --json is the decision-ready brief: directive = close.
    let status = status_json(config_home.path(), user_home.path(), project.path());
    assert_eq!(status["schema_version"], "stateroot.status.v1");
    assert_eq!(status["plan"]["directive"], "close");
    assert!(status["continuity"]["attention"].is_array());
    assert!(status["boundary_journal"].is_object());

    // The resume digest pushes the closure directive, never "execute".
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--harness", "codex"])
        .assert()
        .success();
    let digest = stdout_of(out);
    assert!(digest.contains("## Needs Attention"), "digest: {digest}");
    assert!(
        digest.contains("do not restart implementation"),
        "digest: {digest}"
    );
    assert!(
        !digest.contains("Execute it as written; do not re-plan or re-explore"),
        "digest must not re-issue the executor directive: {digest}"
    );
}

/// All-completed plan-bound todos keep the plan ACTIVE until
/// `plan done --evidence`; the receipt then carries everything, the
/// attention disappears, and so does the executor directive.
#[test]
fn completed_todos_await_explicit_receipt_then_receipt_closes_everything() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let id = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Receipt Me",
    );
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "approve", &id])
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "activate", &id])
        .assert()
        .success();

    // Structurally completed plan-bound todos (store shape, as the Cursor
    // frontmatter sync would write it).
    let todos = project.path().join(".stateroot/todos/cursor");
    std::fs::create_dir_all(&todos).expect("todos dir");
    std::fs::write(
        todos.join("native-plan.json"),
        serde_json::json!({
            "schema_version": "stateroot.todo.v1",
            "harness": "cursor",
            "session_id": "native-plan",
            "plan_id": id,
            "items": [
                {"key": "a", "content": "A", "status": "completed"},
                {"key": "b", "content": "B", "status": "completed"}
            ],
            "updated_at": "2026-10-01T00:00:00Z",
            "provenance": "observed · test"
        })
        .to_string(),
    )
    .expect("write todos");

    // Reconcile via status: receipt pending, plan still active.
    let status = status_json(config_home.path(), user_home.path(), project.path());
    assert_eq!(status["plan"]["directive"], "close");
    let kinds = attention_kinds(project.path());
    assert!(
        kinds.contains(&"plan_receipt_pending".to_string()),
        "kinds: {kinds:?}"
    );
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "list"])
        .assert()
        .success();
    assert!(stdout_of(out).contains("active"), "plan stays active");

    // plan done without evidence is refused; with evidence it completes and
    // records the full receipt.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "done", &id])
        .assert()
        .failure();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "done", &id, "--evidence", "workspace gate green"])
        .assert()
        .success();
    assert!(stdout_of(out).contains("receipt:"));

    let sidecar: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            project
                .path()
                .join(".stateroot")
                .join("plans")
                .join(format!("{id}.json")),
        )
        .expect("sidecar"),
    )
    .expect("sidecar json");
    assert_eq!(sidecar["status"], "done");
    let receipt = &sidecar["completion_receipt"];
    assert_eq!(receipt["evidence"], "workspace gate green");
    assert_eq!(receipt["completed_by"], "cli");
    assert!(receipt["body_digest"].as_str().expect("digest").len() > 32);
    assert!(receipt["completion_root"].is_string());

    // Attention cleared; the plan no longer surfaces a directive.
    let status = status_json(config_home.path(), user_home.path(), project.path());
    let kinds: Vec<&str> = status["continuity"]["attention"]
        .as_array()
        .expect("attention")
        .iter()
        .filter_map(|i| i["kind"].as_str())
        .collect();
    assert!(
        !kinds.contains(&"plan_receipt_pending") && !kinds.contains(&"plan_closure"),
        "kinds after receipt: {kinds:?}"
    );
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--harness", "codex"])
        .assert()
        .success();
    let digest = stdout_of(out);
    assert!(
        !digest.contains("Execute it as written"),
        "done plan surfaces no executor directive: {digest}"
    );
}

/// Deterministic reconciliation across the remaining attention kinds —
/// every item derives from store structure, never text heuristics.
#[test]
fn deterministic_reconciliation_covers_each_attention_kind() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // plan_unassigned: an approved plan with no executor.
    let unassigned = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Orphan",
    );
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "approve", &unassigned])
        .assert()
        .success();
    let status = status_json(config_home.path(), user_home.path(), project.path());
    assert_eq!(status["plan"]["directive"], "assign");
    assert!(attention_kinds(project.path()).contains(&"plan_unassigned".to_string()));

    // delegation_failed against the open plan (store shape — the writer's
    // real `schema_version` key).
    let delegations = project.path().join(".stateroot/delegations");
    std::fs::create_dir_all(&delegations).expect("delegations dir");
    std::fs::write(
        delegations.join("d-fail.json"),
        serde_json::json!({
            "schema_version": "stateroot.delegation.v2",
            "id": "d-fail",
            "plan_id": unassigned,
            "outcome": "failed"
        })
        .to_string(),
    )
    .expect("write delegation");
    // Direct store writes need a reconcile (status) before the projection
    // reflects them.
    let _ = status_json(config_home.path(), user_home.path(), project.path());
    assert!(attention_kinds(project.path()).contains(&"delegation_failed".to_string()));

    // boundary_journal_manual: a parked finalize job (store shape).
    let journal = project.path().join(".stateroot/local/finalize-journal");
    std::fs::create_dir_all(&journal).expect("journal dir");
    std::fs::write(
        journal.join("job-1.json"),
        serde_json::json!({
            "schema": "stateroot.finalize-journal.v1",
            "id": "job-1",
            "harness": "codex",
            "session_id": "s1",
            "transcript": null,
            "lineage_ref": "refs/stateroot/lineage",
            "ingest_key": "k",
            "enqueued_at": "2026-10-01T00:00:00Z",
            "phase": "queued",
            "root": null,
            "handoff_seq": null,
            "attempt": 10,
            "next_attempt_at": "2026-10-01T00:00:00Z",
            "last_error": "simulated repeated failure",
            "state": "manual_attention"
        })
        .to_string(),
    )
    .expect("write job");
    let _ = status_json(config_home.path(), user_home.path(), project.path());
    assert!(attention_kinds(project.path()).contains(&"boundary_journal_manual".to_string()));

    // handoff_routed: routed to kimi, unaccepted; resolved by acceptance.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--to",
            "kimi",
            "--objective",
            "Routed work",
            "--next",
            "pick it up",
        ])
        .assert()
        .success();
    assert!(attention_kinds(project.path()).contains(&"handoff_routed".to_string()));
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["handoff", "accept", "--by", "kimi"])
        .assert()
        .success();
    assert!(!attention_kinds(project.path()).contains(&"handoff_routed".to_string()));

    // handoff_stale: newer activity postdates the handoff boundary.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["checkpoint", "--note", "work continued after the handoff"])
        .assert()
        .success();
    assert!(attention_kinds(project.path()).contains(&"handoff_stale".to_string()));

    // Human status renders the push section and the journal health.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["status"])
        .assert()
        .success();
    let human = stdout_of(out);
    assert!(
        human.contains("## Needs Attention"),
        "human status: {human}"
    );
    assert!(human.contains("boundary journal:"), "human status: {human}");
    assert!(human.contains("handoff:"), "human status: {human}");
}

/// Missing registry projects surface as attention in the surviving ones.
#[test]
fn missing_registry_project_produces_attention() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // A second registered project that then vanishes from disk.
    let ghost = tempfile::tempdir().expect("ghost");
    init_project(config_home.path(), user_home.path(), ghost.path());
    let ghost_path = ghost.path().to_path_buf();
    drop(ghost); // deletes the directory

    // Reconcile the surviving project: the missing registration surfaces.
    let _ = status_json(config_home.path(), user_home.path(), project.path());
    let kinds = attention_kinds(project.path());
    assert!(
        kinds.contains(&"registry_project_missing".to_string()),
        "kinds: {kinds:?} (ghost {})",
        ghost_path.display()
    );
}

// ---------------------------------------------------------------------------
// M6: one shared delegation-status interpretation (CLI == continuity)
// ---------------------------------------------------------------------------

/// The recorded PID-namespace identity of this host, computed with the
/// writer's own inputs (the format is a stability contract: changing it
/// silently invalidates every existing record). None where unsupported.
#[cfg(target_os = "linux")]
fn current_namespace() -> Option<String> {
    use sha2::{Digest, Sha256};
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let link = std::fs::read_link("/proc/self/ns/pid").ok()?;
    let identity = format!("linux:{}:{}", boot.trim(), link.display());
    Some(format!("sha256:{:x}", Sha256::digest(identity.as_bytes())))
}
#[cfg(windows)]
fn current_namespace() -> Option<String> {
    use sha2::{Digest, Sha256};
    let identity = format!("windows:{}", std::env::var("COMPUTERNAME").ok()?);
    Some(format!("sha256:{:x}", Sha256::digest(identity.as_bytes())))
}
#[cfg(not(any(target_os = "linux", windows)))]
fn current_namespace() -> Option<String> {
    None
}

/// Write one delegation record in the REAL writer's shape (`schema_version`,
/// never the continuity reader's historic `schema` miskey).
fn write_delegation(project: &Path, record: serde_json::Value) {
    let dir = project.join(".stateroot/delegations");
    std::fs::create_dir_all(&dir).expect("delegations dir");
    let id = record["id"].as_str().expect("record id").to_string();
    let mut record = record;
    record
        .as_object_mut()
        .expect("object")
        .insert("schema_version".into(), "stateroot.delegation.v2".into());
    std::fs::write(dir.join(format!("{id}.json")), record.to_string()).expect("write record");
}

#[test]
fn bound_running_delegation_counts_as_executor_everywhere() {
    let Some(namespace) = current_namespace() else {
        eprintln!("skipping: namespace identity unsupported on this platform");
        return;
    };
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let plan = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Executed",
    );
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "approve", &plan])
        .assert()
        .success();
    // A live, owned, plan-bound worker (this test process is the live pid).
    write_delegation(
        project.path(),
        serde_json::json!({
            "id": "d-live",
            "plan_id": plan,
            "harness": "codex",
            "task": "bound work",
            "ts": "2026-10-22T00:00:00Z",
            "status": "running",
            "pid": std::process::id(),
            "process_namespace": namespace,
        }),
    );
    // CLI view: running (already true today — the agreement half).
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["delegate", "list"])
        .assert()
        .success();
    let stdout = stdout_of(out);
    assert!(stdout.contains("· running ·"), "stdout: {stdout}");
    // Continuity view (reconciled): the same evidence must count as an
    // executor — no false "no executor" guidance.
    let status = status_json(config_home.path(), user_home.path(), project.path());
    assert!(
        !attention_kinds(project.path()).contains(&"plan_unassigned".to_string()),
        "a bound live worker must suppress plan_unassigned"
    );
    assert_eq!(status["plan"]["directive"], "execute");
}

#[test]
fn foreign_delegation_is_unknown_not_failed_and_not_absent() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let plan = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Foreign",
    );
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "approve", &plan])
        .assert()
        .success();
    // A foreign-namespace worker whose pid coincides with a dead local pid:
    // unknown — never lost/failed, and never proof that no executor exists.
    write_delegation(
        project.path(),
        serde_json::json!({
            "id": "d-foreign",
            "plan_id": plan,
            "harness": "codex",
            "task": "bound work",
            "ts": "2026-10-22T00:00:00Z",
            "status": "running",
            "pid": 4_000_000u32,
            "process_namespace": "another-host-namespace",
        }),
    );
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["delegate", "list"])
        .assert()
        .success();
    let stdout = stdout_of(out);
    assert!(stdout.contains("· unknown ·"), "stdout: {stdout}");
    let _ = status_json(config_home.path(), user_home.path(), project.path());
    let kinds = attention_kinds(project.path());
    assert!(
        !kinds.contains(&"delegation_failed".to_string()),
        "unknown must never become a failure signal: {kinds:?}"
    );
    assert!(
        !kinds.contains(&"plan_unassigned".to_string()),
        "unknown must never prove the absence of an executor: {kinds:?}"
    );
    assert!(
        kinds.contains(&"delegation_status_unknown".to_string()),
        "the honest observation replaces the false guidance: {kinds:?}"
    );
}

#[test]
fn unknown_bound_delegation_does_not_close_an_active_plan() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let plan = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Active",
    );
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "approve", &plan])
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "activate", &plan])
        .assert()
        .success();
    // A plan-bound handoff declaring no remaining work…
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--plan",
            &plan,
            "--objective",
            "wrapped",
            "--task",
            "done",
            "--context-summary",
            "everything recorded",
        ])
        .assert()
        .success();
    // …plus a plan-bound delegation whose status is unknowable here.
    write_delegation(
        project.path(),
        serde_json::json!({
            "id": "d-unverifiable",
            "plan_id": plan,
            "harness": "kimi",
            "task": "bound work",
            "ts": "2026-10-22T00:00:00Z",
            "status": "running",
            "pid": 4_000_000u32,
            "process_namespace": "another-host-namespace",
        }),
    );
    // Unknown must not become proof that the work completed.
    let status = status_json(config_home.path(), user_home.path(), project.path());
    assert_ne!(
        status["plan"]["directive"], "close",
        "unknown evidence must never close a plan"
    );
    assert!(
        !attention_kinds(project.path()).contains(&"plan_closure".to_string()),
        "unknown evidence must never produce closure guidance"
    );
}

#[test]
fn degraded_delegation_records_are_surfaced_not_dropped() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let dir = project.path().join(".stateroot/delegations");
    std::fs::create_dir_all(&dir).expect("delegations dir");
    // Unparseable bytes, an unsupported schema and a record without id —
    // preserved and reported, never silently equated with absence.
    std::fs::write(dir.join("broken.json"), b"{not json".as_slice()).expect("broken");
    std::fs::write(
        dir.join("future.json"),
        serde_json::json!({"schema_version": "stateroot.delegation.v9", "id": "future"})
            .to_string(),
    )
    .expect("future");
    std::fs::write(
        dir.join("noname.json"),
        serde_json::json!({"schema_version": "stateroot.delegation.v2", "status": "running"})
            .to_string(),
    )
    .expect("noname");
    let _ = status_json(config_home.path(), user_home.path(), project.path());
    let kinds = attention_kinds(project.path());
    assert!(
        kinds.contains(&"delegation_record_degraded".to_string()),
        "degraded records must surface as observations: {kinds:?}"
    );
    // Bytes preserved by the read path.
    assert_eq!(
        std::fs::read(dir.join("broken.json")).expect("broken bytes"),
        b"{not json".as_slice()
    );
}
