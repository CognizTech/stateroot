//! WS1 failure matrix: durable hook evidence end-to-end through the real
//! binary. Isolated homes per test; zero provider calls (capture has no
//! network path); fixture processes are owned and killed, never real agents.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::{json, Value};
use tempfile::TempDir;

fn stateroot(config_home: &Path, user_home: &Path, cwd: &Path) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("stateroot").expect("binary");
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

fn homes() -> (TempDir, TempDir) {
    (
        tempfile::tempdir().expect("config home"),
        tempfile::tempdir().expect("user home"),
    )
}

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
}

fn hook(
    config_home: &Path,
    user_home: &Path,
    project: &Path,
    event: &str,
    payload: Value,
) -> assert_cmd::assert::Assert {
    stateroot(config_home, user_home, project)
        .args(["hook", event, "--harness", "codex"])
        .write_stdin(payload.to_string())
        .assert()
}

fn capture_payload(session: &str, tool_use_id: Option<&str>, body: &str) -> Value {
    let mut payload = json!({
        "session_id": session,
        "tool_name": "Bash",
        "tool_response": body,
    });
    if let Some(id) = tool_use_id {
        payload["tool_use_id"] = json!(id);
    }
    payload
}

fn segments_dir(project: &Path) -> PathBuf {
    project.join(".stateroot").join("spool").join("segments")
}

/// All durable event records (replay sightings excluded) across segments.
fn segment_records(project: &Path) -> Vec<Value> {
    let mut records = Vec::new();
    let dir = segments_dir(project);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return records;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "jsonl") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("segment reads");
        for line in text.lines() {
            // Torn/corrupt lines are evidence for health(), not records.
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if value["capture"]["status"].as_str() == Some("replay") {
                continue;
            }
            records.push(value);
        }
    }
    records
}

fn replay_sightings(project: &Path) -> usize {
    let mut count = 0;
    let dir = segments_dir(project);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "jsonl") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("segment reads");
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if value["capture"]["status"].as_str() == Some("replay") {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn long_utf8_and_structured_payloads_survive_uncapped() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // 1400+ chars of multibyte UTF-8 with a tail sentinel (the fixture that
    // lost its sentinel under the former 1000-char evidence cap).
    let body = format!("{}漢字🦀 tail-sentinel-ორიგინალი", "é".repeat(1400));
    hook(
        config_home.path(),
        user_home.path(),
        project.path(),
        "PostToolUse",
        capture_payload("s-long", Some("tu-long"), &body),
    )
    .success();

    let records = segment_records(project.path());
    assert_eq!(records.len(), 1);
    let record = &records[0];
    let text = record["text"].as_str().expect("text");
    assert!(
        text.contains("tail-sentinel-ორიგინალი"),
        "tail sentinel survived: {} bytes",
        text.len()
    );
    assert!(text.len() > 1400, "no 1000-char cap: {} bytes", text.len());
    assert_eq!(
        record["capture"]["source_status"].as_str(),
        Some("complete")
    );
    // Raw authorized source retained, uncapped, with digest.
    let source = &record["source"];
    assert_eq!(
        source["payload"]["tool_response"].as_str(),
        Some(body.as_str())
    );
    assert!(source["bytes"].as_u64().expect("bytes") > 1400);
    assert!(!source["digest"].as_str().unwrap_or("").is_empty());
    assert_eq!(record["schema"].as_str(), Some("stateroot.observation.v2"));
    assert!(record["capture_id"]
        .as_str()
        .expect("id")
        .starts_with("cap_"));
    assert_eq!(record["session_identity"].as_str(), Some("native"));
    assert_eq!(record["event_identity"]["status"].as_str(), Some("native"));
}

#[test]
fn eight_concurrent_duplicate_deliveries_yield_one_event() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let payload = capture_payload("s-dup", Some("tu-dup"), "same body");
    let mut handles = Vec::new();
    for _ in 0..8 {
        let config = config_home.path().to_path_buf();
        let user = user_home.path().to_path_buf();
        let proj = project.path().to_path_buf();
        let payload = payload.clone();
        handles.push(std::thread::spawn(move || {
            hook(&config, &user, &proj, "PostToolUse", payload).success();
        }));
    }
    for handle in handles {
        handle.join().expect("no panic");
    }

    let records = segment_records(project.path());
    assert_eq!(
        records.len(),
        1,
        "eight duplicate deliveries yield one event: {records:?}"
    );
    assert_eq!(
        replay_sightings(project.path()),
        7,
        "seven replay sightings recorded"
    );
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["observations", "health"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("1 captured/conflict"), "{stdout}");
    assert!(stdout.contains("7 replay"), "{stdout}");
}

