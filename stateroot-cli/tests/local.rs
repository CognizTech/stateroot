//! Offline integration tests for the M1 continuity surface — tempdir
//! fixtures only, nothing network-shaped (no mock server anywhere).

use std::path::Path;

use assert_cmd::Command;

fn stateroot(config_home: &Path, user_home: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::cargo_bin("stateroot").expect("binary");
    cmd.env("STATEROOT_HOME", config_home)
        .env("STATEROOT_TEST_HOME", user_home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .current_dir(cwd);
    cmd
}

fn seed_config_home(home: &Path) {
    std::fs::create_dir_all(home).expect("config home");
}

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
    std::fs::write(
        project.join("handoff-input.json"),
        r#"{"objective":"continue the project","task":"continue implementation","context_summary":"The project has captured local state ready for a receiving agent.","next_actions":["Continue from the captured state"],"failures":[]}"#,
    )
    .expect("handoff input");
}

#[test]
fn init_creates_project_and_convenience_layer() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path());
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    std::fs::create_dir_all(project.path().join("sub")).expect("sub");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("init")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("initialized"), "stdout: {stdout}");
    assert!(project.path().join(".stateroot/manifest.json").is_file());
    assert!(project.path().join(".stateroot/first-run.json").is_file());
    assert!(project.path().join(".stateroot/learnings").is_dir());
    assert!(project
        .path()
        .join(".stateroot/project/state.json")
        .is_file());
    let agents = std::fs::read_to_string(project.path().join("AGENTS.md")).expect("AGENTS.md");
    assert!(agents.contains("stateroot"), "AGENTS.md: {agents}");
    // Registry entry exists.
    let registry = std::fs::read_to_string(config_home.path().join("projects.toml"));
    assert!(registry.is_ok(), "projects.toml must exist");

    // Idempotent second run.
    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("init")
        .assert()
        .success();
}

#[test]
fn checkpoint_handoff_resume_and_log_flow() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path());
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // checkpoint
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "checkpoint",
            "--note",
            "wired the parser",
            "--files",
            "src/a.rs",
        ])
        .assert()
        .success();

    // handoff write → show → list → accept
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--to",
            "codex",
            "--input",
            "handoff-input.json",
            "--objective",
            "ship the parser",
        ])
        .assert()
        .success();
    assert!(project
        .path()
        .join(".stateroot/handoffs/current.json")
        .is_file());
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["handoff", "show"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("ship the parser"), "show: {stdout}");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["handoff", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("codex"), "list: {stdout}");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["handoff", "accept", "--by", "codex"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("accepted"), "accept: {stdout}");

    // resume renders the handoff (first delivery this session)
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("resume")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("# StateRoot Resume"), "resume: {stdout}");
    assert!(stdout.contains("ship the parser"), "resume: {stdout}");

    // duplicate resume is deduped; --force reprints
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("resume")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("skipping duplicate"), "dedupe: {stdout}");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("ship the parser"), "force: {stdout}");
    let packet: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.path().join(".stateroot/handoffs/current.json"))
            .expect("handoff"),
    )
    .expect("handoff json");
    assert!(
        !packet["accepted_by"]
            .as_array()
            .is_some_and(|actors| actors.iter().any(|actor| actor == "statesmith")),
        "resume without --harness must not fabricate StateSmith acceptance: {packet}"
    );

    // log shows checkpoint + handoff history
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("log")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("wired the parser"), "log: {stdout}");
    assert!(stdout.contains("## Handoffs (1)"), "log: {stdout}");
}

#[test]
fn status_and_doctor_are_local() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path());
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("status")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("project:"), "status: {stdout}");
    assert!(stdout.contains("federated skills:"), "status: {stdout}");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("doctor")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("all local checks pass"), "doctor: {stdout}");
}

