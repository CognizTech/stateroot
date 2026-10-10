//! WS3 C7 — install/upgrade smoke against the real installer in isolated
//! homes: no pre-existing project, one and multiple harnesses, no harness
//! CLI, missing projections, interrupted update journal. Asserts exact
//! artifact identity (hook commands reference the tested binary) and the
//! typed `install --json` / `doctor --json` contracts.

use std::path::Path;

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

fn homes() -> (TempDir, TempDir, TempDir) {
    (
        tempfile::tempdir().expect("config home"),
        tempfile::tempdir().expect("user home"),
        tempfile::tempdir().expect("cwd"),
    )
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
}

fn doctor_json(config_home: &Path, user_home: &Path, cwd: &Path) -> serde_json::Value {
    let out = stateroot(config_home, user_home, cwd)
        .args(["doctor", "--json"])
        .assert()
        .success();
    serde_json::from_slice(&out.get_output().stdout).expect("doctor json")
}

fn health_row<'a>(doc: &'a serde_json::Value, harness: &str) -> &'a serde_json::Value {
    doc["harnesses"]
        .as_array()
        .expect("harnesses")
        .iter()
        .find(|h| h["harness"] == harness)
        .unwrap_or_else(|| panic!("no health row for {harness}: {doc}"))
}

#[test]
fn install_with_no_harnesses_is_honest_and_empty() {
    let (config_home, user_home, cwd) = homes();
    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .arg("install")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    // The bundled dot skill is seeded on every install and self-detects;
    // it is a configured local skill, not a connected runtime.
    assert!(stdout.contains("Installed for: dot"), "stdout: {stdout}");

    // Machine mode: no harness binary exists anywhere, so NOTHING may be
    // observed-working and every row must show the binary missing. Marker
    // (config-leftover) detection is deliberate product behavior — a row
    // may read configured from markers alone, but never working.
    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .args(["install", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("typed document");
    assert_eq!(doc["schema_version"], "stateroot.integration-health.v1");
    let rows = doc["harnesses"].as_array().expect("rows");
    assert!(
        rows.iter().any(|r| r["harness"] == "dot"),
        "the bundled dot skill row: {rows:?}"
    );
    for row in rows {
        assert_ne!(row["status"], "observed_working", "{row}");
        assert_eq!(row["binary"]["state"], "missing", "{row}");
        assert!(
            row["last_delivery"].is_null() && row["last_capture"].is_null(),
            "{row}"
        );
    }
}

#[test]
fn install_wires_one_harness_and_reports_configured_not_observed() {
    let (config_home, user_home, cwd) = homes();
    std::fs::create_dir_all(user_home.path().join(".cursor")).expect("marker");

    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .arg("install")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("Installed for: cursor"), "stdout: {stdout}");

    // Exact artifact identity: hooks reference THIS binary, canonicalized —
    // never a bare name and never a stale target cache.
    let exe = std::fs::canonicalize(assert_cmd::cargo::cargo_bin("stateroot")).expect("exe");
    let hooks = read_json(&user_home.path().join(".cursor/hooks.json"));
    let command = hooks["hooks"]["sessionStart"][0]["command"]
        .as_str()
        .expect("hook command");
    assert!(
        command.contains(&format!(
            "{} hook session_start --harness cursor",
            exe.display()
        )),
        "hook command references the tested binary: {command}"
    );
    // MCP + instruction block + skill entrypoint.
    // The MCP entry is the stdio bridge on PATH (bare `stateroot`) — the
    // hook configs are what pin the exact tested binary.
    let mcp = read_json(&user_home.path().join(".cursor/mcp.json"));
    assert_eq!(mcp["mcpServers"]["stateroot"]["command"], "stateroot");
    assert_eq!(mcp["mcpServers"]["stateroot"]["args"][0], "mcp-stdio");
    let block = std::fs::read_to_string(user_home.path().join(".cursor/AGENTS.md")).expect("block");
    assert!(block.contains("<!-- stateroot:begin -->"));
    assert!(user_home
        .path()
        .join(".agents/skills/stateroot/SKILL.md")
        .is_file());

    // Typed truth: configured, never observed-working before any session.
    let doc = doctor_json(config_home.path(), user_home.path(), cwd.path());
    let row = health_row(&doc["integrations"], "cursor");
    assert_eq!(row["status"], "configured");
    assert_eq!(row["hooks"]["state"], "ok");
    assert_eq!(row["mcp"]["state"], "ok");
    assert_eq!(row["instructions"]["state"], "ok");
    assert_eq!(row["skills"]["state"], "ok");
    assert!(row["last_capture"].is_null());
}

#[test]
fn install_wires_multiple_harnesses() {
    let (config_home, user_home, cwd) = homes();
    std::fs::create_dir_all(user_home.path().join(".cursor")).expect("cursor marker");
    std::fs::create_dir_all(user_home.path().join(".kimi-code")).expect("kimi marker");

    stateroot(config_home.path(), user_home.path(), cwd.path())
        .arg("install")
        .assert()
        .success();

    let kimi_toml_path = user_home.path().join(".kimi-code/config.toml");
    let kimi_toml = std::fs::read_to_string(&kimi_toml_path).expect("kimi config.toml");
    // Parse the generated TOML with the production health parser — the
    // command on native Windows is an escaped, absolute stateroot.exe path,
    // so a bare-substring search is wrong; the decoded command is what the
    // harness actually runs.
    let commands = stateroot_core::harness_install::health::extract_hook_commands(
        &kimi_toml_path,
        stateroot_core::harness_install::registry::HookFormat::TomlHooks,
    );
    assert!(
        !commands.is_empty(),
        "kimi toml registers stateroot hooks: {kimi_toml}"
    );
    let session_start = commands
        .iter()
        .find(|command| command.contains("hook session_start"))
        .unwrap_or_else(|| panic!("a session_start hook: {commands:?}"));
    assert!(
        session_start.ends_with("--harness kimi-code"),
        "the session_start hook binds kimi-code: {session_start}"
    );
    // The binary must BE the tested binary — Path identity, never a
    // substring of it.
    let binary = stateroot_core::harness_install::health::binary_of_command(session_start)
        .expect("hook binary");
    let exe = std::fs::canonicalize(assert_cmd::cargo::cargo_bin("stateroot")).expect("exe");
    let hook_exe = std::fs::canonicalize(Path::new(&binary)).expect("hook binary resolves on disk");
    assert_eq!(
        hook_exe,
        exe,
        "hook binary is the tested binary: {binary} vs {}",
        exe.display()
    );
    let doc = doctor_json(config_home.path(), user_home.path(), cwd.path());
    for id in ["cursor", "kimi-code"] {
        let row = health_row(&doc["integrations"], id);
        assert_eq!(row["status"], "configured", "{id}: {row}");
        assert_eq!(row["hooks"]["state"], "ok", "{id}: {row}");
    }
}

#[test]
fn missing_projection_is_specific_and_navigable() {
    let (config_home, user_home, cwd) = homes();
    std::fs::create_dir_all(user_home.path().join(".cursor")).expect("marker");
    stateroot(config_home.path(), user_home.path(), cwd.path())
        .arg("install")
        .assert()
        .success();

    // The skill projection vanishes (user cleanup, sync conflict, …) —
    // health must name the component and the repair.
    std::fs::remove_dir_all(user_home.path().join(".agents/skills/stateroot")).expect("rm skill");
    let doc = doctor_json(config_home.path(), user_home.path(), cwd.path());
    let row = health_row(&doc["integrations"], "cursor");
    assert_eq!(row["skills"]["state"], "missing", "{row}");
    assert_eq!(row["status"], "missing", "{row}");
    assert!(
        row["problems"]
            .as_array()
            .expect("problems")
            .iter()
            .any(|p| p.as_str().unwrap_or("").contains("skills")),
        "{row}"
    );
    assert!(
        row["repair"]
            .as_array()
            .expect("repair")
            .iter()
            .any(|r| r.as_str() == Some("stateroot install")),
        "{row}"
    );
}

#[test]
fn interrupted_update_journal_surfaces_with_repair() {
    let (config_home, user_home, cwd) = homes();
    std::fs::write(
        config_home.path().join("update-journal.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "from_version": "0.2.5",
            "to_version": "v0.2.6",
            "started_at": "2026-10-09T01:02:03Z",
            "status": "in_progress"
        }))
        .expect("json"),
    )
    .expect("journal");

    let doc = doctor_json(config_home.path(), user_home.path(), cwd.path());
    assert_eq!(doc["schema_version"], "stateroot.doctor.v1");
    let check = doc["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|c| c["label"] == "self-update")
        .expect("self-update check");
    assert_eq!(check["ok"], false);
    assert_eq!(check["repair"], "stateroot self-update");

    // Human output keeps working and names the repair.
    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .arg("doctor")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("update interrupted"), "stdout: {stdout}");
    assert!(stdout.contains("stateroot self-update"), "stdout: {stdout}");
}

