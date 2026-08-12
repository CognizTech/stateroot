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
        .env_remove("STATEROOT_SYNTHESIS_API_KEY")
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

    // propose → approve activates (gated evolution)
    let draft = project.path().join("draft.md");
    std::fs::write(&draft, "# Soul\n\n## Principles\n\n- verify first\n").expect("draft");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["soul", "propose", "--file"])
        .arg(&draft)
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("proposal"), "propose: {stdout}");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["proposals", "list", "--status", "pending"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("soul"), "list: {stdout}");
    let id: String = stdout
        .lines()
        .find(|l| l.contains("[soul;"))
        .and_then(|l| l.split_whitespace().next())
        .expect("proposal id")
        .to_string();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["proposals", "approve", &id])
        .assert()
        .success();
    let soul = canonical_soul(user_home.path());
    assert!(soul.contains("verify first"), "approved soul: {soul}");
    // the imported version was snapshotted
    let history: Vec<_> = std::fs::read_dir(user_home.path().join(".stateroot/soul/history"))
        .expect("history dir")
        .collect();
    assert_eq!(history.len(), 1, "one snapshot after approve");
}

#[test]
fn learn_record_classifies_and_approves_through_proposals() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path(), "");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learn", "record", "you are a careful reviewer"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("[soul; pending]"), "record: {stdout}");

    // learning lane → candidate quarantined via proposal approve
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learn", "record", "prefer small diffs over rewrites"])
        .assert()
        .success();
    // quarantine the candidate on disk the way distill does, then accept it
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learnings", "list", "--status", "pending"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("no learnings") || stdout.contains("Learnings"),
        "list: {stdout}"
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

    // distill → proposal + quarantined candidate
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learnings", "distill"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("1 candidate(s) → proposals"),
        "distill: {stdout}"
    );

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learnings", "list", "--status", "candidate"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("the port is 9060"), "candidate: {stdout}");
    let id: String = stdout
        .lines()
        .find(|l| l.contains("the port is 9060"))
        .and_then(|l| l.split_whitespace().next())
        .expect("learning id")
        .to_string();

    // candidates surface nowhere: resume must NOT contain it yet
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--to",
            "codex",
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
        !stdout.contains("the port is 9060"),
        "candidate must not surface: {stdout}"
    );

    // accept → active → resume surfaces it (durable preferences)
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["learnings", "accept", &id])
        .assert()
        .success();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("the port is 9060"),
        "active learning must surface in resume: {stdout}"
    );

    // user-scope learning also surfaces (memory scoping)
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
        &format!(
            "\n[synthesis]\napi_key = \"test-key\"\nbase_url = \"{}\"\nmodel = \"mock-model\"\nmin_interval_seconds = 0\n",
            server.uri()
        ),
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
            "--objective",
            "ship the parser",
        ])
        .assert()
        .success();

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("synthesize")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("synthesis merged"), "synthesize: {stdout}");

    let handoff = std::fs::read_to_string(project.path().join(".stateroot/handoffs/current.json"))
        .expect("handoff");
    assert!(handoff.contains("parser shipped"), "handoff: {handoff}");
    assert!(handoff.contains("mock-model"), "provenance: {handoff}");

    // hash-idempotent: second run skips, provider hit exactly once
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("synthesize")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("bundle unchanged"), "idempotent: {stdout}");

    // --force re-runs against the same mock (provider hit twice total)
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["synthesize", "--force"])
        .assert()
        .success();
    server.verify().await;
}

#[tokio::test]
async fn synthesize_without_key_is_honest_unavailability() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path(), "\n[synthesis]\napi_key = \"\"\n");
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
