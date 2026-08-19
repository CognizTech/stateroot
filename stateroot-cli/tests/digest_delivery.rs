//! Cross-harness identity delivery: first usable prompt gets the digest,
//! missed session-start recovers on first prompt, and sessions stay independent.

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

fn seed_marid(config_home: &Path, user_home: &Path) {
    std::fs::create_dir_all(config_home).expect("config home");
    std::fs::write(
        config_home.join("persona.md"),
        "## Working relationship\n\nYou are Marid, a precise systems engineer.\n",
    )
    .expect("persona");
    std::fs::create_dir_all(user_home.join(".stateroot/user")).expect("user dir");
    std::fs::write(
        user_home.join(".stateroot/user/USER.md"),
        "Human: Lin. Prefers short answers unless asked for depth.\n",
    )
    .expect("USER.md");
}

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
}

fn stdout_of(cmd: &assert_cmd::assert::Assert) -> String {
    String::from_utf8(cmd.get_output().stdout.clone()).expect("utf8")
}

fn contains_marid(text: &str) -> bool {
    text.contains("You are Marid") && text.contains("Active identity")
}

#[test]
fn no_handoff_identity_is_injected_and_recorded() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_marid(config_home.path(), user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());

    let first = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "SessionStart", "--harness", "claude-code"])
        .write_stdin(r#"{"session_id":"s-none"}"#)
        .assert()
        .success();
    let first_out = stdout_of(&first);
    assert!(contains_marid(&first_out), "first inject: {first_out}");
    assert!(
        project
            .path()
            .join(".stateroot/local/digest-delivery.v1.json")
            .is_file(),
        "ledger must be written even with no handoff"
    );

    let second = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "SessionStart", "--harness", "claude-code"])
        .write_stdin(r#"{"session_id":"s-none"}"#)
        .assert()
        .success();
    let second_out = stdout_of(&second);
    assert!(
        !contains_marid(&second_out),
        "same session must not reprint: {second_out}"
    );
}

#[test]
fn cursor_missed_session_start_recovers_on_first_prompt() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_marid(config_home.path(), user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());

    let prompt = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "beforeSubmitPrompt", "--harness", "cursor"])
        .write_stdin(r#"{"conversation_id":"skills-chat","prompt":"Can you list all of the skills that you have?"}"#)
        .assert()
        .success();
    let out = stdout_of(&prompt);
    assert!(out.contains("additional_context"), "cursor envelope: {out}");
    assert!(
        contains_marid(&out),
        "missed session-start must recover: {out}"
    );
}

#[test]
fn claude_primary_delivery_makes_prompt_capture_only() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_marid(config_home.path(), user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());

    let start = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "SessionStart", "--harness", "claude-code"])
        .write_stdin(r#"{"session_id":"claude-1"}"#)
        .assert()
        .success();
    assert!(contains_marid(&stdout_of(&start)));

    let prompt = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "UserPromptSubmit", "--harness", "claude-code"])
        .write_stdin(r#"{"session_id":"claude-1","prompt":"hello"}"#)
        .assert()
        .success();
    let prompt_out = stdout_of(&prompt);
    assert!(
        !contains_marid(&prompt_out),
        "prompt after successful primary must stay capture-only: {prompt_out}"
    );
}

#[test]
fn two_session_ids_are_independent() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_marid(config_home.path(), user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());

    for id in ["chat-a", "chat-b"] {
        let out = stateroot(config_home.path(), user_home.path(), project.path())
            .args(["hook", "beforeSubmitPrompt", "--harness", "cursor"])
            .write_stdin(format!(r#"{{"conversation_id":"{id}"}}"#))
            .assert()
            .success();
        let text = stdout_of(&out);
        assert!(contains_marid(&text), "{id}: {text}");
    }
}

#[test]
fn persona_change_redelivers_same_session() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_marid(config_home.path(), user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());

    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "SessionStart", "--harness", "claude-code"])
        .write_stdin(r#"{"session_id":"same"}"#)
        .assert()
        .success();

    std::fs::write(
        config_home.path().join("persona.md"),
        "## Working relationship\n\nYou are Yinyue now.\n",
    )
    .expect("persona change");

    let again = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "SessionStart", "--harness", "claude-code"])
        .write_stdin(r#"{"session_id":"same"}"#)
        .assert()
        .success();
    let out = stdout_of(&again);
    assert!(
        out.contains("You are Yinyue now."),
        "stale identity must reprint: {out}"
    );
}

