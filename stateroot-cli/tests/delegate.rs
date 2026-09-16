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
        .env("STATEROOT_NO_AUTO_UPDATE", "1")
        .env("STATEROOT_DISABLE_SCHEDULED_UPDATE", "1")
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
    // The worker waits on a sentinel file instead of sleeping: the "running"
    // observation window below is exactly as long as the test needs — no
    // sleep to out-race on a loaded WSL/DrvFs host. (Worker cwd = project.)
    let (_bin, path) = fake_claude(
        "#!/bin/sh\nwhile [ ! -f .stateroot-delegate-test-go ]; do sleep 0.2; done\necho 'conclusion: parser wired'\n",
    );

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

    // Release the worker, then it finalizes: outcome, exit code, log body,
    // episodic lineage. The 60s window only pays out on failure.
    std::fs::write(project.path().join(".stateroot-delegate-test-go"), b"go").expect("sentinel");
    let record = wait_for_outcome(project.path(), 60);
    assert_eq!(record["outcome"], "completed");
    assert_eq!(record["exit_code"], 0);
    assert!(record.get("status").is_none(), "status replaced by outcome");
    // The worker publishes its outcome before appending episodic lineage.
    // Observe that final write before inspecting all of the worker's artifacts.
    let episodic_path = project.path().join(".stateroot/memories/episodic.jsonl");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let episodic = loop {
        let contents = std::fs::read_to_string(&episodic_path).expect("episodic");
        if contents.contains("delegated to claude: slow build → completed")
            || std::time::Instant::now() >= deadline
        {
            break contents;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert!(
        episodic.contains("delegated to claude: slow build → completed"),
        "episodic: {episodic}"
    );
    let log = std::fs::read_to_string(project.path().join(&log_rel)).expect("log");
    assert!(log.contains("conclusion: parser wired"), "log: {log}");
    assert!(log.contains("--- stdout ---"), "log: {log}");

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

#[cfg(unix)]
#[test]
fn delegate_to_copilot_spawns_and_completes() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    // Fake `copilot` CLI on PATH, echoing its argv so the template is proven.
    let bin = tempfile::tempdir().expect("bin");
    let fake = bin.path().join("copilot");
    std::fs::write(
        &fake,
        "#!/bin/sh\nprintf '%s\\n' \"copilot argv: $*\"\necho 'copilot conclusion'\n",
    )
    .expect("fake copilot");
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").expect("PATH")
    );

    stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args([
            "delegate",
            "--to",
            "copilot",
            "--task",
            "summarize the diff",
        ])
        .assert()
        .success();

    let record = wait_for_outcome(project.path(), 60);
    assert_eq!(record["outcome"], "completed");
    assert_eq!(record["exit_code"], 0);
    let log_rel = record["log"].as_str().expect("log").to_string();
    let log = std::fs::read_to_string(project.path().join(&log_rel)).expect("log");
    // Append-only log fds (open_log_append) keep the worker's late writes
    // from overwriting finalize's sections even under sweep load.
    assert!(log.contains("--allow-all-tools"), "log: {log}");
    assert!(log.contains("--prompt="), "log: {log}");
    assert!(log.contains("copilot conclusion"), "log: {log}");
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

#[cfg(unix)]
#[test]
fn same_key_never_double_spawns() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let (_bin, path) = fake_claude(
        "#!/bin/sh\nwhile [ ! -f .stateroot-delegate-test-go ]; do sleep 0.2; done\necho done\n",
    );

    stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args(["delegate", "--to", "claude", "--task", "t", "--key", "k1"])
        .assert()
        .success();
    // Same key while the worker is alive: re-attach, never a second spawn.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args(["delegate", "--to", "claude", "--task", "t", "--key", "k1"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("already running"), "stdout: {stdout}");
    assert!(stdout.contains("no double-spawn"), "stdout: {stdout}");
    assert_eq!(read_records(project.path()).len(), 1, "one record only");

    // Same key after a terminal outcome is a hard error, not a respawn.
    std::fs::write(project.path().join(".stateroot-delegate-test-go"), b"go").expect("sentinel");
    let record = wait_for_outcome(project.path(), 60);
    assert_eq!(record["outcome"], "completed");
    stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args(["delegate", "--to", "claude", "--task", "t", "--key", "k1"])
        .assert()
        .failure();
}

#[cfg(unix)]
#[test]
fn failed_key_retries_with_same_identity_and_preserves_attempt_history() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let (_bin, path) = fake_claude("#!/bin/sh\necho transient failure >&2\nexit 1\n");

    stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args([
            "delegate", "--to", "claude", "--task", "t", "--key", "retry-k1",
        ])
        .assert()
        .success();
    let first = wait_for_outcome(project.path(), 60);
    assert_eq!(first["outcome"], "failed");

    stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args([
            "delegate", "--to", "claude", "--task", "t", "--key", "retry-k1",
        ])
        .assert()
        .success();
    let retried = wait_for_outcome(project.path(), 60);
    assert_eq!(retried["outcome"], "failed");
    assert_eq!(retried["attempt"], 2);
    assert_eq!(retried["retries"][0]["attempt"], 1);
    assert_eq!(retried["retries"][0]["outcome"], "failed");
    assert_eq!(
        read_records(project.path()).len(),
        1,
        "same key keeps one record"
    );
}

