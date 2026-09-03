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

/// Seed the cross-scope traces a project's sessions leave behind.
fn seed_traces(user_home: &Path, project: &Path, workspace_id: &str) {
    // Traces freeze the path as a running process sees it — canonicalize so
    // the fixture matches CLI-side resolution on every host (Windows %TEMP%
    // is an 8.3-short path on some runners).
    let canonical = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    let native = canonical.to_string_lossy().to_string();
    // Workspace learnings bubble.
    let bubble = user_home.join(format!(".stateroot/workspaces/{workspace_id}/learnings"));
    std::fs::create_dir_all(&bubble).unwrap();
    std::fs::write(bubble.join("general.md"), "- a learning\n").unwrap();
    // Persona-injection state keyed to this path.
    let persona = user_home.join(".stateroot/local/persona-injection");
    std::fs::create_dir_all(&persona).unwrap();
    std::fs::write(
        persona.join("aaa.json"),
        serde_json::json!({"key": format!("kimi-code:{native}:sess-1")}).to_string(),
    )
    .unwrap();
    // Session-registry anchor (plus one unrelated anchor that must survive).
    let local = user_home.join(".stateroot/local");
    std::fs::create_dir_all(&local).unwrap();
    let mut anchors = serde_json::Map::new();
    anchors.insert(
        format!("kimi-code|{native}"),
        serde_json::json!({"session_id":"anon-1","last_seen":"2026-09-03T06:00:00Z","last_event":"session_end"}),
    );
    anchors.insert(
        "kimi-code|/elsewhere".to_string(),
        serde_json::json!({"session_id":"anon-2","last_seen":"2026-09-03T06:00:00Z","last_event":"user_prompt_submit"}),
    );
    std::fs::write(
        local.join("session-registry.json"),
        serde_json::Value::Object(anchors).to_string(),
    )
    .unwrap();
    // kimi-code transcript session for this path.
    let session_dir = user_home.join(".kimi-code/sessions/wd_demo/session_demo-1");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(session_dir.join("wire.jsonl"), "{}\n").unwrap();
    std::fs::write(
        user_home.join(".kimi-code/session_index.jsonl"),
        serde_json::json!({
            "sessionId": "session_demo-1",
            "sessionDir": session_dir.to_string_lossy(),
            "workDir": native,
        })
        .to_string(),
    )
    .unwrap();
    // claude-code transcript dir for this path. Slug from the normalized
    // path form (no `\\?\` verbatim prefix — `?` is illegal on Windows),
    // colon folded to a dash; the CLI tries every spelling's variants.
    let norm = {
        let mut s = native.trim().replace('\\', "/");
        if let Some(rest) = s.strip_prefix("//?/") {
            s = rest.to_string();
        }
        if s.len() >= 2 && s.as_bytes()[1] == b':' {
            let mut chars: Vec<char> = s.chars().collect();
            chars[0] = chars[0].to_ascii_lowercase();
            s = chars.into_iter().collect();
        }
        s
    };
    let slug = norm.replace(['/', ':'], "-");
    let claude_dir = user_home.join(format!(".claude/projects/{slug}"));
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(claude_dir.join("s.jsonl"), "{}\n").unwrap();
}

#[test]
fn remove_full_purges_cross_scope_traces() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");

    // Project A: plain remove must NOT touch cross-scope traces.
    let project_a = tempfile::tempdir().expect("project a");
    init_project(config_home.path(), user_home.path(), project_a.path());
    let manifest_a = std::fs::read_to_string(project_a.path().join(".stateroot/manifest.json"))
        .expect("manifest a");
    let ws_a = serde_json::from_str::<serde_json::Value>(&manifest_a).unwrap()["project_id"]
        .as_str()
        .unwrap()
        .to_string();
    seed_traces(user_home.path(), project_a.path(), &ws_a);
    let kimi_session_a = user_home
        .path()
        .join(".kimi-code/sessions/wd_demo/session_demo-1");

    stateroot(config_home.path(), user_home.path(), project_a.path())
        .args(["remove", "--yes"])
        .assert()
        .success();
    assert!(
        kimi_session_a.is_dir(),
        "plain remove keeps harness transcripts"
    );
    assert!(
        user_home
            .path()
            .join(format!(".stateroot/workspaces/{ws_a}"))
            .is_dir(),
        "plain remove keeps the workspace bubble"
    );

    // Project B: --full purges everything keyed to its path.
    let project_b = tempfile::tempdir().expect("project b");
    init_project(config_home.path(), user_home.path(), project_b.path());
    let manifest_b = std::fs::read_to_string(project_b.path().join(".stateroot/manifest.json"))
        .expect("manifest b");
    let ws_b = serde_json::from_str::<serde_json::Value>(&manifest_b).unwrap()["project_id"]
        .as_str()
        .unwrap()
        .to_string();
    seed_traces(user_home.path(), project_b.path(), &ws_b);

    let out = stateroot(config_home.path(), user_home.path(), project_b.path())
        .env("STATEROOT_REMOVE_DEBUG", "1")
        .args(["remove", "--yes", "--full"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("workspace learnings/state bubble"),
        "{stdout}"
    );

    assert!(
        !user_home
            .path()
            .join(format!(".stateroot/workspaces/{ws_b}"))
            .exists(),
        "workspace bubble purged"
    );
    let persona_left: Vec<_> =
        std::fs::read_dir(user_home.path().join(".stateroot/local/persona-injection"))
            .map(|r| r.flatten().collect())
            .unwrap_or_default();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(
        persona_left.is_empty(),
        "persona keys purged: {persona_left:?}\ncli debug: {stderr}"
    );
    let kimi_session_b = user_home
        .path()
        .join(".kimi-code/sessions/wd_demo/session_demo-1");
    assert!(!kimi_session_b.is_dir(), "kimi transcript dir purged");
    let index = std::fs::read_to_string(user_home.path().join(".kimi-code/session_index.jsonl"))
        .expect("index");
    assert!(
        index.contains(r#"{"sessionId":"session_demo-1","deleted":true}"#),
        "session marked deleted: {index}"
    );
    let registry = std::fs::read_to_string(
        user_home
            .path()
            .join(".stateroot/local/session-registry.json"),
    )
    .expect("registry");
    assert!(
        !registry.contains("anon-1"),
        "project anchor pruned: {registry}"
    );
    assert!(
        registry.contains("anon-2"),
        "unrelated anchor survives: {registry}"
    );
    let canonical_b =
        std::fs::canonicalize(project_b.path()).unwrap_or_else(|_| project_b.path().to_path_buf());
    let norm_b = {
        let mut s = canonical_b.to_string_lossy().trim().replace('\\', "/");
        if let Some(rest) = s.strip_prefix("//?/") {
            s = rest.to_string();
        }
        if s.len() >= 2 && s.as_bytes()[1] == b':' {
            let mut chars: Vec<char> = s.chars().collect();
            chars[0] = chars[0].to_ascii_lowercase();
            s = chars.into_iter().collect();
        }
        s
    };
    let slug_b = norm_b.replace(['/', ':'], "-");
    assert!(
        !user_home
            .path()
            .join(format!(".claude/projects/{slug_b}"))
            .exists(),
        "claude transcript dir purged"
    );
}
