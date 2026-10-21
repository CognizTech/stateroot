//! Interrupted-work continuity end-to-end proof: the EXISTING durable
//! observation, finalize-journal and resume primitives COMPOSED into one
//! interruption/next-session acceptance scenario. No new recovery machinery —
//! the fault seams are the ones already shipped (owned fixture kills, journal
//! phase rewind, exact-occurrence replay).
//!
//! Evidence matrix (each row is asserted, not narrated):
//!
//! | fault boundary                  | observed root        | observed session/receipt |
//! |---------------------------------|----------------------|--------------------------|
//! | writer killed pre-publication   | n/a                  | no record; next capture recovers |
//! | writer killed post-publication  | n/a                  | record + frontier survive; replay dedups |
//! | boundary enqueued               | boundary_source_root | job frozen: plan_ref + 3-record watermark |
//! | crash after root snap           | R1 kept, seq null    | replay finalizes against R1, same seq |
//! | crash after handoff publication | R1 kept, seq 1       | replay re-ingests; ONE ack receipt |
//! | new turn after old boundary     | R2, seq 2            | new key/receipt; old receipt untouched |
//! | next-session resume             | digest root == job   | objective + outstanding plan, no replay |
//! | torn tail after recovery        | n/a                  | health reports 1 torn; bytes retained |

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

/// Fixture paths inside JSON payloads: forward slashes survive both JSON
/// and normalize_path (mirrors core's cfg(test) path_for_json).
fn pj(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn hook(
    config_home: &Path,
    user_home: &Path,
    project: &Path,
    event: &str,
    payload: Value,
) -> assert_cmd::assert::Assert {
    stateroot(config_home, user_home, project)
        .args(["hook", event, "--harness", "pi"])
        .write_stdin(payload.to_string())
        .assert()
}

fn capture_payload(session: &str, tool_use_id: &str, body: &str) -> Value {
    json!({
        "session_id": session,
        "tool_name": "Bash",
        "tool_response": body,
        "tool_use_id": tool_use_id,
    })
}

/// A pi session for `id` in the user home, cwd-bound to the project.
fn write_pi_session(user_home: &Path, project: &Path, id: &str, phrase: &str) {
    let cwd = pj(project);
    let dir = user_home.join(".pi/agent/sessions/--tmp-demo--");
    std::fs::create_dir_all(&dir).expect("pi dir");
    std::fs::write(
        dir.join(format!("{id}.jsonl")),
        format!(
            concat!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-08-20T10:00:00.000Z\",\"cwd\":\"{cwd}\"}}\n",
                "{{\"type\":\"message\",\"id\":\"m1\",\"parentId\":null,\"timestamp\":\"2026-08-20T10:00:01.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"{phrase}\",\"timestamp\":1}}}}\n",
            ),
            cwd = cwd,
            id = id,
            phrase = phrase,
        ),
    )
    .expect("write session");
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

fn journal_files(project: &Path) -> Vec<Value> {
    let dir = project.join(".stateroot/local/finalize-journal");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok())
        .collect()
}

fn journal_dir(project: &Path) -> PathBuf {
    project.join(".stateroot/local/finalize-journal")
}

fn receipt_path(project: &Path, ingest_key: &str) -> PathBuf {
    project
        .join(".stateroot/spool/acknowledgements")
        .join(format!("{ingest_key}.json"))
}

fn current_handoff(project: &Path) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(project.join(".stateroot/handoffs/current.json"))
            .expect("current handoff"),
    )
    .expect("current handoff json")
}

fn history_packets(project: &Path) -> Vec<Value> {
    let dir = project.join(".stateroot/handoffs/history");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| serde_json::from_slice(&std::fs::read(entry.path()).ok()?).ok())
        .collect()
}

fn drain(config_home: &Path, user_home: &Path, project: &Path) {
    stateroot(config_home, user_home, project)
        .arg("_drain-finalize")
        .assert()
        .success();
}

