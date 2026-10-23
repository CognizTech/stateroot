//! `stateroot projects` — the global registry window: listing, JSON shape,
//! missing-dir honesty, and --prune.

// Each test binary includes this module but uses a subset of its helpers.
#[path = "support/mod.rs"]
mod support;

use std::path::Path;

use assert_cmd::Command;

/// Registry-window commands need a config home and a cwd; the user home is
/// hermetic too (a disposable child of the config home).
fn stateroot(config_home: &Path, cwd: &Path) -> Command {
    support::stateroot(config_home, &config_home.join("test-home"), cwd)
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
        // The registry keys fold to the Windows↔WSL identity form
        // (lowercase drive, forward slashes) — compare through the same
        // normalization, not the raw verbatim spelling.
        let path = stateroot_core::path_identity::normalize_host_path(
            &dir.path().canonicalize().unwrap().display().to_string(),
        );
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
    let gone_path = stateroot_core::path_identity::normalize_host_path(
        &gone.path().canonicalize().unwrap().display().to_string(),
    );
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
