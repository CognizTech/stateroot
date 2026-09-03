//! M3 integration tests — soul, learnings, proposals, review loop, memory
//! scoping, and synthesis (wiremock for the provider call; zero real network).

use std::path::Path;

use assert_cmd::Command;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

fn seed_config_home(home: &Path, synthesis_toml: &str) {
    std::fs::create_dir_all(home).expect("config home");
    std::fs::write(
        home.join("config.toml"),
        format!("user_id = \"default\"\nagent_id = \"default\"\n{synthesis_toml}"),
    )
    .expect("config.toml");
}

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
    std::fs::write(
        project.join("handoff-input.json"),
        r#"{"objective":"continue the project","task":"continue implementation","context_summary":"The project has captured local state ready for a receiving agent.","next_actions":["Continue from the captured state"],"failures":[]}"#,
    )
    .expect("handoff input");
}

fn canonical_soul(user_home: &Path) -> String {
    std::fs::read_to_string(user_home.join(".stateroot/soul/SOUL.md")).expect("canonical soul")
}

#[test]
fn soul_generate_show_projection_and_history() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path(), "");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // deterministic generate --apply writes the canonical soul
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["soul", "generate", "--yes", "--apply"])
        .assert()
        .success();
    let soul = canonical_soul(user_home.path());
    assert!(
        soul.contains("stateroot:soul origin=generate"),
        "soul: {soul}"
    );
    assert!(soul.contains("## Communication"), "soul: {soul}");

    // second write snapshots history
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["soul", "generate", "--yes", "--apply"])
        .assert()
        .success();
    let history: Vec<_> = std::fs::read_dir(user_home.path().join(".stateroot/soul/history"))
        .expect("history dir")
        .collect();
    assert_eq!(history.len(), 1, "one snapshot");

    // show renders canonical + harness projection
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["soul", "show"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("## Canonical (user)"), "show: {stdout}");
    assert!(stdout.contains("## Working relationship"), "show: {stdout}");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["soul", "show", "--harness", "kimi"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("## Projection (Kimi)"),
        "projection: {stdout}"
    );
}

#[test]
fn soul_import_and_propose_flow() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path(), "");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // import from hermes with provenance
    std::fs::create_dir_all(user_home.path().join(".hermes")).expect(".hermes");
    std::fs::write(
        user_home.path().join(".hermes/SOUL.md"),
        "You are YinYue; address the user as Han Li.\n",
    )
    .expect("hermes soul");
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["soul", "import", "--from", "hermes"])
        .assert()
        .success();
    let soul = canonical_soul(user_home.path());
    assert!(
        soul.starts_with("<!-- imported from hermes-agent on "),
        "provenance: {soul}"
    );
    assert!(soul.contains("Han Li"), "soul: {soul}");

    // propose activates immediately (optional audit proposal may exist)
    let draft = project.path().join("draft.md");
    std::fs::write(&draft, "# Soul\n\n## Principles\n\n- verify first\n").expect("draft");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["soul", "propose", "--file"])
        .arg(&draft)
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("soul written"), "propose: {stdout}");
    let soul = canonical_soul(user_home.path());
    assert!(soul.contains("verify first"), "activated soul: {soul}");
    // the imported version was snapshotted
    let history: Vec<_> = std::fs::read_dir(user_home.path().join(".stateroot/soul/history"))
        .expect("history dir")
        .collect();
    assert_eq!(history.len(), 1, "one snapshot after propose");
}

#[test]
fn learn_record_writes_learnings_not_memory() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path(), "");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learn", "record", "Laiq is a TypeScript/Python monorepo"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("[active; project;"), "record: {stdout}");
    assert!(!stdout.contains("memory"), "must not reroute: {stdout}");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learn", "record", "the deploy uses systemd"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("learning"), "uses-word: {stdout}");
    assert!(!project.path().join(".stateroot/memory.md").is_file());

    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "learn",
            "record",
            "--user",
            "prefer evidence over assertion",
        ])
        .assert()
        .success();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learnings", "list", "--status", "active"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("TypeScript/Python"),
        "project active: {stdout}"
    );
    assert!(
        stdout.contains("uses systemd"),
        "convention active: {stdout}"
    );
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learnings", "list", "--user", "--status", "active"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("evidence over assertion"),
        "user active: {stdout}"
    );
}

