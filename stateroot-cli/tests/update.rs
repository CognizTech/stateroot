//! Coming-soon gate + auto-update tests (all network mocked via wiremock).

use std::path::Path;

use assert_cmd::Command;
use serde_json::json;
use wiremock::matchers::{method, path};
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

fn seed_config(home: &Path, extra_toml: &str) {
    std::fs::create_dir_all(home).expect("config home");
    std::fs::write(
        home.join("config.toml"),
        format!("user_id = \"default\"\nagent_id = \"default\"\n{extra_toml}"),
    )
    .expect("config.toml");
}

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
}

// --- A: coming-soon gate -------------------------------------------------

#[test]
fn cloud_commands_coming_soon_when_gated_off() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config(config_home.path(), "");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    for args in [
        vec!["login", "--via", "github"],
        vec!["logout"],
        vec!["repo", "status"],
        vec!["sync"],
        vec!["run", "--cloud", "x"],
        vec!["runs", "list"],
    ] {
        let out = stateroot(config_home.path(), user_home.path(), project.path())
            .args(&args)
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
        assert!(
            stdout.contains("coming soon") && stdout.contains("fully local today"),
            "{args:?} must print the coming-soon message: {stdout}"
        );
    }
}

#[test]
fn cloud_preview_flag_enables_real_behavior() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config(config_home.path(), "");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // With the preview on, `sync` runs the real path — which honestly
    // reports the missing repo link (NOT the coming-soon message).
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("STATEROOT_CLOUD_PREVIEW", "1")
        .arg("sync")
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(stderr.contains("not linked"), "{stderr}");
}

// --- B: auto-update ------------------------------------------------------

fn release_body(tag: &str, asset_url: &str, checksums_url: &str) -> serde_json::Value {
    // Include every platform asset name the CLI looks up — Windows CI
    // requests `stateroot-windows-x64.exe`; a linux-only fixture never
    // writes the update cache, so the second check hits the network again.
    json!({
        "tag_name": tag,
        "assets": [
            {"name": "stateroot-linux-x64", "browser_download_url": asset_url},
            {"name": "stateroot-windows-x64.exe", "browser_download_url": asset_url},
            {"name": "stateroot-macos-aarch64", "browser_download_url": asset_url},
            {"name": "checksums.txt", "browser_download_url": checksums_url}
        ]
    })
}

fn update_config_toml() -> String {
    "[update]\nrepo = \"stateroot-dev/stateroot\"\n".to_string()
}

#[tokio::test]
async fn version_check_caches_for_24h() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/stateroot-dev/stateroot/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release_body(
            "v0.1.0",
            "http://x/asset",
            "http://x/checksums.txt",
        )))
        .expect(1)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    seed_config(config_home.path(), &update_config_toml());
    let user_home = tempfile::tempdir().expect("user home");
    let cwd = tempfile::tempdir().expect("cwd");

    for _ in 0..2 {
        stateroot(config_home.path(), user_home.path(), cwd.path())
            .env("STATEROOT_GITHUB_API_BASE", server.uri())
            .args(["self-update", "--check"])
            .assert()
            .success();
    }
    // One network call across two invocations (cache covers the second).
    server.verify().await;
    assert!(config_home.path().join("update-check.json").is_file());
}

#[tokio::test]
async fn updater_never_runs_on_hook_but_runs_on_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/stateroot-dev/stateroot/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release_body(
            "v0.1.0",
            "http://x/asset",
            "http://x/checksums.txt",
        )))
        .expect(1)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    seed_config(config_home.path(), &update_config_toml());
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // hook path: NO update check (harness event flows stay fast).
    stateroot(config_home.path(), user_home.path(), project.path())
        .env("STATEROOT_GITHUB_API_BASE", server.uri())
        .args(["hook", "SessionStart", "--harness", "claude-code"])
        .assert()
        .success();
    // status path: the check fires (once — cached afterwards).
    stateroot(config_home.path(), user_home.path(), project.path())
        .env("STATEROOT_GITHUB_API_BASE", server.uri())
        .arg("status")
        .assert()
        .success();
    server.verify().await; // expect(1): exactly the status call
}

#[tokio::test]
async fn disabled_paths_and_placeholder_repo() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config(config_home.path(), "");
    let user_home = tempfile::tempdir().expect("user home");
    let cwd = tempfile::tempdir().expect("cwd");

    // env opt-out
    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .env("STATEROOT_NO_AUTO_UPDATE", "1")
        .args(["self-update", "--check"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("auto-update is disabled"), "{stdout}");

    // placeholder repo → honest "not configured"
    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .args(["self-update", "--check"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("no public release repo configured"),
        "{stdout}"
    );

    // config opt-out
    seed_config(
        config_home.path(),
        "[update]\nrepo = \"stateroot-dev/stateroot\"\nenabled = false\n",
    );
    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .args(["self-update", "--check"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("auto-update is disabled"), "{stdout}");
}
