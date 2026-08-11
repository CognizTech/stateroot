use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

fn stateroot(config: &Path, home: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::cargo_bin("stateroot").unwrap();
    cmd.env("STATEROOT_HOME", config)
        .env("STATEROOT_TEST_HOME", home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env_remove("HERMES_HOME")
        .current_dir(cwd);
    cmd
}

fn init(config: &Path, home: &Path, project: &Path) {
    std::fs::create_dir_all(config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "user_id = \"default\"\nagent_id = \"default\"\n",
    )
    .unwrap();
    stateroot(config, home, project)
        .arg("init")
        .assert()
        .success();
}

#[test]
fn fresh_init_has_no_project_identity_files() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init(config.path(), home.path(), project.path());
    let root = project.path().join(".stateroot");
    assert!(!root.join("soul/SOUL.md").exists());
    assert!(!root.join("soul/OVERLAY.md").exists());
    assert!(!root.join("user/USER.md").exists());
}

#[test]
fn setup_imports_openclaw_persona_and_user_separately() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init(config.path(), home.path(), project.path());
    let ws = home.path().join(".openclaw/workspace");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("IDENTITY.md"), "# Identity\n\nAgent Ada\n").unwrap();
    std::fs::write(ws.join("SOUL.md"), "# Soul\n\nBe exact.\n").unwrap();
    std::fs::write(ws.join("USER.md"), "# User\n\nHuman Lin\n").unwrap();

    stateroot(config.path(), home.path(), project.path())
        .args(["setup", "--only", "identity", "--yes"])
        .assert()
        .success();
    let soul = std::fs::read_to_string(home.path().join(".stateroot/soul/SOUL.md")).unwrap();
    let user = std::fs::read_to_string(home.path().join(".stateroot/user/USER.md")).unwrap();
    assert!(soul.contains("Agent Ada") && soul.contains("Be exact."));
    assert!(!soul.contains("Human Lin"));
    assert!(user.contains("Human Lin"));
    assert!(soul.contains("origin=openclaw:"));
    assert!(user.contains("origin=openclaw:"));
}

#[test]
fn setup_honors_hermes_home_without_network() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init(config.path(), home.path(), project.path());
    let hermes = home.path().join("hermes-custom");
    std::fs::create_dir_all(hermes.join("memories")).unwrap();
    std::fs::write(hermes.join("SOUL.md"), "Hermes persona").unwrap();
    std::fs::write(hermes.join("memories/USER.md"), "Hermes human").unwrap();

    stateroot(config.path(), home.path(), project.path())
        .env("HERMES_HOME", "~/hermes-custom")
        .args(["setup", "--only", "identity", "--yes"])
        .assert()
        .success();
    assert!(
        std::fs::read_to_string(home.path().join(".stateroot/soul/SOUL.md"))
            .unwrap()
            .contains("Hermes persona")
    );
    assert!(
        std::fs::read_to_string(home.path().join(".stateroot/user/USER.md"))
            .unwrap()
            .contains("Hermes human")
    );
}

#[test]
fn configured_setup_selects_hermes_and_refreshes_persona_cache() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let hermes = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init(config.path(), home.path(), project.path());
    let openclaw = home.path().join(".openclaw/workspace");
    std::fs::create_dir_all(&openclaw).unwrap();
    std::fs::write(openclaw.join("SOUL.md"), "OpenClaw persona").unwrap();
    std::fs::create_dir_all(hermes.path().join("memories")).unwrap();
    std::fs::write(hermes.path().join("SOUL.md"), "Hermes selected persona").unwrap();
    std::fs::write(
        hermes.path().join("memories/USER.md"),
        "Hermes selected user",
    )
    .unwrap();
    std::fs::write(config.path().join("persona.md"), "stale persona").unwrap();
    let answers = project.path().join("answers.yaml");
    std::fs::write(&answers, "identity.source: 1\n").unwrap();

    stateroot(config.path(), home.path(), project.path())
        .env("HERMES_HOME", hermes.path())
        .args(["setup", "--only", "identity", "--config"])
        .arg(&answers)
        .assert()
        .success();
    let soul = std::fs::read_to_string(home.path().join(".stateroot/soul/SOUL.md")).unwrap();
    let user = std::fs::read_to_string(home.path().join(".stateroot/user/USER.md")).unwrap();
    let cache = std::fs::read_to_string(config.path().join("persona.md")).unwrap();
    assert!(soul.contains("Hermes selected persona"));
    assert!(!soul.contains("OpenClaw persona"));
    assert!(user.contains("Hermes selected user"));
    assert!(cache.contains("Hermes selected persona"));
    assert!(!cache.contains("stale persona"));
}

#[test]
fn resume_reads_global_user_and_project_memory() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init(config.path(), home.path(), project.path());
    std::fs::create_dir_all(home.path().join(".stateroot/user")).unwrap();
    std::fs::write(home.path().join(".stateroot/user/USER.md"), "global-human").unwrap();
    std::fs::write(
        project.path().join(".stateroot/memories/MEMORY.md"),
        "project-memory",
    )
    .unwrap();
    stateroot(config.path(), home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("global-human"))
        .stdout(predicate::str::contains("project-memory"));

    std::fs::create_dir_all(project.path().join(".stateroot/handoffs")).unwrap();
    std::fs::write(
        project.path().join(".stateroot/handoffs/current.json"),
        r#"{"schema_version":"stateroot.handoff.v1","objective":"continue"}"#,
    )
    .unwrap();
    stateroot(config.path(), home.path(), project.path())
        .args(["hook", "SessionStart", "--harness", "claude-code"])
        .assert()
        .success()
        .stdout(predicate::str::contains("global-human"))
        .stdout(predicate::str::contains("project-memory"));
}

