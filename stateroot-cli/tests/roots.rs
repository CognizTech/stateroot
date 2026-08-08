//! Offline integration tests for the M2 git-plumbing roots surface.

use std::path::Path;

use assert_cmd::Command;

fn stateroot(config_home: &Path, user_home: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::cargo_bin("stateroot").expect("binary");
    cmd.env("STATEROOT_HOME", config_home)
        .env("STATEROOT_TEST_HOME", user_home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .current_dir(cwd);
    cmd
}

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn root_hash(stdout: &str) -> String {
    stdout
        .lines()
        .find(|l| l.starts_with("root "))
        .expect("root line")
        .trim_start_matches("root ")
        .trim()
        .to_string()
}

#[test]
fn init_auto_inits_git_and_snap_log_show_flow() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // M2: non-git folder got a silent repo at init.
    assert!(project.path().join(".git").is_dir(), "auto git init");

    // state_only on a bare project (init's convenience layer files would
    // otherwise count; the bare manifest case is the honest empty tree).
    let bare = tempfile::tempdir().expect("bare project");
    std::fs::create_dir_all(bare.path().join(".stateroot")).expect("bare stateroot");
    std::fs::write(
        bare.path().join(".stateroot/manifest.json"),
        r#"{"schema_version":"stateroot.manifest.v1","project_id":"ws-bare","name":"bare","created_at":"2026-08-07T00:00:00Z"}"#,
    )
    .expect("bare manifest");
    let out = stateroot(config_home.path(), user_home.path(), bare.path())
        .args(["snap", "--reason", "genesis"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("coverage: state-only"), "snap: {stdout}");

    // full coverage first root (init wrote convenience-layer files).
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "genesis"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("coverage: files:"), "snap1: {stdout}");
    let first = root_hash(&stdout);

    // full coverage second root, parented on the first.
    write(project.path(), "src/main.rs", "fn main() {}\n");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "add main"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("coverage: files: 6 pinned"),
        "snap2: {stdout}"
    );
    let second = root_hash(&stdout);

    // log: lineage with coverage lines.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("log")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("## Roots (2)"), "log: {stdout}");
    assert!(stdout.contains(&second[..12]), "log: {stdout}");
    assert!(
        stdout.contains("[files: 5]"),
        "genesis coverage line: {stdout}"
    );

    // show by prefix.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["show", &second[..12]])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains(&format!("parents: {}", &first[..12])),
        "show: {stdout}"
    );
    assert!(stdout.contains("reason: add main"), "show: {stdout}");
}

#[test]
fn diff_content_revert_and_fork() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    write(project.path(), "a.txt", "one\ntwo\n");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "v1"])
        .assert()
        .success();
    let first = root_hash(&String::from_utf8(out.get_output().stdout.clone()).expect("utf8"));

    write(project.path(), "a.txt", "one\nTWO\n");
    write(project.path(), "b.txt", "new file\n");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "v2"])
        .assert()
        .success();
    let second = root_hash(&String::from_utf8(out.get_output().stdout.clone()).expect("utf8"));

    // names + status
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["diff", &first, &second])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("M a.txt"), "diff: {stdout}");
    assert!(stdout.contains("A b.txt"), "diff: {stdout}");

    // unified content
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["diff", &first, &second, "--content"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("### a.txt"), "content: {stdout}");
    assert!(stdout.contains("-two"), "content: {stdout}");
    assert!(stdout.contains("+TWO"), "content: {stdout}");

    // receipt for the second transition (verified tier = git delta).
    let transitions_dir = project.path().join(".stateroot/transitions");
    let tids: Vec<String> = std::fs::read_dir(&transitions_dir)
        .expect("transitions")
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".json").map(str::to_string))
        })
        .collect();
    assert_eq!(tids.len(), 2, "one transition per snap");
    let latest_tid = tids.iter().max().expect("two transitions");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["receipt", latest_tid])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("# Transition receipt"), "receipt: {stdout}");
    assert!(
        stdout.contains("## Verified (git diff)"),
        "receipt: {stdout}"
    );
    assert!(stdout.contains("M a.txt"), "receipt: {stdout}");

    // fork: branch ref + report (no worktree in M2).
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["fork", &first, "--branch", "claude-line"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("fork claude-line"), "fork: {stdout}");
    assert!(stdout.contains("git worktree add"), "fork: {stdout}");

    // append-only revert: new root with the v1 tree; v2 still listed.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["revert", &first[..12], "--yes"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("reverted to"), "revert: {stdout}");
    let a_txt = std::fs::read_to_string(project.path().join("a.txt")).expect("a.txt");
    assert_eq!(
        a_txt, "one\nTWO\n",
        "worktree is NOT rewritten by revert (roots only)"
    );
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("log")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("## Roots (3)"), "append-only: {stdout}");
    assert!(
        stdout.contains(&second[..12]),
        "v2 root still present: {stdout}"
    );
}