#[test]
fn hook_session_start_injects_digest_once() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path());
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--to",
            "claude",
            "--input",
            "handoff-input.json",
            "--objective",
            "hook demo",
        ])
        .assert()
        .success();

    let first = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "SessionStart", "--harness", "claude-code"])
        .assert()
        .success();
    let first_out = String::from_utf8(first.get_output().stdout.clone()).expect("utf8");
    assert!(
        first_out.contains("hook demo"),
        "first session_start must inject: {first_out}"
    );

    let second = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "SessionStart", "--harness", "claude-code"])
        .assert()
        .success();
    let second_out = String::from_utf8(second.get_output().stdout.clone()).expect("utf8");
    assert!(
        second_out.trim().is_empty() || !second_out.contains("hook demo"),
        "duplicate session_start must not re-inject: {second_out}"
    );

    let marker: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.path().join(".stateroot/local/active-harness.json"))
            .expect("active harness marker"),
    )
    .expect("marker json");
    assert_eq!(marker["harness"], "claude");
}

#[test]
fn handoff_source_attribution_is_explicit_or_locally_observed() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path());
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // Explicit aliases normalize to the canonical packet id.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "Codex",
            "--to",
            "cursor",
            "--input",
            "handoff-input.json",
        ])
        .assert()
        .success();
    let current = project.path().join(".stateroot/handoffs/current.json");
    let packet: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&current).expect("handoff"))
            .expect("handoff json");
    assert_eq!(packet["last_harness"], "codex");
    assert_eq!(packet["created_by_harness"], "codex");
    assert_eq!(packet["recommended_next_harness"], "cursor");
    assert_ne!(packet["last_harness"], "statesmith");

    // Resume records a canonical active marker, which is the fallback source.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--harness", "claude-code", "--force"])
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--to",
            "codex",
            "--input",
            "handoff-input.json",
        ])
        .assert()
        .success();
    let packet: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&current).expect("handoff"))
            .expect("handoff json");
    assert_eq!(packet["last_harness"], "claude");
    assert_eq!(packet["created_by_harness"], "claude");

    // Explicit evidence wins over the active marker.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "cursor",
            "--to",
            "codex",
            "--input",
            "handoff-input.json",
        ])
        .assert()
        .success();
    let packet: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&current).expect("handoff"))
            .expect("handoff json");
    assert_eq!(packet["last_harness"], "cursor");
    assert_eq!(packet["created_by_harness"], "cursor");
}

#[test]
fn handoff_rejects_unknown_and_missing_source() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path());
    let user_home = tempfile::tempdir().expect("user home");

    let missing_project = tempfile::tempdir().expect("missing source project");
    init_project(config_home.path(), user_home.path(), missing_project.path());
    let missing = stateroot(config_home.path(), user_home.path(), missing_project.path())
        .args(["handoff", "write", "--to", "codex"])
        .assert()
        .failure();
    let stderr = String::from_utf8(missing.get_output().stderr.clone()).expect("utf8");
    assert!(stderr.contains("pass --from <harness>"), "stderr: {stderr}");
    assert!(!missing_project
        .path()
        .join(".stateroot/handoffs/current.json")
        .exists());

    let unknown_project = tempfile::tempdir().expect("unknown source project");
    init_project(config_home.path(), user_home.path(), unknown_project.path());
    let unknown = stateroot(config_home.path(), user_home.path(), unknown_project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "not-a-harness",
            "--to",
            "codex",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8(unknown.get_output().stderr.clone()).expect("utf8");
    assert!(
        stderr.contains("unknown handoff source 'not-a-harness'"),
        "stderr: {stderr}"
    );
    assert!(!unknown_project
        .path()
        .join(".stateroot/handoffs/current.json")
        .exists());
}

#[test]
fn resume_refreshes_active_marker_before_deduplicating_output() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path());
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--to",
            "cursor",
            "--input",
            "handoff-input.json",
        ])
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--harness", "codex"])
        .assert()
        .success();

    let marker = project.path().join(".stateroot/local/active-harness.json");
    std::fs::remove_file(&marker).expect("remove marker to test refresh");
    let duplicate = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--harness", "codex"])
        .assert()
        .success();
    let stdout = String::from_utf8(duplicate.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("skipping duplicate"), "stdout: {stdout}");
    let marker: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(marker).expect("refreshed marker"))
            .expect("marker json");
    assert_eq!(marker["harness"], "codex");
}