#[test]
fn install_json_keeps_stdout_clean_and_reports_on_stderr() {
    let (config_home, user_home, cwd) = homes();
    std::fs::create_dir_all(user_home.path().join(".cursor")).expect("marker");
    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .args(["install", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is pure JSON");
    assert_eq!(doc["schema_version"], "stateroot.integration-health.v1");
    let row = health_row(&doc, "cursor");
    assert_eq!(row["status"], "configured");
    // Human progress moved to stderr.
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(stderr.contains("Installed for: cursor"), "stderr: {stderr}");
}

/// Telemetry spool observer (dev builds emit nothing unless forced).
fn telemetry_events(config_home: &Path) -> Vec<serde_json::Value> {
    let spool = config_home.join("local/telemetry/spool");
    let Ok(entries) = std::fs::read_dir(&spool) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str(&t).ok())
        .collect()
}

fn install_forced(
    config_home: &Path,
    user_home: &Path,
    cwd: &Path,
    args: &[&str],
) -> assert_cmd::Command {
    let mut cmd = stateroot(config_home, user_home, cwd);
    cmd.env("STATEROOT_TELEMETRY_FORCE", "1").args(args);
    cmd
}

#[test]
fn integration_telemetry_is_success_only_and_the_document_carries_the_outcome() {
    let (config_home, user_home, cwd) = homes();
    std::fs::create_dir_all(user_home.path().join(".cursor")).expect("cursor marker");
    // Break cursor's hook registration: hooks.json as a DIRECTORY makes the
    // write fail — a partial integration.
    std::fs::create_dir_all(user_home.path().join(".cursor/hooks.json")).expect("dir hooks.json");

    // Human mode: the partial install bails; machine mode: succeeds as a
    // command but the typed document carries the explicit failure.
    install_forced(
        config_home.path(),
        user_home.path(),
        cwd.path(),
        &["install"],
    )
    .assert()
    .failure();
    let out = install_forced(
        config_home.path(),
        user_home.path(),
        cwd.path(),
        &["install", "--json"],
    )
    .assert()
    .success();
    let doc: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("typed document");
    assert!(
        doc["install"]["failed"]
            .as_array()
            .expect("failed")
            .iter()
            .any(|h| h == "cursor"),
        "the partial failure is explicit in the document: {doc}"
    );
    assert_eq!(doc["install"]["cli_only"], false);

    // Success-only telemetry: NO integration_completed event may exist for
    // the failed passes.
    let events = telemetry_events(config_home.path());
    assert!(
        events.iter().all(|e| e["event"] != "integration_completed"),
        "a partial install never emits success telemetry: {events:?}"
    );

    // Positive control: repair the fixture, install again — exactly one
    // success event.
    std::fs::remove_dir_all(user_home.path().join(".cursor/hooks.json")).expect("unbreak");
    install_forced(
        config_home.path(),
        user_home.path(),
        cwd.path(),
        &["install"],
    )
    .assert()
    .success();
    let events = telemetry_events(config_home.path());
    assert_eq!(
        events
            .iter()
            .filter(|e| e["event"] == "integration_completed")
            .count(),
        1,
        "exactly one success event after the repaired full install: {events:?}"
    );
    // …and the typed document now records a clean outcome.
    let out = install_forced(
        config_home.path(),
        user_home.path(),
        cwd.path(),
        &["install", "--json"],
    )
    .assert()
    .success();
    let doc: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("typed document");
    assert_eq!(
        doc["install"]["failed"].as_array().expect("failed").len(),
        0
    );
    assert!(
        doc["install"]["configured"]
            .as_array()
            .expect("configured")
            .iter()
            .any(|h| h == "cursor"),
        "{doc}"
    );
}

#[test]
fn no_agent_machine_is_an_explicit_cli_only_outcome() {
    let (config_home, user_home, cwd) = homes();
    let out = install_forced(
        config_home.path(),
        user_home.path(),
        cwd.path(),
        &["install", "--json"],
    )
    .assert()
    .success();
    let doc: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("typed document");
    assert_eq!(doc["install"]["cli_only"], true, "{doc}");
    assert_eq!(
        doc["install"]["failed"].as_array().expect("failed").len(),
        0
    );
    // CLI-only is NOT agent readiness: no integration_completed event.
    let events = telemetry_events(config_home.path());
    assert!(
        events.iter().all(|e| e["event"] != "integration_completed"),
        "a no-agent install is not an integration success: {events:?}"
    );
}
