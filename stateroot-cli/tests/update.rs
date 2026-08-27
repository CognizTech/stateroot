//! Auto-update tests (all network mocked via wiremock).

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
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_BASE")
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
        .expect(3)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    seed_config(config_home.path(), &update_config_toml());
    let user_home = tempfile::tempdir().expect("user home");
    let cwd = tempfile::tempdir().expect("cwd");

    // BACKGROUND path honors the 24h cache: first `status` fetches (cache
    // miss), the second is served from update-check.json (no request).
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    for _ in 0..2 {
        stateroot(config_home.path(), user_home.path(), project.path())
            .env("STATEROOT_GITHUB_API_BASE", server.uri())
            .arg("status")
            .assert()
            .success();
    }
    assert!(config_home.path().join("update-check.json").is_file());

    // EXPLICIT checks always refresh (never report stale metadata): each
    // `--check` is one request regardless of the cache.
    for _ in 0..2 {
        stateroot(config_home.path(), user_home.path(), cwd.path())
            .env("STATEROOT_GITHUB_API_BASE", server.uri())
            .args(["self-update", "--check"])
            .assert()
            .success();
    }
    // Total: 1 (background, first status) + 2 (explicit refreshes).
    server.verify().await;
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

    // hook path: NO update check inline (harness event flows stay fast).
    // The scheduled detached worker is disabled here — by design the hook
    // DOES fire it (v0.1.9), and its timing is nondeterministic against a
    // request-counting mock.
    stateroot(config_home.path(), user_home.path(), project.path())
        .env("STATEROOT_GITHUB_API_BASE", server.uri())
        .env("STATEROOT_DISABLE_SCHEDULED_UPDATE", "1")
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
async fn tagged_self_update_check_hits_release_tag_not_latest() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/stateroot-dev/stateroot/releases/tags/nightly"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release_body(
            "nightly",
            "http://x/asset",
            "http://x/checksums.txt",
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/stateroot-dev/stateroot/releases/latest"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    seed_config(config_home.path(), &update_config_toml());
    let user_home = tempfile::tempdir().expect("user home");
    let cwd = tempfile::tempdir().expect("cwd");

    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .env("STATEROOT_GITHUB_API_BASE", server.uri())
        .args(["self-update", "--check", "--tag", "nightly"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("nightly"), "{stdout}");
    assert!(stdout.contains("rolling preview"), "{stdout}");
    assert!(stdout.contains("self-update --tag nightly"), "{stdout}");
    server.verify().await;
}

#[tokio::test]
async fn production_tag_normalizes_bare_semver() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/stateroot-dev/stateroot/releases/tags/v0.1.2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release_body(
            "v0.1.2",
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

    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .env("STATEROOT_GITHUB_API_BASE", server.uri())
        .args(["self-update", "--check", "--tag", "0.1.2"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("v0.1.2"), "{stdout}");
    assert!(stdout.contains("production"), "{stdout}");
    server.verify().await;
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

    seed_config(
        config_home.path(),
        "[update]\nrepo = \"OWNER/placeholder\"\n",
    );
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