/// Crash seam: rewind the persisted job to an earlier committed phase, as a
/// process crash between atomic phase commits would leave it on disk.
fn rewind_job(project: &Path, job: &Value, phase: &str, handoff_seq: Option<i64>) {
    let path = journal_dir(project).join(format!("{}.json", job["id"].as_str().expect("id")));
    let mut on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("job file")).expect("json");
    on_disk["phase"] = json!(phase);
    on_disk["state"] = json!("active");
    on_disk["handoff_seq"] = handoff_seq.map_or(Value::Null, Value::from);
    on_disk["next_attempt_at"] = json!("2020-01-01T00:00:00Z");
    std::fs::write(&path, serde_json::to_string_pretty(&on_disk).expect("ser")).expect("rewind");
}

fn record_active_plan(config_home: &Path, user_home: &Path, project: &Path, title: &str) -> String {
    let out = stateroot(config_home, user_home, project)
        .args(["plan", "record", "--stdin", "--title", title])
        .write_stdin(format!("# {title}\n\nDo the work.\n"))
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    let id = stdout
        .split_whitespace()
        .find(|w| w.starts_with("plan_"))
        .expect("plan id in output")
        .to_string();
    stateroot(config_home, user_home, project)
        .args(["plan", "approve", &id])
        .assert()
        .success();
    stateroot(config_home, user_home, project)
        .args(["plan", "activate", &id])
        .assert()
        .success();
    id
}

/// Outstanding work for the accepted plan: one pending plan-bound todo, as a
/// harness-native federation write would have persisted it. Without recorded
/// remaining work the continuity assessment honestly classifies the plan as
/// structurally complete (Close), which is not the scenario under test.
fn bind_outstanding_todo(project: &Path, plan_id: &str, session: &str) {
    let dir = project.join(".stateroot").join("todos").join("pi");
    std::fs::create_dir_all(&dir).expect("todos dir");
    let record = json!({
        "schema_version": "stateroot.todos.v1",
        "harness": "pi",
        "session_id": session,
        "plan_id": plan_id,
        "items": [{
            "key": "finish-proof",
            "content": "Finish the interrupted-work proof",
            "status": "pending",
        }],
        "updated_at": "2026-08-20T10:00:02Z",
        "provenance": "observed · fixture",
    });
    std::fs::write(
        dir.join(format!("{session}.json")),
        format!("{}\n", serde_json::to_string_pretty(&record).expect("ser")),
    )
    .expect("todo record");
}

fn set_objective(project: &Path, objective: &str) {
    let path = project.join(".stateroot/project/state.json");
    let mut state: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("state")).expect("state json");
    state["objective"] = json!(objective);
    std::fs::write(&path, serde_json::to_string_pretty(&state).expect("ser")).expect("state write");
}

