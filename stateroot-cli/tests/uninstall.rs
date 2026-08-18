//! Full-uninstall tests (fixture homes; the self-delete helper is tested by
//! structure — the test binary is never harmed).

use std::path::Path;

use assert_cmd::Command;
use serde_json::json;

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

/// Seed a home with stateroot + foreign registrations in every shape.
fn seed_registrations(home: &Path) {
    // cursor MCP: ours + foreign
    std::fs::create_dir_all(home.join(".cursor")).unwrap();
    std::fs::write(
        home.join(".cursor/mcp.json"),
        json!({"mcpServers": {
            "stateroot": {"command": "stateroot", "args": ["mcp-stdio"]},
            "github": {"command": "npx", "args": ["srv"]}
        }})
        .to_string(),
    )
    .unwrap();
    // hermes MCP: YAML shape, ours + foreign, with no backup to restore.
    std::fs::create_dir_all(home.join(".hermes")).unwrap();
    std::fs::write(
        home.join(".hermes/config.yaml"),
        "model: gpt-4\nmcp_servers:\n  stateroot:\n    command: stateroot\n    args: [mcp-stdio]\n  foreign:\n    command: other\n",
    )
    .unwrap();
    // claude hooks (nested) + instruction block with user content
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(
        home.join(".claude/settings.json"),
        json!({"hooks": {
            "SessionStart": [{"matcher": "", "hooks": [{"type": "command", "command": "stateroot hook SessionStart --harness claude-code"}]}],
            "UserPromptSubmit": [{"matcher": "", "hooks": [{"type": "command", "command": "my-own-linter"}]}]
        }})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        home.join(".claude/CLAUDE.md"),
        "# My rules\n\nUser text stays.\n\n<!-- stateroot:begin -->\nmanaged block\n<!-- stateroot:end -->\n",
    )
    .unwrap();
    // openclaw extension + legacy plugins debris
    std::fs::create_dir_all(home.join(".openclaw/extensions/stateroot")).unwrap();
    std::fs::write(
        home.join(".openclaw/extensions/stateroot/index.ts"),
        "export function register() {}",
    )
    .unwrap();
    std::fs::create_dir_all(home.join(".openclaw/plugins/stateroot")).unwrap();
    std::fs::write(home.join(".openclaw/plugins/stateroot/index.ts"), "old").unwrap();
}

#[test]
fn uninstall_cleans_registrations_and_preserves_foreign_and_data() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    seed_registrations(user_home.path());
    // user-global data that must survive without --purge
    let soul_dir = user_home.path().join(".stateroot/soul");
    std::fs::create_dir_all(&soul_dir).unwrap();
    std::fs::write(soul_dir.join("SOUL.md"), "# Soul\n").unwrap();

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["uninstall", "--yes"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");

    // ours removed
    let mcp: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(user_home.path().join(".cursor/mcp.json")).unwrap(),
    )
    .unwrap();
    assert!(mcp["mcpServers"].get("stateroot").is_none(), "{mcp}");
    // foreign preserved
    assert!(mcp["mcpServers"].get("github").is_some(), "{mcp}");

    let hermes: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(user_home.path().join(".hermes/config.yaml")).unwrap(),
    )
    .unwrap();
    assert!(hermes["mcp_servers"]["stateroot"].is_null());
    assert_eq!(hermes["mcp_servers"]["foreign"]["command"], "other");
    assert!(!stdout.contains("hermes MCP removal failed"), "{stdout}");

    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(user_home.path().join(".claude/settings.json")).unwrap(),
    )
    .unwrap();
    assert!(
        settings["hooks"].get("SessionStart").is_none(),
        "{settings}"
    );
    assert!(
        settings["hooks"].get("UserPromptSubmit").is_some(),
        "{settings}"
    );

    let claude_md = std::fs::read_to_string(user_home.path().join(".claude/CLAUDE.md")).unwrap();
    assert!(claude_md.contains("User text stays."), "{claude_md}");
    assert!(!claude_md.contains("managed block"), "{claude_md}");

    assert!(!user_home
        .path()
        .join(".openclaw/extensions/stateroot")
        .exists());
    assert!(!user_home
        .path()
        .join(".openclaw/plugins/stateroot")
        .exists());

    // user-global data preserved; config dir gone; project untouched
    assert!(soul_dir.join("SOUL.md").is_file());
    assert!(!config_home.path().exists());
    assert!(project.path().join(".stateroot/manifest.json").is_file());
    assert!(stdout.contains("left untouched"), "{stdout}");
    assert!(stdout.contains("kept user-global data"), "{stdout}");

    // self-delete refused (test exe lives in a cargo target dir)
    assert!(stdout.contains("not self-deleting"), "{stdout}");
}

#[test]
fn uninstall_purge_removes_user_global_data() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let soul_dir = user_home.path().join(".stateroot/soul");
    std::fs::create_dir_all(&soul_dir).unwrap();
    std::fs::write(soul_dir.join("SOUL.md"), "# Soul\n").unwrap();

    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["uninstall", "--yes", "--purge"])
        .assert()
        .success();
    assert!(!user_home.path().join(".stateroot").exists());
    assert!(project.path().join(".stateroot/manifest.json").is_file());
}

#[test]
fn uninstall_interactive_refusal_without_yes() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    // non-interactive without --yes refuses (exit non-zero, nothing removed)
    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("uninstall")
        .assert()
        .failure();
    assert!(
        config_home.path().exists(),
        "config dir must survive refusal"
    );
}
