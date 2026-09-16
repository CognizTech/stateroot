//! Fork-bound handoffs end to end (repair Phase 6D): `handoff write
//! --worktree` validates against the registry and stores an opaque fork id;
//! `resume` in the wrong directory fails closed with the exact recovery.

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
        .current_dir(cwd);
    cmd
}

fn homes() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_home = tempfile::tempdir().expect("config home");
    std::fs::create_dir_all(config_home.path()).expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    (config_home, user_home)
}

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
}

fn snap_root(config_home: &Path, user_home: &Path, project: &Path) -> String {
    std::fs::write(project.join("seed.txt"), "seed\n").expect("seed");
    let out = stateroot(config_home, user_home, project)
        .arg("snap")
        .assert()
        .success();
    let line = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    line.split_whitespace()
        .find(|t| t.len() >= 12 && t.chars().all(|c| c.is_ascii_hexdigit()))
        .expect("root hash in snap output")
        .to_string()
}

fn make_fork_worktree(config_home: &Path, user_home: &Path, project: &Path, root: &str, wt: &Path) {
    stateroot(config_home, user_home, project)
        .args(["fork", root, "--worktree", &wt.to_string_lossy()])
        .assert()
        .success();
}

fn write_bound_handoff(config_home: &Path, user_home: &Path, project: &Path, wt: &Path) {
    stateroot(config_home, user_home, project)
        .args([
            "handoff",
            "write",
            "--from",
            "kimi-code",
            "--objective",
            "bound goal",
            "--task",
            "bound task",
            "--context-summary",
            "bound context",
            "--worktree",
            &wt.to_string_lossy(),
        ])
        .assert()
        .success();
}

#[test]
fn bound_handoff_stores_fork_id_not_a_path() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let root = snap_root(config_home.path(), user_home.path(), project.path());
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("checkout");
    make_fork_worktree(
        config_home.path(),
        user_home.path(),
        project.path(),
        &root,
        &wt,
    );

    // An unregistered directory is refused.
    let plain = tempfile::tempdir().expect("plain");
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "kimi-code",
            "--objective",
            "x",
            "--task",
            "y",
            "--context-summary",
            "z",
            "--worktree",
            &plain.path().to_string_lossy(),
        ])
        .assert()
        .failure();

    write_bound_handoff(config_home.path(), user_home.path(), project.path(), &wt);
    let current = std::fs::read_to_string(project.path().join(".stateroot/handoffs/current.json"))
        .expect("current");
    let packet: serde_json::Value = serde_json::from_str(&current).expect("json");
    let fork_id = packet["fork_id"].as_str().expect("fork_id present");
    assert!(!fork_id.is_empty());
    assert!(
        packet.get("worktree").is_none(),
        "the raw path must NOT enter the shared packet: {packet:?}"
    );
}

#[test]
fn resume_in_the_wrong_directory_fails_closed_with_the_recovery() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let root = snap_root(config_home.path(), user_home.path(), project.path());
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("checkout");
    make_fork_worktree(
        config_home.path(),
        user_home.path(),
        project.path(),
        &root,
        &wt,
    );
    write_bound_handoff(config_home.path(), user_home.path(), project.path(), &wt);

    // Resume in the TRUNK (wrong tree for a bound handoff): hard refusal
    // with the exact recovery command.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(stderr.contains("bound to fork"), "stderr: {stderr}");
    assert!(stderr.contains("Recovery:"), "stderr: {stderr}");
    assert!(
        stderr.contains(&wt.display().to_string()),
        "stderr: {stderr}"
    );

    // Resume INSIDE the fork worktree: allowed (the fork is registered in
    // the worktree's own lineage).
    stateroot(config_home.path(), user_home.path(), &wt)
        .args(["resume", "--force"])
        .assert()
        .success();
}
