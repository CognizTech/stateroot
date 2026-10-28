//! M3 integration tests — soul, learnings, proposals, review loop, memory
//! scoping, and synthesis (wiremock for the provider call; zero real network).

// Each test binary includes this module but uses a subset of its helpers.
#[path = "support/mod.rs"]
mod support;

use std::path::Path;

use serde_json::json;
use support::stateroot;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn seed_config_home(home: &Path, synthesis_toml: &str) {
    std::fs::create_dir_all(home).expect("config home");
    std::fs::write(
        home.join("config.toml"),
        format!("user_id = \"default\"\nagent_id = \"default\"\n{synthesis_toml}"),
    )
    .expect("config.toml");
}

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    support::init_project(config_home, user_home, project);
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
fn full_learning_lookup_and_json_roundtrip_survive_index_unavailability() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    let statement = format!(
        "Prefer complete saved evidence. {} End of rule.",
        "Long rule detail. ".repeat(30)
    );
    for (scope, flags) in [
        ("project", vec![]),
        ("user", vec!["--user"]),
        ("workspace", vec!["--workspace"]),
        ("domain:testing", vec!["--domain", "testing"]),
    ] {
        let learning = stateroot_core::learnings::record_note(
            project.path(),
            home.path(),
            &statement,
            scope,
            "full-rule-test",
        )
        .unwrap();
        let output = stateroot(config.path(), home.path(), project.path())
            .args(["learnings", "show", &learning.0, "--json"])
            .args(&flags)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let shown: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(shown["statement"], statement);
        assert_eq!(shown["sources"], "full-rule-test");
        assert_eq!(shown["status"], "active");
        let output = stateroot(config.path(), home.path(), project.path())
            .args(["learnings", "list", "--json"])
            .args(&flags)
            .output()
            .unwrap();
        let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(listed
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == learning.0 && row["statement"] == statement));
    }
}

#[test]
fn concurrent_process_recall_uses_a_committed_generation_or_labeled_source_fallback() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    stateroot_core::learnings::record_note(
        project.path(),
        home.path(),
        "Prefer heliotropic continuity evidence",
        "project",
        "parallel-source",
    )
    .unwrap();
    std::thread::scope(|scope| {
        let run = || {
            let output = stateroot(config.path(), home.path(), project.path())
                .args(["memory", "recall", "heliotropic"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("heliotropic"));
        };
        let first = scope.spawn(run);
        let second = scope.spawn(run);
        first.join().unwrap();
        second.join().unwrap();
    });
}

#[test]
fn recall_process_reads_committed_rollback_generation_during_uncommitted_rebuild() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    let dir = home.path().join(".codex/sessions/test");
    std::fs::create_dir_all(&dir).unwrap();
    let events = [
        json!({"type":"session_meta","payload":{"id":"committed","cwd":project.path(),"timestamp":"2026-10-08T00:00:00Z"}}),
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"committedcanary unchanged native evidence"}]}}),
    ];
    std::fs::write(
        dir.join("rollout-committed.jsonl"),
        events
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    stateroot_core::memory_index::rebuild(project.path(), home.path()).unwrap();
    stateroot_core::learnings::record_note(
        project.path(),
        home.path(),
        "Prefer current authored evidence",
        "project",
        "reader-test",
    )
    .unwrap();
    let _writer = stateroot_core::safe_io::ResourceLock::acquire(
        project.path().join(".stateroot/local/memory-index.lock"),
    )
    .unwrap();
    let conn = rusqlite::Connection::open(
        project
            .path()
            .join(".stateroot")
            .join(stateroot_core::memory_index::INDEX_DB_REL),
    )
    .unwrap();
    assert_eq!(
        conn.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    conn.execute_batch("PRAGMA cache_spill=OFF; BEGIN IMMEDIATE; DELETE FROM docs; INSERT INTO docs_fts(docs_fts) VALUES('delete-all');").unwrap();
    let output = stateroot(config.path(), home.path(), project.path())
        .args(["memory", "recall", "committedcanary"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("committedcanary"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("stale"));
    conn.execute_batch("ROLLBACK").unwrap();
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

// ---------------------------------------------------------------------------
// Provider transport robustness (plan_2026-10-20T07-10-54Z): Unicode error
// boundaries, bounded bodies, redirect origin rules. All fixtures are local
// wiremock servers — no live provider traffic.
// ---------------------------------------------------------------------------

/// Error body whose byte 300 lands mid-codepoint ("界" starts at byte 299).
/// The old `&text[..300]` byte slice panicked on exactly this shape.
fn straddling_unicode_error_body() -> Vec<u8> {
    let mut body = b"error-code-teapot ".to_vec();
    body.resize(299, b'x');
    body.extend_from_slice("界".as_bytes());
    body.extend_from_slice(b" detail after the wide char");
    body
}

/// One-line summary of what a redirect target server actually received —
/// the evidence channel for cross-origin leak checks.
async fn observed_requests_summary(server: &MockServer) -> String {
    let requests = server.received_requests().await.unwrap_or_default();
    if requests.is_empty() {
        return "<no requests>".to_string();
    }
    requests
        .iter()
        .map(|r| {
            let auth = r
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("<none>");
            format!(
                "{} {} auth={auth} body_bytes={} body_has_marker={}",
                r.method,
                r.url.path(),
                r.body.len(),
                String::from_utf8_lossy(&r.body).contains("ZebraQuasar")
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn valid_synthesis_response(marker: &str) -> ResponseTemplate {
    let sections = json!({
        "progress_report": [marker],
        "decisions_and_amendments": [],
        "residual_work": [],
        "resolutions": []
    });
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{"message": {"content": sections.to_string()}}]
    }))
}

/// Project holding only a README marker — the observed context pack carries
/// the marker into the provider request body, making body forwarding visible.
fn marker_project(config_home: &Path, user_home: &Path) -> tempfile::TempDir {
    seed_config_home(config_home, "");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home, user_home, project.path());
    std::fs::write(
        project.path().join("README.md"),
        "# Marker\n\nZebraQuasar synthesis marker.\n",
    )
    .expect("readme");
    project
}

#[tokio::test]
async fn provider_transport_unicode_error_body_falls_back_without_panic() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_bytes(straddling_unicode_error_body()))
        .expect(1)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = marker_project(config_home.path(), user_home.path());

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("DEEPSEEK_API_KEY", "test-key")
        .env("STATEROOT_SYNTHESIS_API_BASE", server.uri())
        .arg("synthesize")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("synthesis skipped or failed — deterministic digest intact"),
        "unicode error must degrade to deterministic, not crash: {stdout}"
    );
    server.verify().await;
}

