use assert_cmd::Command;
use predicates::prelude::*;
use std::{fs, path::Path};

fn dot(project: &Path, home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("stateroot").unwrap();
    cmd.env("STATEROOT_HOME", home.join("invalid-config"))
        .env("STATEROOT_TEST_HOME", home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .arg("dot")
        .arg("--project")
        .arg(project);
    cmd
}

#[test]
fn explicit_project_continuity_ignores_host_and_preserves_existing_files() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    // A broken config makes accidental host context loading observable.
    fs::create_dir_all(home.path().join("invalid-config")).unwrap();
    fs::write(home.path().join("invalid-config/config.toml"), "[broken").unwrap();
    fs::write(project.path().join("AGENTS.md"), "user instructions").unwrap();
    dot(project.path(), home.path())
        .arg("init")
        .assert()
        .success();
    let manifest = fs::read(project.path().join(".stateroot/manifest.json")).unwrap();
    dot(project.path(), home.path())
        .arg("init")
        .assert()
        .success();
    assert_eq!(
        manifest,
        fs::read(project.path().join(".stateroot/manifest.json")).unwrap()
    );
    assert_eq!(
        fs::read_to_string(project.path().join("AGENTS.md")).unwrap(),
        "user instructions"
    );
    assert!(!home.path().join(".codex").exists());
    assert!(!project.path().join(".agents").exists());
    dot(project.path(), home.path())
        .args(["checkpoint", "portable milestone"])
        .assert()
        .success();
    dot(project.path(), home.path())
        .arg("resume")
        .assert()
        .success()
        .stdout(predicate::str::contains("portable milestone"));
    dot(project.path(), home.path())
        .args(["recall", "portable"])
        .assert()
        .success()
        .stdout(predicate::str::contains("portable milestone"));
    let input = project.path().join("packet.json");
    fs::write(&input, r#"{"objective":"ship","task":"tests","context_summary":"checks passed","next_actions":["review"]}"#).unwrap();
    for _ in 0..2 {
        dot(project.path(), home.path())
            .arg("handoff")
            .arg("--input")
            .arg(&input)
            .assert()
            .success();
    }
    let packet: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project.path().join(".stateroot/handoffs/current.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(packet["seq"], 2);
    assert_eq!(packet["created_by_harness"], "dot");
    assert_eq!(
        fs::read_dir(project.path().join(".stateroot/handoffs/history"))
            .unwrap()
            .count(),
        2
    );
    dot(project.path(), home.path())
        .arg("resume")
        .assert()
        .success()
        .stdout(predicate::str::contains("checks passed"));
    // The same store is usable at another path without a host registry.
    let moved = project.path().with_extension("moved");
    fs::rename(project.path(), &moved).unwrap();
    dot(&moved, home.path())
        .arg("resume")
        .assert()
        .success()
        .stdout(predicate::str::contains("portable milestone"));
    fs::rename(&moved, project.path()).unwrap();
}

#[test]
fn project_is_required_and_bad_handoff_cannot_replace_current() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    Command::cargo_bin("stateroot")
        .unwrap()
        .args(["dot", "resume"])
        .assert()
        .failure();
    dot(project.path(), home.path())
        .arg("resume")
        .assert()
        .failure();
    dot(project.path(), home.path())
        .arg("init")
        .assert()
        .success();
    let input = project.path().join("bad.json");
    fs::write(&input, "{}").unwrap();
    dot(project.path(), home.path())
        .arg("handoff")
        .arg("--input")
        .arg(&input)
        .assert()
        .failure();
    assert!(!project
        .path()
        .join(".stateroot/handoffs/current.json")
        .exists());
}

#[test]
fn snapshot_reuses_git_lineage_without_moving_user_head_and_skill_matches_source() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    dot(project.path(), home.path())
        .arg("init")
        .assert()
        .success();
    fs::write(project.path().join("work.txt"), "project work").unwrap();
    let repo = git2::Repository::init(project.path()).unwrap();
    let head_before = repo
        .find_reference("HEAD")
        .unwrap()
        .symbolic_target()
        .unwrap()
        .to_string();
    dot(project.path(), home.path())
        .args(["snap", "--reason", "verified milestone"])
        .assert()
        .success()
        .stdout(predicate::str::contains("root "));
    assert_eq!(
        repo.find_reference("HEAD")
            .unwrap()
            .symbolic_target()
            .unwrap(),
        head_before
    );
    assert!(repo
        .references_glob("refs/stateroot/*")
        .unwrap()
        .next()
        .is_some());
    assert!(!project
        .path()
        .join(".stateroot/local/memory.sqlite")
        .exists());
    let root_id = stateroot_core::roots::latest_root(project.path())
        .unwrap()
        .unwrap();
    fs::write(project.path().join("work.txt"), "later edit").unwrap();
    let (restored, _) =
        stateroot_core::roots::revert_to_root(project.path(), &root_id, "cli").unwrap();
    assert_ne!(restored.id, root_id);
    // Revert is lineage-only in the existing engine; it does not check out files.
    assert_eq!(
        fs::read_to_string(project.path().join("work.txt")).unwrap(),
        "later edit"
    );
    let (fork, _) =
        stateroot_core::roots::fork_root(project.path(), &root_id, Some("restore-check"), "cli")
            .unwrap();
    let restored_dir = home.path().join("restored-worktree");
    stateroot_core::roots::fork_materialize(project.path(), &fork, &restored_dir, None).unwrap();
    assert_eq!(
        fs::read_to_string(restored_dir.join("work.txt")).unwrap(),
        "project work"
    );
    dot(project.path(), home.path())
        .arg("resume")
        .assert()
        .success();
    let output = dot(project.path(), home.path())
        .arg("skill")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        String::from_utf8(output).unwrap().replace("\r\n", "\n"),
        include_str!("../../skills/stateroot-dot/SKILL.md").replace("\r\n", "\n")
    );
}