#[test]
fn init_migration_is_safe_idempotent_and_preserves_conflict() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init(config.path(), home.path(), project.path());
    let root = project.path().join(".stateroot");
    std::fs::create_dir_all(root.join("soul")).unwrap();
    std::fs::create_dir_all(root.join("user")).unwrap();
    std::fs::create_dir_all(home.path().join(".stateroot/user")).unwrap();
    std::fs::write(root.join("soul/SOUL.md"), "project persona").unwrap();
    std::fs::write(root.join("user/USER.md"), "project human").unwrap();
    std::fs::write(home.path().join(".stateroot/user/USER.md"), "global human").unwrap();

    stateroot(config.path(), home.path(), project.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("warning: preserved conflicting"));
    assert_eq!(
        std::fs::read_to_string(root.join("soul/OVERLAY.md")).unwrap(),
        "project persona"
    );
    assert!(!root.join("soul/SOUL.md").exists());
    assert!(!root.join("user/USER.md").exists());
    let history = home.path().join(".stateroot/user/history");
    let candidates = std::fs::read_dir(&history).unwrap().count();
    assert_eq!(candidates, 1);
    stateroot(config.path(), home.path(), project.path())
        .arg("init")
        .assert()
        .success();
    assert_eq!(std::fs::read_dir(history).unwrap().count(), 1);
}

#[test]
fn init_repairs_known_openclaw_composed_soul_with_history() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init(config.path(), home.path(), project.path());
    std::fs::create_dir_all(home.path().join(".stateroot/soul")).unwrap();
    std::fs::write(
        home.path().join(".stateroot/soul/SOUL.md"),
        "# Soul\n\n## Identity (IDENTITY.md)\n\nAgent Ada\n\n## User (USER.md)\n\nHuman Lin\n\n## Persona (SOUL.md)\n\nBe exact.\n",
    )
    .unwrap();
    stateroot(config.path(), home.path(), project.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("removed human USER section"));
    let soul = std::fs::read_to_string(home.path().join(".stateroot/soul/SOUL.md")).unwrap();
    let user = std::fs::read_to_string(home.path().join(".stateroot/user/USER.md")).unwrap();
    assert!(soul.contains("Agent Ada") && soul.contains("Be exact."));
    assert!(!soul.contains("Human Lin"));
    assert!(user.contains("Human Lin"));
    assert_eq!(
        std::fs::read_dir(home.path().join(".stateroot/soul/history"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn migration_treats_provenance_wrapped_user_as_duplicate() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init(config.path(), home.path(), project.path());
    let root = project.path().join(".stateroot");
    std::fs::create_dir_all(root.join("user")).unwrap();
    std::fs::create_dir_all(home.path().join(".stateroot/user")).unwrap();
    std::fs::write(root.join("user/USER.md"), "Human Lin").unwrap();
    std::fs::write(
        home.path().join(".stateroot/user/USER.md"),
        "<!-- stateroot:user origin=setup; at=then -->\nHuman Lin\n",
    )
    .unwrap();
    stateroot(config.path(), home.path(), project.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("removed duplicate project user"));
    assert!(!root.join("user/USER.md").exists());
    assert!(!home.path().join(".stateroot/user/history").exists());
}

#[test]
fn migration_removes_duplicate_soul_and_archives_conflict_once() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let duplicate_project = tempfile::tempdir().unwrap();
    init(config.path(), home.path(), duplicate_project.path());
    let duplicate_root = duplicate_project.path().join(".stateroot/soul");
    std::fs::create_dir_all(&duplicate_root).unwrap();
    std::fs::write(duplicate_root.join("SOUL.md"), "same persona").unwrap();
    std::fs::write(duplicate_root.join("OVERLAY.md"), "same persona\n").unwrap();
    stateroot(config.path(), home.path(), duplicate_project.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "removed duplicate legacy project soul",
        ));
    assert!(!duplicate_root.join("SOUL.md").exists());
    assert!(!duplicate_root.join("history").exists());

    let conflict_project = tempfile::tempdir().unwrap();
    init(config.path(), home.path(), conflict_project.path());
    let conflict_root = conflict_project.path().join(".stateroot/soul");
    std::fs::create_dir_all(&conflict_root).unwrap();
    std::fs::write(conflict_root.join("SOUL.md"), "legacy persona").unwrap();
    std::fs::write(conflict_root.join("OVERLAY.md"), "current overlay").unwrap();
    stateroot(config.path(), home.path(), conflict_project.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "preserved conflicting legacy project soul",
        ));
    assert!(!conflict_root.join("SOUL.md").exists());
    let history = conflict_root.join("history");
    let archives = std::fs::read_dir(&history).unwrap().collect::<Vec<_>>();
    assert_eq!(archives.len(), 1);
    let archive = archives[0].as_ref().unwrap().path();
    assert_eq!(std::fs::read_to_string(archive).unwrap(), "legacy persona");
    stateroot(config.path(), home.path(), conflict_project.path())
        .arg("init")
        .assert()
        .success();
    assert_eq!(std::fs::read_dir(history).unwrap().count(), 1);
}
