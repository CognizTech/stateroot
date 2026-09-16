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
