//! Init seeding tests — deterministic seed always, opt-in LLM enrichment
//! behind `--synthesize` (wiremock for the API path, a PATH fixture script
//! for the harness-CLI path; zero real network).

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

fn homes() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_home = tempfile::tempdir().expect("config home");
    std::fs::create_dir_all(config_home.path()).expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    (config_home, user_home)
}

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent");
    }
    std::fs::write(path, body).expect("write");
}

fn commit_all(repo: &git2::Repository, subject: &str) {
    let sig = git2::Signature::now("Test", "test@example.com").expect("sig");
    let mut index = repo.index().expect("index");
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .expect("add all");
    index.write().expect("index write");
    let tree = repo
        .find_tree(index.write_tree().expect("tree oid"))
        .expect("tree");
    let parents: Vec<git2::Commit> = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .and_then(|oid| repo.find_commit(oid).ok())
        .into_iter()
        .collect();
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, subject, &tree, &parent_refs)
        .expect("commit");
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("json file")).expect("json")
}

#[test]
fn init_seeds_state_objectives_memory_and_first_handoff() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    write(
        project.path(),
        "README.md",
        "# SiderAgents\n\nLive upgrade target.\n",
    );
    write(
        project.path(),
        "TODO.md",
        "# Todo\n\n- [ ] wire the parser\n- [x] done\n",
    );
    let repo = git2::Repository::init(project.path()).expect("git init");
    commit_all(&repo, "first commit");
    write(project.path(), "src/main.rs", "fn main() {}\n");
    commit_all(&repo, "second commit");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("init")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("seeded objective from README.md (observed)"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("seeded handoffs/current.json (observed)"),
        "stdout: {stdout}"
    );

    let state = read_json(&project.path().join(".stateroot/project/state.json"));
    assert_eq!(
        state["objective"].as_str().expect("objective"),
        "SiderAgents — Live upgrade target."
    );
    assert_eq!(state["current_phase"], "init");

    let objectives =
        std::fs::read_to_string(project.path().join(".stateroot/project/objectives.md"))
            .expect("objectives");
    assert!(
        objectives.contains("SiderAgents — Live upgrade target."),
        "{objectives}"
    );
    assert!(objectives.contains("- wire the parser"), "{objectives}");

    let memory = std::fs::read_to_string(project.path().join(".stateroot/memories/MEMORY.md"))
        .expect("memory");
    assert!(memory.contains("## Seed (observed at init)"), "{memory}");
    assert!(
        memory.contains("Recent commits at init: second commit; first commit"),
        "{memory}"
    );

    let handoff = read_json(&project.path().join(".stateroot/handoffs/current.json"));
    assert_eq!(handoff["seq"], 1);
    assert_eq!(handoff["origin"], "init-seed");
    assert_eq!(handoff["created_by_harness"], "cli");
    assert_eq!(handoff["provenance"], "observed");
    assert_eq!(
        handoff["next_actions"],
        serde_json::json!(["wire the parser"])
    );
}

#[test]
fn init_seed_writes_placeholders_only() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    write(project.path(), "README.md", "# P\n\nGoal.\n");

    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("init")
        .assert()
        .success();
    let objectives_path = project.path().join(".stateroot/project/objectives.md");
    std::fs::write(&objectives_path, "# Objectives\n\nUser-curated goal.\n").expect("edit");

    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("init")
        .assert()
        .success();
    let objectives = std::fs::read_to_string(&objectives_path).expect("objectives");
    assert_eq!(objectives, "# Objectives\n\nUser-curated goal.\n");
}

#[test]
fn init_empty_dir_seeds_nothing() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("init")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("nothing to seed (no repo docs)"),
        "stdout: {stdout}"
    );
    assert!(!project
        .path()
        .join(".stateroot/handoffs/current.json")
        .exists());
}

#[test]
fn init_synthesize_without_backends_keeps_deterministic_seed() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    write(project.path(), "README.md", "# P\n\nObserved goal.\n");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["init", "--synthesize"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(
        stdout.contains("seeded objective from README.md (observed)"),
        "stdout: {stdout}"
    );
    assert!(stderr.contains("synthesis skipped"), "stderr: {stderr}");
    let state = read_json(&project.path().join(".stateroot/project/state.json"));
    assert_eq!(state["objective"], "P — Observed goal.");
}

#[cfg(unix)]
#[test]
fn init_synthesize_auto_prefers_a_harness_cli() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    write(project.path(), "README.md", "# P\n\nObserved goal.\n");

    let bin = tempfile::tempdir().expect("bin");
    let fake = bin.path().join("claude");
    std::fs::write(
        &fake,
        "#!/bin/sh\nprintf '%s\\n' '{\"objective\":\"synth objective\",\"context_summary\":\"synth context\",\"next_actions\":[\"synth next\"],\"memory_facts\":[\"synth fact\"]}'\n",
    )
    .expect("fake harness");
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").expect("PATH")
    );

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("STATEROOT_TEST_CMD_PROBES", "claude")
        .env("PATH", path)
        .args(["init", "--synthesize"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("synthesized seed via claude (unverified)"),
        "stdout: {stdout}"
    );

    let state = read_json(&project.path().join(".stateroot/project/state.json"));
    assert_eq!(state["objective"], "synth objective");
    let handoff = read_json(&project.path().join(".stateroot/handoffs/current.json"));
    assert_eq!(handoff["provenance"], "synthesized — unverified (claude)");
    assert_eq!(handoff["objective"], "synth objective");
    assert_eq!(handoff["next_actions"], serde_json::json!(["synth next"]));
    assert!(handoff.get("synthesized").is_some(), "handoff: {handoff}");
    let memory = std::fs::read_to_string(project.path().join(".stateroot/memories/MEMORY.md"))
        .expect("memory");
    assert!(
        memory.contains("## Seed (synthesized — unverified (claude) at init)"),
        "{memory}"
    );
    assert!(!memory.contains("## Seed (observed at init)"), "{memory}");
}

#[tokio::test]
async fn init_synthesize_with_deepseek_uses_the_api() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let seed = serde_json::json!({
        "objective": "api objective",
        "context_summary": "api context",
        "next_actions": ["api next"],
        "memory_facts": ["api fact"],
    });
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": seed.to_string()}}]
        })))
        .mount(&server)
        .await;

    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    write(project.path(), "README.md", "# P\n\nObserved goal.\n");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("DEEPSEEK_API_KEY", "test-key")
        .env("STATEROOT_SYNTHESIS_API_BASE", server.uri())
        .args(["init", "--synthesize", "--synthesize-with", "deepseek"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("synthesized seed via deepseek (unverified)"),
        "stdout: {stdout}"
    );

    let state = read_json(&project.path().join(".stateroot/project/state.json"));
    assert_eq!(state["objective"], "api objective");
    let handoff = read_json(&project.path().join(".stateroot/handoffs/current.json"));
    assert_eq!(handoff["provenance"], "synthesized — unverified (deepseek)");
}

#[test]
fn init_synthesize_with_unknown_backend_errors_with_valid_list() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["init", "--synthesize", "--synthesize-with", "bogus"])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(
        stderr.contains("unknown synthesis backend 'bogus'"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("deepseek, openai"), "stderr: {stderr}");
}
