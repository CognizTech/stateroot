//! Session-boundary journal end to end (repair Phase 3): one durable job
//! per boundary, phases commit in order, the first automatic handoff binds
//! the boundary's exact root, and an unavailable transcript is retryable
//! error state — never a false success.

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

/// Fixture paths inside JSON payloads: forward slashes survive both JSON
/// and normalize_path (mirrors core's cfg(test) path_for_json).
fn pj(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
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

fn journal_files(project: &Path) -> Vec<serde_json::Value> {
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

fn hook_event(
    config_home: &Path,
    user_home: &Path,
    project: &Path,
    event: &str,
    payload: &str,
) -> assert_cmd::assert::Assert {
    stateroot(config_home, user_home, project)
        .args(["hook", event, "--harness", "pi"])
        .write_stdin(payload)
        .assert()
}

#[test]
fn inspection_lists_every_retained_job_without_advancing_or_rewriting() {
    use stateroot_core::finalize_journal as journal;
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().unwrap();
    init_project(config_home.path(), user_home.path(), project.path());
    let mut retained = Vec::new();
    for index in 0..6 {
        let mut job = journal::enqueue(
            project.path(),
            "codex",
            &format!("inspect-{index}"),
            None,
            "refs/stateroot/latest",
        )
        .unwrap();
        job.state = "manual_attention".into();
        job.attempt = journal::MAX_ATTEMPTS;
        job.last_error = Some("retained fixture error".into());
        journal::save(project.path(), &job).unwrap();
        let path = project
            .path()
            .join(".stateroot")
            .join(journal::JOURNAL_DIR)
            .join(format!("{}.json", job.id));
        retained.push((job.id, std::fs::read(&path).unwrap(), path));
    }
    let output = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["handoff", "inspect"])
        .assert()
        .success();
    let document: serde_json::Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(document["read_only"], true);
    assert_eq!(document["jobs"].as_array().unwrap().len(), retained.len());
    let output = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["handoff", "inspect", "--job", &retained[0].0])
        .assert()
        .success();
    let selected: serde_json::Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(selected["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(selected["jobs"][0]["id"], retained[0].0);
    for (_, bytes, path) in retained {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn boundary_job_flows_to_one_snap_one_bound_handoff_one_ingest() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    write_pi_session(user_home.path(), project.path(), "ses-j1", "boundary work");

    // The hook enqueues the durable job (kick is disabled under the test
    // harness — the test drives the drain explicitly).
    hook_event(
        config_home.path(),
        user_home.path(),
        project.path(),
        "session_end",
        &format!(
            "{{\"session_id\":\"ses-j1\",\"cwd\":\"{}\"}}",
            pj(project.path())
        ),
    )
    .success();
    let jobs = journal_files(project.path());
    assert_eq!(jobs.len(), 1, "one composite job, not a trio: {jobs:?}");
    assert_eq!(jobs[0]["phase"], "queued");

    // Drain to completion.
    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("_drain-finalize")
        .assert()
        .success();
    let jobs = journal_files(project.path());
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["phase"], "complete", "job: {jobs:?}");
    assert_eq!(jobs[0]["state"], "terminal");
    let root = jobs[0]["root"].as_str().expect("root recorded");
    let seq = jobs[0]["handoff_seq"].as_i64().expect("seq recorded");
    assert_eq!(seq, 1, "first automatic handoff allowed when none existed");

    // The handoff exists AND binds the boundary's exact root.
    let current = std::fs::read_to_string(project.path().join(".stateroot/handoffs/current.json"))
        .expect("current handoff");
    let packet: serde_json::Value = serde_json::from_str(&current).expect("json");
    assert_eq!(packet["latest_root"].as_str(), Some(root));
    assert_eq!(packet["seq"].as_i64(), Some(1));
}

#[test]
fn unavailable_transcript_is_retryable_error_never_success() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    // No transcript anywhere for this harness.
    hook_event(
        config_home.path(),
        user_home.path(),
        project.path(),
        "session_end",
        &format!(
            "{{\"session_id\":\"ses-missing\",\"cwd\":\"{}\"}}",
            pj(project.path())
        ),
    )
    .success();

    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("_drain-finalize")
        .assert()
        .success();
    let jobs = journal_files(project.path());
    assert_eq!(jobs.len(), 1);
    let job = &jobs[0];
    assert_eq!(job["state"], "active", "job must stay active, not complete");
    assert_eq!(job["phase"], "snapped", "snap committed, finalize failed");
    assert!(
        job["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("no verified"),
        "retained error: {job:?}"
    );
    assert!(
        job["attempt"].as_u64().unwrap_or(0) >= 1,
        "attempt counted: {job:?}"
    );

    // The transcript ARRIVES later; once the backoff elapses, the next
    // drain resumes and completes. (Rewind the persisted next_attempt_at
    // instead of sleeping — the backoff itself is asserted above.)
    write_pi_session(
        user_home.path(),
        project.path(),
        "ses-missing",
        "late arrival",
    );
    let job_file = project
        .path()
        .join(".stateroot/local/finalize-journal")
        .join(format!("{}.json", job["id"].as_str().expect("id")));
    let mut on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&job_file).expect("job file")).expect("json");
    on_disk["next_attempt_at"] = serde_json::json!("2020-01-01T00:00:00Z");
    std::fs::write(
        &job_file,
        serde_json::to_string_pretty(&on_disk).expect("ser"),
    )
    .expect("rewind");
    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("_drain-finalize")
        .assert()
        .success();
    let jobs = journal_files(project.path());
    assert_eq!(jobs[0]["phase"], "complete", "job: {jobs:?}");
    assert_eq!(jobs[0]["state"], "terminal");
}

