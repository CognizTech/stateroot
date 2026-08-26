//! `stateroot delegate` tests — async-only contract: spawn records `running`
//! and exits; the detached worker finalizes; list/status/digest observe.
//! Hermetic homes plus a fake harness CLI on PATH (mirrors the init_seed
//! auto-backend fixture; zero real harnesses, zero network).

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

/// Temp `bin/` holding an executable fake `claude`; returns (dir, PATH) so the
/// caller can scope the fixture's lifetime and prepend it to PATH.
#[cfg(unix)]
fn fake_claude(body: &str) -> (tempfile::TempDir, String) {
    let bin = tempfile::tempdir().expect("bin");
    let fake = bin.path().join("claude");
    std::fs::write(&fake, body).expect("fake harness");
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").expect("PATH")
    );
    (bin, path)
}

fn delegations(project: &Path) -> std::path::PathBuf {
    project.join(".stateroot/delegations")
}

#[cfg(unix)] // all call sites are unix-gated fixture tests (windows clippy: dead code)
fn read_records(project: &Path) -> Vec<serde_json::Value> {
    let dir = delegations(project);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok())
        .collect()
}

/// Poll the store until one record carries a final outcome (the worker is
/// detached — completion is observed, never blocked on in the CLI itself).
#[cfg(unix)] // all call sites are unix-gated fixture tests (windows clippy: dead code)
fn wait_for_outcome(project: &Path, secs: u64) -> serde_json::Value {
    for _ in 0..(secs * 10) {
        if let Some(record) = read_records(project)
            .into_iter()
            .find(|r| r.get("outcome").is_some())
        {
            return record;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("delegation did not complete within {secs}s");
}

#[cfg(unix)]
#[test]
fn spawn_returns_immediately_and_worker_completes() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let (_bin, path) = fake_claude("#!/bin/sh\nsleep 2\necho 'conclusion: parser wired'\n");

    // The spawn path exits 0 immediately with a running record.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args(["delegate", "--to", "claude", "--task", "slow build"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("running in background (pid "),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("observe: `stateroot delegate status"),
        "stdout: {stdout}"
    );

    // The record the parent wrote before exiting: running, with a pid.
    let records = read_records(project.path());
    assert_eq!(records.len(), 1, "records: {records:?}");
    let record = &records[0];
    assert_eq!(record["status"], "running");
    assert!(record["pid"].as_u64().expect("pid") > 0);
    let id = record["id"].as_str().expect("id").to_string();
    let log_rel = record["log"].as_str().expect("log").to_string();

    // The worker finalizes: outcome, exit code, log body, episodic lineage.
    let record = wait_for_outcome(project.path(), 20);
    assert_eq!(record["outcome"], "completed");
    assert_eq!(record["exit_code"], 0);
    assert!(record.get("status").is_none(), "status replaced by outcome");
    let log = std::fs::read_to_string(project.path().join(&log_rel)).expect("log");
    assert!(log.contains("conclusion: parser wired"), "log: {log}");
    assert!(log.contains("--- stdout ---"), "log: {log}");
    let episodic =
        std::fs::read_to_string(project.path().join(".stateroot/memories/episodic.jsonl"))
            .expect("episodic");
    assert!(
        episodic.contains("delegated to claude: slow build → completed"),
        "episodic: {episodic}"
    );

    // status <id> shows the record + the tail; list shows the outcome.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["delegate", "status", &id])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("· completed"), "stdout: {stdout}");
    assert!(
        stdout.contains("conclusion: parser wired"),
        "stdout: {stdout}"
    );
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["delegate", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("claude · completed · slow build"),
        "stdout: {stdout}"
    );

    // Completions surface in the digest.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("## Recent Delegations"), "digest: {stdout}");
    assert!(
        stdout.contains("claude · completed · slow build"),
        "digest: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn status_shows_a_bounded_log_tail() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let (_bin, path) = fake_claude("#!/bin/sh\nhead -c 20480 /dev/zero | tr '\\0' 'x'\n");

    stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args(["delegate", "--to", "claude", "--task", "flood"])
        .assert()
        .success();
    let record = wait_for_outcome(project.path(), 20);
    assert_eq!(record["outcome"], "completed");
    let id = record["id"].as_str().expect("id");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["delegate", "status", id])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    let full_run = "x".repeat(20_000);
    assert!(
        !stdout.contains(&full_run),
        "the status tail is bounded, never the full flood"
    );
    assert!(
        stdout.contains(&"x".repeat(500)),
        "the tail still shows the end of the flood"
    );
    let log_rel = record["log"].as_str().expect("log");
    let log_len = std::fs::read_to_string(project.path().join(log_rel))
        .expect("log")
        .len();
    assert!(
        stdout.len() < log_len,
        "status output ({}) is smaller than the full log ({log_len})",
        stdout.len()
    );
}