#[test]
fn eight_independent_same_text_events_yield_eight() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    for idx in 0..8 {
        hook(
            config_home.path(),
            user_home.path(),
            project.path(),
            "PostToolUse",
            capture_payload("s-many", Some(&format!("tu-{idx}")), "identical text"),
        )
        .success();
    }
    // No host identity at all: still never collapsed by text.
    for _ in 0..2 {
        hook(
            config_home.path(),
            user_home.path(),
            project.path(),
            "PostToolUse",
            capture_payload("s-many", None, "identical text"),
        )
        .success();
    }
    let records = segment_records(project.path());
    assert_eq!(records.len(), 10, "same text ≠ same event: {records:?}");
    let no_identity: Vec<_> = records
        .iter()
        .filter(|r| r["event_identity"]["status"].as_str() == Some("unavailable"))
        .collect();
    assert_eq!(no_identity.len(), 2, "identity unavailability is labeled");
}

#[test]
fn same_identity_different_content_is_a_conflict_with_both_bodies() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    hook(
        config_home.path(),
        user_home.path(),
        project.path(),
        "PostToolUse",
        capture_payload("s-conflict", Some("tu-c"), "original body"),
    )
    .success();
    hook(
        config_home.path(),
        user_home.path(),
        project.path(),
        "PostToolUse",
        capture_payload("s-conflict", Some("tu-c"), "rewritten body"),
    )
    .success();

    let records = segment_records(project.path());
    assert_eq!(records.len(), 2, "both evidence bodies retained");
    let statuses: Vec<_> = records
        .iter()
        .filter_map(|r| r["capture"]["status"].as_str())
        .collect();
    assert!(statuses.contains(&"captured") && statuses.contains(&"conflict"));
    let texts: Vec<_> = records.iter().filter_map(|r| r["text"].as_str()).collect();
    assert!(
        texts.iter().any(|t| t.contains("original body")),
        "original body retained: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("rewritten body")),
        "rewritten body retained: {texts:?}"
    );
}

#[test]
fn stop_preserves_history_and_never_seals_synchronously() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // Plant a legacy spool row: history must survive untouched.
    let legacy = project
        .path()
        .join(".stateroot")
        .join("spool")
        .join("observations.jsonl");
    std::fs::create_dir_all(legacy.parent().expect("parent")).expect("mkdir");
    let legacy_bytes =
        "{\"ts\":\"2026-01-01T00:00:00Z\",\"event\":\"user_prompt_submit\",\"harness\":\"codex\",\"text\":\"legacy prompt\"}\n";
    std::fs::write(&legacy, legacy_bytes).expect("legacy spool");

    // Capture more than the former 256-KiB rotation threshold across two
    // sessions; the first event carries a unique sentinel.
    let big = "x".repeat(8 * 1024);
    for session in ["s-a", "s-b"] {
        hook(
            config_home.path(),
            user_home.path(),
            project.path(),
            "PostToolUse",
            capture_payload(
                session,
                Some(&format!("{session}-first")),
                "FIRST-EVENT-SENTINEL",
            ),
        )
        .success();
        for idx in 0..20 {
            hook(
                config_home.path(),
                user_home.path(),
                project.path(),
                "PostToolUse",
                capture_payload(session, Some(&format!("{session}-{idx}")), &big),
            )
            .success();
        }
    }
    let total: usize = std::fs::read_dir(segments_dir(project.path()))
        .expect("segments")
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .map(|e| e.path().metadata().map(|m| m.len() as usize).unwrap_or(0))
        .sum();
    assert!(
        total > 256 * 1024,
        "captured {total} bytes, beyond the old rotation threshold"
    );

    // Two sessions stopping concurrently.
    let mut handles = Vec::new();
    for session in ["s-a", "s-b"] {
        let config = config_home.path().to_path_buf();
        let user = user_home.path().to_path_buf();
        let proj = project.path().to_path_buf();
        handles.push(std::thread::spawn(move || {
            hook(
                &config,
                &user,
                &proj,
                "Stop",
                json!({"session_id": session}),
            )
            .success();
        }));
    }
    for handle in handles {
        handle.join().expect("no panic");
    }

    // Nothing was cleared: legacy bytes intact, all 42 events readable.
    assert_eq!(
        std::fs::read_to_string(&legacy).expect("legacy reads"),
        legacy_bytes,
        "legacy spool is never cleared or rotated"
    );
    assert_eq!(segment_records(project.path()).len(), 42);

    // Stop NEVER seals synchronously — sealing is the post-ingest drainer's
    // step (WS2). The seal markers stay absent after the stop hooks.
    let seals: Vec<_> = std::fs::read_dir(segments_dir(project.path()))
        .expect("segments")
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".sealed.json"))
        .collect();
    assert!(
        seals.is_empty(),
        "stop never seals synchronously: {seals:?}"
    );

    // Every capture wrote its bounded per-session frontier.
    let frontiers: Vec<_> = std::fs::read_dir(segments_dir(project.path()))
        .expect("segments")
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".frontier.json"))
        .collect();
    assert_eq!(frontiers.len(), 2, "one frontier per captured session");

    // The earliest event is still inspectable through the CLI.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["observations", "list", "--limit", "100"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("obs_1"), "legacy row listed: {stdout}");
    let first_id = segment_records(project.path())
        .into_iter()
        .find(|r| {
            r["text"]
                .as_str()
                .is_some_and(|t| t.contains("FIRST-EVENT-SENTINEL"))
                && r["event_identity"]["value"]
                    .as_str()
                    .is_some_and(|v| v == "s-a-first")
        })
        .and_then(|r| r["capture_id"].as_str().map(str::to_string))
        .expect("first event id");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["observations", "show", &first_id])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("FIRST-EVENT-SENTINEL"), "{stdout}");
}

