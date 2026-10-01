//! Active-continuity acceptance: the Telemetry-v2 repro, evidence-gated
//! completion, deterministic reconciliation for every attention kind, and
//! the human/JSON status + digest surfaces.

use std::path::Path;

use assert_cmd::assert::Assert;
use tempfile::TempDir;

fn stateroot(config_home: &Path, user_home: &Path, cwd: &Path) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("stateroot").expect("binary");
    cmd.env("STATEROOT_HOME", config_home)
        .env("STATEROOT_TEST_HOME", user_home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_BASE")
        .current_dir(cwd);
    cmd
}

fn homes() -> (TempDir, TempDir) {
    (
        tempfile::tempdir().expect("config home"),
        tempfile::tempdir().expect("user home"),
    )
}

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
}

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

    // delegation_failed against the open plan (store shape).
    let delegations = project.path().join(".stateroot/delegations");
    std::fs::create_dir_all(&delegations).expect("delegations dir");
    std::fs::write(
        delegations.join("d-fail.json"),
        serde_json::json!({
            "schema": "stateroot.delegation.v2",
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
