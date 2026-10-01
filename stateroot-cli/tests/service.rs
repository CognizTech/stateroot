//! `stateroot service`: single-instance resident loop, registry-wide
//! reconciliation, heartbeat, restart safety, bounded log, and graceful
//! degradation when OS registration is unavailable.

use std::path::Path;
use std::process::{Child, Command};

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

/// Raw spawn (the resident loop never returns, so assert_cmd's run-to-
/// completion API does not fit).
fn spawn_service(config_home: &Path, user_home: &Path, cwd: &Path) -> Child {
    let exe = assert_cmd::cargo::cargo_bin("stateroot");
    Command::new(exe)
        .args(["service", "run"])
        .env("STATEROOT_HOME", config_home)
        .env("STATEROOT_TEST_HOME", user_home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .expect("spawn service")
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

fn heartbeat_path(config_home: &Path) -> std::path::PathBuf {
    config_home.join("continuity-service.heartbeat.json")
}

fn wait_for(predicate: impl Fn() -> bool, what: &str) {
    for _ in 0..200 {
        if predicate() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn service_status_json_reports_unregistered_by_default() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "status", "--json"])
        .assert()
        .success();
    let payload: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("json");
    assert_eq!(payload["schema_version"], "stateroot.service-status.v1");
    assert_eq!(payload["enabled"], true);
    assert_eq!(payload["registered"], false);
    assert_eq!(payload["running"], false);

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "status"])
        .assert()
        .success();
    let human = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(human.contains("registered: no"), "human: {human}");
}

#[test]
fn resident_loop_is_single_instance_registry_wide_and_restart_safe() {
    let (config_home, user_home) = homes();
    let project_a = tempfile::tempdir().expect("project a");
    let project_b = tempfile::tempdir().expect("project b");
    init_project(config_home.path(), user_home.path(), project_a.path());
    init_project(config_home.path(), user_home.path(), project_b.path());

    // First instance: heartbeat appears and BOTH registered projects get a
    // projection — one service reconciles the whole registry.
    let mut first = spawn_service(config_home.path(), user_home.path(), project_a.path());
    wait_for(
        || heartbeat_path(config_home.path()).exists(),
        "first heartbeat",
    );
    wait_for(
        || {
            project_a
                .path()
                .join(".stateroot/local/projections/continuity.v1.json")
                .exists()
                && project_b
                    .path()
                    .join(".stateroot/local/projections/continuity.v1.json")
                    .exists()
        },
        "registry-wide projections",
    );
    let beat: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(heartbeat_path(config_home.path())).expect("heartbeat"),
    )
    .expect("heartbeat json");
    assert_eq!(beat["schema_version"], "stateroot.continuity-heartbeat.v1");
    assert_eq!(beat["pid"], first.id());
    assert!(beat["projects_scanned"].as_u64().unwrap_or(0) >= 2);

    // Second instance: exits immediately (single-instance lock), leaving
    // the first alive.
    let second = spawn_service(config_home.path(), user_home.path(), project_a.path());
    let output = second.wait_with_output().expect("second exits");
    assert!(output.status.success());

    first.kill().expect("kill first");
    let _ = first.wait();

    // Restart-safe: the stale lock from the killed process is reclaimed and
    // a fresh instance heartbeats again.
    let _ = std::fs::remove_file(heartbeat_path(config_home.path()));
    let mut third = spawn_service(config_home.path(), user_home.path(), project_a.path());
    wait_for(
        || heartbeat_path(config_home.path()).exists(),
        "restart heartbeat",
    );
    third.kill().expect("kill third");
    let _ = third.wait();
}

#[test]
fn install_without_os_registration_degrades_gracefully() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // Probes are disabled in tests, so OS registration is unavailable: the
    // service must register the detached fallback and report honestly.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "install"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("detached") || stdout.contains("registered"),
        "install: {stdout}"
    );
    let registration: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            config_home
                .path()
                .join("continuity-service.registration.json"),
        )
        .expect("registration file"),
    )
    .expect("registration json");
    // Probes are disabled: registration calls are skipped everywhere. On
    // native Linux/macOS/Windows CI that degrades to "detached"; under WSL
    // the Windows-host task path is chosen (its schtasks call is skipped
    // too). Either way the file exists and nothing actually ran.
    let kind = registration["kind"].as_str().expect("kind");
    assert!(matches!(kind, "detached" | "wsl-schtasks"), "kind: {kind}");
    if kind == "detached" {
        assert!(registration["detail"]
            .as_str()
            .expect("detail")
            .contains("degraded"));
    }

    // No real detached process was spawned under test probes: status shows
    // registered-but-not-running (degraded coverage, hooks/CLI reconcile).
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "status", "--json"])
        .assert()
        .success();
    let payload: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("json");
    assert_eq!(payload["registered"], true);
    assert_eq!(payload["running"], false);

    // Remove cleans the registration up.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "remove"])
        .assert()
        .success();
    assert!(!config_home
        .path()
        .join("continuity-service.registration.json")
        .exists());
}

#[test]
fn disabled_continuity_refuses_to_run() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    std::fs::write(
        config_home.path().join("config.toml"),
        "[continuity]\nenabled = false\n",
    )
    .expect("config");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "run"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("disabled"), "run: {stdout}");
    assert!(!heartbeat_path(config_home.path()).exists());
}