fn ordinary(project: &Path, home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("stateroot").unwrap();
    cmd.current_dir(project)
        .env("STATEROOT_HOME", home.join("config"))
        .env("STATEROOT_TEST_HOME", home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env("STATEROOT_NO_PING", "1");
    cmd
}

#[test]
fn ordinary_handoff_compatibility_recovery_and_input_provenance() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    dot(project.path(), home.path())
        .arg("init")
        .assert()
        .success();
    let input = project.path().join("packet.json");
    let content = serde_json::json!({"objective":"ship", "task":"review", "context_summary":"explicit evidence", "next_actions":["test"], "failed_approaches":[{"approach":"first try", "outcome":"failed", "reason":"test failed"}]});
    fs::write(&input, content.to_string()).unwrap();
    ordinary(project.path(), home.path())
        .args(["handoff", "write", "--from", "codex", "--input"])
        .arg(&input)
        .assert()
        .success();
    dot(project.path(), home.path())
        .arg("resume")
        .assert()
        .success()
        .stdout(predicate::str::contains("explicit evidence"));
    dot(project.path(), home.path())
        .arg("handoff")
        .arg("--input")
        .arg(&input)
        .assert()
        .success();
    ordinary(project.path(), home.path())
        .args(["handoff", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("explicit evidence"));
    let current = project.path().join(".stateroot/handoffs/current.json");
    let before = fs::read(&current).unwrap();
    for (field, value) in [
        ("seq", serde_json::json!(900)),
        ("plan_ref", serde_json::json!({"id":"forged"})),
        ("worktree", serde_json::json!("../other")),
        ("mystery", serde_json::json!("unknown")),
        (
            "failed_approaches",
            serde_json::json!([{"approach":"try", "outcome":"invented"}]),
        ),
    ] {
        let mut bad = content.clone();
        bad[field] = value;
        fs::write(&input, bad.to_string()).unwrap();
        dot(project.path(), home.path())
            .arg("handoff")
            .arg("--input")
            .arg(&input)
            .assert()
            .failure();
        assert_eq!(before, fs::read(&current).unwrap());
    }
    // Restore an older current packet: next sequence must still use history max.
    let old = stateroot_core::local_store::list_handoffs_local(project.path())
        .unwrap()
        .into_iter()
        .find(|p| p["seq"] == 1)
        .unwrap();
    fs::write(&current, old.to_string()).unwrap();
    fs::write(&input, content.to_string()).unwrap();
    dot(project.path(), home.path())
        .arg("handoff")
        .arg("--input")
        .arg(&input)
        .assert()
        .success();
    assert_eq!(
        stateroot_core::local_store::read_handoff_local(project.path())
            .unwrap()
            .unwrap()["seq"],
        3
    );
    // Corruption fails closed; repair remains the ordinary canonical recovery.
    fs::write(&current, "{").unwrap();
    dot(project.path(), home.path())
        .arg("handoff")
        .arg("--input")
        .arg(&input)
        .assert()
        .failure();
    ordinary(project.path(), home.path())
        .args(["handoff", "repair"])
        .assert()
        .success();
    dot(project.path(), home.path())
        .arg("resume")
        .assert()
        .success();
}

#[test]
fn partial_init_keeps_identity_and_bound_or_foreign_handoffs_fail_closed() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    dot(project.path(), home.path())
        .arg("init")
        .assert()
        .success();
    let root = project.path().join(".stateroot");
    let manifest = fs::read(root.join("manifest.json")).unwrap();
    fs::write(root.join("memories/MEMORY.md"), "curated user data").unwrap();
    fs::remove_file(root.join("project/state.json")).unwrap();
    dot(project.path(), home.path())
        .arg("init")
        .assert()
        .success();
    let m: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("project/state.json")).unwrap()).unwrap();
    assert_eq!(m["project_id"], state["project_id"]);
    assert_eq!(
        fs::read_to_string(root.join("memories/MEMORY.md")).unwrap(),
        "curated user data"
    );
    let mut packet = serde_json::json!({"project_id":m["project_id"], "fork_id":"other-fork"});
    fs::write(root.join("handoffs/current.json"), packet.to_string()).unwrap();
    dot(project.path(), home.path())
        .arg("resume")
        .assert()
        .failure();
    dot(project.path(), home.path())
        .args(["checkpoint", "must not record"])
        .assert()
        .failure();
    packet["fork_id"] = serde_json::Value::Null;
    packet["project_id"] = serde_json::json!("other-project");
    fs::write(root.join("handoffs/current.json"), packet.to_string()).unwrap();
    dot(project.path(), home.path())
        .arg("resume")
        .assert()
        .failure();
}

