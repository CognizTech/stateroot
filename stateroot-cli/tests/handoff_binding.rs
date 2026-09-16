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

fn record_plan(config_home: &Path, user_home: &Path, project: &Path) -> String {
    let out = stateroot(config_home, user_home, project)
        .args(["plan", "record", "--stdin", "--title", "Fork-bound plan"])
        .write_stdin("# Fork-bound plan\n\nDo the fork work.\n")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    stdout
        .split_whitespace()
        .nth(2)
        .expect("recorded plan id")
        .to_string()
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
    let current =
        std::fs::read_to_string(wt.join(".stateroot/handoffs/current.json")).expect("current");
    let packet: serde_json::Value = serde_json::from_str(&current).expect("json");
    let fork_id = packet["fork_id"].as_str().expect("fork_id present");
    assert!(!fork_id.is_empty());
    assert!(
        packet.get("worktree").is_none(),
        "the raw path must NOT enter the shared packet: {packet:?}"
    );
}

#[test]
fn bound_handoff_uses_the_fork_active_plan_and_resolves_its_worktree() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let root = snap_root(config_home.path(), user_home.path(), project.path());
    let plan = record_plan(config_home.path(), user_home.path(), project.path());
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "approve", &plan])
        .assert()
        .success();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("checkout");
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "fork",
            &root,
            "--worktree",
            &wt.to_string_lossy(),
            "--plan",
            &plan,
        ])
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), &wt)
        .args(["plan", "activate", &plan])
        .assert()
        .success();

    write_bound_handoff(config_home.path(), user_home.path(), project.path(), &wt);
    let current =
        std::fs::read_to_string(wt.join(".stateroot/handoffs/current.json")).expect("current");
    let packet: serde_json::Value = serde_json::from_str(&current).expect("json");
    assert_eq!(packet["plan_ref"]["id"].as_str(), Some(plan.as_str()));
    assert_eq!(packet["plan_ref"]["status"].as_str(), Some("active"));

    let shown = stateroot(config_home.path(), user_home.path(), &wt)
        .args(["handoff", "show"])
        .assert()
        .success();
    let stdout = String::from_utf8(shown.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains(&wt.display().to_string()),
        "stdout: {stdout}"
    );
    assert!(stdout.contains(&plan), "stdout: {stdout}");
    assert!(stdout.contains("(active)"), "stdout: {stdout}");
}

#[test]
fn bound_handoff_delivers_only_to_its_fork_worktree() {
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

    // The parent retains no bound current packet: another fork can receive a
    // different handoff without displacing this delivery.
    assert!(
        !project
            .path()
            .join(".stateroot/handoffs/current.json")
            .exists(),
        "bound delivery must not replace the parent's current handoff"
    );

    // Resume in the parent remains usable and unbound.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success();

    // Resume inside the fork receives the packet and resolves its own path.
    let out = stateroot(config_home.path(), user_home.path(), &wt)
        .args(["resume", "--force"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains(&wt.display().to_string()),
        "stdout: {stdout}"
    );
}
