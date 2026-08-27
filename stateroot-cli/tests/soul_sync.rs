//! `stateroot soul sync` — end-to-end through the binary: bootstrap link,
//! native-edit adoption, canonical-edit push, conflict + accept.

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

fn soul_sync(config_home: &Path, user_home: &Path, cwd: &Path, args: &[&str]) -> String {
    let out = stateroot(config_home, user_home, cwd)
        .arg("soul")
        .arg("sync")
        .args(args)
        .assert()
        .success();
    String::from_utf8(out.get_output().stdout.clone()).expect("utf8")
}

fn seed(home: &Path) {
    // Canonical soul (openclaw-shaped, as adopted at setup)…
    let soul_dir = home.join(".stateroot/soul");
    std::fs::create_dir_all(&soul_dir).expect("soul dir");
    std::fs::write(
        soul_dir.join("SOUL.md"),
        "<!-- stateroot:soul origin=openclaw:/x; at=t -->\n# Soul\n\n<!-- composed from openclaw workspace /x -->\n\n## Identity (IDENTITY.md)\n\n- Name: Marid\n\n## Persona (SOUL.md)\n\nI am Marid, jinn of the lamp.\n",
    )
    .expect("canonical");
    // …and the matching openclaw native files.
    let ws = home.join(".openclaw/workspace");
    std::fs::create_dir_all(&ws).expect("ws");
    std::fs::write(ws.join("IDENTITY.md"), "# Identity\n\n- Name: Marid\n").expect("id");
    std::fs::write(
        ws.join("SOUL.md"),
        "# Soul\n\nI am Marid, jinn of the lamp.\n",
    )
    .expect("soul");
}

#[test]
fn sync_links_then_adopts_native_edit_end_to_end() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    seed(user_home.path());
    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("init")
        .assert()
        .success();

    // Bootstrap: the already-equal pair links silently.
    let out = soul_sync(config_home.path(), user_home.path(), project.path(), &[]);
    assert!(out.contains("openclaw: linked"), "bootstrap: {out}");

    // Native edit → adopted into the canonical soul.
    std::fs::write(
        user_home.path().join(".openclaw/workspace/SOUL.md"),
        "# Soul\n\nI am Marid, jinn of the lamp — now poetic.\n",
    )
    .expect("native edit");
    let out = soul_sync(config_home.path(), user_home.path(), project.path(), &[]);
    assert!(out.contains("adopted"), "adoption: {out}");
    let canonical = std::fs::read_to_string(user_home.path().join(".stateroot/soul/SOUL.md"))
        .expect("canonical");
    assert!(
        canonical.contains("now poetic"),
        "canonical adopted: {canonical}"
    );

    // Converged: a dry run reports nothing further.
    let out = soul_sync(
        config_home.path(),
        user_home.path(),
        project.path(),
        &["--dry-run"],
    );
    assert!(!out.contains("would push"), "converged: {out}");

    // Canonical edit (any harness) → pushed back into openclaw's files.
    let canonical = canonical.replace("now poetic", "now precise");
    std::fs::write(user_home.path().join(".stateroot/soul/SOUL.md"), canonical).expect("edit");
    let out = soul_sync(config_home.path(), user_home.path(), project.path(), &[]);
    assert!(out.contains("pushed"), "push: {out}");
    let native = std::fs::read_to_string(user_home.path().join(".openclaw/workspace/SOUL.md"))
        .expect("native");
    assert!(native.contains("now precise"), "native received: {native}");
    assert!(
        native.contains("stateroot:synced"),
        "managed marker: {native}"
    );
}

#[test]
fn sync_conflict_and_accept_mine() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    seed(user_home.path());
    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("init")
        .assert()
        .success();
    let _ = soul_sync(config_home.path(), user_home.path(), project.path(), &[]);

    // Both sides move.
    std::fs::write(
        user_home.path().join(".openclaw/workspace/SOUL.md"),
        "# Soul\n\nnative voice\n",
    )
    .expect("edit");
    let canonical = std::fs::read_to_string(user_home.path().join(".stateroot/soul/SOUL.md"))
        .expect("canonical")
        .replace("jinn of the lamp", "canonical voice");
    std::fs::write(user_home.path().join(".stateroot/soul/SOUL.md"), canonical).expect("edit");

    let out = soul_sync(config_home.path(), user_home.path(), project.path(), &[]);
    assert!(out.contains("CONFLICT (openclaw)"), "conflict: {out}");

    let out = soul_sync(
        config_home.path(),
        user_home.path(),
        project.path(),
        &["--accept-mine", "openclaw"],
    );
    assert!(out.contains("pushed"), "accept-mine pushes: {out}");
    let native = std::fs::read_to_string(user_home.path().join(".openclaw/workspace/SOUL.md"))
        .expect("native");
    assert!(
        native.contains("canonical voice"),
        "canonical won: {native}"
    );
    // Resolved: the next pass is clean.
    let out = soul_sync(config_home.path(), user_home.path(), project.path(), &[]);
    assert!(!out.contains("CONFLICT"), "resolved: {out}");
}