#[cfg(unix)]
#[test]
fn failed_children_record_failed_and_the_spawn_still_exits_zero() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let (_bin, path) =
        fake_claude("#!/bin/sh\necho 'partial answer'\necho 'boom went wrong' >&2\nexit 3\n");

    // Async contract: the SPAWN exits 0 even when the child will fail.
    stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args(["delegate", "--to", "claude", "--task", "fail on purpose"])
        .assert()
        .success();
    let record = wait_for_outcome(project.path(), 20);
    assert_eq!(record["outcome"], "failed");
    assert_eq!(record["exit_code"], 3);

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["delegate", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("· failed ·"), "stdout: {stdout}");
    let id = record["id"].as_str().expect("id");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["delegate", "status", id])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("boom went wrong"),
        "stderr in tail: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn list_marks_a_dead_worker_lost() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // A running record whose pid cannot be alive (worker died pre-outcome).
    let dir = delegations(project.path());
    std::fs::create_dir_all(&dir).expect("mkdir");
    let record = serde_json::json!({
        "schema_version": "stateroot.delegation.v1",
        "id": "2026-08-26T00-00-00Z-claude",
        "ts": "2026-08-26T00:00:00Z",
        "depth": 0,
        "harness": "claude",
        "task": "never finishes",
        "command": "claude",
        "status": "running",
        "pid": 4_000_000u32,
        "log": ".stateroot/delegations/2026-08-26T00-00-00Z-claude-d0.log",
    });
    std::fs::write(
        dir.join("2026-08-26T00-00-00Z-claude.json"),
        serde_json::to_string_pretty(&record).expect("json"),
    )
    .expect("record");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["delegate", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("· lost ·"), "stdout: {stdout}");
    // Reaped on disk — never a silent running-forever.
    let records = read_records(project.path());
    assert_eq!(records[0]["outcome"], "lost");
    assert!(records[0].get("status").is_none());
}

#[cfg(unix)]
#[test]
fn delegate_refuses_past_the_depth_cap_without_spawning() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let marker = project.path().join("spawned.marker");
    let (_bin, path) = fake_claude("#!/bin/sh\ntouch \"$MARKER\"\necho spawned\n");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .env("STATEROOT_DELEGATION_DEPTH", "2")
        .env("MARKER", &marker)
        .args(["delegate", "--to", "claude", "--task", "recurse"])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(
        stderr.contains("delegation depth cap reached"),
        "stderr: {stderr}"
    );
    assert!(!marker.exists(), "nothing may spawn past the cap");
    assert!(
        !delegations(project.path()).exists(),
        "a refused delegation writes no records"
    );
}

#[test]
fn delegate_rejects_non_cli_and_unknown_harnesses() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // cursor is handoff_only — a clear error, not a fake launch.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["delegate", "--to", "cursor", "--task", "draw the owl"])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(
        stderr.contains("harness 'cursor' has no CLI delegation"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("cli-mode harnesses:"), "stderr: {stderr}");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["delegate", "--to", "bogus", "--task", "draw the owl"])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(
        stderr.contains("unknown harness 'bogus'"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("cli-mode harnesses:"), "stderr: {stderr}");
    assert!(
        !delegations(project.path()).exists(),
        "refusals write no records"
    );
}

#[test]
fn digest_section_stays_absent_without_delegations() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        !stdout.contains("## Recent Delegations"),
        "no section when the store is empty: {stdout}"
    );

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["delegate", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("no delegations recorded"),
        "stdout: {stdout}"
    );
}