#[test]
fn stop_stays_bounded_on_a_long_segment() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // Plant a long segment directly (owned fixture): thousands of records,
    // megabytes of evidence — the old synchronous stop-time work scanned it.
    let dir = segments_dir(project.path());
    std::fs::create_dir_all(&dir).expect("segments dir");
    let segment = dir.join("codex__s-long.jsonl");
    let mut content = String::new();
    for idx in 0..3000 {
        content.push_str(
            &serde_json::json!({
                "schema": "stateroot.observation.v2",
                "capture_id": format!("cap_planted_{idx}"),
                "ts": "2026-10-08T06:00:00Z",
                "event": "post_tool_use",
                "harness": "codex",
                "text": format!("planted evidence line {idx} {}", "y".repeat(200)),
                "capture": {"status": "captured", "source_status": "complete"},
            })
            .to_string(),
        );
        content.push('\n');
    }
    assert!(
        content.len() > 1024 * 1024,
        "a long segment: {}",
        content.len()
    );
    std::fs::write(&segment, &content).expect("plant segment");

    let start = std::time::Instant::now();
    hook(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Stop",
        json!({"session_id": "s-long"}),
    )
    .success();
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "stop stayed bounded on a {} byte segment: {elapsed:?}",
        content.len()
    );
    // No synchronous seal, no truncation, no clearing.
    let seals: Vec<_> = std::fs::read_dir(segments_dir(project.path()))
        .expect("segments")
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".sealed.json"))
        .collect();
    assert!(seals.is_empty(), "stop never seals synchronously");
    assert_eq!(
        std::fs::read_to_string(&segment).expect("segment intact"),
        content,
        "the long segment is byte-identical after stop"
    );
}

