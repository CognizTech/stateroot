//! Doctor hook-binary checks end to end — the Cursor-on-Windows incident
//! shape: hooks.json points at a bare `stateroot` that resolves to a stale
//! binary on PATH; doctor must name the version (fail-open hooks never do).

#[cfg(unix)]
use std::path::Path;

#[cfg(unix)]
use assert_cmd::Command;

#[cfg(unix)] // only the unix-gated exec-stub tests invoke the binary
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

/// Temp `bin/` with an executable bare `stateroot` stub printing the given
/// version on `--version`; returns (dir, PATH) with the dir prepended.
#[cfg(unix)]
fn stub_on_path(version: &str) -> (tempfile::TempDir, String) {
    let bin = tempfile::tempdir().expect("bin");
    let stub = bin.path().join("stateroot");
    std::fs::write(&stub, format!("#!/bin/sh\necho 'stateroot {version}'\n")).expect("stub");
    std::fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").expect("PATH")
    );
    (bin, path)
}

#[cfg(unix)]
fn write_cursor_hooks(user_home: &Path) {
    let config = user_home.join(".cursor/hooks.json");
    std::fs::create_dir_all(config.parent().expect("parent")).expect("mkdir");
    let doc = serde_json::json!({
        "version": 1,
        "hooks": {
            "sessionStart": [{"type": "command", "command": "stateroot hook session_start --harness cursor", "matcher": ""}],
            "stop": [{"type": "command", "command": "stateroot hook stop --harness cursor", "matcher": ""}]
        }
    });
    std::fs::write(&config, serde_json::to_string_pretty(&doc).expect("json")).expect("hooks");
}

#[cfg(unix)]
#[test]
fn doctor_flags_a_stale_hook_binary_by_name() {
    let config_home = tempfile::tempdir().expect("config home");
    std::fs::create_dir_all(config_home.path()).expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let cwd = tempfile::tempdir().expect("cwd");
    write_cursor_hooks(user_home.path());
    let (_bin, path) = stub_on_path("0.1.1");

    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .env("PATH", &path)
        .env("STATEROOT_TEST_CMD_PROBES", "stateroot")
        .arg("doctor")
        .assert()
        .success(); // warnings never hard-fail
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("hook binary (cursor)"), "stdout: {stdout}");
    assert!(
        stdout.contains("cursor hook binary is stateroot 0.1.1"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("self-update"), "stdout: {stdout}");
    // The duplicate event wiring is deduped to one line per harness/binary.
    assert_eq!(
        stdout.matches("hook binary (cursor)").count(),
        1,
        "stdout: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn doctor_oks_a_current_hook_binary() {
    let config_home = tempfile::tempdir().expect("config home");
    std::fs::create_dir_all(config_home.path()).expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let cwd = tempfile::tempdir().expect("cwd");
    write_cursor_hooks(user_home.path());
    let (_bin, path) = stub_on_path(env!("CARGO_PKG_VERSION"));

    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .env("PATH", &path)
        .env("STATEROOT_TEST_CMD_PROBES", "stateroot")
        .arg("doctor")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("[ok] hook binary (cursor)"),
        "stdout: {stdout}"
    );
}
