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

#[cfg(target_os = "linux")]
#[test]
fn foreign_manager_descriptor_causes_zero_mutations_and_retains_controls() {
    use std::os::unix::fs::PermissionsExt as _;
    for loaded_foreign in [false, true] {
        let (config_home, user_home) = homes();
        let project = tempfile::tempdir().unwrap();
        init_project(config_home.path(), user_home.path(), project.path());
        stateroot(config_home.path(), user_home.path(), project.path())
            .args(["service", "install"])
            .assert()
            .success();
        let registration = config_home
            .path()
            .join("continuity-service.registration.json");
        let mut reg: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registration).unwrap()).unwrap();
        reg["kind"] = serde_json::json!("systemd-user");
        std::fs::write(&registration, serde_json::to_vec(&reg).unwrap()).unwrap();
        let xdg = user_home.path().join("xdg");
        let dir = xdg.join("systemd/user");
        std::fs::create_dir_all(&dir).unwrap();
        let unit = dir.join("stateroot-continuity.service");
        let text = if loaded_foreign {
            format!("[Unit]\nDescription=StateRoot continuity service (deterministic local reconciliation)\n\n[Service]\nType=simple\nEnvironment=\"STATEROOT_HOME={}\"\nExecStart=\"{}\" service run\nRestart=on-failure\nRestartSec=10\n\n[Install]\nWantedBy=default.target\n",config_home.path().display(),reg["exe"].as_str().unwrap())
        } else {
            "foreign descriptor; mentions stateroot but is not ours\n".into()
        };
        std::fs::write(&unit, &text).unwrap();
        let bin = user_home.path().join("fake-bin");
        std::fs::create_dir(&bin).unwrap();
        let manager = bin.join("systemctl");
        std::fs::write(&manager,"#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FIXTURE_MANAGER_LOG\"\ncase \"$*\" in\n *FragmentPath*) printf '%s\\n' \"$FIXTURE_UNIT\";;\n *ExecStart*) echo '{ path=/foreign/exe ; argv[]=/foreign/exe service run ; }';;\n *Environment*) printf 'STATEROOT_HOME=%s\\n' \"$STATEROOT_HOME\";;\n *is-system-running*) echo running;;\n *) echo MUTATION >> \"$FIXTURE_MANAGER_LOG\";exit 0;;\nesac\n").unwrap();
        std::fs::set_permissions(&manager, std::fs::Permissions::from_mode(0o700)).unwrap();
        let log = user_home.path().join("manager.log");
        let initial = std::fs::read(&registration).unwrap();
        for command in ["start", "stop", "remove", "install"] {
            let mut cmd = stateroot(config_home.path(), user_home.path(), project.path());
            cmd.env_remove("STATEROOT_TEST_CMD_PROBES")
                .env("PATH", &bin)
                .env("HOME", user_home.path())
                .env("XDG_CONFIG_HOME", &xdg)
                .env("FIXTURE_MANAGER_LOG", &log)
                .env("FIXTURE_UNIT", &unit)
                .env("STATEROOT_NO_AUTO_UPDATE", "1")
                .args(["service", command])
                .assert()
                .failure();
            assert_eq!(std::fs::read(&unit).unwrap(), text.as_bytes());
            assert_eq!(std::fs::read(&registration).unwrap(), initial);
        }
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(!calls.contains("MUTATION"), "{calls}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn same_path_replacement_rearms_verified_predecessor() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().unwrap();
    init_project(config_home.path(), user_home.path(), project.path());
    let executable = user_home.path().join("installed-stateroot");
    std::fs::copy(assert_cmd::cargo::cargo_bin("stateroot"), &executable).unwrap();
    let installed_cmd = || {
        let mut c = assert_cmd::Command::new(&executable);
        c.current_dir(project.path())
            .env("STATEROOT_HOME", config_home.path())
            .env("STATEROOT_TEST_HOME", user_home.path())
            .env("STATEROOT_TEST_CMD_PROBES", "")
            .env("STATEROOT_NO_AUTO_UPDATE", "1");
        c
    };
    installed_cmd()
        .args(["service", "install"])
        .assert()
        .success();
    let registration = config_home
        .path()
        .join("continuity-service.registration.json");
    let before: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registration).unwrap()).unwrap();
    let mut predecessor = Command::new(&executable)
        .args(["service", "run"])
        .env("STATEROOT_HOME", config_home.path())
        .env("STATEROOT_TEST_HOME", user_home.path())
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env("STATEROOT_NO_AUTO_UPDATE", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_for(
        || heartbeat_path(config_home.path()).exists(),
        "owned predecessor heartbeat",
    );
    let replacement = user_home.path().join("replacement");
    std::fs::copy(assert_cmd::cargo::cargo_bin("stateroot"), &replacement).unwrap();
    std::fs::rename(&replacement, &executable).unwrap();
    installed_cmd()
        .args(["service", "install"])
        .assert()
        .success();
    wait_for(
        || predecessor.try_wait().unwrap().is_some(),
        "same-path predecessor exit",
    );
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registration).unwrap()).unwrap();
    assert_eq!(before["exe"], after["exe"]);
    assert_ne!(before["exe_identity"], after["exe_identity"]);
    assert_eq!(
        after["config_home"],
        config_home.path().display().to_string()
    );
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