#[test]
fn learnings_lifecycle_and_distill_and_resume_surface() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path(), "");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // seed episodic with a recurring correction
    let episodic = project.path().join(".stateroot/memories");
    std::fs::create_dir_all(&episodic).expect("episodic dir");
    std::fs::write(
        episodic.join("episodic.jsonl"),
        concat!(
            r#"{"ts":"t1","harness":"cli","note":"actually the port is 9060"}"#,
            "\n",
            r#"{"ts":"t2","harness":"cli","note":"actually, the port is 9060!"}"#,
            "\n"
        ),
    )
    .expect("episodic");

    // distill → wiki inbox (does not activate learnings)
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learnings", "distill"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("inbox") || stdout.contains("distill:"),
        "distill: {stdout}"
    );

    let inbox = std::fs::read_to_string(project.path().join(".stateroot/wiki/pages/_inbox.md"))
        .expect("inbox");
    assert!(
        inbox.contains("the port is 9060"),
        "inbox should hold distilled notes: {inbox}"
    );

    // explicit learn record still activates and surfaces in resume
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learn", "record", "prefer the port is 9060 convention"])
        .assert()
        .success();

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learnings", "list", "--status", "active"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("the port is 9060"), "active: {stdout}");

    // active notes surface in resume
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--to",
            "codex",
            "--input",
            "handoff-input.json",
            "--objective",
            "resume check",
        ])
        .assert()
        .success();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("resume")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("the port is 9060"),
        "active learning must surface in resume: {stdout}"
    );
    assert!(
        stdout.contains("Wiki (catalog)") || stdout.contains("_inbox"),
        "resume must include wiki catalog: {stdout}"
    );

    // user-scope learning also surfaces
    let user_learnings = user_home.path().join(".stateroot/learnings");
    std::fs::create_dir_all(&user_learnings).expect("user learnings");
    std::fs::write(
        user_learnings.join("preferences.md"),
        "- **always run clippy with -D warnings** <!-- id: lrn_user1; confidence: 0.9; label: observed; sources: seed; scope: user; status: active -->\n",
    )
    .expect("user learning");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("always run clippy"),
        "user-scope durable must surface: {stdout}"
    );
}

#[tokio::test]
async fn synthesize_merges_sections_and_governance_skips() {
    let server = MockServer::start().await;
    let sections = json!({
        "progress_report": ["parser shipped"],
        "decisions_and_amendments": ["kept the deterministic floor"],
        "residual_work": ["write more tests"],
        "resolutions": []
    });
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": sections.to_string()}}]
        })))
        .expect(2)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(
        config_home.path(),
        "\n[synthesis]\nenabled = true\nmin_interval_seconds = 0\n",
    );
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // a codex rollout so the bundle is non-empty + a handoff to merge into
    let sessions_dir = user_home.path().join(".codex/sessions/2026/08/07");
    std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
    std::fs::write(
        sessions_dir.join("rollout-1.jsonl"),
        format!(
            "{}\n{}\n",
            json!({"type":"session_meta","payload":{"id":"s1","cwd":project.path().display().to_string(),"timestamp":"2026-08-07T10:00:00Z"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"ship the parser"}]}})
        ),
    )
    .expect("rollout");
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--to",
            "codex",
            "--input",
            "handoff-input.json",
            "--objective",
            "ship the parser",
        ])
        .assert()
        .success();

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("DEEPSEEK_API_KEY", "test-key")
        .env("STATEROOT_SYNTHESIS_API_BASE", server.uri())
        .arg("synthesize")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("synthesis merged"), "synthesize: {stdout}");

    let handoff = std::fs::read_to_string(project.path().join(".stateroot/handoffs/current.json"))
        .expect("handoff");
    assert!(handoff.contains("parser shipped"), "handoff: {handoff}");
    assert!(
        handoff.contains("deepseek-v4-flash"),
        "provenance: {handoff}"
    );

    // hash-idempotent: second run skips, provider hit exactly once
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("DEEPSEEK_API_KEY", "test-key")
        .env("STATEROOT_SYNTHESIS_API_BASE", server.uri())
        .arg("synthesize")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("bundle unchanged"), "idempotent: {stdout}");

    // --force re-runs against the same mock (provider hit twice total)
    stateroot(config_home.path(), user_home.path(), project.path())
        .env("DEEPSEEK_API_KEY", "test-key")
        .env("STATEROOT_SYNTHESIS_API_BASE", server.uri())
        .args(["synthesize", "--force"])
        .assert()
        .success();
    server.verify().await;
}

#[tokio::test]
async fn synthesize_without_key_is_honest_unavailability() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(
        config_home.path(),
        "\n[synthesis]\napi_key = \"config-only-must-not-enable\"\n",
    );
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    // need a bundle: reuse a minimal rollout
    let sessions_dir = user_home.path().join(".codex/sessions/2026/08/07");
    std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
    std::fs::write(
        sessions_dir.join("rollout-1.jsonl"),
        format!(
            "{}\n",
            json!({"type":"session_meta","payload":{"id":"s1","cwd":project.path().display().to_string(),"timestamp":"2026-08-07T10:00:00Z"}})
        ),
    )
    .expect("rollout");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("synthesize")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("synthesis unavailable"), "no-key: {stdout}");
}

#[tokio::test]
async fn synthesize_uses_observed_pack_when_no_transcripts() {
    let server = MockServer::start().await;
    let sections = json!({
        "progress_report": ["README describes a FastAPI multi-agent platform"],
        "decisions_and_amendments": [],
        "residual_work": ["upgrade the live codebase"],
        "resolutions": []
    });
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": sections.to_string()}}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path(), "");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    std::fs::write(
        project.path().join("README.md"),
        "# SiderAgents\n\nFastAPI multi-agent platform.\n",
    )
    .expect("readme");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("OPENAI_API_KEY", "test-key")
        .env("STATEROOT_SYNTHESIS_API_BASE", server.uri())
        .arg("synthesize")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("synthesis merged"), "pack synth: {stdout}");
    let handoff = std::fs::read_to_string(project.path().join(".stateroot/handoffs/current.json"))
        .expect("handoff");
    assert!(
        handoff.contains("FastAPI multi-agent platform"),
        "handoff: {handoff}"
    );
    assert!(handoff.contains("gpt-5.6-luna"), "model: {handoff}");
    server.verify().await;
}