/// Own a fixture hook process, deliver the payload, then kill it wherever it
/// stands AFTER the post-fsync frontier publication names `expect_records`.
fn kill_fixture_after_publication(
    config_home: &Path,
    user_home: &Path,
    project: &Path,
    session: &str,
    payload: Value,
    expect_records: u64,
) {
    let binary = assert_cmd::cargo::cargo_bin("stateroot");
    let mut cmd = std::process::Command::new(binary);
    cmd.args(["hook", "post_tool_use", "--harness", "pi"])
        .env("STATEROOT_HOME", config_home)
        .env("STATEROOT_TEST_HOME", user_home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .current_dir(project)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().expect("spawn");
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().expect("stdin");
        stdin
            .write_all(payload.to_string().as_bytes())
            .expect("payload");
        // Closing stdin lets the hook proceed to capture.
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let published = matches!(
            stateroot_core::observations::session_watermark(project, "pi", session),
            stateroot_core::observations::SessionWatermark::Present(watermark)
                if watermark.records == expect_records && watermark.last_capture_id.is_some()
        );
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
}

/// Own a fixture hook process parked on its stdin read (the payload never
/// arrives), then kill it: nothing may be published.
fn kill_fixture_before_publication(config_home: &Path, user_home: &Path, project: &Path) {
    let binary = assert_cmd::cargo::cargo_bin("stateroot");
    let mut cmd = std::process::Command::new(binary);
    cmd.args(["hook", "post_tool_use", "--harness", "pi"])
        .env("STATEROOT_HOME", config_home)
        .env("STATEROOT_TEST_HOME", user_home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .current_dir(project)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().expect("spawn");
    std::thread::sleep(std::time::Duration::from_millis(300));
    child.kill().expect("kill -9 the parked fixture");
    let _ = child.wait();
}

#[test]
fn interrupted_boundary_composes_into_a_bound_next_session_resume() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // --- accepted task: an objective and an active (outstanding) plan ------
    set_objective(project.path(), "Prove interrupted-work continuity");
    let plan = record_active_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Continuity proof plan",
    );
    bind_outstanding_todo(project.path(), &plan, "s-run");

    // --- observed work: two durable captures in session s-run --------------
    write_pi_session(
        user_home.path(),
        project.path(),
        "s-run",
        "prove resume after interruption",
    );
    for (id, body) in [("tu-1", "durable work one"), ("tu-2", "durable work two")] {
        hook(
            config_home.path(),
            user_home.path(),
            project.path(),
            "post_tool_use",
            capture_payload("s-run", id, body),
        )
        .success();
    }

    // --- interruption AFTER durable publication: the writer dies, the ------
    // --- record and its frontier survive, and a redelivery dedups.   -------
    kill_fixture_after_publication(
        config_home.path(),
        user_home.path(),
        project.path(),
        "s-run",
        capture_payload("s-run", "tu-3", "interrupted mid-flight"),
        3,
    );
    let records = segment_records(project.path());
    assert_eq!(records.len(), 3, "the durable record survived the kill");
    assert!(records.iter().any(|r| r["text"]
        .as_str()
        .is_some_and(|t| t.contains("interrupted mid-flight"))));

    // --- interruption BEFORE durable publication: nothing is published -----
    // --- and the next capture recovers (the redelivery).             -------
    kill_fixture_before_publication(config_home.path(), user_home.path(), project.path());
    assert_eq!(
        segment_records(project.path()).len(),
        3,
        "a killed writer publishes nothing"
    );
    hook(
        config_home.path(),
        user_home.path(),
        project.path(),
        "post_tool_use",
        capture_payload("s-run", "tu-3", "interrupted mid-flight"),
    )
    .success();
    assert_eq!(
        segment_records(project.path()).len(),
        3,
        "redelivery dedups against the durable record"
    );
    assert_eq!(replay_sightings(project.path()), 1, "one replay sighting");

    // --- interrupted turn boundary: the journal freezes plan + frontier ----
    hook(
        config_home.path(),
        user_home.path(),
        project.path(),
        "session_end",
        json!({"session_id":"s-run","event_id":"boundary-run-1","cwd":pj(project.path())}),
    )
    .success();
    let jobs = journal_files(project.path());
    assert_eq!(jobs.len(), 1, "one composite boundary job: {jobs:?}");
    let job = &jobs[0];
    assert_eq!(job["phase"], "queued");
    assert_eq!(job["occurrence"], "boundary-run-1");
    assert_eq!(
        job["plan_ref"]["id"].as_str(),
        Some(plan.as_str()),
        "the accepted plan is frozen with the boundary"
    );
    assert_eq!(job["capture_watermark"]["status"], "present");
    assert_eq!(
        job["capture_watermark"]["watermark"]["records"].as_u64(),
        Some(3),
        "the frontier freezes at the boundary, before later captures"
    );
    let frozen_watermark = job["capture_watermark"]["watermark"].clone();
    let ingest_key = job["ingest_key"].as_str().expect("ingest key").to_string();

    // --- post-boundary work: a source edit plus same/other-session captures
    // --- that must NOT be covered by the frozen boundary receipt.    -------
    std::fs::write(project.path().join("work.txt"), "post-boundary edit").expect("edit");
    hook(
        config_home.path(),
        user_home.path(),
        project.path(),
        "post_tool_use",
        capture_payload("s-run", "tu-4", "captured after boundary"),
    )
    .success();
    hook(
        config_home.path(),
        user_home.path(),
        project.path(),
        "post_tool_use",
        capture_payload("s-other", "tu-o1", "other session capture"),
    )
    .success();

    // --- recovery: the drain snaps, finalizes against THAT root, ingests, --
    // --- and acknowledges exactly the frozen frontier.                ------
    drain(config_home.path(), user_home.path(), project.path());
    let jobs = journal_files(project.path());
    assert_eq!(jobs.len(), 1);
    let job = &jobs[0];
    assert_eq!(job["phase"], "complete", "job: {job:?}");
    assert_eq!(job["state"], "terminal");
    let root_one = job["root"].as_str().expect("root recorded").to_string();
    assert_eq!(job["handoff_seq"].as_i64(), Some(1));
    assert!(
        job["snapshot_timing"]
            .as_str()
            .unwrap_or("")
            .contains("post-boundary edits"),
        "honest coverage disclosure, never a false restore claim: {job:?}"
    );

    // The handoff binds the boundary's exact root, the accepted objective and
    // the frozen watermark — and says it is observed, not author-written.
    let packet = current_handoff(project.path());
    assert_eq!(packet["latest_root"].as_str(), Some(root_one.as_str()));
    assert_eq!(packet["seq"].as_i64(), Some(1));
    assert_eq!(
        packet["boundary_ingest_key"].as_str(),
        Some(ingest_key.as_str())
    );
    assert_eq!(
        packet["objective"].as_str(),
        Some("Prove interrupted-work continuity")
    );
    assert_eq!(packet["plan_ref"]["id"].as_str(), Some(plan.as_str()));
    assert_eq!(packet["capture_watermark"]["watermark"], frozen_watermark);
    let warnings: Vec<&str> = packet["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .filter_map(|w| w.as_str())
        .collect();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("observed, not author-written")),
        "warnings: {warnings:?}"
    );

    // One authoritative completion receipt, bound to harness+session+key.
    let receipt_bytes = std::fs::read(receipt_path(project.path(), &ingest_key)).expect("receipt");
    let receipt: Value = serde_json::from_slice(&receipt_bytes).expect("receipt json");
    assert_eq!(receipt["schema"], "stateroot.observations.ack.v1");
    assert_eq!(receipt["ingest_key"].as_str(), Some(ingest_key.as_str()));
    assert_eq!(receipt["harness"].as_str(), Some("pi"));
    assert_eq!(receipt["session_id"].as_str(), Some("s-run"));
    assert_eq!(receipt["watermark"], frozen_watermark);

    // Later and other-session captures are NOT acknowledged by this boundary.
    let segments = stateroot_core::observations::list_segments(project.path());
    assert!(
        segments.iter().all(|segment| !segment.sealed),
        "frontier moved past the frozen watermark: nothing seals: {segments:?}"
    );

    // --- crash after root snap: replay finalizes against the SAME root with
    // --- the SAME seq; one keyed packet, one receipt, byte-identical. -----
    rewind_job(project.path(), job, "snapped", None);
    drain(config_home.path(), user_home.path(), project.path());
    let jobs = journal_files(project.path());
    assert_eq!(jobs[0]["phase"], "complete", "job: {jobs:?}");
    assert_eq!(jobs[0]["root"].as_str(), Some(root_one.as_str()));
    assert_eq!(jobs[0]["handoff_seq"].as_i64(), Some(1));
    let keyed = history_packets(project.path())
        .into_iter()
        .filter(|packet| packet["boundary_ingest_key"].as_str() == Some(ingest_key.as_str()))
        .count();
    assert_eq!(keyed, 1, "exactly one packet for the boundary key");
    assert_eq!(
        std::fs::read(receipt_path(project.path(), &ingest_key)).expect("receipt"),
        receipt_bytes,
        "the receipt is byte-identical after replay"
    );

    // --- crash after handoff publication but before journal completion: ----
    // --- replay re-ingests and re-acknowledges idempotently.          -----
    rewind_job(project.path(), &jobs[0], "handoff_finalized", Some(1));
    drain(config_home.path(), user_home.path(), project.path());
    let jobs = journal_files(project.path());
    assert_eq!(jobs[0]["phase"], "complete", "job: {jobs:?}");
    assert_eq!(jobs[0]["handoff_seq"].as_i64(), Some(1));
    let keyed = history_packets(project.path())
        .into_iter()
        .filter(|packet| packet["boundary_ingest_key"].as_str() == Some(ingest_key.as_str()))
        .count();
    assert_eq!(keyed, 1, "still exactly one packet for the boundary key");
    assert_eq!(
        std::fs::read(receipt_path(project.path(), &ingest_key)).expect("receipt"),
        receipt_bytes,
        "the receipt is still byte-identical after re-ingest"
    );

    // --- the SAME verified boundary occurrence cannot open a second job; ---
    // --- a NEW turn finalizes under its OWN key, never the old receipt. ---
    hook(
        config_home.path(),
        user_home.path(),
        project.path(),
        "session_end",
        json!({"session_id":"s-run","event_id":"boundary-run-1","cwd":pj(project.path())}),
    )
    .success();
    assert_eq!(
        journal_files(project.path()).len(),
        1,
        "occurrence replay does not duplicate the boundary"
    );
    hook(
        config_home.path(),
        user_home.path(),
        project.path(),
        "session_end",
        json!({"session_id":"s-run","event_id":"boundary-run-2","cwd":pj(project.path())}),
    )
    .success();
    let jobs = journal_files(project.path());
    assert_eq!(jobs.len(), 2, "the new turn is its own boundary");
    let job_two = jobs
        .iter()
        .find(|j| j["occurrence"] == "boundary-run-2")
        .expect("second job");
    assert_eq!(
        job_two["capture_watermark"]["watermark"]["records"].as_u64(),
        Some(4),
        "the new turn freezes the moved frontier"
    );
    let key_two = job_two["ingest_key"].as_str().expect("key").to_string();
    assert_ne!(key_two, ingest_key);
    drain(config_home.path(), user_home.path(), project.path());
    let jobs = journal_files(project.path());
    let job_two = jobs
        .iter()
        .find(|j| j["occurrence"] == "boundary-run-2")
        .expect("second job");
    assert_eq!(job_two["phase"], "complete", "job: {job_two:?}");
    let root_two = job_two["root"].as_str().expect("root").to_string();
    assert_eq!(job_two["handoff_seq"].as_i64(), Some(2));
    // The new receipt names the new frontier; the old receipt is untouched.
    let receipt_two: Value = serde_json::from_slice(
        &std::fs::read(receipt_path(project.path(), &key_two)).expect("receipt two"),
    )
    .expect("receipt two json");
    assert_eq!(receipt_two["watermark"]["records"].as_u64(), Some(4));
    assert_eq!(
        std::fs::read(receipt_path(project.path(), &ingest_key)).expect("receipt"),
        receipt_bytes,
        "the old boundary receipt is never reused or rewritten"
    );
    // The current handoff now binds the SECOND boundary's root and key.
    let packet = current_handoff(project.path());
    assert_eq!(packet["seq"].as_i64(), Some(2));
    assert_eq!(packet["latest_root"].as_str(), Some(root_two.as_str()));
    assert_eq!(
        packet["boundary_ingest_key"].as_str(),
        Some(key_two.as_str())
    );
    // The moved frontier now seals under the new key; s-other never seals.
    let segments = stateroot_core::observations::list_segments(project.path());
    let s_run = segments
        .iter()
        .find(|s| s.file.contains("s-run"))
        .expect("s-run segment");
    let s_other = segments
        .iter()
        .find(|s| s.file.contains("s-other"))
        .expect("s-other segment");
    assert!(s_run.sealed, "the acknowledged prefix seals: {segments:?}");
    assert!(
        !s_other.sealed,
        "another session is never sealed: {segments:?}"
    );

    // --- next-session resume: the digest is bound to THIS project, root and
    // --- accepted objective, and outstanding work is not reported done. ---
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--harness", "pi"])
        .assert()
        .success();
    let digest = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(digest.contains("## Objective"), "digest: {digest}");
    assert!(
        digest.contains("Prove interrupted-work continuity"),
        "the accepted objective survives interruption: {digest}"
    );
    assert!(digest.contains("## Work State Lineage"), "digest: {digest}");
    let short_root: String = root_two.chars().take(12).collect();
    assert!(
        digest.contains(&format!("Current root: `{short_root}`")),
        "the digest binds the boundary's root: {digest}"
    );
    assert!(
        digest.contains("Execute it as written; do not re-plan or re-explore"),
        "outstanding plan keeps its executor directive: {digest}"
    );
    assert!(
        !digest.contains("do not restart implementation"),
        "outstanding work is never reported complete: {digest}"
    );
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["status", "--json"])
        .assert()
        .success();
    let status: Value =
        serde_json::from_str(&String::from_utf8(out.get_output().stdout.clone()).expect("utf8"))
            .expect("status json");
    assert_eq!(status["plan"]["directive"], "execute");
    assert!(status["boundary_journal"].is_object());

    // Acknowledged evidence stays resolvable; tool replay is outside
    // StateRoot authority, so the proof asserts the continuity EVIDENCE:
    // the receipt binds harness+session+watermark and the captures resolve.
    let first_id = segment_records(project.path())
        .into_iter()
        .find(|r| {
            r["text"]
                .as_str()
                .is_some_and(|t| t.contains("durable work one"))
        })
        .and_then(|r| r["capture_id"].as_str().map(str::to_string))
        .expect("first capture id");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["observations", "show", &first_id])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("durable work one"), "{stdout}");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["observations", "list", "--limit", "100"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("captured after boundary"),
        "unacknowledged post-boundary work stays visible: {stdout}"
    );

    // --- torn evidence after recovery: preserved on disk with an honest ---
    // --- diagnostic, never discarded and never accepted as a record.  -----
    let segment = segments_dir(project.path()).join("pi__s-run.jsonl");
    let intact = std::fs::read_to_string(&segment).expect("segment");
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&segment)
        .expect("append")
        .write_all(b"{\"schema\":\"stateroot.observation.v2\",\"cap")
        .expect("torn tail");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["observations", "health"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("corrupt/torn lines: 1"), "{stdout}");
    // tu-1..tu-4 in s-run plus tu-o1 in s-other: 5 durable records, and the
    // torn tail adds none.
    assert_eq!(
        segment_records(project.path()).len(),
        5,
        "torn bytes are not a record"
    );
    assert!(
        std::fs::read_to_string(&segment)
            .expect("segment")
            .starts_with(&intact),
        "torn evidence is preserved in place, never discarded"
    );
}