#[test]
fn linked_store_is_rejected_without_reading_or_writing_target() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("sentinel"), "untouched").unwrap();
    let link = project.path().join(".stateroot");
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    #[cfg(windows)]
    {
        // Junctions work without Developer Mode or symlink privileges.
        let result = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    dot(project.path(), home.path())
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("symlinks or reparse points"));
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
    assert_eq!(
        fs::read_to_string(outside.path().join("sentinel")).unwrap(),
        "untouched"
    );
    // Remove only the link, never traverse or delete the target tree.
    #[cfg(windows)]
    fs::remove_dir(&link).unwrap();
    #[cfg(unix)]
    fs::remove_file(&link).unwrap();
}

#[test]
fn snapshot_links_fail_closed_but_ignored_links_are_not_captured() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("sentinel.txt"), "outside project").unwrap();
    dot(project.path(), home.path())
        .arg("init")
        .assert()
        .success();
    let link = project.path().join("linked");
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    #[cfg(windows)]
    {
        let result = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(result.status.success());
    }
    dot(project.path(), home.path())
        .arg("snap")
        .assert()
        .failure()
        .stderr(predicate::str::contains("snapshot cannot follow"));
    assert!(!project.path().join(".git").exists());
    fs::write(project.path().join(".gitignore"), "/linked/\n").unwrap();
    dot(project.path(), home.path())
        .arg("snap")
        .assert()
        .success();
    let repo = git2::Repository::open(project.path()).unwrap();
    let id = stateroot_core::roots::latest_root(project.path())
        .unwrap()
        .unwrap();
    let tree = repo
        .find_commit(git2::Oid::from_str(&id).unwrap())
        .unwrap()
        .tree()
        .unwrap();
    assert!(tree.get_path(Path::new("linked/sentinel.txt")).is_err());
    // Move the same link into the store: validation rejects it before wiki traversal.
    fs::rename(&link, project.path().join(".stateroot/wiki")).unwrap();
    dot(project.path(), home.path())
        .args(["recall", "sentinel"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("symlinks or reparse points"));
    #[cfg(windows)]
    fs::remove_dir(project.path().join(".stateroot/wiki")).unwrap();
    #[cfg(unix)]
    fs::remove_file(project.path().join(".stateroot/wiki")).unwrap();
    assert_eq!(
        fs::read_to_string(outside.path().join("sentinel.txt")).unwrap(),
        "outside project"
    );
}