fn wait_for(mut predicate: impl FnMut() -> bool, what: &str) {
    for _ in 0..200 {
        if predicate() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn copied_actual_heartbeat_from_other_config_never_authorizes_signal() {
    let (first_home, user_home) = homes();
    let second_home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init_project(first_home.path(), user_home.path(), project.path());
    stateroot(first_home.path(), user_home.path(), project.path())
        .args(["service", "install"])
        .assert()
        .success();
    stateroot(second_home.path(), user_home.path(), project.path())
        .args(["service", "install"])
        .assert()
        .success();
    let mut first = spawn_service(first_home.path(), user_home.path(), project.path());
    wait_for(
        || heartbeat_path(first_home.path()).exists(),
        "owned heartbeat in first config",
    );
    let copied = std::fs::read(heartbeat_path(first_home.path())).unwrap();
    std::fs::write(heartbeat_path(second_home.path()), &copied).unwrap();
    let out = stateroot(second_home.path(), user_home.path(), project.path())
        .args(["service", "stop"])
        .assert()
        .failure();
    assert!(String::from_utf8_lossy(&out.get_output().stderr).contains("ownership unverified"));
    assert!(
        first.try_wait().unwrap().is_none(),
        "other-config process must survive"
    );
    assert_eq!(
        std::fs::read(heartbeat_path(second_home.path())).unwrap(),
        copied
    );
    let out = stateroot(second_home.path(), user_home.path(), project.path())
        .args(["service", "status", "--json"])
        .assert()
        .success();
    let status: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(status["running"], false);
    stateroot(first_home.path(), user_home.path(), project.path())
        .args(["service", "stop"])
        .assert()
        .success();
    wait_for(
        || first.try_wait().unwrap().is_some(),
        "owned first-config exit",
    );
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
    // Probes are disabled: registration calls are skipped everywhere. The
    // descriptor chosen is platform-shaped — detached (native Linux/macOS),
    // wsl-schtasks (WSL), schtasks (native Windows) — and none of them
    // actually ran anything.
    let kind = registration["kind"].as_str().expect("kind");
    assert!(
        matches!(kind, "detached" | "wsl-schtasks" | "schtasks"),
        "kind: {kind}"
    );
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

// ---------------------------------------------------------------------
// WS3 C5/C6: honest lock taxonomy, registration validation, pid ownership
// ---------------------------------------------------------------------

fn registration_path(config_home: &Path) -> std::path::PathBuf {
    config_home.join("continuity-service.registration.json")
}

/// A live lock holder is reported as already-running; unverifiable
/// contention and I/O failures are NEVER already-running (C5 error
/// taxonomy), and each OS's real taxonomy is asserted honestly.
#[test]
fn run_distinguishes_live_lock_from_io_failure() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // (a) Live owner: this test process holds the lock; the service must
    // exit 0 saying already-running.
    let lock = config_home.path().join("continuity-service.lock");
    let _guard = stateroot_core::safe_io::ResourceLock::acquire_with_budget(&lock, 1, 1)
        .expect("acquire lock");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "run"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("already running"), "run: {stdout}");
    assert!(!heartbeat_path(config_home.path()).exists());
    drop(_guard);

    // (b) Unverifiable owner, cross-platform: a regular FILE with a
    // malformed owner record occupies the lock path. `create_new` fails with
    // AlreadyExists on every OS, the owner record can never be parsed, and
    // the service stays fail-closed (does not start) yet must NOT claim
    // already-running.
    std::fs::remove_file(&lock).ok();
    std::fs::write(&lock, "not a lock owner record").expect("malformed owner file");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "run"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("unverifiable"),
        "held-unverifiable: {stdout}"
    );
    assert!(
        !stdout.contains("already running"),
        "an unverifiable owner is never already-running: {stdout}"
    );
    std::fs::remove_file(&lock).expect("cleanup malformed owner");

    // (c) A DIRECTORY at the lock path — the taxonomy differs per OS and
    // each is asserted as itself, never as already-running:
    // - unix: `create_new` reports AlreadyExists → contention with an owner
    //   record that can never be read → held-unverifiable (exit 0).
    // - Windows: `create_new` reports AccessDenied → an outright lock I/O
    //   failure → the run FAILS (never a silent skip).
    std::fs::create_dir_all(&lock).expect("lock as dir");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "run"])
        .assert();
    if cfg!(windows) {
        let out = out.failure();
        let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
        let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
        assert!(
            stderr.contains("lock I/O failure") || stdout.contains("lock I/O failure"),
            "windows maps a directory lock to an I/O failure: {stdout} {stderr}"
        );
        assert!(
            !stdout.contains("already running"),
            "an I/O failure is never already-running: {stdout}"
        );
    } else {
        let out = out.success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
        assert!(
            stdout.contains("unverifiable"),
            "unix maps a directory lock to unverifiable contention: {stdout}"
        );
        assert!(
            !stdout.contains("already running"),
            "unverifiable contention is never already-running: {stdout}"
        );
    }
    std::fs::remove_dir(&lock).expect("cleanup lock dir");

    // (d) True I/O failure: the config dir is read-only, so creating the
    // lock errors — the run FAILS and never says already-running.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = config_home.path();
        let original = std::fs::metadata(dir).expect("meta").permissions();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        let probe = std::fs::File::create(dir.join("probe-write"));
        if probe.is_ok() {
            // Filesystem ignored the mode (root, some mounts) — no way to
            // simulate EACCES here.
            std::fs::remove_file(dir.join("probe-write")).ok();
            std::fs::set_permissions(dir, original).ok();
            return;
        }
        let out = stateroot(config_home.path(), user_home.path(), project.path())
            .args(["service", "run"])
            .assert()
            .failure();
        let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
        let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
        assert!(
            stderr.contains("lock I/O failure") || stdout.contains("lock I/O failure"),
            "io failure must surface as an error: {stdout} {stderr}"
        );
        assert!(
            !stdout.contains("already running"),
            "an I/O failure is never already-running: {stdout}"
        );
        std::fs::set_permissions(dir, original).expect("restore");
    }
}

