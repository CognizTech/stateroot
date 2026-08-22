//! Persona injection scheduler — end-to-end through the hook binary.
//! FULL on boundaries/change, COMPRESSED on the 8th, dedupe window,
//! per-harness start detection, state in the user-global local dir.

use std::path::Path;

use assert_cmd::Command;

fn stateroot(config_home: &Path, user_home: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::cargo_bin("stateroot").expect("binary");
    cmd.env("STATEROOT_HOME", config_home)
        .env("STATEROOT_TEST_HOME", user_home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env_remove("STATEROOT_HOOK_NOW")
        .current_dir(cwd);
    cmd
}

fn seed_identity(user_home: &Path) {
    let soul_dir = user_home.join(".stateroot/soul");
    std::fs::create_dir_all(&soul_dir).expect("soul dir");
    std::fs::write(
        soul_dir.join("SOUL.md"),
        "# Soul\n\nI am the Test Djinn — exact and brief.\n\n## Communication\n\n- Tone: direct\n\n## Principles\n\n- be exact\n",
    )
    .expect("soul");
    let user_dir = user_home.join(".stateroot/user");
    std::fs::create_dir_all(&user_dir).expect("user dir");
    std::fs::write(user_dir.join("USER.md"), "# User\n\nName: Lin\n").expect("user");
}

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
}

fn hook_prompt(
    config_home: &Path,
    user_home: &Path,
    cwd: &Path,
    session: &str,
    now: i64,
) -> String {
    let out = stateroot(config_home, user_home, cwd)
        .env("STATEROOT_HOOK_NOW", now.to_string())
        .args(["hook", "UserPromptSubmit", "--harness", "kimi-code"])
        .write_stdin(format!(r#"{{"session_id": "{session}"}}"#))
        .assert()
        .success();
    String::from_utf8(out.get_output().stdout.clone()).expect("utf8")
}

const MARKER: &str = "be exact";

#[test]
fn first_prompt_full_then_dedupe_then_compressed_cadence() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    seed_identity(user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());

    // First prompt_submit of the session → FULL.
    let out = hook_prompt(
        config_home.path(),
        user_home.path(),
        project.path(),
        "s1",
        1_000,
    );
    assert!(out.contains(MARKER), "first prompt must inject FULL: {out}");

    // Prompts 2–7: dedupe (within 3 prompts / 60s windows as spaced).
    for i in 2..=7 {
        let out = hook_prompt(
            config_home.path(),
            user_home.path(),
            project.path(),
            "s1",
            1_000 + i * 61,
        );
        assert!(out.trim().is_empty(), "prompt {i} must be silent: {out}");
    }

    // 8th: COMPRESSED — pointer with voice anchor, no persona body.
    let out = hook_prompt(
        config_home.path(),
        user_home.path(),
        project.path(),
        "s1",
        1_000 + 8 * 61,
    );
    assert!(
        out.contains("unchanged since last full injection"),
        "compressed pointer: {out}"
    );
    assert!(out.contains("SOUL.md"), "persona path in pointer: {out}");
    assert!(out.contains("be exact"), "voice anchor in pointer: {out}");
    assert!(
        !out.contains("## Principles"),
        "no full body in compressed: {out}"
    );

    // State file lives in the user-global local dir (never the project).
    assert!(user_home
        .path()
        .join(".stateroot/local/persona-injection.json")
        .is_file());
    assert!(!project
        .path()
        .join(".stateroot/local/persona-injection.json")
        .exists());
}

#[test]
fn content_change_forces_full_and_new_session_restarts() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    seed_identity(user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());

    let out = hook_prompt(
        config_home.path(),
        user_home.path(),
        project.path(),
        "s1",
        5_000,
    );
    assert!(out.contains(MARKER), "first FULL: {out}");

    // Edit the persona → next prompt (past the dedupe windows) is FULL again.
    std::fs::write(
        user_home.path().join(".stateroot/soul/SOUL.md"),
        "# Soul\n\n## Communication\n\n- Tone: warm\n\n## Principles\n\n- stay curious\n",
    )
    .expect("edit soul");
    let out = hook_prompt(
        config_home.path(),
        user_home.path(),
        project.path(),
        "s1",
        5_000 + 200,
    );
    assert!(
        out.contains("stay curious"),
        "content change forces FULL: {out}"
    );

    // A different session id starts over with FULL immediately.
    let out = hook_prompt(
        config_home.path(),
        user_home.path(),
        project.path(),
        "s2",
        5_000 + 210,
    );
    assert!(out.contains("stay curious"), "new session id → FULL: {out}");
}

#[test]
fn claude_session_start_full_and_compact_boundary_full() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    seed_identity(user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());

    // session_start → FULL (claude's JSON envelope).
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("STATEROOT_HOOK_NOW", "9000")
        .args(["hook", "SessionStart", "--harness", "claude-code"])
        .write_stdin(r#"{"session_id": "c1"}"#)
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("additionalContext"),
        "claude envelope: {stdout}"
    );
    assert!(stdout.contains(MARKER), "session_start FULL: {stdout}");

    // pre_compact boundary → FULL again (dedupe window spaced).
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("STATEROOT_HOOK_NOW", "9061")
        .args(["hook", "PreCompact", "--harness", "claude-code"])
        .write_stdin(r#"{"session_id": "c1"}"#)
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains(MARKER), "pre_compact FULL: {stdout}");
}

#[test]
fn no_state_first_call_is_full_and_dedupe_blocks_immediate_repeat() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    seed_identity(user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());

    let out = hook_prompt(
        config_home.path(),
        user_home.path(),
        project.path(),
        "s1",
        100,
    );
    assert!(out.contains(MARKER), "no-state first call → FULL: {out}");
    // Immediate repeat (same minute, prompt 2): dedupe → silent.
    let out = hook_prompt(
        config_home.path(),
        user_home.path(),
        project.path(),
        "s1",
        110,
    );
    assert!(out.trim().is_empty(), "immediate repeat deduped: {out}");
}
