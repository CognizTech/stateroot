//! Command Code harness integration — CLI-level contract.
//!
//! Covers the surfaces that only exist end to end: `resume --harness
//! commandcode` (the original failure), matcher-less hook installation, native
//! session import, and the pull-only taste import.

#[path = "support/mod.rs"]
mod support;

use std::path::Path;

use support::{homes, init_project, stateroot};

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, body).expect("write");
}

fn seed_transcript(user_home: &Path, project: &Path, session_id: &str) {
    let slug = project
        .file_name()
        .expect("slug")
        .to_string_lossy()
        .to_string();
    let file = user_home
        .join(".commandcode/projects")
        .join(slug)
        .join(format!("{session_id}.jsonl"));
    let events = [
        serde_json::json!({
            "type": "session",
            "version": 3,
            "id": session_id,
            "timestamp": "2026-11-18T10:00:00.000Z",
            "cwd": project,
        }),
        serde_json::json!({
            "type": "message",
            "id": "m1",
            "parentId": null,
            "timestamp": "2026-11-18T10:00:01.000Z",
            "message": {"role": "user", "content": [{"type": "text", "text": "implement the adapter"}]},
        }),
        serde_json::json!({
            "type": "message",
            "id": "m2",
            "parentId": "m1",
            "timestamp": "2026-11-18T10:00:02.000Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "Adapter implemented and verified against fixtures."}]},
        }),
    ];
    let body: Vec<String> = events.iter().map(serde_json::Value::to_string).collect();
    write(&file, &(body.join("\n") + "\n"));
    // A sidecar must never be mistaken for a transcript.
    write(
        &file.with_extension("checkpoints.jsonl"),
        "{\"type\":\"checkpoint\"}\n",
    );
}

#[test]
fn resume_accepts_the_commandcode_harness() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--harness", "commandcode"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(!stdout.trim().is_empty(), "digest must render");
    // Aliases resolve to the same harness (registry contract).
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--harness", "command-code"])
        .assert()
        .success();
}

#[test]
fn install_writes_matcherless_hooks_mcp_and_instruction_block() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    // Detection marker: Command Code is "installed" for this hermetic home.
    std::fs::create_dir_all(user_home.path().join(".commandcode")).expect("mkdir");

    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("install")
        .assert()
        .success();

    let settings = std::fs::read_to_string(user_home.path().join(".commandcode/settings.json"))
        .expect("settings.json");
    for event in ["SessionStart", "PreToolUse", "PostToolUse", "Stop"] {
        assert!(settings.contains(event), "{event} missing: {settings}");
    }
    assert!(
        !settings.contains("\"matcher\""),
        "Command Code never matches on a matcher: {settings}"
    );
    // Events Command Code does not implement must not be registered.
    assert!(!settings.contains("UserPromptSubmit"), "{settings}");
    assert!(!settings.contains("PreCompact"), "{settings}");

    let mcp =
        std::fs::read_to_string(user_home.path().join(".commandcode/mcp.json")).expect("mcp.json");
    assert!(mcp.contains("\"mcpServers\""), "{mcp}");
    assert!(mcp.contains("mcp-stdio"), "{mcp}");

    let block = std::fs::read_to_string(user_home.path().join(".commandcode/AGENTS.md"))
        .expect("AGENTS.md");
    assert!(block.contains("<!-- stateroot:begin -->"), "{block}");
}

#[test]
fn session_sync_imports_native_commandcode_transcripts() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    seed_transcript(user_home.path(), project.path(), "cc-session-1");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["session", "sync", "--harness", "commandcode"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("commandcode"), "stdout: {stdout}");

    let list = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["session", "list", "--harness", "commandcode"])
        .assert()
        .success();
    let listed = String::from_utf8(list.get_output().stdout.clone()).expect("utf8");
    assert!(listed.contains("cc-session-1"), "listed: {listed}");
    assert!(
        !listed.contains("checkpoints"),
        "sidecar files are not sessions: {listed}"
    );
}

#[test]
fn taste_import_is_pull_only_and_reports_dry_run() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let taste = project.path().join(".commandcode/taste/taste.md");
    write(
        &taste,
        "# Rust\n\n- Prefer const over let. Confidence: 0.95\n",
    );
    let original = std::fs::read_to_string(&taste).expect("read");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learnings", "import-taste", "--dry-run", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert_eq!(doc["imported"], 1, "{doc}");
    assert_eq!(doc["harness"], "commandcode");

    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learnings", "import-taste"])
        .assert()
        .success();
    let listed = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learnings", "list", "--status", "candidate"])
        .assert()
        .success();
    let listed_stdout = String::from_utf8(listed.get_output().stdout.clone()).expect("utf8");
    assert!(
        listed_stdout.contains("Prefer const over let"),
        "candidate imported: {listed_stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(&taste).expect("read"),
        original,
        "Command Code's taste store is never written"
    );
}