/// Install validates the registration against the selected binary and
/// config home (C5); a drifted exe re-registers (C6 rearm path).
#[test]
fn install_records_and_revalidates_the_selected_binary() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "install"])
        .assert()
        .success();
    let reg: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(registration_path(config_home.path())).expect("registration"),
    )
    .expect("registration json");
    let exe = assert_cmd::cargo::cargo_bin("stateroot");
    assert_eq!(reg["exe"].as_str().expect("exe"), exe.display().to_string());
    assert_eq!(
        reg["config_home"].as_str().expect("config_home"),
        config_home.path().display().to_string()
    );

    // Idempotent: a second install with the same binary does not rewrite.
    let before = std::fs::read_to_string(registration_path(config_home.path())).expect("reg");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "install"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("already registered"), "install: {stdout}");
    assert_eq!(
        std::fs::read_to_string(registration_path(config_home.path())).expect("reg"),
        before
    );

    // A different build stamp at the SAME path requires rearm; manager
    // ownership must stay internally consistent, not forge a foreign action.
    let mut drifted: serde_json::Value = serde_json::from_str(&before).expect("reg");
    drifted["build_version"] = serde_json::json!("previous-build-fixture");
    std::fs::write(
        registration_path(config_home.path()),
        serde_json::to_string_pretty(&drifted).expect("json"),
    )
    .expect("write drifted");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "install"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("re-registering"), "install: {stdout}");
    let reg: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(registration_path(config_home.path())).expect("registration"),
    )
    .expect("registration json");
    assert_eq!(reg["exe"].as_str().expect("exe"), exe.display().to_string());
}

