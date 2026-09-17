//! Accelerator telemetry end-to-end: which product boundaries create which
//! events, and the durable spool → drain → acknowledge loop through the real
//! binary. Unit-level semantics live in `stateroot_core::telemetry`; this
//! file proves the CLI wiring (Phase 7 of the metrics plan).
//!
//! Dev builds emit nothing by contract, so these tests set the explicit
//! test-only `STATEROOT_TELEMETRY_FORCE=1` override; the drain worker is
//! invoked directly (`_drain-telemetry`) because detached spawns are
//! suppressed under `STATEROOT_TEST_CMD_PROBES`.

use std::path::Path;

use assert_cmd::Command;

fn stateroot(config_home: &Path, user_home: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::cargo_bin("stateroot").expect("binary");
    cmd.env("STATEROOT_HOME", config_home)
        .env("STATEROOT_TEST_HOME", user_home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env("STATEROOT_TELEMETRY_FORCE", "1")
        .env("STATEROOT_NO_AUTO_UPDATE", "1")
        .env_remove("STATEROOT_NO_PING")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_BASE")
        .current_dir(cwd);
    cmd
}

fn seed_persona(config_home: &Path, user_home: &Path) {
    std::fs::create_dir_all(config_home).expect("config home");
    std::fs::write(
        config_home.join("persona.md"),
        "## Working relationship\n\nYou are Marid, a precise systems engineer.\n",
    )
    .expect("persona");
    std::fs::create_dir_all(user_home.join(".stateroot/user")).expect("user dir");
    std::fs::write(
        user_home.join(".stateroot/user/USER.md"),
        "Human: Lin. Prefers short answers.\n",
    )
    .expect("USER.md");
}

fn spool_events(config_home: &Path) -> Vec<serde_json::Value> {
    let spool = config_home.join("local/telemetry/spool");
    let Ok(entries) = std::fs::read_dir(&spool) else {
        return Vec::new();
    };
    let mut events: Vec<serde_json::Value> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str(&t).ok())
        .collect();
    events.sort_by_key(|e| e["event"].as_str().unwrap_or("").to_string());
    events
}

fn event_kinds(config_home: &Path) -> Vec<String> {
    spool_events(config_home)
        .iter()
        .map(|e| e["event"].as_str().unwrap_or("").to_string())
        .collect()
}

#[test]
fn acquisition_boundaries_never_activate() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_persona(config_home.path(), user_home.path());
    std::fs::create_dir_all(project.path()).expect("project dir");

    // `--version`: the classic first command after a manual install.
    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("--version")
        .assert()
        .success();
    // `init` alone: never activation.
    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("init")
        .assert()
        .success();

    let kinds = event_kinds(config_home.path());
    assert_eq!(
        kinds,
        ["install_observed"],
        "install + init produce one acquisition event and zero activations: {kinds:?}"
    );
    let event = &spool_events(config_home.path())[0];
    assert_eq!(event["kind"].as_str(), Some("install"));
    assert_eq!(event["cohort"].as_str(), Some("measured_new"));
    assert!(event.get("project_id").is_none(), "no project on install");
}