#[tokio::test]
async fn provider_transport_unicode_error_note_is_clean_and_keeps_content() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_bytes(straddling_unicode_error_body()))
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path(), "");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    std::fs::write(
        project.path().join("README.md"),
        "# Marker\n\nZebraQuasar synthesis marker.\n",
    )
    .expect("readme");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("DEEPSEEK_API_KEY", "test-key")
        .env("STATEROOT_SYNTHESIS_API_BASE", server.uri())
        .args(["init", "--synthesize", "--synthesize-with", "deepseek"])
        .assert()
        .success();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(
        stderr.contains("synthesis via deepseek failed"),
        "init must surface the provider failure, not panic: {stderr}"
    );
    assert!(
        stderr.contains("error-code-teapot"),
        "error note must keep useful provider content: {stderr}"
    );
    assert!(
        !stderr.contains("test-key"),
        "error note must never leak the bearer token: {stderr}"
    );
}

/// Cross-origin redirects never forward the prompt body or token to the
/// redirect target — one journey per redirect status (302/307/308 cover the
/// redirect family; the case count is unchanged, the journey is shared).
#[tokio::test]
async fn provider_transport_cross_origin_redirects_never_reach_target() {
    for status in [302u16, 307, 308] {
        let target = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(valid_synthesis_response("redirected"))
            .mount(&target)
            .await;

        let entry = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("location", format!("{}/chat/completions", target.uri())),
            )
            .mount(&entry)
            .await;

        let config_home = tempfile::tempdir().expect("config home");
        let user_home = tempfile::tempdir().expect("user home");
        let project = marker_project(config_home.path(), user_home.path());

        let out = stateroot(config_home.path(), user_home.path(), project.path())
            .env("DEEPSEEK_API_KEY", "test-key")
            .env("STATEROOT_SYNTHESIS_API_BASE", entry.uri())
            .arg("synthesize")
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
        assert!(
            stdout.contains("synthesis skipped or failed — deterministic digest intact"),
            "{status}: cross-origin redirect must be refused, not followed: {stdout}"
        );
        let observed = observed_requests_summary(&target).await;
        assert_eq!(
            observed, "<no requests>",
            "{status}: redirect target must never receive the prompt body or token"
        );
    }
}

#[tokio::test]
async fn provider_transport_same_origin_307_redirect_still_merges() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}/v1/chat/completions", server.uri())),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(valid_synthesis_response("same-origin gateway"))
        .expect(1)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = marker_project(config_home.path(), user_home.path());

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("DEEPSEEK_API_KEY", "test-key")
        .env("STATEROOT_SYNTHESIS_API_BASE", server.uri())
        .arg("synthesize")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("synthesis merged"),
        "same-origin gateway redirect must keep working: {stdout}"
    );
    server.verify().await;
    let requests = server.received_requests().await.unwrap_or_default();
    let gateway = requests
        .iter()
        .find(|r| r.url.path() == "/v1/chat/completions")
        .expect("gateway request recorded");
    let auth = gateway
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(
        auth, "Bearer test-key",
        "same-origin redirect must preserve the bearer token"
    );
    assert!(
        String::from_utf8_lossy(&gateway.body).contains("ZebraQuasar"),
        "same-origin 307 must carry the request body through"
    );
}

#[tokio::test]
async fn provider_transport_redirect_loop_is_capped() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}/chat/completions", server.uri())),
        )
        .expect(5)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = marker_project(config_home.path(), user_home.path());

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("DEEPSEEK_API_KEY", "test-key")
        .env("STATEROOT_SYNTHESIS_API_BASE", server.uri())
        .arg("synthesize")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("synthesis skipped or failed — deterministic digest intact"),
        "redirect loop must be refused after the cap, not spun: {stdout}"
    );
    server.verify().await;
}

