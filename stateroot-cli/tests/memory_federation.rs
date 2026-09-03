//! Integration tests for `stateroot memory sync` (M1 pull + M2 push) on the
//! hermetic tempdir pattern — fake harness homes via `STATEROOT_TEST_HOME`.

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

fn write(home: &Path, rel: &str, content: &str) {
    let path = home.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("parent");
    std::fs::write(path, content).expect("write");
}

#[test]
fn sync_pulls_codex_then_pushes_managed_brief() {
    let config_home = tempfile::tempdir().expect("config home");
    std::fs::create_dir_all(config_home.path()).expect("config home dir");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    write(
        user_home.path(),
        ".codex/memories/project-status.md",
        "integration-token-99 status",
    );

    // Pull: import the codex note as an observed wiki page.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("memory")
        .arg("sync")
        .arg("--harness")
        .arg("codex")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("1 found · 1 imported"), "stdout: {stdout}");
    let page = project
        .path()
        .join(".stateroot/wiki/pages/harness/codex/project-status.md");
    assert!(page.is_file(), "page not written: {page:?}");
    let text = std::fs::read_to_string(&page).expect("page");
    assert!(text.contains("type: Harness Note"), "{text}");
    assert!(text.contains("stateroot_import"), "{text}");
    assert!(text.contains("integration-token-99"), "{text}");

    // Idempotent: second pull reports duplicates.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("memory")
        .arg("sync")
        .arg("--harness")
        .arg("codex")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("1 duplicates"), "stdout: {stdout}");

    // Dry-run reports without writing.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("memory")
        .arg("sync")
        .arg("--harness")
        .arg("codex")
        .arg("--dry-run")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("dry-run"), "stdout: {stdout}");

    // Push: managed brief lands in the codex memory home.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("memory")
        .arg("sync")
        .arg("--push")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("codex:"), "stdout: {stdout}");
    let brief = user_home.path().join(".codex/memories/stateroot.md");
    assert!(brief.is_file(), "brief not written: {brief:?}");
    let text = std::fs::read_to_string(&brief).expect("brief");
    assert!(
        text.contains("<!-- stateroot:managed v1 -->"),
        "missing managed marker: {text}"
    );
}

#[test]
fn sync_push_refuses_unmanaged_existing_file() {
    let config_home = tempfile::tempdir().expect("config home");
    std::fs::create_dir_all(config_home.path()).expect("config home dir");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    write(
        user_home.path(),
        ".codex/memories/stateroot.md",
        "foreign memory",
    );
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("memory")
        .arg("sync")
        .arg("--push")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("conflict"), "stdout: {stdout}");
    assert_eq!(
        std::fs::read_to_string(user_home.path().join(".codex/memories/stateroot.md"))
            .expect("file"),
        "foreign memory",
        "unmanaged file must be left untouched"
    );
}

#[test]
fn sync_rejects_unknown_harness() {
    let config_home = tempfile::tempdir().expect("config home");
    std::fs::create_dir_all(config_home.path()).expect("config home dir");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("memory")
        .arg("sync")
        .arg("--harness")
        .arg("hermes")
        .assert()
        .failure();
}