#[cfg(unix)]
#[test]
fn cancel_is_two_phase_and_records_cancelled_with_root() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let (_bin, path) = fake_claude(
        "#!/bin/sh\nwhile [ ! -f .stateroot-delegate-test-go ]; do sleep 0.2; done\necho done\n",
    );
    stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args(["delegate", "--to", "claude", "--task", "t", "--key", "k2"])
        .assert()
        .success();

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["delegate", "cancel", "k2"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("cancelled_with_root"), "stdout: {stdout}");

    let records = read_records(project.path());
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record["outcome"], "cancelled_with_root");
    assert_eq!(record["cancel_confirmed"], true);
    assert!(
        record
            .get("outcome_root")
            .and_then(|v| v.as_str())
            .is_some(),
        "partial capture must land before the terminal record: {record:?}"
    );
    let events: Vec<&str> = record["events"]
        .as_array()
        .expect("events")
        .iter()
        .filter_map(|e| e["event"].as_str())
        .collect();
    assert!(events.contains(&"spawn"), "{events:?}");
    assert!(events.contains(&"cancel-requested"), "{events:?}");
    assert!(events.contains(&"cancel-confirmed"), "{events:?}");

    // Second cancel reports the terminal state, changes nothing.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["delegate", "cancel", "k2"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("already cancelled_with_root"),
        "stdout: {stdout}"
    );
    // Let the orphaned fake harness exit so the tempdir cleans up.
    std::fs::write(project.path().join(".stateroot-delegate-test-go"), b"go").expect("sentinel");
}

#[cfg(unix)]
#[test]
fn worktree_runs_there_and_the_record_stays_with_the_caller() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    // A REAL fork worktree (6B validation requires fork context + record).
    std::fs::write(project.path().join("seed.txt"), "seed\n").expect("seed");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("snap")
        .assert()
        .success();
    let root_line = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    let root_hash = root_line
        .split_whitespace()
        .find(|t| t.len() >= 12 && t.chars().all(|c| c.is_ascii_hexdigit()))
        .expect("root hash in snap output")
        .to_string();
    let worktree = tempfile::tempdir().expect("worktree parent");
    let wt_path = worktree.path().join("checkout");
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["fork", &root_hash, "--worktree", &wt_path.to_string_lossy()])
        .assert()
        .success();
    let (_bin, path) = fake_claude("#!/bin/sh\necho ran-in-$(pwd)\n");

    // A worktree without .stateroot is rejected up front.
    let plain = tempfile::tempdir().expect("plain");
    stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args([
            "delegate",
            "--to",
            "claude",
            "--task",
            "t",
            "--worktree",
            &plain.path().to_string_lossy(),
        ])
        .assert()
        .failure();

    stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args([
            "delegate",
            "--to",
            "claude",
            "--task",
            "t",
            "--worktree",
            &wt_path.to_string_lossy(),
            "--key",
            "k3",
        ])
        .assert()
        .success();
    let record = wait_for_outcome(project.path(), 60);
    assert_eq!(record["outcome"], "completed");
    assert_eq!(
        record["worktree"].as_str().expect("worktree field"),
        wt_path.to_string_lossy().as_ref()
    );
    // The worker ran IN the worktree: the fake harness's pwd is in the log.
    let log = std::fs::read_to_string(
        project
            .path()
            .join(record["log"].as_str().expect("log rel")),
    )
    .expect("log");
    assert!(
        log.contains(&format!("ran-in-{}", wt_path.display())),
        "worker did not run in the worktree: {log}"
    );
    // The record lives with the CALLER, not the worktree.
    assert!(
        read_records(&wt_path).is_empty(),
        "worktree must not own the delegation record"
    );
}

#[cfg(unix)]
fn pid_alive_unix(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(unix)]
#[test]
fn cancel_kills_the_harness_process_tree_not_just_the_worker() {
    // Repair-plan F1 fixture (audit): `delegate cancel` that stops only the
    // worker leaves the actual harness child running orphaned — a false
    // cancellation. The whole process tree must be verified dead.
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let (_bin, path) = fake_claude(
        "#!/bin/sh\necho $$ > .stateroot-grandchild-pid\nwhile [ ! -f .stateroot-delegate-test-go ]; do sleep 0.2; done\necho done\n",
    );
    stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args([
            "delegate", "--to", "claude", "--task", "t", "--key", "k-tree",
        ])
        .assert()
        .success();

    // Wait for the harness grandchild to write its pid.
    let mut grandchild = 0u32;
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(project.path().join(".stateroot-grandchild-pid"))
        {
            if let Ok(pid) = text.trim().parse::<u32>() {
                grandchild = pid;
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(grandchild != 0, "harness never wrote its pid");
    assert!(pid_alive_unix(grandchild), "fixture sanity: child alive");

    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["delegate", "cancel", "k-tree"])
        .assert()
        .success();

    assert!(
        !pid_alive_unix(grandchild),
        "cancel reported success but harness child {grandchild} is still running"
    );

    // Cleanup in case the assertion above failed (leave no orphan behind).
    let _ = std::process::Command::new("kill")
        .args(["-9", &grandchild.to_string()])
        .status();
    std::fs::write(project.path().join(".stateroot-delegate-test-go"), b"go").expect("sentinel");
}
