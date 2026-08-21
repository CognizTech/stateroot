//! `stateroot delegate` tests — temp home/project fixtures plus a fake
//! harness CLI on PATH (mirrors the init_seed auto-backend fixture; zero real
//! harnesses, zero network).

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

fn read_record(project: &Path) -> serde_json::Value {
    let dir = delegations(project);
    let entry = std::fs::read_dir(&dir)
        .expect("delegations dir")
        .flatten()
        .find(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .expect("delegation record");
    serde_json::from_str(&std::fs::read_to_string(entry.path()).expect("record")).expect("json")
}

#[cfg(unix)]
#[test]
fn delegate_happy_path_returns_conclusion_and_persists_lineage() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let (_bin, path) = fake_claude(
        "#!/bin/sh\necho \"depth=$STATEROOT_DELEGATION_DEPTH\"\necho 'conclusion: parser wired'\n",
    );

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args(["delegate", "--to", "claude", "--task", "wire the parser"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("delegated to claude · exit 0"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("conclusion: parser wired"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("full log: .stateroot/delegations/"),
        "stdout: {stdout}"
    );

    // Full log exists and shows the child ran at depth + 1.
    let log_name = stdout
        .split("full log: .stateroot/delegations/")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("log name in header");
    let log = std::fs::read_to_string(delegations(project.path()).join(log_name)).expect("log");
    assert!(log.contains("depth=1"), "log: {log}");
    assert!(log.contains("--- stderr ---"), "log: {log}");

    // stateroot.delegation.v1 record.
    let record = read_record(project.path());
    assert_eq!(record["schema_version"], "stateroot.delegation.v1");
    assert_eq!(record["harness"], "claude");
    assert_eq!(record["task"], "wire the parser");
    assert_eq!(record["command"], "claude");
    assert_eq!(record["depth"], 0);
    assert_eq!(record["exit_code"], 0);
    assert_eq!(record["outcome"], "completed");
    assert!(record["log"]
        .as_str()
        .expect("log path")
        .ends_with("-d0.log"));

    // Episodic lineage.
    let episodic =
        std::fs::read_to_string(project.path().join(".stateroot/memories/episodic.jsonl"))
            .expect("episodic");
    assert!(
        episodic.contains("delegated to claude: wire the parser → completed"),
        "episodic: {episodic}"
    );

    // --json emits the record plus the bounded tail.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args([
            "delegate",
            "--to",
            "claude",
            "--task",
            "wire the parser",
            "--json",
        ])
        .assert()
        .success();
    let envelope: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("json envelope");
    assert_eq!(envelope["schema_version"], "stateroot.delegation.v1");
    assert!(
        envelope["stdout_tail"]
            .as_str()
            .expect("stdout_tail")
            .contains("conclusion: parser wired"),
        "envelope: {envelope}"
    );
}

#[cfg(unix)]
#[test]
fn delegate_output_is_bounded_to_the_tail() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let (_bin, path) = fake_claude("#!/bin/sh\nhead -c 20480 /dev/zero | tr '\\0' 'x'\n");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args([
            "delegate",
            "--to",
            "claude",
            "--task",
            "flood",
            "--max-output-chars",
            "100",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("full log:"), "stdout: {stdout}");
    let expected: String = "x".repeat(100);
    assert_eq!(
        stdout.lines().last(),
        Some(expected.as_str()),
        "caller gets only the 100-char tail: {stdout}"
    );
    // The log keeps the full flood.
    let log_name = stdout
        .split("full log: .stateroot/delegations/")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("log name");
    let log = std::fs::read_to_string(delegations(project.path()).join(log_name)).expect("log");
    assert!(log.contains(&"x".repeat(20_000)), "log keeps everything");
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
fn delegate_child_failure_exits_nonzero_with_stderr_tail() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let (_bin, path) =
        fake_claude("#!/bin/sh\necho 'partial answer'\necho 'boom went wrong' >&2\nexit 3\n");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args(["delegate", "--to", "claude", "--task", "fail on purpose"])
        .assert()
        .code(3);
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("delegated to claude · exit 3"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("partial answer"), "stdout: {stdout}");
    assert!(stdout.contains("boom went wrong"), "stdout: {stdout}");

    let record = read_record(project.path());
    assert_eq!(record["outcome"], "failed");
    assert_eq!(record["exit_code"], 3);
}

#[cfg(unix)]
#[test]
fn delegate_failure_with_empty_stdout_still_surfaces_stderr() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let (_bin, path) = fake_claude("#!/bin/sh\necho 'silent boom' >&2\nexit 4\n");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args(["delegate", "--to", "claude", "--task", "fail quietly"])
        .assert()
        .code(4);
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("delegated to claude · exit 4"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("silent boom"), "stdout: {stdout}");
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(
        !stderr.contains("returned empty stdout"),
        "failed runs take the stderr-tail path, not the empty-stdout error: {stderr}"
    );

    let record = read_record(project.path());
    assert_eq!(record["outcome"], "failed");
    assert_eq!(record["exit_code"], 4);
}

#[cfg(unix)]
#[test]
fn delegate_timeout_kills_the_child_and_records_timed_out() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let (_bin, path) = fake_claude("#!/bin/sh\nsleep 5\necho late\n");

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .env("PATH", &path)
        .args([
            "delegate",
            "--to",
            "claude",
            "--task",
            "hang forever",
            "--timeout-secs",
            "1",
        ])
        .assert()
        .failure();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("timed out after 1s"), "stdout: {stdout}");

    let record = read_record(project.path());
    assert_eq!(record["outcome"], "timed_out");
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