#[test]
fn import_from_codex_rollout_writes_local_records() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path());
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // Minimal codex rollout fixture (line 1 session_meta, then messages).
    let sessions_dir = user_home.path().join(".codex/sessions/2026/08/07");
    std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
    let rollout = format!(
        "{}\n{}\n{}\n{}\n{}\n",
        serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": "sess-1",
                "cwd": project.path().display().to_string(),
                "timestamp": "2026-08-07T10:00:00Z"
            }
        }),
        serde_json::json!({
            "type": "response_item",
            "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "fix the importer"}]}
        }),
        serde_json::json!({
            "type": "response_item",
            "payload": {"type": "function_call", "name": "exec_command", "arguments": "{\"cmd\":\"cargo test\"}", "call_id": "failed-test"}
        }),
        serde_json::json!({
            "type": "response_item",
            "payload": {"type": "function_call_output", "call_id": "failed-test", "output": "Error: importer regression failed"}
        }),
        serde_json::json!({
            "type": "response_item",
            "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "done, tests pass"}]}
        })
    );
    std::fs::write(sessions_dir.join("rollout-1.jsonl"), rollout).expect("rollout");

    // dry-run writes nothing
    let spool = project.path().join(".stateroot/spool/observations.jsonl");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["import", "--dry-run"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("codex") && stdout.contains("fix the importer"),
        "dry-run: {stdout}"
    );
    assert!(!spool.exists(), "dry-run must not write the spool");

    // real import: spool + episodic + handoff + objective
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("import")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("import"), "import: {stdout}");
    assert!(spool.is_file(), "spool must exist after import");
    let handoff = std::fs::read_to_string(project.path().join(".stateroot/handoffs/current.json"))
        .expect("handoff");
    assert!(handoff.contains("fix the importer"), "handoff: {handoff}");
    let packet: serde_json::Value = serde_json::from_str(&handoff).expect("handoff json");
    assert_eq!(
        packet["failures"],
        serde_json::json!(["Error: importer regression failed"])
    );
    assert!(!packet["implementation_status"]
        .as_str()
        .unwrap_or("")
        .is_empty());
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["handoff", "show"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "Error: importer regression failed",
        ));
    let state = std::fs::read_to_string(project.path().join(".stateroot/project/state.json"))
        .expect("state");
    assert!(state.contains("fix the importer"), "state: {state}");
}

#[test]
fn install_and_setup_skills_are_local() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path());
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // Harness presence via marker dirs.
    std::fs::create_dir_all(user_home.path().join(".codex")).expect(".codex");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("install")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("codex"), "install: {stdout}");
    let agents =
        std::fs::read_to_string(user_home.path().join(".codex/AGENTS.md")).expect("codex AGENTS");
    assert!(
        agents.contains("stateroot:begin") || agents.contains("StateRoot"),
        "block: {agents}"
    );

    // setup skills section: dry-run lists, writes nothing.
    let skill_dir = user_home.path().join(".hermes/skills/research/tavily");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    std::fs::write(skill_dir.join("SKILL.md"), "---\nname: tavily\n---\n").expect("md");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["setup", "--only", "skills", "--dry-run", "--yes"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("would copy"), "setup dry-run: {stdout}");
    assert!(!project.path().join(".stateroot/skills/tavily").exists());

    // real run imports with provenance header
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["setup", "--only", "skills", "--yes"])
        .assert()
        .success();
    let imported =
        std::fs::read_to_string(project.path().join(".stateroot/skills/tavily/SKILL.md"))
            .expect("imported");
    assert!(
        imported.starts_with("<!-- imported from hermes-agent on "),
        "provenance: {imported}"
    );
}
