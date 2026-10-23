//! Auto-update tests (all network mocked via wiremock).

#[path = "support/mod.rs"]
mod support;

use std::path::Path;

use assert_cmd::Command;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// This suite tests the updater itself — the test-harness marker is
/// RELEASED so the updater stays live here (and only here). The cargo-built
/// binary is a build artifact the automatic updater refuses to touch.
fn stateroot(config_home: &Path, user_home: &Path, cwd: &Path) -> Command {
    let mut cmd = support::stateroot(config_home, user_home, cwd);
    cmd.env_remove("STATEROOT_TEST_CMD_PROBES");
    cmd
}

/// The M4 installed-path fixture (shared in `support`): an installed copy
/// the updater's eligibility admits, in a disposable directory.
fn installed_stateroot(
    install_dir: &Path,
    config_home: &Path,
    user_home: &Path,
    cwd: &Path,
) -> Command {
    support::installed_stateroot(install_dir, config_home, user_home, cwd)
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
async fn polled_views_do_not_wait_for_release_network() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(4)))
        .expect(0)
        .mount(&server)
        .await;
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_config(config_home.path(), "[update]\nenabled = false\n");
    init_project(config_home.path(), user_home.path(), project.path());
    seed_config(config_home.path(), &update_config_toml());
    // The detached worker has its own scheduling tests. Suppress it here
    // so this test proves that no foreground request holds a poll open.
    for args in [vec!["delegate", "list"], vec!["log", "--json"]] {
        stateroot(config_home.path(), user_home.path(), project.path())
            .env("STATEROOT_GITHUB_API_BASE", server.uri())
            .env("STATEROOT_DISABLE_SCHEDULED_UPDATE", "1")
            .env("STATEROOT_TEST_BUILD_VERSION", "0.1.12")
            .timeout(std::time::Duration::from_secs(2))
            .args(args)
            .assert()
            .success();
    }
    server.verify().await;
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
    // The seam poses as a production build — dev builds check the nightly
    // channel instead (background_update_never_crosses_channels).
    // The automatic route only runs from an eligible executable: an
    // "installed" copy, never the cargo artifact under test.
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let install = tempfile::tempdir().expect("install dir");
    for _ in 0..2 {
        installed_stateroot(
            install.path(),
            config_home.path(),
            user_home.path(),
            project.path(),
        )
        .env("STATEROOT_GITHUB_API_BASE", server.uri())
        .env("STATEROOT_TEST_BUILD_VERSION", "0.1.12")
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
            .env("STATEROOT_TEST_BUILD_VERSION", "0.1.12")
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
        .env("STATEROOT_TEST_BUILD_VERSION", "0.1.12")
        .args(["hook", "SessionStart", "--harness", "claude-code"])
        .assert()
        .success();
    // status path: the check fires (once — cached afterwards). The
    // automatic route requires an eligible executable, so `status` runs
    // from the installed copy while the hook keeps the cargo artifact.
    let install = tempfile::tempdir().expect("install dir");
    installed_stateroot(
        install.path(),
        config_home.path(),
        user_home.path(),
        project.path(),
    )
    .env("STATEROOT_GITHUB_API_BASE", server.uri())
    .env("STATEROOT_TEST_BUILD_VERSION", "0.1.12")
    .arg("status")
    .assert()
    .success();
    server.verify().await; // expect(1): exactly the status call
}