/// stop() signals only a VERIFIED service pid (C6): a live foreign pid in
/// the heartbeat (a reused number) is never killed, and stop says so.
#[cfg(unix)]
#[test]
fn stop_never_signals_an_unverified_pid() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // A live process that is NOT a stateroot service.
    let mut sleeper = Command::new("sleep")
        .arg("30")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("sleep");
    let foreign_pid = sleeper.id();

    // Register detached + heartbeat pointing at the foreign pid.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "install"])
        .assert()
        .success();
    std::fs::write(
        heartbeat_path(config_home.path()),
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": "stateroot.continuity-heartbeat.v1",
            "pid": foreign_pid,
            "beat_at": "2026-10-09T00:00:00Z",
            "version": "test",
            "exe": "",
        }))
        .expect("json"),
    )
    .expect("heartbeat");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "stop"])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stderr.contains("ownership unverified") || stdout.contains("ownership unverified"),
        "stop must refuse blind signalling: {stdout} {stderr}"
    );
    // Unknown stop retains evidence/control rather than enabling a restart
    // to duplicate an instance whose ownership was not established.
    assert!(
        sleeper.try_wait().expect("wait").is_none(),
        "foreign pid must not be signalled"
    );
    assert!(heartbeat_path(config_home.path()).exists());
    sleeper.kill().expect("kill sleep");
    let _ = sleeper.wait();
}

/// stop() on a VERIFIED service pid terminates it within the shutdown bound
/// and clears the heartbeat (orphan prevention, C6).
#[test]
fn stop_terminates_verified_service_and_clears_heartbeat() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let mut service = spawn_service(config_home.path(), user_home.path(), project.path());
    wait_for(|| heartbeat_path(config_home.path()).exists(), "heartbeat");
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "stop"])
        .assert()
        .success();
    wait_for(
        || service.try_wait().expect("wait").is_some(),
        "service exit",
    );
    assert!(!heartbeat_path(config_home.path()).exists());
}

/// Update-rearm shape (C6): a drifted registration re-registers AND the
/// running old-binary instance is replaced through the verified stop path —
/// never two instances, never an orphan.
#[test]
fn rearm_replaces_the_running_instance() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // Register detached + start a real service instance.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "install"])
        .assert()
        .success();
    let mut first = spawn_service(config_home.path(), user_home.path(), project.path());
    wait_for(
        || heartbeat_path(config_home.path()).exists(),
        "first heartbeat",
    );

    // The build stamp drifts while the verified manager action remains ours.
    let reg_path = registration_path(config_home.path());
    let mut reg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&reg_path).expect("reg")).expect("json");
    reg["build_version"] = serde_json::json!("previous-build-fixture");
    std::fs::write(&reg_path, serde_json::to_string_pretty(&reg).expect("json")).expect("write");

    // Install again: re-register + the old instance is stopped (verified
    // pid), and the heartbeat is cleared for the new instance to claim.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["service", "install"])
        .assert()
        .success();
    wait_for(|| service_pid_gone(&mut first), "old instance exit");
    let reg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&reg_path).expect("reg")).expect("json");
    let exe = assert_cmd::cargo::cargo_bin("stateroot");
    assert_eq!(reg["exe"].as_str().expect("exe"), exe.display().to_string());
}

/// Whether the spawned child has exited (test-local reaping probe).
fn service_pid_gone(child: &mut Child) -> bool {
    child.try_wait().expect("wait").is_some()
}