#[test]
fn cross_harness_deliveries_are_independent() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_marid(config_home.path(), user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());

    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "SessionStart", "--harness", "claude-code"])
        .write_stdin(r#"{"session_id":"shared"}"#)
        .assert()
        .success();

    let cursor = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "beforeSubmitPrompt", "--harness", "cursor"])
        .write_stdin(r#"{"conversation_id":"shared"}"#)
        .assert()
        .success();
    assert!(
        contains_marid(&stdout_of(&cursor)),
        "cursor must still inject after claude delivered"
    );
}

#[test]
fn kimi_code_injects_on_prompt_submit_not_session_start() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_marid(config_home.path(), user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());

    let start = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "SessionStart", "--harness", "kimi-code"])
        .write_stdin(r#"{"session_id":"kimi-1"}"#)
        .assert()
        .success();
    assert!(
        !contains_marid(&stdout_of(&start)),
        "kimi-code SessionStart stdout is discarded"
    );

    let prompt = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "UserPromptSubmit", "--harness", "kimi-code"])
        .write_stdin(r#"{"session_id":"kimi-1"}"#)
        .assert()
        .success();
    assert!(contains_marid(&stdout_of(&prompt)));
}

#[test]
fn openclaw_prompt_build_injects_identity() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_marid(config_home.path(), user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "before_prompt_build", "--harness", "openclaw"])
        .write_stdin(r#"{"session_id":"oc-1"}"#)
        .assert()
        .success();
    assert!(contains_marid(&stdout_of(&out)));
}

#[test]
fn compaction_reinjects_after_session_delivery() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_marid(config_home.path(), user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());

    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "SessionStart", "--harness", "claude-code"])
        .write_stdin(r#"{"session_id":"compact-1"}"#)
        .assert()
        .success();

    let compact = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "PreCompact", "--harness", "claude-code"])
        .write_stdin(r#"{"session_id":"compact-1"}"#)
        .assert()
        .success();
    assert!(
        contains_marid(&stdout_of(&compact)),
        "compaction must re-inject identity"
    );
}

#[test]
fn legacy_hook_marker_suppresses_resume() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_marid(config_home.path(), user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--harness", "claude", "--force"])
        .assert()
        .success();

    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            project
                .path()
                .join(".stateroot/local/digest-delivery.v1.json"),
        )
        .expect("ledger"),
    )
    .expect("json");
    let fp = ledger["entries"][0]["content_fp"]
        .as_str()
        .expect("fp")
        .to_string();
    std::fs::remove_file(
        project
            .path()
            .join(".stateroot/local/digest-delivery.v1.json"),
    )
    .expect("remove ledger");
    std::fs::write(
        project.path().join(".stateroot/hook-resume-delivered.json"),
        format!(
            r#"{{"harness":"claude-code","handoff_seq":0,"content_fp":"{fp}","delivered_at":"2026-08-18T00:00:00Z"}}"#
        ),
    )
    .expect("legacy marker");

    let duplicate = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--harness", "claude"])
        .assert()
        .success();
    let out = stdout_of(&duplicate);
    assert!(out.contains("skipping duplicate"), "legacy migrate: {out}");
}

#[test]
fn sideragents_demo_both_chats_receive_marid_identity() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_marid(config_home.path(), user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());

    let skills = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "beforeSubmitPrompt", "--harness", "cursor"])
        .write_stdin(
            r#"{"conversation_id":"5d08e155-3828-403b-8a87-bf05871d083d","prompt":"Can you list all of the skills that you have?"}"#,
        )
        .assert()
        .success();
    let hi = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "beforeSubmitPrompt", "--harness", "cursor"])
        .write_stdin(r#"{"conversation_id":"5be521b7-ac2c-41f1-b2a7-676646ad3cb1","prompt":"Hi"}"#)
        .assert()
        .success();

    let skills_out = stdout_of(&skills);
    let hi_out = stdout_of(&hi);
    assert!(
        contains_marid(&skills_out),
        "list-skills chat: {skills_out}"
    );
    assert!(contains_marid(&hi_out), "hi chat: {hi_out}");
    assert!(skills_out.contains("additional_context"));
    assert!(hi_out.contains("additional_context"));
}

#[test]
fn resume_without_handoff_dedupes_until_force() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_marid(config_home.path(), user_home.path());
    init_project(config_home.path(), user_home.path(), project.path());

    let first = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--harness", "cursor"])
        .assert()
        .success();
    assert!(contains_marid(&stdout_of(&first)));

    let second = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--harness", "cursor"])
        .assert()
        .success();
    assert!(stdout_of(&second).contains("skipping duplicate"));

    let forced = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--harness", "cursor", "--force"])
        .assert()
        .success();
    assert!(contains_marid(&stdout_of(&forced)));
}
