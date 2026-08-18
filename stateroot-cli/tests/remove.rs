//! `stateroot remove` tests — local full-plan removal, dry-run, and refusal.

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

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
}

#[test]
fn remove_full_plan_local() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // roots create our git refs
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "x"])
        .assert()
        .success();
    let repo = git2::Repository::open(project.path()).unwrap();
    assert!(repo.refname_to_id("refs/stateroot/latest").is_ok());

    // AGENTS.md with user content around the block (excise case)
    let agents = project.path().join("AGENTS.md");
    let base = std::fs::read_to_string(&agents).unwrap();
    std::fs::write(&agents, format!("# My rules\n\n{base}")).unwrap();
    // a modified stub (kept) and an untouched one (deleted)
    let stub = project.path().join(".cursor/rules/stateroot.mdc");
    assert!(stub.is_file(), "init installs the cursor stub");
    std::fs::write(&stub, "USER EDITED THIS STUB\n").unwrap();
    let claude_stub = project.path().join(".claude/commands/stateroot.md");
    assert!(claude_stub.is_file(), "init installs the claude stub");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["remove", "--yes"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");

    assert!(!project.path().join(".stateroot").exists(), "store gone");
    assert!(
        stdout.contains("unregistered from projects.toml"),
        "{stdout}"
    );
    // registry entry gone
    let registry = std::fs::read_to_string(config_home.path().join("projects.toml")).unwrap();
    assert!(!registry.contains("local-"), "{registry}");
    // git refs gone, repo itself intact
    let repo = git2::Repository::open(project.path()).unwrap();
    assert!(
        repo.refname_to_id("refs/stateroot/latest").is_err(),
        "latest gone"
    );
    assert_eq!(
        repo.references_glob("refs/stateroot/roots/*")
            .unwrap()
            .count(),
        0,
        "root refs gone"
    );
    assert!(stdout.contains("git ref(s)"), "{stdout}");
    // AGENTS.md excised (user content kept)
    let text = std::fs::read_to_string(&agents).unwrap();
    assert!(text.contains("# My rules"), "{text}");
    assert!(!text.contains("stateroot:begin"), "{text}");
    // modified stub kept, byte-identical stub deleted
    assert!(stub.is_file(), "modified stub must be kept");
    assert!(stdout.contains("modified since install"), "{stdout}");
    assert!(!claude_stub.exists(), "untouched stub deleted");
}

#[test]
fn remove_dry_run_touches_nothing() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "x"])
        .assert()
        .success();

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["remove", "--dry-run"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("dry-run — nothing was touched"), "{stdout}");
    assert!(
        stdout.contains("git ref(s) under refs/stateroot/"),
        "{stdout}"
    );
    assert!(project.path().join(".stateroot").is_dir());
    let repo = git2::Repository::open(project.path()).unwrap();
    assert!(repo.refname_to_id("refs/stateroot/latest").is_ok());
}

#[test]
fn remove_refuses_non_interactive_without_yes() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("remove")
        .assert()
        .failure();
    assert!(project.path().join(".stateroot").is_dir());
}
