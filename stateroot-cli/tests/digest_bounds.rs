//! Digest budget regression — oversized rules/pack fixtures must still
//! produce a bounded resume digest (the 67KB-digest backlog item).

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

#[test]
fn digest_stays_bounded_with_oversized_content() {
    let config_home = tempfile::tempdir().expect("config home");
    std::fs::create_dir_all(config_home.path()).expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");

    // Oversized repo docs: 3 × 9000 chars — past the 16000 pack budget.
    let big_doc = |name: &str| format!("# {name}\n\n{}\n", "content line. ".repeat(600));
    std::fs::write(project.path().join("README.md"), big_doc("README.md")).expect("readme");
    std::fs::write(project.path().join("TODO.md"), big_doc("TODO.md")).expect("todo");
    std::fs::write(
        project.path().join("ARCHITECTURE.md"),
        big_doc("ARCHITECTURE.md"),
    )
    .expect("arch");
    // An oversized project rule: AGENTS.md imports via `rules sync`.
    std::fs::write(
        project.path().join("AGENTS.md"),
        format!(
            "# House Rules\n\n{}\n",
            (0..40)
                .map(|i| format!("## Section {i}\n\nRule body line {i}.\n"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    )
    .expect("agents");

    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("init")
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["rules", "sync"])
        .assert()
        .success();

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");

    // The whole digest stays bounded even with the 30-section constitution,
    // a 40-section project rule, and 27KB of repo docs.
    assert!(
        stdout.len() < 40_960,
        "digest must stay under ~40KB, got {} bytes",
        stdout.len()
    );
    // Pointers/markers prove the loss is declared, never silent.
    assert!(
        stdout.contains("… full rule: `stateroot rules show product-intent`"),
        "rules pointer: {stdout}"
    );
    assert!(stdout.contains("## Shared Rules"), "stdout: {stdout}");
    assert!(
        stdout.contains("capped — 2 more docs on disk"),
        "pack marker: {stdout}"
    );
    // The work body stays fully inline.
    assert!(
        stdout.contains("## Context pack (observed)"),
        "stdout: {stdout}"
    );
}