#[test]
fn continuity_delivery_activates_once_and_dedups_same_day() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_persona(config_home.path(), user_home.path());
    std::fs::create_dir_all(project.path()).expect("project dir");
    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("init")
        .assert()
        .success();

    // First successful continuity delivery in a recognized harness.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "SessionStart", "--harness", "claude-code"])
        .write_stdin(r#"{"session_id":"s-t1"}"#)
        .assert()
        .success();
    let kinds = event_kinds(config_home.path());
    assert_eq!(
        kinds,
        ["active_day", "continuity_activated", "install_observed"],
        "first delivery = exactly one activation + one active day: {kinds:?}"
    );
    let activation = spool_events(config_home.path())
        .into_iter()
        .find(|e| e["event"] == "continuity_activated")
        .expect("activation");
    // Harness alias collapsed to the canonical registry id.
    assert_eq!(activation["harness"].as_str(), Some("claude"));
    let project_digest = activation["project_id"].as_str().expect("project");
    assert_eq!(project_digest.len(), 64, "opaque keyed digest");
    assert!(!project_digest.contains('/'), "never path material");

    // Repeated same-day hook events: nothing new.
    for session in ["s-t1", "s-t2"] {
        stateroot(config_home.path(), user_home.path(), project.path())
            .args(["hook", "SessionStart", "--harness", "claude"])
            .write_stdin(format!(r#"{{"session_id":"{session}"}}"#))
            .assert()
            .success();
    }
    let kinds = event_kinds(config_home.path());
    assert_eq!(
        kinds,
        ["active_day", "continuity_activated", "install_observed"],
        "same-day repeats add nothing: {kinds:?}"
    );

    // Same project, a second harness: one verified transition + its day row.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "SessionStart", "--harness", "codex"])
        .write_stdin(r#"{"session_id":"s-t3"}"#)
        .assert()
        .success();
    let events = spool_events(config_home.path());
    let transitions: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["event"] == "harness_transition")
        .collect();
    assert_eq!(transitions.len(), 1, "exactly one verified A→B transition");
    assert_eq!(transitions[0]["from_harness"].as_str(), Some("claude"));
    assert_eq!(transitions[0]["harness"].as_str(), Some("codex"));
    assert_eq!(
        transitions[0]["project_id"].as_str(),
        Some(project_digest),
        "transition on the same opaque project"
    );

    // A successful checkpoint is qualifying daily activity (cli actor: no
    // harness on the wire) — and it never activates again.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["checkpoint", "--note", "telemetry test"])
        .assert()
        .success();
    let events = spool_events(config_home.path());
    let days: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["event"] == "active_day")
        .collect();
    assert_eq!(days.len(), 3, "claude + codex + cli day rows: {days:?}");
    assert!(days.iter().any(|e| e.get("harness").is_none()));
    assert_eq!(
        events
            .iter()
            .filter(|e| e["event"] == "continuity_activated")
            .count(),
        1
    );
}

#[test]
fn opt_out_writes_nothing_through_any_boundary() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_persona(config_home.path(), user_home.path());
    std::fs::create_dir_all(project.path()).expect("project dir");

    let opted_out = |args: &[&str]| {
        stateroot(config_home.path(), user_home.path(), project.path())
            .env("STATEROOT_NO_PING", "1")
            .args(args)
            .assert()
            .success();
    };
    opted_out(&["--version"]);
    opted_out(&["init"]);
    opted_out(&["checkpoint", "--note", "quiet"]);
    let mut hook = stateroot(config_home.path(), user_home.path(), project.path());
    hook.env("STATEROOT_NO_PING", "1")
        .args(["hook", "SessionStart", "--harness", "claude"])
        .write_stdin(r#"{"session_id":"s-opt"}"#)
        .assert()
        .success();

    assert!(
        !config_home.path().join("local/telemetry").exists(),
        "opt-out: no telemetry state, spool, or logs"
    );
}

#[test]
fn drain_delivers_queued_events_and_clears_the_spool() {
    let config_home = tempfile::tempdir().expect("config");
    let user_home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    seed_persona(config_home.path(), user_home.path());
    std::fs::create_dir_all(project.path()).expect("project dir");
    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("init")
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["hook", "SessionStart", "--harness", "claude"])
        .write_stdin(r#"{"session_id":"s-d1"}"#)
        .assert()
        .success();
    let queued = spool_events(config_home.path());
    assert!(queued.len() >= 2, "events queued: {queued:?}");

    // Multi-request sink: 204 for every POST, recording request text.
    let (tx, rx) = std::sync::mpsc::channel();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        use std::io::{Read as _, Write as _};
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
        }
    });

    stateroot(config_home.path(), user_home.path(), project.path())
        .env("STATEROOT_TELEMETRY_URL", &addr)
        .arg("_drain-telemetry")
        .assert()
        .success();

    let mut seen = Vec::new();
    for _ in 0..queued.len() {
        seen.push(
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .expect("one request per queued event"),
        );
    }
    for event in &queued {
        let id = event["event_id"].as_str().unwrap();
        assert!(
            seen.iter()
                .any(|req| req.contains(&format!("x-sr-event-id: {id}"))),
            "event {id} delivered"
        );
    }
    assert!(
        spool_events(config_home.path()).is_empty(),
        "acked events leave the spool"
    );
    // Retries do not duplicate: the server already dedups by event id, and a
    // second drain over an empty spool sends nothing.
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(300))
            .is_err(),
        "no extra requests"
    );
}
