//! `stateroot obligation` lifecycle: creation, idempotent retry, UTC and
//! relative deadlines, overdue surfacing, snooze, completion, cancellation,
//! assignment, corrupt-event preservation, concurrent appends.

use std::path::Path;
use std::process::Command;

use assert_cmd::assert::Assert;
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

fn stdout_of(out: Assert) -> String {
    String::from_utf8(out.get_output().stdout.clone()).expect("utf8")
}

fn obligations_dir(project: &Path) -> std::path::PathBuf {
    project.join(".stateroot").join("obligations")
}

/// Extract the first obligation id from a definition file.
fn only_obligation_id(project: &Path) -> String {
    let entries: Vec<_> = std::fs::read_dir(obligations_dir(project))
        .expect("obligations dir")
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one obligation definition"
    );
    let text = std::fs::read_to_string(entries[0].path()).expect("read definition");
    let value: serde_json::Value = serde_json::from_str(&text).expect("definition json");
    value["id"].as_str().expect("id").to_string()
}

#[test]
fn obligation_add_due_and_relative_and_idempotent_retry() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // RFC3339 due is normalized to UTC.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "obligation",
            "add",
            "--task",
            "Review the campaign at +24h",
            "--due",
            "2026-10-02T12:00:00+02:00",
        ])
        .assert()
        .success();
    assert!(stdout_of(out).contains("due 2026-10-02T10:00:00Z"));

    // Relative due lands ~24h out (UTC).
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "obligation",
            "add",
            "--task",
            "Weekly metrics review",
            "--in",
            "24h",
        ])
        .assert()
        .success();
    assert!(stdout_of(out).contains("due 20"));

    // Idempotent retry: the same operation id returns the same obligation.
    let first = stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "obligation",
            "add",
            "--task",
            "Idempotent me",
            "--operation-id",
            "op-123",
        ])
        .assert()
        .success();
    let second = stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "obligation",
            "add",
            "--task",
            "Idempotent me",
            "--operation-id",
            "op-123",
        ])
        .assert()
        .success();
    assert!(stdout_of(second).contains("already recorded"));
    let _ = first;
    let count = std::fs::read_dir(obligations_dir(project.path()))
        .expect("dir")
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .count();
    assert_eq!(count, 3, "retry must not duplicate (3 real adds, no 4th)");

    // Bad inputs are usage errors.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["obligation", "add", "--task", "x", "--in", "7x"])
        .assert()
        .failure();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["obligation", "add", "--task", "x", "--due", "not-a-date"])
        .assert()
        .failure();
}

#[test]
fn obligation_overdue_snooze_done_cancel_flow() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "obligation",
            "add",
            "--task",
            "Past-due review",
            "--due",
            "2020-01-01T00:00:00Z",
            "--assign",
            "kimi",
        ])
        .assert()
        .success();
    let id = only_obligation_id(project.path());

    // Overdue surfaces in list with the assignment.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["obligation", "list"])
        .assert()
        .success();
    let list = stdout_of(out);
    assert!(list.contains("due"), "list: {list}");
    assert!(list.contains("Past-due review"), "list: {list}");
    assert!(list.contains("kimi"), "list: {list}");

    // Snooze suppresses the due surfacing.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "obligation",
            "snooze",
            &id,
            "--until",
            "2999-01-01T00:00:00Z",
        ])
        .assert()
        .success();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["obligation", "list"])
        .assert()
        .success();
    let list = stdout_of(out);
    assert!(list.contains("snoozed"), "list: {list}");
    assert!(!list.contains("due  2020"), "list: {list}");

    // Done requires evidence; the receipt shows it.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["obligation", "done", &id, "--evidence", ""])
        .assert()
        .failure();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "obligation",
            "done",
            &id,
            "--evidence",
            "report published to the wiki",
        ])
        .assert()
        .success();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["obligation", "show", &id])
        .assert()
        .success();
    let show = stdout_of(out);
    assert!(show.contains("state:     done"), "show: {show}");
    assert!(
        show.contains("evidence:  report published to the wiki"),
        "show: {show}"
    );
    // Terminal obligations are hidden without --all.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["obligation", "list"])
        .assert()
        .success();
    assert!(stdout_of(out).contains("no obligations"));
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["obligation", "list", "--all"])
        .assert()
        .success();
    assert!(stdout_of(out).contains("done"));

    // Re-completing a terminal obligation is an error.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["obligation", "done", &id, "--evidence", "again"])
        .assert()
        .failure();

    // Cancel records the reason.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["obligation", "add", "--task", "Drop me"])
        .assert()
        .success();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["obligation", "list"])
        .assert()
        .success();
    let list = stdout_of(out);
    assert!(list.contains("Drop me"), "list: {list}");
    let cancel_id = {
        let entries: Vec<_> = std::fs::read_dir(obligations_dir(project.path()))
            .expect("dir")
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .collect();
        let mut found = String::new();
        for entry in entries {
            let text = std::fs::read_to_string(entry.path()).expect("read");
            let value: serde_json::Value = serde_json::from_str(&text).expect("json");
            if value["task"].as_str() == Some("Drop me") {
                found = value["id"].as_str().expect("id").to_string();
            }
        }
        assert!(!found.is_empty());
        found
    };
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["obligation", "cancel", &cancel_id, "--reason", "superseded"])
        .assert()
        .success();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["obligation", "show", &cancel_id])
        .assert()
        .success();
    let show = stdout_of(out);
    assert!(show.contains("state:     cancelled"), "show: {show}");
    assert!(show.contains("superseded"), "show: {show}");
}