#[tokio::test]
async fn provider_transport_oversize_success_body_is_bounded() {
    let server = MockServer::start().await;
    // Valid chat-completions JSON trailed by 2 MiB of legal JSON whitespace:
    // an unbounded reader parses it and merges; a bounded reader must refuse.
    let sections = json!({
        "progress_report": ["oversize"],
        "decisions_and_amendments": [],
        "residual_work": [],
        "resolutions": []
    });
    let mut body = serde_json::to_vec(&json!({
        "choices": [{"message": {"content": sections.to_string()}}]
    }))
    .expect("json");
    body.resize(2 * 1024 * 1024, b' ');
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .expect(1)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = marker_project(config_home.path(), user_home.path());

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("DEEPSEEK_API_KEY", "test-key")
        .env("STATEROOT_SYNTHESIS_API_BASE", server.uri())
        .arg("synthesize")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("synthesis skipped or failed — deterministic digest intact"),
        "oversize body must be reported and dropped, not read whole: {stdout}"
    );
    server.verify().await;
}

/// I1 end-to-end: a real hook capture is discoverable through `memory
/// recall` immediately — raw-labeled, with a locator
/// that resolves. No index build stands between the capture and recall.
#[test]
fn fresh_capture_is_discoverable_via_recall_raw_labeled() {
    use std::io::Write;
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());

    stateroot(config.path(), home.path(), project.path())
        .args(["hook", "PostToolUse", "--harness", "codex"])
        .write_stdin(
            json!({
                "session_id": "s-fresh-e2e",
                "tool_name": "Bash",
                "tool_response": "ZebraQuasar end-to-end evidence",
                "tool_use_id": "tu-e2e"
            })
            .to_string(),
        )
        .assert()
        .success();

    let segment = std::fs::read_dir(project.path().join(".stateroot/spool/segments"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&segment)
        .unwrap()
        .write_all(b"{broken}\n\xff\n{\"torn\"")
        .unwrap();
    let damaged_bytes = std::fs::read(&segment).unwrap();
    let out = stateroot(config.path(), home.path(), project.path())
        .args(["memory", "recall", "ZebraQuasar"])
        .assert()
        .success();
    let coverage = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        coverage.contains("coverage partial") && coverage.contains("invalid UTF-8"),
        "{coverage}"
    );
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("observation")
            && stdout.contains("raw observation")
            && !stdout.contains("pending index"),
        "the capture appears as a raw observation: {stdout}"
    );
    assert!(
        stdout.contains("ZebraQuasar end-to-end evidence"),
        "the evidence text itself: {stdout}"
    );
    let cap = stdout
        .split_whitespace()
        .find(|token| token.starts_with("cap_"))
        .map(|token| token.trim_end_matches(['|', ']']))
        .expect("a resolvable capture locator in the output");
    let shown = stateroot(config.path(), home.path(), project.path())
        .args(["observations", "show", cap])
        .output()
        .unwrap();
    assert!(shown.status.success());
    let shown_text = String::from_utf8(shown.stdout).unwrap();
    assert!(shown_text.contains("ZebraQuasar end-to-end evidence"));
    assert!(shown_text.contains("s-fresh-e2e"));
    let db = project.path().join(".stateroot/local/memory.sqlite");
    std::fs::remove_file(&db).unwrap();
    std::fs::create_dir(&db).unwrap();
    let outage = stateroot(config.path(), home.path(), project.path())
        .args(["memory", "recall", "ZebraQuasar", "--limit", "1"])
        .output()
        .unwrap();
    assert!(
        outage.status.success(),
        "{}",
        String::from_utf8_lossy(&outage.stderr)
    );
    let outage_text = String::from_utf8(outage.stdout).unwrap();
    assert!(outage_text.contains(cap));
    assert_eq!(
        outage_text
            .lines()
            .filter(|line| line.starts_with('['))
            .count(),
        1
    );
    assert!(String::from_utf8_lossy(&outage.stderr).contains("Unavailable"));
    assert_eq!(std::fs::read(segment).unwrap(), damaged_bytes);
}

// I3: real CLI boundaries with isolated homes and an in-process provider.
struct IngestFixture {
    config: tempfile::TempDir,
    home: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl IngestFixture {
    fn new() -> Self {
        let fixture = Self {
            config: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
        };
        init_project(
            fixture.config.path(),
            fixture.home.path(),
            fixture.project.path(),
        );
        stateroot_core::local_store::append_episodic(
            fixture.project.path(),
            &json!({
                "note": "The deployment source remains the ordinary working file"
            }),
        )
        .unwrap();
        fixture
    }

    fn compile(&self, provider: Option<&str>, args: &[&str], interrupt: Option<&str>) -> String {
        let mut command = stateroot(self.config.path(), self.home.path(), self.project.path());
        command.args(["wiki", "compile"]).args(args).env("PATH", "");
        for name in [
            "OPENCLAW_BUNDLED_SKILLS_DIR",
            "OPENCLAW_STATE_DIR",
            "OPENCLAW_CONFIG_PATH",
            "NVM_SYMLINK",
            "NVM_HOME",
            "APPDATA",
            "LOCALAPPDATA",
        ] {
            command.env_remove(name);
        }
        if let Some(provider) = provider {
            command
                .env("DEEPSEEK_API_KEY", "fixture-only")
                .env("STATEROOT_SYNTHESIS_API_BASE", provider);
        }
        if let Some(count) = interrupt {
            command.env("STATEROOT_TEST_INGEST_INTERRUPT_AFTER", count);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn governance(&self) -> serde_json::Value {
        serde_json::from_slice(
            &std::fs::read(self.project.path().join(".stateroot/ingest-gov.json")).unwrap(),
        )
        .unwrap()
    }
}

fn ingest_response(page: &str, facts: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"choices": [{"message": {"content": json!({
        "pages": [{"slug": "deployment", "body": page, "summary": "deployment", "kind": "concept"}],
        "memory_facts": facts
    }).to_string()}}]}))
}