#[tokio::test]
async fn background_update_never_crosses_channels() {
    // The demo-day revert bug: a dev/nightly build whose base predates the
    // newest prod tag was silently replaced by the PROD release on the first
    // user-facing command. The background updater is channel-strict now:
    // a dev build checks only the rolling preview — prod is never consulted.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/stateroot-dev/stateroot/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release_body(
            "v9.9.9",
            "http://127.0.0.1:1/asset",
            "http://127.0.0.1:1/checksums.txt",
        )))
        .expect(0)
        .mount(&server)
        .await;
    let mut nightly = release_body(
        "nightly",
        "http://127.0.0.1:1/asset",
        "http://127.0.0.1:1/checksums.txt",
    );
    nightly["name"] = json!("StateRoot 0.1.13-dev.999 (rolling preview)");
    Mock::given(method("GET"))
        .and(path("/repos/stateroot-dev/stateroot/releases/tags/nightly"))
        .respond_with(ResponseTemplate::new(200).set_body_json(nightly))
        .expect(1)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    seed_config(config_home.path(), &update_config_toml());
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // A dev build runs a user-facing command: the background updater checks
    // the rolling preview (never prod), finds 0.1.13-dev.999 > 0.1.13-dev.100,
    // and attempts the nightly asset (download fails against the stub URL —
    // silently, by design). The command runs from an installed copy: the
    // automatic route refuses the cargo artifact before any lookup.
    let install = tempfile::tempdir().expect("install dir");
    installed_stateroot(
        install.path(),
        config_home.path(),
        user_home.path(),
        project.path(),
    )
    .env("STATEROOT_GITHUB_API_BASE", server.uri())
    .env("STATEROOT_TEST_BUILD_VERSION", "0.1.13-dev.100")
    .arg("status")
    .assert()
    .success();
    server.verify().await;
    assert!(
        config_home
            .path()
            .join("update-check-nightly.json")
            .is_file(),
        "the nightly check cached"
    );
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

#[tokio::test]
async fn test_marker_suppresses_the_awaited_automatic_route() {
    // Every other suite carries STATEROOT_TEST_CMD_PROBES; the awaited
    // post-dispatch route must honor it exactly like the detached worker.
    // An ELIGIBLE (installed) binary is used, so only the marker can explain
    // the silence.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let config_home = tempfile::tempdir().expect("config home");
    seed_config(config_home.path(), &update_config_toml());
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    std::fs::create_dir_all(project.path()).expect("project dir");
    let install = tempfile::tempdir().expect("install dir");
    for args in [vec!["init"], vec!["status"]] {
        installed_stateroot(
            install.path(),
            config_home.path(),
            user_home.path(),
            project.path(),
        )
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env("STATEROOT_GITHUB_API_BASE", server.uri())
        .env("STATEROOT_TEST_BUILD_VERSION", "0.1.12")
        .args(args)
        .assert()
        .success();
    }
    server.verify().await;
}

#[tokio::test]
async fn automatic_route_from_a_cargo_artifact_makes_no_requests() {
    // Even with the updater fully live (marker released), automatic
    // maintenance from a cargo build artifact returns before any release
    // lookup.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let config_home = tempfile::tempdir().expect("config home");
    seed_config(config_home.path(), &update_config_toml());
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    stateroot(config_home.path(), user_home.path(), project.path())
        .env("STATEROOT_GITHUB_API_BASE", server.uri())
        .env("STATEROOT_TEST_BUILD_VERSION", "0.1.12")
        .arg("status")
        .assert()
        .success();
    server.verify().await;
}

#[tokio::test]
async fn explicit_self_update_from_a_cargo_artifact_refuses_before_downloading() {
    // An explicit install from a build artifact still inspects the release
    // metadata (the user asked), but the refusal lands before any asset or
    // checksum request — and before any download temp file exists.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/stateroot-dev/stateroot/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release_body(
            "v9.9.9",
            &format!("{}/asset", server.uri()),
            &format!("{}/checksums.txt", server.uri()),
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/asset"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/checksums.txt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    seed_config(config_home.path(), &update_config_toml());
    let user_home = tempfile::tempdir().expect("user home");
    let cwd = tempfile::tempdir().expect("cwd");

    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .env("STATEROOT_GITHUB_API_BASE", server.uri())
        .env("STATEROOT_TEST_BUILD_VERSION", "0.1.12")
        .args(["self-update"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("refusing to self-replace a cargo build artifact"),
        "{stdout}"
    );
    server.verify().await;
}

#[tokio::test]
async fn polled_view_from_a_cargo_artifact_spawns_no_update_worker() {
    // The detached route shares the same eligibility: from a cargo artifact
    // no worker is spawned and no lock file is written — with no scheduling
    // seam involved.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let config_home = tempfile::tempdir().expect("config home");
    seed_config(config_home.path(), &update_config_toml());
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    stateroot(config_home.path(), user_home.path(), project.path())
        .env("STATEROOT_GITHUB_API_BASE", server.uri())
        .env("STATEROOT_TEST_BUILD_VERSION", "0.1.12")
        .args(["log", "--json"])
        .assert()
        .success();
    assert!(
        !config_home.path().join("update-in-progress").exists(),
        "no update worker spawned from a cargo build artifact"
    );
    server.verify().await;
}
