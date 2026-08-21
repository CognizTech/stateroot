//! Extension-subcommand tests — git-style `stateroot-<name>` executables on
//! PATH. Hermetic homes plus sh fixtures on a prepended temp PATH (mirrors
//! the delegate fixtures; `cfg(unix)` where an executable script is needed).

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
        .env_remove("STATEROOT_PROJECT_DIR")
        .env_remove("STATEROOT_PROJECT_ID")
        .env_remove("STATEROOT_DELEGATION_DEPTH")
        .current_dir(cwd);
    cmd
}

fn homes() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_home = tempfile::tempdir().expect("config home");
    std::fs::create_dir_all(config_home.path()).expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    (config_home, user_home)
}

/// Temp `bin/` holding executable `stateroot-<name>` fixtures; returns
/// (dir, PATH) with the dir prepended so discovery sees the fixtures first.
#[cfg(unix)]
fn fake_extensions(fixtures: &[(&str, &str)]) -> (tempfile::TempDir, String) {
    let bin = tempfile::tempdir().expect("bin");
    for (name, body) in fixtures {
        let path = bin.path().join(format!("stateroot-{name}"));
        std::fs::write(&path, body).expect("fixture");
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
    }
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").expect("PATH")
    );
    (bin, path)
}

#[cfg(unix)]
#[test]
fn ext_passthrough_forwards_args_stdio_and_exit_code() {
    let (config_home, user_home) = homes();
    let cwd = tempfile::tempdir().expect("cwd");
    let (_bin, path) = fake_extensions(&[(
        "hello",
        "#!/bin/sh\necho \"hello args:$*\"\necho 'to stderr' >&2\nexit 7\n",
    )]);

    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .env("PATH", &path)
        .args(["hello", "a", "b"])
        .assert()
        .code(7);
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert_eq!(stdout.trim(), "hello args:a b");
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(stderr.contains("to stderr"), "stderr inherited: {stderr}");
}

#[cfg(unix)]
#[test]
fn ext_env_contract_exposes_project_and_version() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("init")
        .assert()
        .success();
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.path().join(".stateroot/manifest.json"))
            .expect("manifest"),
    )
    .expect("manifest json");
    let project_id = manifest["project_id"].as_str().expect("project_id");

    let (_bin, path) = fake_extensions(&[(
        "where",
        "#!/bin/sh\necho \"id=$STATEROOT_PROJECT_ID\"\necho \"ver=$STATEROOT_VERSION\"\necho \"dir=${STATEROOT_PROJECT_DIR:-unset}\"\nif [ -n \"$STATEROOT_PROJECT_DIR\" ]; then echo marked > \"$STATEROOT_PROJECT_DIR/ext-marker\"; fi\n",
    )]);
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .arg("where")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains(&format!("id={project_id}")),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("ver=") && !stdout.contains("ver=\n"),
        "version injected: {stdout}"
    );
    assert!(
        project.path().join("ext-marker").is_file(),
        "STATEROOT_PROJECT_DIR must point at the project"
    );

    // Outside a project the project vars stay unset (ambient passes through).
    let bare = tempfile::tempdir().expect("bare cwd");
    let out = stateroot(config_home.path(), user_home.path(), bare.path())
        .env("PATH", &path)
        .arg("where")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("id=\n"),
        "no project id outside a project: {stdout}"
    );
    assert!(stdout.contains("dir=unset"), "stdout: {stdout}");
    assert!(!bare.path().join("ext-marker").exists());
}

#[test]
fn ext_unknown_subcommand_is_a_clap_styled_usage_error() {
    let (config_home, user_home) = homes();
    let cwd = tempfile::tempdir().expect("cwd");

    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .arg("statsu")
        .assert()
        .code(2);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(
        stderr.contains("error: unrecognized subcommand 'statsu'"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("'status'"), "did-you-mean: {stderr}");
    assert!(
        stderr.contains("Usage: stateroot <COMMAND>"),
        "stderr: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn ext_unknown_subcommand_suggests_extension_names_too() {
    let (config_home, user_home) = homes();
    let cwd = tempfile::tempdir().expect("cwd");
    let (_bin, path) = fake_extensions(&[("helo", "#!/bin/sh\necho nope\n")]);

    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .env("PATH", &path)
        .arg("hello")
        .assert()
        .code(2);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(
        stderr.contains("unrecognized subcommand 'hello'"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("'helo'"), "extension suggestion: {stderr}");
}

#[cfg(unix)]
#[test]
fn ext_never_shadows_a_builtin() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("init")
        .assert()
        .success();
    let (_bin, path) = fake_extensions(&[("status", "#!/bin/sh\necho 'EXTENSION RAN'\nexit 42\n")]);

    // The builtin still runs.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .arg("status")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("project:"), "builtin status: {stdout}");
    assert!(!stdout.contains("EXTENSION RAN"), "shadowed: {stdout}");

    // `ext list` marks it as a shadowed builtin.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args(["ext", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("status — ") && stdout.contains("shadowed builtin (ignored)"),
        "ext list: {stdout}"
    );
}

#[test]
fn ext_list_reports_empty_path() {
    let (config_home, user_home) = homes();
    let cwd = tempfile::tempdir().expect("cwd");
    let empty_bin = tempfile::tempdir().expect("empty bin");

    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .env("PATH", empty_bin.path())
        .args(["ext", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert_eq!(stdout.trim(), "no extensions found on PATH (stateroot-*)");
}

#[cfg(unix)]
#[test]
fn ext_list_shows_discovered_extensions() {
    let (config_home, user_home) = homes();
    let cwd = tempfile::tempdir().expect("cwd");
    let (_bin, path) = fake_extensions(&[("hello", "#!/bin/sh\necho hi\n")]);

    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .env("PATH", &path)
        .args(["ext", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("hello — "), "ext list: {stdout}");
    assert!(stdout.contains("stateroot-hello"), "ext list: {stdout}");
    assert!(!stdout.contains("shadowed builtin"), "ext list: {stdout}");
}
