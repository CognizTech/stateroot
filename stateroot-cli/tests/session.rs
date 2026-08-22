//! `stateroot session` tests — canonical sync/list/show plus cross-harness
//! transfer, over hermetic homes and fixture session stores (pi via
//! `PI_CODING_AGENT_DIR`, dsh via `DSH_HOME`; zero real harness data).

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
        .env_remove("STATEROOT_DELEGATION_DEPTH")
        .current_dir(cwd);
    cmd
}

fn homes() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_home = tempfile::tempdir().expect("config home");
    std::fs::create_dir_all(config_home.path()).expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    (config_home, user_home)
}

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
}

fn write_lines(path: &Path, lines: &[String]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, format!("{}\n", lines.join("\n"))).expect("write");
}

/// A pi session fixture about `cwd`; returns the agent dir to point
/// `PI_CODING_AGENT_DIR` at.
fn pi_fixture(cwd: &str) -> tempfile::TempDir {
    let agent = tempfile::tempdir().expect("pi agent");
    // JSON-escape the cwd — Windows paths contain backslashes, which are
    // invalid raw JSON escapes and would silently void the fixture.
    let cwd_json = serde_json::to_string(cwd).expect("cwd json");
    let lines = vec![
        format!(r#"{{"type":"session","version":3,"id":"pi-sess-1","timestamp":"2026-08-20T10:00:00.000Z","cwd":{cwd_json}}}"#),
        r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-08-20T10:00:01.000Z","message":{"role":"user","content":"write the migration","timestamp":1784272801000}}"#.to_string(),
        r#"{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-08-20T10:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"migrating now"}],"timestamp":1784272802000}}"#.to_string(),
        r#"{"type":"model_change","id":"mc1","parentId":"m2","timestamp":"2026-08-20T10:00:03.000Z","provider":"deepseek","modelId":"deepseek-v4-flash"}"#.to_string(),
    ];
    write_lines(
        &agent
            .path()
            .join("sessions/--tmp-demo--/2026-08-20T10-00-00_pi-sess-1.jsonl"),
        &lines,
    );
    agent
}

/// A dsh session fixture about `cwd`; returns the home to point `DSH_HOME` at.
fn dsh_fixture(cwd: &str) -> tempfile::TempDir {
    let dsh_home = tempfile::tempdir().expect("dsh home");
    let cwd_json = serde_json::to_string(cwd).expect("cwd json");
    let lines = vec![
        format!(r#"{{"type":"session","version":0,"id":"dsh-sess-1","createdAt":1784272800000,"cwd":{cwd_json},"delegationDepth":0}}"#),
        r#"{"type":"turn/start","seq":0,"time":1784272801000,"data":{"turn":1}}"#.to_string(),
        r#"{"type":"user/message","seq":1,"time":1784272801001,"data":{"id":"u1","role":"user","content":[{"type":"text","text":"migrate the schema"}],"source":{"kind":"user"}},"surfaceOp":"append"}"#.to_string(),
        r#"{"type":"step/start","seq":2,"time":1784272801002,"data":{"turn":1,"step":1}}"#.to_string(),
        r#"{"type":"assistant/message","seq":3,"time":1784272801003,"data":{"turn":1,"step":1,"message":{"id":"a1","role":"assistant","content":[{"type":"text","text":"schema migrated"}],"source":{"kind":"model","provider":"deepseek","model":"deepseek-v4-flash"}}},"surfaceOp":"append"}"#.to_string(),
        r#"{"type":"step/end","seq":4,"time":1784272801004,"data":{"turn":1,"step":1}}"#.to_string(),
        r#"{"type":"turn/end","seq":5,"time":1784272801005,"data":{"turn":1,"reason":{"kind":"completed"}}}"#.to_string(),
    ];
    write_lines(
        &dsh_home
            .path()
            .join("sessions/--tmp-demo--/dsh-sess-1/session.jsonl"),
        &lines,
    );
    dsh_home
}

#[test]
fn session_sync_list_show_flow() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let cwd = project.path().display().to_string();
    let pi_agent = pi_fixture(&cwd);
    let dsh_home = dsh_fixture(&cwd);

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PI_CODING_AGENT_DIR", pi_agent.path())
        .env("DSH_HOME", dsh_home.path())
        .args(["session", "sync"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("session sync: 2 sessions canonicalized"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("dsh: 1") && stdout.contains("pi: 1"),
        "stdout: {stdout}"
    );

    let store = project.path().join(".stateroot/local/sessions");
    assert!(store.join("pi-pi-sess-1.jsonl").is_file(), "pi store file");
    assert!(
        store.join("dsh-dsh-sess-1.jsonl").is_file(),
        "dsh store file"
    );

    // Episodic lineage.
    let episodic =
        std::fs::read_to_string(project.path().join(".stateroot/memories/episodic.jsonl"))
            .expect("episodic");
    assert!(
        episodic.contains("session sync: 2 sessions canonicalized"),
        "{episodic}"
    );

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["session", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("pi-sess-1"), "list: {stdout}");
    assert!(stdout.contains("dsh-sess-1"), "list: {stdout}");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["session", "show", "pi-sess-1"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("session pi-sess-1 (pi)"), "show: {stdout}");
    assert!(stdout.contains("write the migration"), "show: {stdout}");
    assert!(stdout.contains("model_change"), "show: {stdout}");

    // Harness filter on sync.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PI_CODING_AGENT_DIR", pi_agent.path())
        .args(["session", "sync", "--harness", "pi"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("1 sessions canonicalized (pi: 1)"),
        "stdout: {stdout}"
    );

    // Unknown id errors honestly.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["session", "show", "nope"])
        .assert()
        .failure();
}

#[test]
fn session_transfer_to_pi_and_dsh_with_fidelity() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let cwd = project.path().display().to_string();
    let pi_agent = pi_fixture(&cwd);
    let dsh_home = dsh_fixture(&cwd);
    let target_pi = tempfile::tempdir().expect("target pi agent");
    let target_dsh = tempfile::tempdir().expect("target dsh home");

    // Sync both fixtures into the canonical store.
    stateroot(config_home.path(), user_home.path(), project.path())
        .env("PI_CODING_AGENT_DIR", pi_agent.path())
        .env("DSH_HOME", dsh_home.path())
        .args(["session", "sync"])
        .assert()
        .success();

    // Dry-run writes nothing but prints the plan + fidelity.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PI_CODING_AGENT_DIR", target_pi.path())
        .args([
            "session",
            "transfer",
            "pi-sess-1",
            "--to",
            "pi",
            "--dry-run",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("would transfer session pi-sess-1 → pi"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("native"), "fidelity: {stdout}");
    assert!(stdout.contains("would write:"), "stdout: {stdout}");
    assert!(
        !target_pi.path().join("sessions").exists(),
        "dry-run must not write"
    );

    // Real transfer to pi: a real session file lands in pi's store.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PI_CODING_AGENT_DIR", target_pi.path())
        .args(["session", "transfer", "pi-sess-1", "--to", "pi"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("transferred session pi-sess-1 → pi"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("wrote:"), "stdout: {stdout}");
    assert!(stdout.contains("resume with: pi (in "), "stdout: {stdout}");
    let written = std::fs::read_dir(target_pi.path().join("sessions"))
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|d| d.path().is_dir())
        .flat_map(|d| std::fs::read_dir(d.path()).ok().into_iter().flatten())
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .expect("a pi session file was written");
    let body = std::fs::read_to_string(&written).expect("written pi session");
    assert!(body.contains(r#""version":3"#), "pi v3 header: {body}");
    assert!(body.contains("write the migration"), "content: {body}");

    // Transfer a dsh canonical session to dsh: exact store layout.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("DSH_HOME", target_dsh.path())
        .args(["session", "transfer", "dsh-sess-1", "--to", "dsh"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("transferred session dsh-sess-1 → dsh"),
        "stdout: {stdout}"
    );
    let mut session_files = Vec::new();
    fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect(&path, out);
                } else if path.file_name().is_some_and(|n| n == "session.jsonl") {
                    out.push(path);
                }
            }
        }
    }
    collect(&target_dsh.path().join("sessions"), &mut session_files);
    assert_eq!(session_files.len(), 1, "one dsh session.jsonl written");
    let body = std::fs::read_to_string(&session_files[0]).expect("dsh session");
    assert!(body.contains(r#""version":0"#), "dsh v0 header: {body}");
    assert!(body.contains(r#""delegationDepth":0"#), "header: {body}");
    assert!(body.contains("migrate the schema"), "content: {body}");

    // Episodic lineage for both transfers.
    let episodic =
        std::fs::read_to_string(project.path().join(".stateroot/memories/episodic.jsonl"))
            .expect("episodic");
    assert!(
        episodic.contains("session transfer: pi-sess-1 → pi"),
        "{episodic}"
    );
    assert!(
        episodic.contains("session transfer: dsh-sess-1 → dsh"),
        "{episodic}"
    );

    // Unknown target and unknown session error honestly.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["session", "transfer", "pi-sess-1", "--to", "cursor"])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(
        stderr.contains("unknown transfer target 'cursor'"),
        "stderr: {stderr}"
    );
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["session", "transfer", "nope", "--to", "pi"])
        .assert()
        .failure();
}

/// Claude/codex/kimi/openclaw file fixtures plus cursor/hermes sqlite stores,
/// all about `cwd`; returns nothing — everything lives under `user_home`.
fn seed_remaining_harnesses(user_home: &Path, cwd: &str) {
    let cwd_json = serde_json::to_string(cwd).expect("cwd json");

    // claude: ~/.claude/projects/<slug>/<session>.jsonl
    write_lines(
        &user_home.join(".claude/projects/-tmp-demo/cl-sess-1.jsonl"),
        &[
            format!(r#"{{"type":"user","uuid":"u1","parentUuid":null,"message":{{"role":"user","content":"claude task"}},"timestamp":"2026-07-10T09:00:01Z","cwd":{cwd_json},"sessionId":"cl-sess-1"}}"#),
            r#"{"type":"assistant","uuid":"u2","parentUuid":"u1","message":{"role":"assistant","content":[{"type":"text","text":"claude answer"}]},"timestamp":"2026-07-10T09:00:02Z"}"#.to_string(),
        ],
    );

    // codex: ~/.codex/sessions/yyyy/mm/dd/rollout-*.jsonl
    write_lines(
        &user_home.join(".codex/sessions/2026/08/01/rollout-codex-1.jsonl"),
        &[
            format!(r#"{{"type":"session_meta","payload":{{"id":"codex-1","cwd":{cwd_json},"timestamp":"2026-08-01T10:00:00Z"}}}}"#),
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"codex task"}]},"timestamp":"2026-08-01T10:00:01Z"}"#.to_string(),
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"text":"codex answer"}]},"timestamp":"2026-08-01T10:00:02Z"}"#.to_string(),
        ],
    );

    // kimi: wire.jsonl + session_index.jsonl for the cwd binding.
    write_lines(
        &user_home.join(".kimi-code/sessions/wd_demo/kimi-1/agents/main/wire.jsonl"),
        &[
            r#"{"type":"metadata","protocol_version":"1.0","created_at":1784310494250}"#.to_string(),
            r#"{"type":"context.append_message","message":{"role":"user","content":[{"type":"text","text":"kimi task"}]},"time":1784310495000}"#.to_string(),
            r#"{"type":"context.append_message","message":{"role":"assistant","content":[{"type":"text","text":"kimi answer"}]},"time":1784310496000}"#.to_string(),
        ],
    );
    write_lines(
        &user_home.join(".kimi-code/session_index.jsonl"),
        &[format!(r#"{{"sessionId":"kimi-1","workDir":{cwd_json}}}"#)],
    );

    // openclaw: ~/.openclaw/agents/main/sessions/*.jsonl
    write_lines(
        &user_home.join(".openclaw/agents/main/sessions/oc-1.jsonl"),
        &[
            format!(r#"{{"type":"session","version":1,"id":"oc-1","timestamp":"2026-07-10T09:00:00Z","cwd":{cwd_json}}}"#),
            r#"{"type":"message","id":"m1","message":{"role":"user","content":[{"type":"text","text":"openclaw task"}]}}"#.to_string(),
            r#"{"type":"message","id":"m2","message":{"role":"assistant","content":[{"type":"text","text":"openclaw answer"}]}}"#.to_string(),
        ],
    );

    // cursor: state.vscdb (composerHeaders + cursorDiskKV bubbles).
    let cursor_dir = user_home.join(".config/Cursor/User/globalStorage");
    std::fs::create_dir_all(&cursor_dir).expect("cursor dir");
    let db = rusqlite::Connection::open(cursor_dir.join("state.vscdb")).expect("cursor db");
    db.execute_batch(
        "CREATE TABLE composerHeaders (composerId TEXT, value TEXT);
         CREATE TABLE cursorDiskKV (key TEXT, value TEXT);",
    )
    .expect("cursor schema");
    db.execute(
        "INSERT INTO composerHeaders (composerId, value) VALUES ('cur-1', ?1)",
        [serde_json::json!({"type":"head","composerId":"cur-1","createdAt":1781508855958i64,"workspaceIdentifier":{"uri":{"fsPath":cwd}}}).to_string()],
    )
    .expect("cursor head");
    for (id, bubble_type, text) in [("b-1", 1, "cursor task"), ("b-2", 2, "cursor answer")] {
        db.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            [
                format!("bubbleId:cur-1:{id}"),
                serde_json::json!({"_v":3,"type":bubble_type,"text":text,"createdAt":"2026-07-01T10:00:01Z"}).to_string(),
            ],
        )
        .expect("bubble");
    }
    drop(db);

    // hermes: ~/.hermes/state.db (sessions + messages).
    let hermes_dir = user_home.join(".hermes");
    std::fs::create_dir_all(&hermes_dir).expect("hermes dir");
    let db = rusqlite::Connection::open(hermes_dir.join("state.db")).expect("hermes db");
    db.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, cwd TEXT, git_repo_root TEXT, started_at REAL, ended_at REAL);
         CREATE TABLE messages (session_id TEXT, role TEXT, content TEXT, tool_calls TEXT, tool_name TEXT, timestamp REAL);",
    )
    .expect("hermes schema");
    db.execute(
        "INSERT INTO sessions (id, cwd, git_repo_root, started_at, ended_at) VALUES ('her-1', ?1, '', 1700000000, 1700000060)",
        [cwd],
    )
    .expect("hermes session");
    for (role, content, ts) in [
        ("user", "hermes task", 1700000001.0),
        ("assistant", "hermes answer", 1700000002.0),
    ] {
        db.execute(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_name, timestamp) VALUES ('her-1', ?1, ?2, '', '', ?3)",
            rusqlite::params![role, content, ts],
        )
        .expect("hermes message");
    }
    drop(db);
}