#[tokio::test]
async fn ingest_reuses_floor_retries_optional_and_tracks_actual_dependencies() {
    let fixture = IngestFixture::new();
    let cold = std::time::Instant::now();
    assert!(fixture.compile(None, &[], None).contains("new inbox"));
    let cold_ms = cold.elapsed().as_millis();
    let repeat = std::time::Instant::now();
    assert!(fixture.compile(None, &[], None).contains("unchanged"));
    let repeat_ms = repeat.elapsed().as_millis();
    assert_eq!(fixture.governance()["floor_runs"], 1);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    assert!(fixture
        .compile(Some(&server.uri()), &[], None)
        .contains("optional ingest failed"));
    let failed = fixture.governance();
    assert_eq!(failed["floor_runs"], 1);
    assert_eq!(failed["optional_success_sha256"], "");
    server.reset().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ingest_response("Deployment is observed", json!([])))
        .mount(&server)
        .await;
    assert!(fixture
        .compile(Some(&server.uri()), &[], None)
        .contains("agentic ingest"));
    let baseline = fixture.governance();
    assert_eq!(baseline["floor_runs"], 1);
    assert_eq!(baseline["optional_attempts"], 2);
    assert_eq!(baseline["optional_status"], "applied");
    let requests = server.received_requests().await.unwrap().len();
    assert!(fixture
        .compile(Some(&server.uri()), &[], None)
        .contains("optional ingest unchanged"));
    assert_eq!(server.received_requests().await.unwrap().len(), requests);

    std::fs::write(fixture.project.path().join("source.txt"), "version one").unwrap();
    std::fs::write(
        fixture.project.path().join("instructions.md"),
        "instruction one",
    )
    .unwrap();
    std::fs::write(fixture.project.path().join("tool.json"), "{\"tool\":1}").unwrap();
    let refs = r#"[{"kind":"source","path":"source.txt"},{"kind":"instruction","path":"instructions.md"},{"kind":"tool_config","path":"tool.json"}]"#;
    fixture.compile(Some(&server.uri()), &["--dependencies", refs], None);
    let declared_floor = fixture.governance()["floor_runs"].clone();
    let count = server.received_requests().await.unwrap().len();
    std::fs::write(
        fixture.project.path().join("unrelated.txt"),
        "ordinary unrelated edit",
    )
    .unwrap();
    fixture.compile(Some(&server.uri()), &[], None);
    assert_eq!(server.received_requests().await.unwrap().len(), count);
    for name in ["source.txt", "instructions.md", "tool.json"] {
        std::fs::write(fixture.project.path().join(name), format!("changed {name}")).unwrap();
        fixture.compile(Some(&server.uri()), &[], None);
    }
    assert_eq!(server.received_requests().await.unwrap().len(), count + 3);
    assert_eq!(fixture.governance()["floor_runs"], declared_floor);
    let request: serde_json::Value = serde_json::from_slice(
        &server
            .received_requests()
            .await
            .unwrap()
            .last()
            .unwrap()
            .body,
    )
    .unwrap();
    assert!(request["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("changed tool.json"));
    seed_config_home(
        fixture.config.path(),
        "\n[synthesis.extra_body]\ntemperature = 0.3\n",
    );
    fixture.compile(Some(&server.uri()), &[], None);
    assert_eq!(server.received_requests().await.unwrap().len(), count + 4);
    assert_eq!(fixture.governance()["floor_runs"], declared_floor);
    eprintln!("I3 deterministic CLI cold={cold_ms}ms repeat={repeat_ms}ms; provider calls after failure/retry/dependency changes={}", fixture.governance()["optional_attempts"]);
}

#[tokio::test]
async fn ingest_selected_skill_resource_refresh_does_not_follow_unselected_packages() {
    let fixture = IngestFixture::new();
    let skill = fixture.project.path().join(".claude/skills/deploy");
    std::fs::create_dir_all(skill.join("references")).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: deploy\ndescription: Deployment procedure\n---\nFollow the deploy procedure.\n",
    )
    .unwrap();
    std::fs::write(skill.join("references/config.txt"), "resource one").unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ingest_response("Skill observed", json!([])))
        .mount(&server)
        .await;
    let refs = r#"[{"kind":"skill","selector":"deploy","file":"references/config.txt"}]"#;
    fixture.compile(Some(&server.uri()), &["--dependencies", refs], None);
    fixture.compile(Some(&server.uri()), &[], None);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    std::fs::write(skill.join("references/config.txt"), "resource two").unwrap();
    fixture.compile(Some(&server.uri()), &[], None);
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: deploy\ndescription: New deployment procedure\n---\nNew body.\n",
    )
    .unwrap();
    fixture.compile(Some(&server.uri()), &[], None);
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    let other = fixture.project.path().join(".claude/skills/unrelated");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        other.join("SKILL.md"),
        "---\nname: unrelated\ndescription: unrelated\n---\nDo another thing.\n",
    )
    .unwrap();
    fixture.compile(Some(&server.uri()), &[], None);
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn ingest_conflicts_preserve_authored_output_and_interrupted_apply_is_idempotent() {
    let fixture = IngestFixture::new();
    fixture.compile(None, &[], None);
    let server = MockServer::start().await;
    let project = fixture.project.path().to_path_buf();
    Mock::given(method("POST"))
        .respond_with(move |_: &wiremock::Request| {
            stateroot_core::wiki::write_page(
                &project,
                "deployment",
                "Authored during generation",
                "owner",
                "concept",
                Some("human"),
            )
            .unwrap();
            ingest_response("Old generated answer", json!([]))
        })
        .mount(&server)
        .await;
    let outcome = fixture.compile(Some(&server.uri()), &[], None);
    assert!(
        outcome.contains("stale") || outcome.contains("conflict"),
        "{outcome}"
    );
    assert!(
        stateroot_core::wiki::show(fixture.project.path(), "deployment")
            .unwrap()
            .contains("Authored during generation")
    );
    assert_eq!(fixture.governance()["optional_success_sha256"], "");
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(ingest_response(
            "Fresh answer",
            json!([{"action":"add","content":"The active deployment region is east"}]),
        ))
        .mount(&server)
        .await;
    let outcome = fixture.compile(Some(&server.uri()), &["--force"], Some("2"));
    assert!(outcome.contains("apply pending"), "{outcome}");
    let interrupted = fixture.governance();
    let floor_runs = interrupted["floor_runs"].clone();
    assert!(interrupted["pending"].is_object());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert!(fixture
        .compile(Some(&server.uri()), &[], None)
        .contains("resumed"));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert_eq!(fixture.governance()["floor_runs"], floor_runs);
    let memory =
        std::fs::read_to_string(fixture.project.path().join(".stateroot/memories/MEMORY.md"))
            .unwrap();
    assert_eq!(
        memory
            .matches("The active deployment region is east")
            .count(),
        1
    );
    let log_before = std::fs::read(fixture.project.path().join(".stateroot/wiki/log.md")).unwrap();
    fixture.compile(Some(&server.uri()), &[], None);
    assert_eq!(
        std::fs::read(fixture.project.path().join(".stateroot/wiki/log.md")).unwrap(),
        log_before
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn ingest_failed_refresh_retains_success_and_rejects_dirty_generation() {
    let fixture = IngestFixture::new();
    std::fs::write(fixture.project.path().join("source.txt"), "original").unwrap();
    let refs = r#"[{"kind":"source","path":"source.txt"}]"#;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ingest_response("Initial answer", json!([])))
        .mount(&server)
        .await;
    fixture.compile(Some(&server.uri()), &["--dependencies", refs], None);
    let success = fixture.governance()["optional_success_sha256"].clone();
    let floor = fixture.governance()["floor_runs"].clone();
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    std::fs::write(fixture.project.path().join("source.txt"), "refresh needed").unwrap();
    assert!(fixture
        .compile(Some(&server.uri()), &[], None)
        .contains("failed"));
    assert_eq!(fixture.governance()["optional_success_sha256"], success);
    assert_eq!(fixture.governance()["floor_runs"], floor);
    server.reset().await;
    let file = fixture.project.path().join("source.txt");
    Mock::given(method("POST"))
        .respond_with(move |_: &wiremock::Request| {
            std::fs::write(&file, "changed while generation ran").unwrap();
            ingest_response("Stale answer", json!([]))
        })
        .mount(&server)
        .await;
    assert!(fixture
        .compile(Some(&server.uri()), &[], None)
        .contains("stale"));
    assert_eq!(fixture.governance()["optional_success_sha256"], success);
    assert!(
        stateroot_core::wiki::show(fixture.project.path(), "deployment")
            .unwrap()
            .contains("Initial answer")
    );
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(ingest_response("Current answer", json!([])))
        .mount(&server)
        .await;
    fixture.compile(Some(&server.uri()), &[], None);
    assert_ne!(fixture.governance()["optional_success_sha256"], success);
    assert_eq!(fixture.governance()["floor_runs"], floor);
}

#[tokio::test]
async fn ingest_compaction_materializes_existing_outputs_and_retries_without_duplicate_history() {
    let fixture = IngestFixture::new();
    let expected = tempfile::tempdir().unwrap();
    stateroot_core::local_store::init_skeleton(expected.path(), "p", "P", "default").unwrap();
    let entries: Vec<_> = (0..12)
        .map(|i| format!("Retained fact {i}: {}", "a".repeat(600)))
        .collect();
    let memory = format!(
        "# Project Memory\n\n{}\n",
        stateroot_core::hot_apex::join_entries(&entries)
    );
    std::fs::write(
        fixture.project.path().join(".stateroot/memories/MEMORY.md"),
        &memory,
    )
    .unwrap();
    std::fs::write(
        expected.path().join(".stateroot/memories/MEMORY.md"),
        &memory,
    )
    .unwrap();
    fixture.compile(None, &[], None);
    let added = format!("New observed fact: {}", "z".repeat(600));
    assert!(
        stateroot_core::hot_apex::add(
            expected.path(),
            fixture.home.path(),
            "memory",
            &added,
            false
        )
        .unwrap()
        .success
    );
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ingest_response(
            "Compaction observed",
            json!([{"action":"add","content":added}]),
        ))
        .mount(&server)
        .await;
    // Interrupt after archive/page preservation, before the remaining writes.
    assert!(fixture
        .compile(Some(&server.uri()), &[], Some("2"))
        .contains("pending"));
    let floor = fixture.governance()["floor_runs"].clone();
    assert!(fixture
        .compile(Some(&server.uri()), &[], None)
        .contains("resumed"));
    assert_eq!(fixture.governance()["floor_runs"], floor);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let actual_memory =
        std::fs::read_to_string(fixture.project.path().join(".stateroot/memories/MEMORY.md"))
            .unwrap();
    let expected_memory =
        std::fs::read_to_string(expected.path().join(".stateroot/memories/MEMORY.md")).unwrap();
    // Pointer timestamps are generated by each genuine mutator invocation;
    // surviving/demoted content and capacity semantics must agree exactly.
    let actual_entries = stateroot_core::hot_apex::split_entries(&actual_memory);
    let expected_entries = stateroot_core::hot_apex::split_entries(&expected_memory);
    assert_eq!(&actual_entries[1..], &expected_entries[1..]);
    let actual_archive =
        stateroot_core::wiki::show(fixture.project.path(), "memory/apex-archive").unwrap();
    let expected_archive =
        stateroot_core::wiki::show(expected.path(), "memory/apex-archive").unwrap();
    let bullets = |text: &str| {
        text.lines()
            .filter(|line| line.starts_with("- "))
            .map(|line| {
                if line.starts_with("- [demoted ") {
                    // Separate real mutator invocations legitimately have
                    // different timestamps; compare every other header byte.
                    line.split_once(" · ").unwrap().1.to_string()
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(bullets(&actual_archive), bullets(&expected_archive));
    let episodic = std::fs::read_to_string(
        fixture
            .project
            .path()
            .join(".stateroot/memories/episodic.jsonl"),
    )
    .unwrap();
    let expected_episodic =
        std::fs::read_to_string(expected.path().join(".stateroot/memories/episodic.jsonl"))
            .unwrap();
    assert_eq!(
        episodic
            .lines()
            .filter(|line| line.contains("apex_compact"))
            .count(),
        expected_episodic
            .lines()
            .filter(|line| line.contains("apex_compact"))
            .count()
    );
    assert!(!actual_archive.contains("/tmp/") && !actual_memory.contains("/tmp/"));
    let before = fixture.governance();
    fixture.compile(Some(&server.uri()), &[], None);
    assert_eq!(
        fixture.governance()["optional_attempts"],
        before["optional_attempts"]
    );
    assert_eq!(fixture.governance()["floor_runs"], before["floor_runs"]);
}

#[tokio::test]
async fn synthesis_effective_config_invalidates_and_concurrent_handoff_writes_survive() {
    let fixture = IngestFixture::new();
    std::fs::write(
        fixture.project.path().join("README.md"),
        "# Product\nObserved product source.\n",
    )
    .unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(valid_synthesis_response("fresh synthesis"))
        .mount(&server)
        .await;
    let run = || {
        let output = stateroot(
            fixture.config.path(),
            fixture.home.path(),
            fixture.project.path(),
        )
        .env("DEEPSEEK_API_KEY", "fixture-only")
        .env("STATEROOT_SYNTHESIS_API_BASE", server.uri())
        .arg("synthesize")
        .output()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    assert!(run().contains("merged"));
    run();
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    seed_config_home(
        fixture.config.path(),
        "\n[synthesis.extra_body]\ntemperature = 0.4\n",
    );
    assert!(run().contains("merged"));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    for initially_absent in [true, false] {
        server.reset().await;
        if initially_absent {
            std::fs::remove_file(
                fixture
                    .project
                    .path()
                    .join(".stateroot/handoffs/current.json"),
            )
            .unwrap();
        }
        std::fs::write(
            fixture.project.path().join("README.md"),
            format!("# Product\nChanged source {initially_absent}.\n"),
        )
        .unwrap();
        let project = fixture.project.path().to_path_buf();
        let config = fixture.config.path().to_path_buf();
        let home = fixture.home.path().to_path_buf();
        Mock::given(method("POST"))
            .respond_with(move |_: &wiremock::Request| {
                stateroot(&config, &home, &project)
                    .args([
                        "handoff",
                        "write",
                        "--from",
                        "codex",
                        "--objective",
                        "New authored objective",
                        "--task",
                        "Preserve current work",
                        "--context-summary",
                        "Written by another process while synthesis waited",
                    ])
                    .assert()
                    .success();
                valid_synthesis_response("stale synthesis")
            })
            .mount(&server)
            .await;
        assert!(run().contains("deterministic"));
        let handoff = stateroot_core::local_store::read_handoff_local(fixture.project.path())
            .unwrap()
            .unwrap();
        assert_eq!(handoff["objective"], "New authored objective");
        assert!(!handoff.to_string().contains("stale synthesis"));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn ingest_preparation_failure_retains_generation_and_bounds_materialization() {
    let fixture = IngestFixture::new();
    stateroot_core::wiki::write_page(
        fixture.project.path(),
        "unrelated",
        &"u".repeat(1_000_000),
        "unrelated",
        "concept",
        None,
    )
    .unwrap();
    let unrelated_before = std::fs::read(
        fixture
            .project
            .path()
            .join(".stateroot/wiki/pages/unrelated.md"),
    )
    .unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ingest_response(
            "Prepared page",
            json!([{"action":"replace","old":"missing fact","content":"replacement"}]),
        ))
        .mount(&server)
        .await;
    assert!(fixture
        .compile(Some(&server.uri()), &[], None)
        .contains("prepare pending"));
    assert!(fixture
        .compile(Some(&server.uri()), &[], None)
        .contains("prepare pending"));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert_eq!(fixture.governance()["floor_runs"], 1);
    assert!(fixture.governance()["generated"].is_object());
    assert!(!fixture
        .project
        .path()
        .join(".stateroot/wiki/pages/deployment.md")
        .exists());
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(ingest_response("Prepared page", json!([])))
        .mount(&server)
        .await;
    fixture.compile(Some(&server.uri()), &["--force"], None);
    let materialization = fixture.governance()["last_success"]["materialization"].clone();
    assert!(
        materialization["copied_bytes"].as_u64().unwrap() < 20_000,
        "{materialization}"
    );
    assert!(
        materialization["prepared_bytes"].as_u64().unwrap() < 20_000,
        "{materialization}"
    );
    assert_eq!(fixture.governance()["floor_runs"], 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert_eq!(
        std::fs::read(
            fixture
                .project
                .path()
                .join(".stateroot/wiki/pages/unrelated.md")
        )
        .unwrap(),
        unrelated_before
    );
    eprintln!("I3 bounded materialization with unrelated 1MB page: {materialization}; output identity scan still reads relevant output catalog bytes");
}

#[tokio::test]
async fn ingest_started_compaction_survives_new_boundary_inputs_and_preserves_conflicts() {
    for interrupt in ["1", "memory"] {
        for changed in ["checkpoint", "configuration", "dependency", "learning"] {
            let fixture = IngestFixture::new();
            let note = "Remember the earlier deployment route";
            let learning = stateroot_core::learnings::record_note(
                fixture.project.path(),
                fixture.home.path(),
                note,
                "project",
                "fixture",
            )
            .unwrap()
            .0;
            stateroot_core::local_store::append_episodic(
                fixture.project.path(),
                &json!({"note":note}),
            )
            .unwrap();
            std::fs::write(
                fixture.project.path().join("source.txt"),
                "original dependency",
            )
            .unwrap();
            fixture.compile(
                None,
                &[
                    "--dependencies",
                    r#"[{"kind":"source","path":"source.txt"}]"#,
                ],
                None,
            );
            assert!(
                !stateroot_core::wiki::show(fixture.project.path(), "_inbox")
                    .unwrap()
                    .contains(note)
            );
            let entries: Vec<_> = (0..12)
                .map(|i| format!("Recoverable fact {i}: {}", "x".repeat(600)))
                .collect();
            std::fs::write(
                fixture.project.path().join(".stateroot/memories/MEMORY.md"),
                format!(
                    "# Project Memory\n\n{}\n",
                    stateroot_core::hot_apex::join_entries(&entries)
                ),
            )
            .unwrap();
            let server = MockServer::start().await;
            Mock::given(method("POST")).respond_with(ingest_response("Recovered page", json!([{"action":"add","content":format!("New retained fact {}", "z".repeat(600))}]))).mount(&server).await;
            assert!(fixture
                .compile(Some(&server.uri()), &[], Some(interrupt))
                .contains("pending"));
            let pending = fixture.governance();
            assert_eq!(pending["pending"]["publication_started"], true);
            let floor_runs = pending["floor_runs"].as_u64().unwrap();
            // The archive must already exist even at the earliest interruption.
            let archive_path = fixture
                .project
                .path()
                .join(".stateroot/wiki/pages/memory/apex-archive.md");
            assert!(archive_path.exists(), "{interrupt}/{changed}");
            match changed {
                "checkpoint" => {
                    stateroot(
                        fixture.config.path(),
                        fixture.home.path(),
                        fixture.project.path(),
                    )
                    .args([
                        "checkpoint",
                        "--note",
                        "A newly authored boundary note must be mined",
                    ])
                    .assert()
                    .success();
                }
                "configuration" => {
                    seed_config_home(fixture.config.path(), "\n[synthesis]\nenabled = false\n")
                }
                "dependency" => std::fs::write(
                    fixture.project.path().join("source.txt"),
                    "changed dependency",
                )
                .unwrap(),
                "learning" => {
                    assert!(stateroot_core::learnings::edit(
                        fixture.project.path(),
                        fixture.home.path(),
                        "project",
                        &learning,
                        "Prefer the corrected deployment procedure"
                    )
                    .unwrap());
                }
                _ => unreachable!(),
            }
            // Recovery is deterministic, so disabling/removing credentials
            // cannot strand a previously started mutation.
            let result = fixture.compile(None, &[], None);
            assert!(
                result.contains("resumed"),
                "{interrupt}/{changed}: {result}"
            );
            let gov = fixture.governance();
            assert!(gov["pending"].is_null());
            assert_eq!(gov["optional_attempts"], 1);
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
            let memory = std::fs::read_to_string(
                fixture.project.path().join(".stateroot/memories/MEMORY.md"),
            )
            .unwrap();
            let archive = std::fs::read_to_string(&archive_path).unwrap();
            for entry in &entries {
                assert!(
                    memory.contains(entry) || archive.contains(entry),
                    "lost {interrupt}/{changed}"
                );
            }
            let inbox = stateroot_core::wiki::show(fixture.project.path(), "_inbox").unwrap();
            if changed == "learning" {
                assert!(
                    inbox.contains(note),
                    "changed learning was marked processed without mining"
                );
                assert_eq!(gov["floor_runs"].as_u64().unwrap(), floor_runs + 1);
            }
            if changed == "checkpoint" {
                assert!(inbox.contains("A newly authored boundary note must be mined"));
                assert_eq!(gov["floor_runs"].as_u64().unwrap(), floor_runs + 1);
            }
            let episodic_path = fixture
                .project
                .path()
                .join(".stateroot/memories/episodic.jsonl");
            let episodic = std::fs::read(&episodic_path).unwrap();
            fixture.compile(None, &[], None);
            assert_eq!(std::fs::read(&episodic_path).unwrap(), episodic);
            assert_eq!(std::fs::read_to_string(&archive_path).unwrap(), archive);
        }
    }

    let fixture = IngestFixture::new();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ingest_response(
            "Pending page",
            json!([{"action":"add","content":"Pending generated fact"}]),
        ))
        .mount(&server)
        .await;
    fixture.compile(Some(&server.uri()), &[], Some("1"));
    let memory_path = fixture.project.path().join(".stateroot/memories/MEMORY.md");
    std::fs::write(
        &memory_path,
        "# Project Memory\n\nNew authored memory after interruption\n",
    )
    .unwrap();
    let authored = std::fs::read(&memory_path).unwrap();
    assert!(fixture
        .compile(None, &["--force"], None)
        .contains("output conflict"));
    assert_eq!(std::fs::read(&memory_path).unwrap(), authored);
    assert_eq!(fixture.governance()["pending"]["publication_started"], true);
    let retained = fixture.governance()["pending"].clone();
    let floor_before = fixture.governance()["floor_runs"].as_u64().unwrap();
    for note in [
        "New note after optional publication conflicted",
        "Another later note must also be mined",
    ] {
        stateroot_core::local_store::append_episodic(
            fixture.project.path(),
            &json!({"note": note}),
        )
        .unwrap();
        assert!(fixture.compile(None, &[], None).contains("output conflict"));
        assert!(stateroot_core::wiki::show(fixture.project.path(), "_inbox")
            .unwrap()
            .contains(note));
        assert_eq!(std::fs::read(&memory_path).unwrap(), authored);
        assert_eq!(
            fixture.governance()["pending"],
            retained,
            "floor must not rebase or discard the older optional journal"
        );
    }
    assert_eq!(
        fixture.governance()["floor_runs"].as_u64().unwrap(),
        floor_before + 2
    );
    fixture.compile(None, &[], None);
    assert_eq!(
        fixture.governance()["floor_runs"].as_u64().unwrap(),
        floor_before + 2
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn ingest_two_interruptions_replay_without_floor_log_conflict() {
    for second in ["episodic", "log"] {
        for new_note in [false, true] {
            let fixture = IngestFixture::new();
            fixture.compile(None, &[], None);
            let entries: Vec<_> = (0..12)
                .map(|i| format!("Double retry fact {i}: {}", "x".repeat(600)))
                .collect();
            std::fs::write(
                fixture.project.path().join(".stateroot/memories/MEMORY.md"),
                format!(
                    "# Project Memory\n\n{}\n",
                    stateroot_core::hot_apex::join_entries(&entries)
                ),
            )
            .unwrap();
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ingest_response(
                    "Double retry page",
                    json!([{"action":"add","content":format!("Fresh fact {}", "y".repeat(600))}]),
                ))
                .mount(&server)
                .await;
            assert!(fixture
                .compile(Some(&server.uri()), &[], Some("1"))
                .contains("pending"));
            let floor_before = fixture.governance()["floor_runs"].as_u64().unwrap();
            if new_note {
                stateroot_core::local_store::append_episodic(
                    fixture.project.path(),
                    &json!({"note":"New evidence between two interruptions"}),
                )
                .unwrap();
            }
            let twice = fixture.compile(None, &[], Some(second));
            assert!(twice.contains("pending"), "{second}/{new_note}: {twice}");
            assert_eq!(fixture.governance()["pending"]["publication_started"], true);
            assert_eq!(
                fixture.governance()["floor_runs"].as_u64().unwrap(),
                floor_before + u64::from(new_note)
            );
            let recovered = fixture.compile(None, &[], None);
            assert!(
                recovered.contains("resumed") && !recovered.contains("conflict"),
                "{second}/{new_note}: {recovered}"
            );
            assert!(fixture.governance()["pending"].is_null());
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
            let log_path = fixture.project.path().join(".stateroot/wiki/log.md");
            let log = std::fs::read_to_string(&log_path).unwrap();
            assert_eq!(log.matches("agentic ingest:").count(), 1, "{log}");
            assert_eq!(log.matches("<!-- ingest:").count(), 1, "{log}");
            if new_note {
                assert!(stateroot_core::wiki::show(fixture.project.path(), "_inbox")
                    .unwrap()
                    .contains("New evidence between two interruptions"));
            }
            fixture.compile(None, &[], None);
            assert_eq!(std::fs::read_to_string(&log_path).unwrap(), log);
        }
    }
}
