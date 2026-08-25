//! `stateroot projects` — the global registry window: listing, JSON shape,
//! missing-dir honesty, and --prune.

use std::path::Path;

use assert_cmd::Command;

fn stateroot(config_home: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::cargo_bin("stateroot").expect("binary");
    cmd.env("STATEROOT_HOME", config_home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_BASE")
        .current_dir(cwd);
    cmd
}

#[test]
fn projects_lists_registered_projects_with_hints() {
    let config_home = tempfile::tempdir().expect("config home");
    let proj_a = tempfile::tempdir().expect("proj a");
    let proj_b = tempfile::tempdir().expect("proj b");
    for dir in [&proj_a, &proj_b] {
        stateroot(config_home.path(), dir.path())
            .arg("init")
            .assert()
            .success();
    }
    let out = stateroot(config_home.path(), proj_a.path())
        .args(["projects"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    for dir in [&proj_a, &proj_b] {
        let path = dir.path().display().to_string();
        assert!(stdout.contains(&path), "missing {path}: {stdout}");
    }
    assert!(stdout.contains("init"), "phase shown: {stdout}");

    // JSON shape carries the registry row + live hints.
    let out = stateroot(config_home.path(), proj_a.path())
        .args(["projects", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let projects = parsed["projects"].as_array().expect("projects array");
    assert_eq!(projects.len(), 2, "{stdout}");
    let row = &projects[0];
    assert!(row.get("path").and_then(|v| v.as_str()).is_some());
    assert_eq!(row["on_disk"], true);
    assert!(row.get("project_id").is_some());
}

#[test]
fn projects_marks_missing_dirs_and_prune_drops_them() {
    let config_home = tempfile::tempdir().expect("config home");
    let proj_a = tempfile::tempdir().expect("proj a");
    let gone = tempfile::tempdir().expect("proj b");
    for dir in [&proj_a, &gone] {
        stateroot(config_home.path(), dir.path())
            .arg("init")
            .assert()
            .success();
    }
    let gone_path = gone.path().display().to_string();
    drop(gone); // delete the project dir from disk

    let out = stateroot(config_home.path(), proj_a.path())
        .args(["projects"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains(&gone_path),
        "missing entry still listed: {stdout}"
    );
    assert!(stdout.contains("MISSING"), "marked honestly: {stdout}");

    let out = stateroot(config_home.path(), proj_a.path())
        .args(["projects", "--prune"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("pruned"), "{stdout}");
    assert!(stdout.contains(&gone_path), "{stdout}");

    let out = stateroot(config_home.path(), proj_a.path())
        .args(["projects"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        !stdout.contains(&gone_path),
        "pruned entry must be gone: {stdout}"
    );
}

#[test]
fn projects_empty_registry_says_so() {
    let config_home = tempfile::tempdir().expect("config home");
    let cwd = tempfile::tempdir().expect("cwd");
    let out = stateroot(config_home.path(), cwd.path())
        .args(["projects"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("no projects registered"), "{stdout}");
}
