//! `stateroot plan` tests — central plan artifacts, lifecycle, digest
//! directives, and handoff plan_ref. Hermetic homes/projects only.

use std::path::Path;

use assert_cmd::Command;

fn stateroot(config_home: &Path, user_home: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::cargo_bin("stateroot").expect("binary");
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

fn homes() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_home = tempfile::tempdir().expect("config home");
    std::fs::create_dir_all(config_home.path()).expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    (config_home, user_home)
}

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    std::fs::write(project.join("README.md"), "# Demo\n\nA demo project.\n").expect("readme");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
}

/// Record a plan from a file; returns the plan id parsed from stdout.
fn record_plan(
    config_home: &Path,
    user_home: &Path,
    project: &Path,
    title: &str,
    body: &str,
) -> String {
    let plan_file = project.join("plan-input.md");
    std::fs::write(&plan_file, body).expect("plan file");
    let out = stateroot(config_home, user_home, project)
        .args([
            "plan",
            "record",
            "--file",
            "plan-input.md",
            "--title",
            title,
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("(draft)"), "stdout: {stdout}");
    stdout
        .strip_prefix("recorded plan ")
        .and_then(|rest| rest.split(' ').next())
        .expect("plan id in stdout")
        .to_string()
}

fn episodic(project: &Path) -> String {
    std::fs::read_to_string(project.join(".stateroot/memories/episodic.jsonl")).expect("episodic")
}

#[test]
fn plan_lifecycle_walk_and_episodic_notes() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let id = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Ship the Parser",
        "# Ship the Parser\n\n1. tokenize\n2. parse\n",
    );
    assert!(id.starts_with("plan_"));
    assert!(project
        .path()
        .join(format!(".stateroot/plans/{id}.md"))
        .is_file());
    assert!(project
        .path()
        .join(format!(".stateroot/plans/{id}.json"))
        .is_file());

    // show prints the verbatim body.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "show", &id])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("# Ship the Parser"), "show: {stdout}");
    assert!(stdout.contains("2. parse"), "show: {stdout}");

    // list shows id/title/status/harness.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains(&format!("{id} · Ship the Parser · draft · cli")),
        "list: {stdout}"
    );

    // approve → activate → done.
    for (action, status) in [
        ("approve", "approved"),
        ("activate", "active"),
        ("done", "done"),
    ] {
        let out = stateroot(config_home.path(), user_home.path(), project.path())
            .args(["plan", action, &id])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
        assert!(
            stdout.contains(&format!("plan {id} → {status}")),
            "{action}: {stdout}"
        );
    }
    let episodic = episodic(project.path());
    for status in ["approved", "active", "done"] {
        assert!(
            episodic.contains(&format!("plan {id} {status} by cli")),
            "episodic: {episodic}"
        );
    }
    assert!(
        episodic.contains(&format!("plan {id} recorded: Ship the Parser")),
        "{episodic}"
    );
}

#[test]
fn plan_one_active_demotes_with_note_and_illegal_transitions_error() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let first = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "First",
        "# First\n",
    );
    let second = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Second",
        "# Second\n",
    );

    // done on a draft is illegal; unknown id errors.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "done", &first])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(stderr.contains("cannot move to done"), "stderr: {stderr}");
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "approve", "no-such-plan"])
        .assert()
        .failure();

    for id in [&first, &second] {
        stateroot(config_home.path(), user_home.path(), project.path())
            .args(["plan", "approve", id])
            .assert()
            .success();
    }
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "activate", &first])
        .assert()
        .success();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "activate", &second])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains(&format!("{first} demoted to approved")),
        "stdout: {stdout}"
    );

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains(&format!("{first} · First · approved")),
        "list: {stdout}"
    );
    assert!(
        stdout.contains(&format!("{second} · Second · active")),
        "list: {stdout}"
    );

    // abandon works from a non-terminal state.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "abandon", &first])
        .assert()
        .success();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains(&format!("{first} · First · abandoned")),
        "list: {stdout}"
    );
}

#[test]
fn plan_digest_directives_and_body_omission() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // Draft-only → planner directive.
    let id = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Refactor Auth",
        "# Refactor Auth\n\nTHE-PLAN-BODY-MUST-NOT-APPEAR\n",
    );
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("## Active Plan"), "digest: {stdout}");
    assert!(
        stdout.contains("refine the plan file; do not implement yet"),
        "digest: {stdout}"
    );
    assert!(
        stdout.contains(&format!(".stateroot/plans/{id}.md")),
        "digest: {stdout}"
    );
    assert!(
        !stdout.contains("THE-PLAN-BODY-MUST-NOT-APPEAR"),
        "digest: {stdout}"
    );

    // Approved → executor directive.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "approve", &id])
        .assert()
        .success();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("Execute it as written; do not re-plan or re-explore"),
        "digest: {stdout}"
    );
    assert!(
        stdout.contains("**Refactor Auth** (approved) — planned by cli"),
        "digest: {stdout}"
    );
    assert!(
        !stdout.contains("THE-PLAN-BODY-MUST-NOT-APPEAR"),
        "digest: {stdout}"
    );
    // The fallback transcript Plan State is suppressed while a central plan exists
    // (nothing seeds plan_state here, so it must simply stay absent).
    assert!(!stdout.contains("## Plan State"), "digest: {stdout}");
}

#[test]
fn plan_handoff_write_attaches_plan_ref() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    std::fs::write(
        project.path().join("handoff-input.json"),
        r#"{"objective":"continue","task":"continue","context_summary":"state captured","next_actions":["continue"]}"#,
    )
    .expect("handoff input");

    // No plan → no plan_ref.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--input",
            "handoff-input.json",
        ])
        .assert()
        .success();
    let packet: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.path().join(".stateroot/handoffs/current.json"))
            .expect("handoff"),
    )
    .expect("json");
    assert!(packet.get("plan_ref").is_none(), "packet: {packet}");

    // Approved plan → plan_ref attached.
    let id = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Pack It",
        "# Pack It\n",
    );
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "approve", &id])
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--input",
            "handoff-input.json",
        ])
        .assert()
        .success();
    let packet: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.path().join(".stateroot/handoffs/current.json"))
            .expect("handoff"),
    )
    .expect("json");
    assert_eq!(packet["plan_ref"]["id"], serde_json::json!(id));
    assert_eq!(packet["plan_ref"]["title"], "Pack It");
    assert_eq!(packet["plan_ref"]["status"], "approved");

    // Activated → plan_ref follows the lifecycle.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "activate", &id])
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--input",
            "handoff-input.json",
        ])
        .assert()
        .success();
    let packet: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.path().join(".stateroot/handoffs/current.json"))
            .expect("handoff"),
    )
    .expect("json");
    assert_eq!(packet["plan_ref"]["status"], "active");
}

#[test]
fn plan_record_from_stdin_and_title_from_heading() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "record", "--stdin"])
        .write_stdin("# Streamed Plan\n\nvia stdin\n")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("recorded plan plan_"), "stdout: {stdout}");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("Streamed Plan"), "list: {stdout}");

    // Empty stdin is refused.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "record", "--stdin"])
        .write_stdin("  \n")
        .assert()
        .failure();
}