#[test]
fn kill_after_durable_publication_preserves_record_and_replay_dedups() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // Own the fixture process: payload delivered, then we watch for the
    // post-fsync frontier publication, rather than merely buffered segment bytes, and
    // kill the writer wherever it stands after publication.
    let binary = assert_cmd::cargo::cargo_bin("stateroot");
    let mut cmd = std::process::Command::new(binary);
    cmd.args(["hook", "PostToolUse", "--harness", "codex"])
        .env("STATEROOT_HOME", config_home.path())
        .env("STATEROOT_TEST_HOME", user_home.path())
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .current_dir(project.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().expect("spawn");
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().expect("stdin");
        stdin
            .write_all(
                capture_payload("s-kill", Some("tu-kill"), "durable-before-kill")
                    .to_string()
                    .as_bytes(),
            )
            .expect("payload");
        // Closing stdin lets the hook proceed to capture.
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let published = matches!(stateroot_core::observations::session_watermark(project.path(),"codex","s-kill"),stateroot_core::observations::SessionWatermark::Present(watermark) if watermark.records==1&&watermark.last_capture_id.is_some());
        if published {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "record was published before the deadline"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.kill().expect("kill -9 the fixture after publication");
    let _ = child.wait();

    // The publication survives the kill, intact and parseable.
    let records = segment_records(project.path());
    assert_eq!(records.len(), 1, "the durable record survived the kill");
    assert!(
        records[0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("durable-before-kill")),
        "record body intact: {records:?}"
    );

    // A redelivery after the kill dedups against the durable record — the
    // replay is acknowledged only after ITS sighting is durable too.
    hook(
        config_home.path(),
        user_home.path(),
        project.path(),
        "PostToolUse",
        capture_payload("s-kill", Some("tu-kill"), "durable-before-kill"),
    )
    .success();
    assert_eq!(
        segment_records(project.path()).len(),
        1,
        "still one event after the replay"
    );
    assert_eq!(replay_sightings(project.path()), 1);
}

#[test]
fn killed_writer_before_publication_leaves_no_record_and_recovers() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // Own a fixture process parked on its stdin read (the payload never
    // arrives), then kill it: nothing may be published.
    let binary = assert_cmd::cargo::cargo_bin("stateroot");
    let mut cmd = std::process::Command::new(binary);
    cmd.args(["hook", "PostToolUse", "--harness", "codex"])
        .env("STATEROOT_HOME", config_home.path())
        .env("STATEROOT_TEST_HOME", user_home.path())
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .current_dir(project.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().expect("spawn");
    std::thread::sleep(std::time::Duration::from_millis(300));
    child.kill().expect("kill -9 the fixture");
    let _ = child.wait();
    assert!(
        segment_records(project.path()).is_empty(),
        "a killed writer publishes nothing"
    );

    // Recovery: the next capture works, and a torn tail from a crashed
    // writer is terminated + reported, never accepted as a record.
    let dir = segments_dir(project.path());
    std::fs::create_dir_all(&dir).expect("segments dir");
    let segment = dir.join("codex__s-torn.jsonl");
    std::fs::write(&segment, "{\"schema\":\"stateroot.observation.v2\",\"cap").expect("torn tail");
    hook(
        config_home.path(),
        user_home.path(),
        project.path(),
        "PostToolUse",
        capture_payload("s-torn", Some("tu-torn"), "recovered write"),
    )
    .success();
    let records = segment_records(project.path());
    assert_eq!(records.len(), 1, "torn bytes are not a record");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["observations", "health"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("corrupt/torn lines: 1"), "{stdout}");
    assert!(stdout.contains("pending-seal"), "{stdout}");
}

#[cfg(unix)]
#[test]
fn storage_permission_failure_is_explicit_never_false_success() {
    use std::os::unix::fs::PermissionsExt;
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // A read-only spool tree: the segment lock cannot be created.
    let spool = project.path().join(".stateroot").join("spool");
    std::fs::create_dir_all(&spool).expect("spool dir");
    let mut perms = spool.metadata().expect("meta").permissions();
    perms.set_mode(0o555);
    std::fs::set_permissions(&spool, perms).expect("read-only");

    hook(
        config_home.path(),
        user_home.path(),
        project.path(),
        "PostToolUse",
        capture_payload("s-perm", Some("tu-perm"), "must not be stored"),
    )
    .failure();

    let mut perms = spool.metadata().expect("meta").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&spool, perms).expect("restore");
    assert!(
        segment_records(project.path()).is_empty(),
        "no partial record on failure"
    );
}

#[test]
fn legacy_ids_resolve_and_unavailable_is_reported_honestly() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let legacy = project
        .path()
        .join(".stateroot")
        .join("spool")
        .join("observations.jsonl");
    std::fs::create_dir_all(legacy.parent().expect("parent")).expect("mkdir");
    std::fs::write(
        &legacy,
        "{\"ts\":\"2026-01-01T00:00:00Z\",\"event\":\"user_prompt_submit\",\"harness\":\"codex\",\"text\":\"legacy prompt\"}\n{not json\n",
    )
    .expect("legacy spool");

    // obs_1 still resolves through the existing CLI.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["observations", "show", "obs_1"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("legacy prompt"), "{stdout}");

    // obs_9 pointed at evidence a historical clear/rotation destroyed:
    // reported unavailable, never mapped onto other rows.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["observations", "show", "obs_9"])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(stderr.contains("unavailable"), "{stderr}");

    // The corrupt legacy line is exposed, not silently skipped.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["observations", "health"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("legacy rows: 1"), "{stdout}");
    assert!(stdout.contains("corrupt/torn lines: 1"), "{stdout}");
}