#[test]
fn stale_boundary_from_another_checkout_never_finalizes_here() {
    let (config_home, user_home) = homes();
    let project_a = tempfile::tempdir().expect("project a");
    let project_b = tempfile::tempdir().expect("project b");
    init_project(config_home.path(), user_home.path(), project_a.path());
    init_project(config_home.path(), user_home.path(), project_b.path());

    // A real boundary in checkout A.
    write_pi_session(user_home.path(), project_a.path(), "s-a", "checkout A work");
    hook(
        config_home.path(),
        user_home.path(),
        project_a.path(),
        "post_tool_use",
        capture_payload("s-a", "tu-a1", "checkout A capture"),
    )
    .success();
    hook(
        config_home.path(),
        user_home.path(),
        project_a.path(),
        "session_end",
        json!({"session_id":"s-a","event_id":"boundary-a","cwd":pj(project_a.path())}),
    )
    .success();
    let jobs_a = journal_files(project_a.path());
    assert_eq!(jobs_a.len(), 1);
    let stale_id = jobs_a[0]["id"].as_str().expect("id").to_string();

    // The stale job file is replayed into checkout B's journal (cross-project
    // stale receipt reuse attempt): it must error and stay retained, never
    // finalize, never produce a handoff or receipt in B.
    std::fs::create_dir_all(journal_dir(project_b.path())).expect("b journal");
    std::fs::copy(
        journal_dir(project_a.path()).join(format!("{stale_id}.json")),
        journal_dir(project_b.path()).join(format!("{stale_id}.json")),
    )
    .expect("replay stale job");
    drain(config_home.path(), user_home.path(), project_b.path());
    let jobs_b = journal_files(project_b.path());
    assert_eq!(jobs_b.len(), 1, "the stale job is retained, never dropped");
    assert_eq!(jobs_b[0]["state"], "active");
    assert!(
        jobs_b[0]["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("checkout mismatch"),
        "retained error: {jobs_b:?}"
    );
    assert!(
        jobs_b[0]["attempt"].as_u64().unwrap_or(0) >= 1,
        "attempt counted: {jobs_b:?}"
    );
    assert!(
        !project_b
            .path()
            .join(".stateroot/handoffs/current.json")
            .exists(),
        "no boundary handoff is published in the wrong checkout"
    );
    assert!(
        history_packets(project_b.path()).is_empty(),
        "no boundary history in the wrong checkout"
    );
    let b_receipts = project_b.path().join(".stateroot/spool/acknowledgements");
    let b_receipt_count = std::fs::read_dir(&b_receipts)
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(b_receipt_count, 0, "no receipt in the wrong checkout");
    assert!(
        journal_dir(project_b.path())
            .join(format!("{stale_id}.json"))
            .is_file(),
        "the stale record stays on disk for inspection"
    );

    // Checkout A drains cleanly: one receipt, and its key cannot be reused
    // for another session or rewritten under a different watermark.
    drain(config_home.path(), user_home.path(), project_a.path());
    let jobs_a = journal_files(project_a.path());
    assert_eq!(jobs_a[0]["phase"], "complete", "job: {jobs_a:?}");
    let key_a = jobs_a[0]["ingest_key"].as_str().expect("key").to_string();
    let receipt: Value = serde_json::from_slice(
        &std::fs::read(receipt_path(project_a.path(), &key_a)).expect("receipt"),
    )
    .expect("receipt json");
    let watermark_a: stateroot_core::observations::CaptureWatermark =
        serde_json::from_value(receipt["watermark"].clone()).expect("watermark");

    // Idempotent replay of the SAME acknowledgement is a no-op success.
    stateroot_core::observations::acknowledge_frontier(
        project_a.path(),
        "pi",
        "s-a",
        &key_a,
        &watermark_a,
    )
    .expect("identical re-acknowledgement");

    // Cross-session reuse of the key is a binding error, never a receipt.
    let cross = stateroot_core::observations::acknowledge_frontier(
        project_a.path(),
        "pi",
        "s-other",
        &key_a,
        &watermark_a,
    );
    assert!(
        cross
            .as_ref()
            .expect_err("cross-session binding refused")
            .to_string()
            .contains("binding invalid"),
        "cross-session error: {cross:?}"
    );

    // Same key with a different (stale) watermark is a conflict, and the
    // authoritative receipt on disk stays byte-identical.
    let before = std::fs::read(receipt_path(project_a.path(), &key_a)).expect("receipt");
    let stale_watermark = stateroot_core::observations::CaptureWatermark {
        records: watermark_a.records + 99,
        ..watermark_a.clone()
    };
    let conflict = stateroot_core::observations::acknowledge_frontier(
        project_a.path(),
        "pi",
        "s-a",
        &key_a,
        &stale_watermark,
    );
    assert!(
        conflict
            .as_ref()
            .expect_err("stale watermark conflict refused")
            .to_string()
            .contains("key conflict"),
        "conflict error: {conflict:?}"
    );
    assert_eq!(
        std::fs::read(receipt_path(project_a.path(), &key_a)).expect("receipt"),
        before,
        "the authoritative receipt is never rewritten by a stale replay"
    );
}