#[test]
fn session_sync_covers_every_harness() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let cwd = project.path().display().to_string();
    let pi_agent = pi_fixture(&cwd);
    let dsh_home = dsh_fixture(&cwd);
    seed_remaining_harnesses(user_home.path(), &cwd);

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PI_CODING_AGENT_DIR", pi_agent.path())
        .env("DSH_HOME", dsh_home.path())
        .args(["session", "sync"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("session sync: 8 sessions canonicalized"),
        "stdout: {stdout}"
    );
    for harness in [
        "claude", "codex", "cursor", "dsh", "hermes", "kimi", "openclaw", "pi",
    ] {
        assert!(
            stdout.contains(&format!("{harness}: 1")),
            "{harness} missing: {stdout}"
        );
    }

    // One canonical file per harness session.
    let store = project.path().join(".stateroot/local/sessions");
    for file in [
        "claude-cl-sess-1.jsonl",
        "codex-codex-1.jsonl",
        "cursor-cur-1.jsonl",
        "dsh-dsh-sess-1.jsonl",
        "hermes-her-1.jsonl",
        "kimi-kimi-1.jsonl",
        "openclaw-oc-1.jsonl",
        "pi-pi-sess-1.jsonl",
    ] {
        assert!(store.join(file).is_file(), "missing {file}");
    }

    // list + show handle every harness.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["session", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    for id in [
        "cl-sess-1",
        "codex-1",
        "cur-1",
        "dsh-sess-1",
        "her-1",
        "kimi-1",
        "oc-1",
        "pi-sess-1",
    ] {
        assert!(stdout.contains(id), "{id} missing from list: {stdout}");
    }
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["session", "show", "kimi-1"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("session kimi-1 (kimi)"), "show: {stdout}");
    assert!(stdout.contains("kimi task"), "show: {stdout}");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["session", "show", "cur-1"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("cursor task"), "show: {stdout}");

    // Episodic lineage counts every harness.
    let episodic =
        std::fs::read_to_string(project.path().join(".stateroot/memories/episodic.jsonl"))
            .expect("episodic");
    assert!(
        episodic.contains("session sync: 8 sessions canonicalized"),
        "{episodic}"
    );
}