#[test]
fn exact_session_replay_keeps_authored_routing_and_boundary_tree() {
    let (config, home) = homes();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    write_pi_session(home.path(), project.path(), "old", "old session evidence");
    write_pi_session(
        home.path(),
        project.path(),
        "new",
        "new session must not satisfy old boundary",
    );
    std::fs::write(project.path().join("work.txt"), "boundary tree").unwrap();
    let payload =
        serde_json::json!({"session_id":"old", "event_id":"boundary-one", "cwd":project.path()})
            .to_string();
    hook_event(
        config.path(),
        home.path(),
        project.path(),
        "session_end",
        &payload,
    )
    .success();
    let frozen = journal_files(project.path())[0]["boundary_source_root"].clone();
    std::fs::write(project.path().join("work.txt"), "later edits").unwrap();
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "kimi",
            "--to",
            "codex",
            "--objective",
            "Owner current objective",
            "--task",
            "Authored current task",
            "--context-summary",
            "Preserve this explicit authored handoff and routing",
            "--next",
            "Implement the owner task",
        ])
        .assert()
        .success();
    let authored: serde_json::Value = serde_json::from_slice(
        &std::fs::read(project.path().join(".stateroot/handoffs/current.json")).unwrap(),
    )
    .unwrap();
    stateroot(config.path(), home.path(), project.path())
        .arg("_drain-finalize")
        .assert()
        .success();
    let current: serde_json::Value = serde_json::from_slice(
        &std::fs::read(project.path().join(".stateroot/handoffs/current.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(current["seq"], authored["seq"]);
    assert_eq!(current["task"], "Authored current task");
    let jobs = journal_files(project.path());
    assert_eq!(jobs[0]["boundary_source_root"], frozen);
    assert!(jobs[0]["snapshot_timing"]
        .as_str()
        .unwrap()
        .contains("post-boundary edits"));
    assert_eq!(jobs[0]["state"], "terminal");
    let history: Vec<serde_json::Value> =
        std::fs::read_dir(project.path().join(".stateroot/handoffs/history"))
            .unwrap()
            .flatten()
            .filter_map(|entry| serde_json::from_slice(&std::fs::read(entry.path()).ok()?).ok())
            .collect();
    let boundary = history
        .iter()
        .find(|packet| packet["boundary_ingest_key"] == jobs[0]["ingest_key"])
        .unwrap();
    assert_eq!(boundary["evidence_ref"]["session_id"], "old");
    assert_eq!(boundary["boundary_source_root"], frozen);
    assert_eq!(boundary["latest_root"], jobs[0]["root"]);
    // Simulate crash after the immutable packet commit but before journal phase commit.
    let mut replay: stateroot_core::finalize_journal::BoundaryJob =
        serde_json::from_value(jobs[0].clone()).unwrap();
    replay.phase = stateroot_core::finalize_journal::Phase::Snapped;
    replay.state = "active".into();
    stateroot_core::finalize_journal::save(project.path(), &replay).unwrap();
    stateroot(config.path(), home.path(), project.path())
        .arg("_drain-finalize")
        .assert()
        .success();
    assert_eq!(
        journal_files(project.path())[0]["handoff_seq"],
        jobs[0]["handoff_seq"]
    );
    let keyed = std::fs::read_dir(project.path().join(".stateroot/handoffs/history"))
        .unwrap()
        .flatten()
        .filter_map(|entry| {
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(entry.path()).ok()?).ok()
        })
        .filter(|packet| packet["boundary_ingest_key"] == jobs[0]["ingest_key"])
        .count();
    assert_eq!(keyed, 1);
    // Same verified event occurrence after completion cannot make a second job/result.
    hook_event(
        config.path(),
        home.path(),
        project.path(),
        "session_end",
        &payload,
    )
    .success();
    assert_eq!(journal_files(project.path()).len(), 1);
}

#[test]
fn recover_validates_relocated_identity_and_advances_only_named_job() {
    let (config, home) = homes();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    let mut chosen = stateroot_core::finalize_journal::enqueue_occurrence(
        project.path(),
        "pi",
        "relocated",
        None,
        "refs/stateroot/latest",
        "chosen",
    )
    .unwrap();
    let untouched = stateroot_core::finalize_journal::enqueue_occurrence(
        project.path(),
        "pi",
        "still-missing",
        None,
        "refs/stateroot/latest",
        "other",
    )
    .unwrap();
    for _ in 0..stateroot_core::finalize_journal::MAX_ATTEMPTS {
        stateroot_core::finalize_journal::mark_error(
            project.path(),
            &mut chosen,
            "original missing evidence",
        )
        .unwrap();
    }
    write_pi_session(
        home.path(),
        project.path(),
        "relocated",
        "relocated verified request",
    );
    let moved = home.path().join("relocated-evidence.jsonl");
    std::fs::rename(
        home.path()
            .join(".pi/agent/sessions/--tmp-demo--/relocated.jsonl"),
        &moved,
    )
    .unwrap();
    stateroot(config.path(), home.path(), project.path())
        .args(["handoff", "recover", "--job", &untouched.id, "--transcript"])
        .arg(&moved)
        .assert()
        .failure();
    stateroot(config.path(), home.path(), project.path())
        .args(["handoff", "recover", "--job", &chosen.id, "--transcript"])
        .arg(&moved)
        .assert()
        .success();
    let all = stateroot_core::finalize_journal::load_all(project.path());
    let completed = all.iter().find(|job| job.id == chosen.id).unwrap();
    assert_eq!(completed.state, "terminal");
    assert_eq!(completed.recoveries.len(), 1);
    assert_eq!(
        completed.recoveries[0]["last_error"],
        "original missing evidence"
    );
    let other = all.iter().find(|job| job.id == untouched.id).unwrap();
    assert_eq!(other.phase, untouched.phase);
    assert_eq!(other.attempt, untouched.attempt);
    assert!(other.root.is_none());
}

#[test]
fn boundary_freezes_one_session_frontier_and_acknowledges_only_that_prefix() {
    let (config, home) = homes();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    write_pi_session(
        home.path(),
        project.path(),
        "bound",
        "native boundary request",
    );
    let payload = |session: &str, text: &str| {
        serde_json::json!({"cwd":project.path(),"session_id":session,"prompt":text}).to_string()
    };
    hook_event(
        config.path(),
        home.path(),
        project.path(),
        "user_prompt_submit",
        &payload("bound", "captured before boundary"),
    )
    .success();
    hook_event(
        config.path(),
        home.path(),
        project.path(),
        "session_end",
        &serde_json::json!({"cwd":project.path(),"session_id":"bound","event_id":"end-bound"})
            .to_string(),
    )
    .success();
    let jobs = journal_files(project.path());
    assert_eq!(jobs[0]["capture_watermark"]["status"], "present");
    let frozen = jobs[0]["capture_watermark"]["watermark"].clone();
    hook_event(
        config.path(),
        home.path(),
        project.path(),
        "user_prompt_submit",
        &payload("other", "other session capture"),
    )
    .success();
    hook_event(
        config.path(),
        home.path(),
        project.path(),
        "user_prompt_submit",
        &payload("bound", "captured after boundary"),
    )
    .success();
    stateroot(config.path(), home.path(), project.path())
        .arg("_drain-finalize")
        .assert()
        .success();
    let completed = journal_files(project.path());
    assert_eq!(completed[0]["state"], "terminal");
    let key = completed[0]["ingest_key"].as_str().unwrap();
    let receipt: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            project
                .path()
                .join(".stateroot/spool/acknowledgements")
                .join(format!("{key}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["watermark"], frozen);
    let segments = stateroot_core::observations::list_segments(project.path());
    assert!(
        segments.iter().all(|segment| !segment.sealed),
        "later and other-session captures remain unacknowledged"
    );
    assert!(stateroot_core::observations::load_spool(project.path())
        .iter()
        .any(|record| record.text.contains("captured after boundary")));
}
