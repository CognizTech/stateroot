//! Phase-3 CLI tests — cloud runs against the wiremock contract
//! (agent-21 builds the server side; these pin the client contract).

use std::path::Path;

use assert_cmd::Command;
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn stateroot(config_home: &Path, user_home: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::cargo_bin("stateroot").expect("binary");
    cmd.env("STATEROOT_HOME", config_home)
        .env("STATEROOT_TEST_HOME", user_home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env("STATEROOT_CREDENTIALS", "file")
        .env("STATEROOT_CLOUD_POLL_MS", "1")
        .current_dir(cwd);
    cmd
}

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
}

fn seed_token(config_home: &Path) {
    std::fs::write(
        config_home.join("credentials.json"),
        json!({"github": {"access_token": "gho_cloud", "obtained_at": "2026-08-08T00:00:00Z"}})
            .to_string(),
    )
    .expect("creds");
}

fn project_id_of(project: &Path) -> String {
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".stateroot/manifest.json")).expect("manifest"),
    )
    .expect("manifest json");
    manifest
        .get("project_id")
        .and_then(|v| v.as_str())
        .expect("project_id")
        .to_string()
}

#[tokio::test]
async fn run_cloud_creates_run_with_full_payload() {
    let server = MockServer::start().await;
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    seed_token(config_home.path());
    let pid = project_id_of(project.path());

    Mock::given(method("POST"))
        .and(path(format!("/stateroot/projects/{pid}/cloud-runs")))
        .and(body_partial_json(json!({
            "objective": "port the lexer",
            "from_root": "root-abc",
            "harness": "codex",
            "verification": "cargo test"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "run": {"id": "run-1234567890", "status": "queued"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("STATEROOT_CLOUD_URL", server.uri())
        .args([
            "run",
            "--cloud",
            "port the lexer",
            "--from",
            "root-abc",
            "--harness",
            "codex",
            "--verification",
            "cargo test",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("cloud run run-1234 (queued)"), "{stdout}");
    assert!(stdout.contains("runs status run-1234567890"), "{stdout}");
    server.verify().await;
}

#[tokio::test]
async fn watch_polls_to_terminal_with_event_tail() {
    let server = MockServer::start().await;
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    seed_token(config_home.path());
    let pid = project_id_of(project.path());
    let base = format!("/stateroot/projects/{pid}/cloud-runs");

    Mock::given(method("POST"))
        .and(path(base.clone()))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "run": {"id": "run-watch-1", "status": "queued"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    // running once, then terminal with a result root (first-mounted wins once)
    Mock::given(method("GET"))
        .and(path(format!("{base}/run-watch-1")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "run": {"id": "run-watch-1", "status": "running"}
        })))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{base}/run-watch-1")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "run": {"id": "run-watch-1", "status": "succeeded", "result_root_id": "root-xyz"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{base}/run-watch-1/events")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "events": [
                {"kind": "sandbox", "message": "daytona sandbox ready"},
                {"kind": "agent", "message": "objective received"},
                {"kind": "verify", "message": "cargo test passed (41)"}
            ]
        })))
        .mount(&server)
        .await;

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("STATEROOT_CLOUD_URL", server.uri())
        .args(["run", "--cloud", "ship it", "--watch"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("[verify] cargo test passed"),
        "event tail: {stdout}"
    );
    assert!(stdout.contains("run run-watc → succeeded"), "{stdout}");
    assert!(stdout.contains("result root: root-xyz"), "{stdout}");
    assert!(stdout.contains("sync --pull"), "{stdout}");
    server.verify().await;
}

#[tokio::test]
async fn runs_list_and_status_render() {
    let server = MockServer::start().await;
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    seed_token(config_home.path());
    let pid = project_id_of(project.path());
    let base = format!("/stateroot/projects/{pid}/cloud-runs");

    Mock::given(method("GET"))
        .and(path(base.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "runs": [
                {"id": "run-aaaa1111", "status": "succeeded", "objective": "port the lexer"},
                {"id": "run-bbbb2222", "status": "running", "objective": "extend parser"}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{base}/run-aaaa1111")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "run": {"id": "run-aaaa1111", "status": "succeeded", "objective": "port the lexer", "result_root_id": "root-9"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{base}/run-aaaa1111/events")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "events": [{"kind": "verify", "message": "cargo test passed"}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("STATEROOT_CLOUD_URL", server.uri())
        .args(["runs", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("run-aaaa [succeeded] port the lexer"),
        "{stdout}"
    );
    assert!(
        stdout.contains("run-bbbb [running] extend parser"),
        "{stdout}"
    );

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("STATEROOT_CLOUD_URL", server.uri())
        .args(["runs", "status", "run-aaaa1111"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("status: succeeded"), "{stdout}");
    assert!(stdout.contains("result root: root-9"), "{stdout}");
    assert!(stdout.contains("[verify] cargo test passed"), "{stdout}");
    server.verify().await;
}

#[tokio::test]
async fn cloud_commands_require_login_honestly() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["run", "--cloud", "anything"])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(stderr.contains("requires `stateroot login`"), "{stderr}");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["runs", "list"])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(stderr.contains("requires `stateroot login`"), "{stderr}");
}

#[tokio::test]
async fn create_failure_is_honest() {
    let server = MockServer::start().await;
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    seed_token(config_home.path());
    let pid = project_id_of(project.path());

    Mock::given(method("POST"))
        .and(path(format!("/stateroot/projects/{pid}/cloud-runs")))
        .respond_with(ResponseTemplate::new(503).set_body_string("worker pool exhausted"))
        .expect(1)
        .mount(&server)
        .await;

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("STATEROOT_CLOUD_URL", server.uri())
        .args(["run", "--cloud", "anything"])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(stderr.contains("HTTP 503"), "{stderr}");
    server.verify().await;
}