#[test]
fn obligation_mutation_idempotency_and_corrupt_events_preserved() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["obligation", "add", "--task", "Mutate me once"])
        .assert()
        .success();
    let id = only_obligation_id(project.path());

    // Same mutation operation id twice: the second is a no-op, not a
    // duplicate event and not an error.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "obligation",
            "done",
            &id,
            "--evidence",
            "proved",
            "--operation-id",
            "mut-1",
        ])
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "obligation",
            "done",
            &id,
            "--evidence",
            "proved",
            "--operation-id",
            "mut-1",
        ])
        .assert()
        .success();
    let events_path = obligations_dir(project.path()).join("events.jsonl");
    let events = std::fs::read_to_string(&events_path).expect("events");
    let done_events = events
        .lines()
        .filter(|l| l.contains("\"kind\":\"done\""))
        .count();
    assert_eq!(done_events, 1, "events: {events}");

    // A corrupt line is preserved on disk and counted, never deleted.
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&events_path)
        .expect("open events");
    writeln!(file, "{{not valid json").expect("write corrupt");
    drop(file);
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["obligation", "list", "--json"])
        .assert()
        .success();
    let payload: serde_json::Value = serde_json::from_str(&stdout_of(out)).expect("json payload");
    assert_eq!(payload["corrupt_events"], 1);
    assert_eq!(payload["schema_version"], "stateroot.obligations.v1");
    let after = std::fs::read_to_string(&events_path).expect("events after");
    assert!(
        after.contains("{not valid json"),
        "corrupt line must be preserved: {after}"
    );
}

#[test]
fn obligation_concurrent_adds_lose_nothing() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let mut handles = Vec::new();
    for n in 0..6 {
        let config = config_home.path().to_path_buf();
        let user = user_home.path().to_path_buf();
        let proj = project.path().to_path_buf();
        handles.push(std::thread::spawn(move || {
            stateroot(&config, &user, &proj)
                .args([
                    "obligation",
                    "add",
                    "--task",
                    &format!("concurrent task {n}"),
                ])
                .assert()
                .success();
        }));
    }
    for handle in handles {
        handle.join().expect("thread");
    }
    let definitions = std::fs::read_dir(obligations_dir(project.path()))
        .expect("dir")
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .count();
    assert_eq!(definitions, 6);
    let events = std::fs::read_to_string(obligations_dir(project.path()).join("events.jsonl"))
        .expect("events");
    let created = events
        .lines()
        .filter(|l| l.contains("\"kind\":\"created\""))
        .count();
    assert_eq!(created, 6, "events: {events}");
}

#[test]
fn obligation_add_binds_plan_and_projection_updates() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // A due obligation lands in the machine-local continuity projection the
    // mutation-time reconcile writes.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "obligation",
            "add",
            "--task",
            "Projection check",
            "--due",
            "2020-06-01T00:00:00Z",
        ])
        .assert()
        .success();
    let projection_path = project
        .path()
        .join(".stateroot")
        .join("local/projections/continuity.v1.json");
    let text = std::fs::read_to_string(&projection_path).expect("projection written");
    let projection: serde_json::Value = serde_json::from_str(&text).expect("projection json");
    assert_eq!(projection["schema_version"], "stateroot.continuity.v1");
    let kinds: Vec<&str> = projection["attention"]
        .as_array()
        .expect("attention array")
        .iter()
        .filter_map(|item| item["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"obligation_due"), "projection: {kinds:?}");
    // Stable attention ids survive reconciles.
    let ids: Vec<&str> = projection["attention"]
        .as_array()
        .expect("attention")
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert!(ids.iter().any(|id| id.starts_with("obligation_due:")));

    let _ = Command::new("true");
}
