//! `stateroot remove` tests — local full-plan removal, dry-run, refusal,
//! stub preservation, and the gated server deletion path (wiremock).

use std::path::Path;

use assert_cmd::Command;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn stateroot(config_home: &Path, user_home: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::cargo_bin("stateroot").expect("binary");
    cmd.env("STATEROOT_HOME", config_home)
        .env("STATEROOT_TEST_HOME", user_home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env("STATEROOT_CREDENTIALS", "file")
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
        json!({"github": {"access_token": "gho_x", "obtained_at": "2026-08-08T00:00:00Z"}})
            .to_string(),
    )
    .expect("creds");
}

fn project_id_of(project: &Path) -> String {
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".stateroot/manifest.json")).expect("manifest"),
    )
    .expect("json");
    manifest["project_id"].as_str().expect("id").to_string()
}

#[test]
fn remove_full_plan_local() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // roots create our git refs
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "x"])
        .assert()
        .success();
    let repo = git2::Repository::open(project.path()).unwrap();
    assert!(repo.refname_to_id("refs/stateroot/latest").is_ok());

    // AGENTS.md with user content around the block (excise case)
    let agents = project.path().join("AGENTS.md");
    let base = std::fs::read_to_string(&agents).unwrap();
    std::fs::write(&agents, format!("# My rules\n\n{base}")).unwrap();
    // a modified stub (kept) and an untouched one (deleted)
    let stub = project.path().join(".cursor/rules/stateroot.mdc");
    assert!(stub.is_file(), "init installs the cursor stub");
    std::fs::write(&stub, "USER EDITED THIS STUB\n").unwrap();
    let claude_stub = project.path().join(".claude/commands/stateroot.md");
    assert!(claude_stub.is_file(), "init installs the claude stub");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["remove", "--yes"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");

    assert!(!project.path().join(".stateroot").exists(), "store gone");
    assert!(
        stdout.contains("unregistered from projects.toml"),
        "{stdout}"
    );
    // registry entry gone
    let registry = std::fs::read_to_string(config_home.path().join("projects.toml")).unwrap();
    assert!(!registry.contains("local-"), "{registry}");
    // git refs gone, repo itself intact
    let repo = git2::Repository::open(project.path()).unwrap();
    assert!(
        repo.refname_to_id("refs/stateroot/latest").is_err(),
        "latest gone"
    );
    assert_eq!(
        repo.references_glob("refs/stateroot/roots/*")
            .unwrap()
            .count(),
        0,
        "root refs gone"
    );
    assert!(stdout.contains("git ref(s)"), "{stdout}");
    // AGENTS.md excised (user content kept)
    let text = std::fs::read_to_string(&agents).unwrap();
    assert!(text.contains("# My rules"), "{text}");
    assert!(!text.contains("stateroot:begin"), "{text}");
    // modified stub kept, byte-identical stub deleted
    assert!(stub.is_file(), "modified stub must be kept");
    assert!(stdout.contains("modified since install"), "{stdout}");
    assert!(!claude_stub.exists(), "untouched stub deleted");
}

#[test]
fn remove_dry_run_touches_nothing() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "x"])
        .assert()
        .success();

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["remove", "--dry-run"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("dry-run — nothing was touched"), "{stdout}");
    assert!(
        stdout.contains("git ref(s) under refs/stateroot/"),
        "{stdout}"
    );
    assert!(project.path().join(".stateroot").is_dir());
    let repo = git2::Repository::open(project.path()).unwrap();
    assert!(repo.refname_to_id("refs/stateroot/latest").is_ok());
}

#[test]
fn remove_refuses_non_interactive_without_yes() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("remove")
        .assert()
        .failure();
    assert!(project.path().join(".stateroot").is_dir());
}

#[tokio::test]
async fn remove_calls_server_delete_when_all_gates_on() {
    let server = MockServer::start().await;
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    seed_token(config_home.path());
    let pid = project_id_of(project.path());

    Mock::given(method("DELETE"))
        .and(path(format!("/stateroot/projects/{pid}")))
        .and(query_param("confirm", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "data": {"deleted": {"tables": {"handoffs": 2, "roots": 5}, "filesystem": 3}}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut cmd = stateroot(config_home.path(), user_home.path(), project.path());
    cmd.env("STATEROOT_CLOUD_PREVIEW", "1")
        .env("STATEROOT_CLOUD_URL", server.uri());
    let out = cmd.args(["remove", "--yes"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("server: project deleted"), "{stdout}");
    assert!(stdout.contains("handoffs: 2"), "{stdout}");
    assert!(stdout.contains("filesystem: 3"), "{stdout}");
    assert!(!project.path().join(".stateroot").exists());
    server.verify().await;
}

#[tokio::test]
async fn remove_is_silently_local_when_preview_off() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .expect(0)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    seed_token(config_home.path());

    let mut cmd = stateroot(config_home.path(), user_home.path(), project.path());
    cmd.env("STATEROOT_CLOUD_URL", server.uri()); // preview OFF (default)
    let out = cmd.args(["remove", "--yes"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(!stdout.contains("server"), "no server mention: {stdout}");
    assert!(!project.path().join(".stateroot").exists());
    server.verify().await;
}

#[tokio::test]
async fn remove_keep_server_state_skips_and_unreachable_warns() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .expect(0)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    seed_token(config_home.path());

    // --keep-server-state: skip even with all gates on
    let mut cmd = stateroot(config_home.path(), user_home.path(), project.path());
    cmd.env("STATEROOT_CLOUD_PREVIEW", "1")
        .env("STATEROOT_CLOUD_URL", server.uri());
    let out = cmd
        .args(["remove", "--yes", "--keep-server-state"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("server: kept (--keep-server-state)"),
        "{stdout}"
    );
    assert!(!project.path().join(".stateroot").exists());
    server.verify().await;

    // unreachable server: warning, local removal completes
    let project2 = tempfile::tempdir().expect("project2");
    init_project(config_home.path(), user_home.path(), project2.path());
    let mut cmd = stateroot(config_home.path(), user_home.path(), project2.path());
    cmd.env("STATEROOT_CLOUD_PREVIEW", "1")
        .env("STATEROOT_CLOUD_URL", "http://127.0.0.1:1");
    let out = cmd.args(["remove", "--yes"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(stderr.contains("server unreachable"), "{stderr}");
    assert!(stdout.contains("removed project"), "{stdout}");
    assert!(!project2.path().join(".stateroot").exists());
}