#[test]
fn shared_intelligence_is_explicit_read_only_and_all_pillars_are_accessible() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let shared = tempfile::tempdir().unwrap();
    dot(project.path(), home.path())
        .arg("init")
        .assert()
        .success();
    assert!(!project.path().join(".git").exists());
    let fixtures = [
        ("soul/SOUL.md", "shared persona evidence"),
        ("user/USER.md", "shared user evidence"),
        ("rules/example.md", "shared rule evidence"),
        ("learnings/general.md", "- **prefer evidence** <!-- id: lrn_test; label: observed; confidence: 0.8; scope: user; status: active -->"),
        ("plans/example.md", "shared plan body"),
        ("skills/example/SKILL.md", "shared skill body"),
        ("tools/mcp.json", "{\"mcpServers\":{}}"),
        ("memories/MEMORY.md", "shared memory evidence"),
    ];
    for (rel, body) in fixtures {
        let path = shared.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    fs::create_dir_all(project.path().join(".stateroot/soul")).unwrap();
    fs::write(
        project.path().join(".stateroot/soul/OVERLAY.md"),
        "project overlay evidence",
    )
    .unwrap();
    dot(project.path(), home.path())
        .arg("resume")
        .assert()
        .success()
        .stdout(predicate::str::contains("shared persona evidence").not())
        .stdout(predicate::str::contains("project overlay evidence"));
    assert!(!project.path().join(".git").exists());
    dot(project.path(), home.path())
        .arg("--shared-state")
        .arg(shared.path())
        .arg("resume")
        .assert()
        .success()
        .stdout(predicate::str::contains("shared persona evidence"))
        .stdout(predicate::str::contains("shared user evidence"))
        .stdout(predicate::str::contains("shared rule evidence"))
        .stdout(predicate::str::contains("prefer evidence"))
        .stdout(predicate::str::contains("shared memory evidence"))
        .stdout(predicate::str::contains(
            "runtime availability must be checked",
        ));
    for rel in [
        "plans/example.md",
        "skills/example/SKILL.md",
        "tools/mcp.json",
    ] {
        dot(project.path(), home.path())
            .arg("--shared-state")
            .arg(shared.path())
            .args(["read", "--shared", rel])
            .assert()
            .success();
    }
    for (rel, body) in fixtures {
        assert_eq!(fs::read_to_string(shared.path().join(rel)).unwrap(), body);
    }
    assert!(!shared.path().join("local").exists());
}

#[test]
fn read_rejects_escape_and_stdin_handoff_preserves_context_labels() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    dot(project.path(), home.path())
        .arg("init")
        .assert()
        .success();
    for path in ["../outside.txt", "plans/../../outside.txt"] {
        dot(project.path(), home.path())
            .args(["read", path])
            .assert()
            .failure();
    }
    dot(project.path(), home.path())
        .arg("read")
        .arg(project.path())
        .assert()
        .failure();
    dot(project.path(), home.path())
        .args(["read", "--shared", "soul/SOUL.md"])
        .assert()
        .failure();
    dot(project.path(), home.path()).args(["handoff", "--input", "-"])
        .write_stdin(r#"{"objective":"ship","task":"verify","context_summary":"evidence","next_actions":["review"],"failed_approaches":[{"approach":"first attempt","outcome":"failed","reason":"test failed"}],"context_only":["Observed background"]}"#)
        .assert().success();
    dot(project.path(), home.path())
        .arg("resume")
        .assert()
        .success()
        .stdout(predicate::str::contains("first attempt"))
        .stdout(predicate::str::contains("Observed background"));
}

#[test]
fn checkpoints_preserve_automatic_lineage_without_bookkeeping_churn() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    dot(project.path(), home.path())
        .arg("init")
        .assert()
        .success();
    fs::write(project.path().join("work.txt"), "first").unwrap();
    dot(project.path(), home.path())
        .args(["checkpoint", "first change"])
        .assert()
        .success();
    let first = stateroot_core::roots::latest_root(project.path()).unwrap();
    dot(project.path(), home.path())
        .args(["checkpoint", "only bookkeeping"])
        .assert()
        .success();
    assert_eq!(
        first,
        stateroot_core::roots::latest_root(project.path()).unwrap()
    );
    fs::write(project.path().join("work.txt"), "second").unwrap();
    dot(project.path(), home.path())
        .args(["checkpoint", "second change"])
        .assert()
        .success();
    assert_ne!(
        first,
        stateroot_core::roots::latest_root(project.path()).unwrap()
    );
    dot(project.path(), home.path())
        .arg("resume")
        .assert()
        .success()
        .stdout(predicate::str::contains("Work State Lineage"));
}
