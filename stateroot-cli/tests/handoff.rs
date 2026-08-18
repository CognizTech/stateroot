//! Strict local handoff contract: temp fixtures only, using native readers.

use std::io::Write as _;
use std::path::Path;

use assert_cmd::Command;
use serde_json::{json, Value};

fn stateroot(config: &Path, home: &Path, cwd: &Path) -> Command {
    let mut command = Command::cargo_bin("stateroot").expect("binary");
    command
        .env("STATEROOT_HOME", config)
        .env("STATEROOT_TEST_HOME", home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_BASE")
        .env_remove("HERMES_HOME")
        .current_dir(cwd);
    command
}

fn project() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
    let config = tempfile::tempdir().expect("config");
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    stateroot(config.path(), home.path(), project.path())
        .arg("init")
        .assert()
        .success();
    (config, home, project)
}

fn write_json(project: &Path, name: &str, value: &Value) -> String {
    let path = project.join(name);
    std::fs::write(&path, serde_json::to_vec(value).expect("json")).expect("input");
    path.to_string_lossy().into_owned()
}

fn current(project: &Path) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(project.join(".stateroot/handoffs/current.json"))
            .expect("current handoff"),
    )
    .expect("handoff json")
}

fn history_count(project: &Path) -> usize {
    std::fs::read_dir(project.join(".stateroot/handoffs/history"))
        .expect("history")
        .count()
}

fn history_packets(project: &Path) -> Vec<Value> {
    std::fs::read_dir(project.join(".stateroot/handoffs/history"))
        .expect("history")
        .map(|entry| {
            let path = entry.expect("history entry").path();
            serde_json::from_slice(&std::fs::read(path).expect("history packet"))
                .expect("history json")
        })
        .collect()
}

fn write_rollout(home: &Path, name: &str, events: &[Value]) {
    let dir = home.join(".codex/sessions/2026/08/12");
    std::fs::create_dir_all(&dir).expect("sessions");
    let mut file = std::fs::File::create(dir.join(name)).expect("rollout");
    for event in events {
        writeln!(file, "{event}").expect("event");
    }
}

fn meta(id: &str, cwd: &Path, ts: &str) -> Value {
    json!({"timestamp":ts,"type":"session_meta","payload":{"id":id,"cwd":cwd,"timestamp":ts}})
}

#[test]
fn strict_input_accepts_every_content_field_and_cli_owns_envelope() {
    let (config, home, project) = project();
    let input = json!({
        "task":"Immediate boundary",
        "objective":"Durable goal from JSON",
        "current_phase":"verify",
        "implementation_status":"Implementation is ready for workspace checks",
        "context_summary":"The structured writer is complete and evidence-backed checks remain.",
        "decisions":["Keep envelope ownership local"],
        "changed_files":["src/a.rs"],
        "tests_run":["cargo test -p stateroot-cli"],
        "failures":[],
        "bugs_found":["Known rendering bug"],
        "blockers":["Waiting for fixture"],
        "open_questions":["Whether to add another harness"],
        "next_actions":["Run workspace checks"],
        "warnings":["Observed only"],
        "relevant_memories":["memory-1"],
        "relevant_skills":["skill-1"],
        "artifacts":["artifact-1"],
        "traces":["trace-1"]
    });
    let path = write_json(project.path(), "all-content.json", &input);
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--to",
            "claude",
            "--input",
            &path,
            "--objective",
            "CLI objective wins",
        ])
        .assert()
        .success();

    let packet = current(project.path());
    assert_eq!(packet["objective"], "CLI objective wins");
    assert_eq!(packet["task"], input["task"]);
    for key in [
        "current_phase",
        "implementation_status",
        "context_summary",
        "decisions",
        "changed_files",
        "tests_run",
        "failures",
        "bugs_found",
        "blockers",
        "open_questions",
        "next_actions",
        "relevant_memories",
        "relevant_skills",
        "artifacts",
        "traces",
    ] {
        assert_eq!(packet[key], input[key], "field {key}");
    }
    assert!(packet["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .any(|v| v == "Observed only"));
    assert_eq!(packet["schema_version"], "stateroot.handoff.v1");
    assert_eq!(packet["project_id"], current(project.path())["project_id"]);
    assert_eq!(packet["seq"], 1);
    assert_eq!(packet["last_harness"], "codex");
    assert_eq!(packet["recommended_next_harness"], "claude");
    assert_eq!(packet["created_by_harness"], "codex");
    assert!(packet.get("created_at").is_some() && packet.get("written_at").is_some());

    for (name, extra) in [
        ("unknown", json!({"surprise":true})),
        ("envelope", json!({"schema_version":"evil"})),
        ("provenance", json!({"created_by_harness":"claude"})),
    ] {
        let mut bad = input.clone();
        bad.as_object_mut()
            .expect("object")
            .extend(extra.as_object().expect("extra").clone());
        let bad_path = write_json(project.path(), &format!("{name}.json"), &bad);
        stateroot(config.path(), home.path(), project.path())
            .args([
                "handoff", "write", "--from", "codex", "--to", "codex", "--input", &bad_path,
            ])
            .assert()
            .failure();
    }
}

#[test]
fn successive_handoffs_advance_current_and_preserve_distinct_history() {
    let (config, home, project) = project();
    for (name, task) in [("first.json", "first task"), ("second.json", "second task")] {
        let input = write_json(
            project.path(),
            name,
            &json!({
                "objective":"durable goal",
                "task":task,
                "context_summary":format!("Verified context for {task}."),
                "failures":[]
            }),
        );
        stateroot(config.path(), home.path(), project.path())
            .args([
                "handoff", "write", "--from", "codex", "--to", "codex", "--input", &input,
            ])
            .assert()
            .success();
    }

    let packet = current(project.path());
    assert_eq!(packet["seq"], 2);
    assert_eq!(packet["task"], "second task");

    let mut history = history_packets(project.path());
    history.sort_by_key(|packet| packet["seq"].as_i64().expect("history seq"));
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["seq"], 1);
    assert_eq!(history[0]["task"], "first task");
    assert_eq!(history[1]["seq"], 2);
    assert_eq!(history[1]["task"], "second task");
    assert_ne!(history[0], history[1]);
}

#[test]
fn latest_matching_native_session_enriches_without_other_harness_or_invention() {
    let (config, home, project) = project();
    write_rollout(
        home.path(),
        "rollout-old.jsonl",
        &[
            meta("old", project.path(), "2026-08-12T08:00:00Z"),
            json!({"timestamp":"2026-08-12T08:01:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"old task must not win"}]}}),
            json!({"timestamp":"2026-08-12T08:02:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"old answer"}]}}),
        ],
    );
    let mut events = vec![meta("latest", project.path(), "2026-08-12T10:00:00Z")];
    for index in 1..=3 {
        events.push(json!({"timestamp":format!("2026-08-12T10:0{index}:00Z"),"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":format!("real user prompt {index}")}]}}));
        events.push(json!({"timestamp":format!("2026-08-12T10:0{index}:30Z"),"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":format!("assistant response {index}")}]}}));
    }
    events.extend([
        json!({"timestamp":"2026-08-12T10:04:00Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Add File: src/new.rs\n*** End Patch","call_id":"p1"}}),
        json!({"timestamp":"2026-08-12T10:04:30Z","type":"response_item","payload":{"type":"function_call_output","call_id":"p1","output":"Error: compiler rejected the first attempt"}}),
        json!({"timestamp":"2026-08-12T10:05:00Z","type":"response_item","payload":{"type":"function_call","name":"update_plan","arguments":"{\"plan\":[{\"step\":\"implemented\",\"status\":\"completed\"},{\"step\":\"run final tests\",\"status\":\"in_progress\"}]}","call_id":"plan"}}),
        json!({"timestamp":"2026-08-12T10:05:30Z","type":"compacted","payload":{"message":"Newest verified compaction summary."}}),
        json!({"timestamp":"2026-08-12T10:06:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Completed the structured handoff implementation and verified its focused regression suite."}]}}),
        json!({"timestamp":"2026-08-12T10:06:30Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"done"}}),
    ]);
    write_rollout(home.path(), "rollout-latest.jsonl", &events);

    // A newer other-harness session must not leak into a codex-authored packet.
    let claude_dir = home.path().join(".claude/projects/demo");
    std::fs::create_dir_all(&claude_dir).expect("claude dir");
    std::fs::write(
        claude_dir.join("other.jsonl"),
        format!("{}\n", json!({"type":"user","message":{"role":"user","content":"CLAUDE SECRET TASK"},"timestamp":"2026-08-12T12:00:00Z","cwd":project.path(),"sessionId":"other"})),
    )
    .expect("claude session");

    let path = write_json(project.path(), "minimal.json", &json!({}));
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff", "write", "--from", "codex", "--to", "codex", "--input", &path,
        ])
        .assert()
        .success();
    let packet = current(project.path());
    assert_eq!(packet["objective"], "real user prompt 1");
    assert_eq!(packet["task"], "real user prompt 3");
    assert_eq!(
        packet["context_summary"],
        "Newest verified compaction summary."
    );
    assert_eq!(packet["changed_files"], json!(["src/new.rs"]));
    assert_eq!(
        packet["failures"],
        json!(["Error: compiler rejected the first attempt"])
    );
    assert_eq!(packet["next_actions"], json!(["run final tests"]));
    assert_eq!(packet["plan_state"].as_array().expect("plan").len(), 2);
    assert_eq!(
        packet["progress_summaries"],
        json!(["Newest verified compaction summary."])
    );
    assert_eq!(
        packet["milestones"].as_array().expect("milestones").len(),
        1
    );
    let tail = packet["conversation_tail"].as_array().expect("tail");
    assert!(
        tail.len() >= 4,
        "full uncapped tail expected, got {}",
        tail.len()
    );
    assert!(
        tail.iter().any(|e| e["text"] == "real user prompt 2"),
        "{tail:?}"
    );
    assert!(
        tail.iter().any(|e| e["text"] == "real user prompt 3"),
        "{tail:?}"
    );
    assert!(
        tail.iter().any(|e| e["text"] == "assistant response 3"),
        "{tail:?}"
    );
    assert!(tail.iter().any(|e| e["text"]
        .as_str()
        .is_some_and(|text| text.contains("Completed the structured"))));
    assert!(packet.to_string().contains("Transcript outcome: completed"));
    assert!(packet["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .any(|warning| warning.as_str().is_some_and(
            |text| text.contains("observed from latest matching codex session latest")
        )));
    assert!(!packet.to_string().contains("CLAUDE SECRET TASK"));
    assert!(!packet.to_string().contains("old task must not win"));

    let explicit_empty = write_json(
        project.path(),
        "explicit-empty-failures.json",
        &json!({
            "objective":"goal",
            "task":"respect explicit emptiness",
            "implementation_status":"Author-verified implementation status",
            "context_summary":"The author states that no failures should be carried forward.",
            "decisions":["Author decision"],
            "changed_files":["author/file.rs"],
            "next_actions":["Author next action"],
            "failures":[]
        }),
    );
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--to",
            "codex",
            "--input",
            &explicit_empty,
        ])
        .assert()
        .success();
    assert_eq!(current(project.path())["failures"], json!([]));
    let packet = current(project.path());
    assert_eq!(packet["objective"], "goal");
    assert_eq!(
        packet["context_summary"],
        "The author states that no failures should be carried forward."
    );
    assert_eq!(
        packet["implementation_status"],
        "Author-verified implementation status"
    );
    assert_eq!(packet["decisions"], json!(["Author decision"]));
    assert_eq!(packet["changed_files"], json!(["author/file.rs"]));
    assert_eq!(packet["next_actions"], json!(["Author next action"]));
}

#[test]
fn quality_warnings_write_anyway_unknown_destination_still_refuses() {
    let (config, home, project) = project();
    let valid = write_json(
        project.path(),
        "valid.json",
        &json!({"objective":"goal","task":"immediate work","context_summary":"Only local author evidence is available.","next_actions":["continue"],"failures":[]}),
    );
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff", "write", "--from", "codex", "--to", "claude", "--input", &valid,
        ])
        .assert()
        .success();
    let packet = current(project.path());
    assert!(packet["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .any(|warning| warning
            .as_str()
            .is_some_and(|text| text.contains("no matching verified codex transcript"))));

    for (name, soft) in [
        (
            "empty-objective",
            json!({"task":"task","context_summary":"summary","next_actions":["next"]}),
        ),
        (
            "empty-task",
            json!({"objective":"goal","context_summary":"summary","next_actions":["next"]}),
        ),
        (
            "same",
            json!({"objective":"goal","task":" Same text ","context_summary":"same TEXT","next_actions":["next"]}),
        ),
        (
            "no-actions",
            json!({"objective":"goal","task":"task","context_summary":"summary","next_actions":[]}),
        ),
    ] {
        let input = write_json(project.path(), &format!("{name}.json"), &soft);
        stateroot(config.path(), home.path(), project.path())
            .args([
                "handoff", "write", "--from", "codex", "--to", "claude", "--input", &input,
            ])
            .assert()
            .success();
    }

    // Snapshot after soft (warn-not-refuse) writes; only unknown destination
    // must leave current.json untouched.
    let path = project.path().join(".stateroot/handoffs/current.json");
    let before = std::fs::read(&path).expect("before unknown dest");
    let history_before = history_count(project.path());

    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--to",
            "not-a-harness",
            "--input",
            &valid,
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown handoff destination"));
    assert_eq!(
        std::fs::read(&path).expect("after unknown destination"),
        before
    );
    assert_eq!(history_count(project.path()), history_before);

    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--to",
            "claude-code",
            "--input",
            &valid,
        ])
        .assert()
        .success();
    assert_eq!(
        current(project.path())["recommended_next_harness"],
        "claude"
    );
    assert!(!current(project.path())["recommended_next_harness"].is_null());
    assert_eq!(history_count(project.path()), history_before + 1);
}

#[test]
fn stdin_windows_paths_bounds_dedupe_and_legacy_labels_are_stable() {
    let (config, home, project) = project();
    let long = "current state ".repeat(500);
    let stdin = json!({
        "objective":"JSON objective",
        "task":"Use Windows fixture paths",
        "context_summary":long,
        "changed_files":["C:\\work\\src\\main.rs","C:\\work\\src\\main.rs"],
        "next_actions":["Run tests","Run tests"],
        "failures":[]
    });
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--to",
            "codex",
            "--input",
            "-",
            "--objective",
            "CLI durable goal",
        ])
        .write_stdin(stdin.to_string())
        .assert()
        .success();
    let packet = current(project.path());
    assert_eq!(packet["objective"], "CLI durable goal");
    assert_eq!(packet["changed_files"], json!(["C:\\work\\src\\main.rs"]));
    assert_eq!(packet["next_actions"], json!(["Run tests"]));
    assert!(
        packet["context_summary"]
            .as_str()
            .expect("summary")
            .chars()
            .count()
            >= 6000
    );

    let minimal = write_json(
        project.path(),
        "legacy-input.json",
        &json!({"objective":"goal","task":"legacy migration"}),
    );
    let legacy_state = "A precise current state with exact evidence. ".repeat(60);
    let note = format!(
        "current state: {legacy_state} DECISIONS/WHY: Keep exact wording and preserve stable order. NEXT ACTIONS: (1) Run all checks; (2) Inspect the final diff FAILED APPROACHES/BUGS: First parser was unsafe."
    );
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff", "write", "--from", "codex", "--to", "codex", "--input", &minimal, "--note",
            &note,
        ])
        .assert()
        .success();
    let packet = current(project.path());
    assert!(packet["context_summary"]
        .as_str()
        .expect("summary")
        .starts_with("A precise current state with exact evidence."));
    assert!(
        packet["context_summary"]
            .as_str()
            .expect("summary")
            .chars()
            .count()
            <= stateroot_core::handoff_bounds::CONTEXT_SUMMARY_MAX
    );
    assert_eq!(
        packet["decisions"],
        json!(["Keep exact wording and preserve stable order."])
    );
    assert_eq!(
        packet["next_actions"],
        json!(["Run all checks", "Inspect the final diff"])
    );
    assert_eq!(packet["failures"], json!(["First parser was unsafe."]));
    assert_ne!(packet["task"], packet["context_summary"]);
    for (key, later_labels) in [
        (
            "context_summary",
            &["DECISIONS/WHY:", "NEXT ACTIONS:", "FAILED APPROACHES/BUGS:"][..],
        ),
        (
            "decisions",
            &["NEXT ACTIONS:", "FAILED APPROACHES/BUGS:"][..],
        ),
        ("next_actions", &["FAILED APPROACHES/BUGS:"][..]),
        ("failures", &[][..]),
    ] {
        let values = if key == "context_summary" {
            vec![packet[key].as_str().expect("context summary")]
        } else {
            packet[key]
                .as_array()
                .expect("canonical array")
                .iter()
                .map(|value| value.as_str().expect("string item"))
                .collect()
        };
        assert!(!values.is_empty(), "{key}");
        for value in values {
            for later_label in later_labels {
                assert!(!value.contains(later_label), "{key}: {value}");
            }
        }
    }

    let prose = "The current state machine is stable; decisions and why they matter are ordinary prose, and next actions are discussed without exact legacy labels.";
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff", "write", "--from", "codex", "--to", "codex", "--input", &minimal, "--note",
            prose,
        ])
        .assert()
        .success();
    let packet = current(project.path());
    assert_eq!(packet["context_summary"], prose);
    assert_eq!(packet["decisions"], json!([]));
    assert_eq!(packet["failures"], json!([]));
}

#[test]
fn detailed_context_summary_preserved_through_write_and_show() {
    let (config, home, project) = project();
    let narrative = "Verified state and rationale. ".repeat(180);
    assert!(
        narrative.chars().count() > 4000 && narrative.chars().count() < 6000,
        "fixture len {}",
        narrative.chars().count()
    );
    let input = write_json(
        project.path(),
        "detailed-handoff.json",
        &json!({
            "objective": "Ship detailed continuity",
            "task": "Validate narrative preservation",
            "context_summary": narrative,
            "decisions": ["Keep bounded detailed prose"],
            "failures": [],
            "next_actions": ["Run regression tests"]
        }),
    );
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff", "write", "--from", "codex", "--to", "cursor", "--input", &input,
        ])
        .assert()
        .success();
    let packet = current(project.path());
    assert_eq!(
        packet["context_summary"].as_str().expect("summary"),
        narrative
    );
    assert!(!packet["warnings"]
        .as_array()
        .map(|w| w.iter().any(|item| {
            item.as_str()
                .is_some_and(|text| text.contains("context_summary truncated"))
        }))
        .unwrap_or(false));
    let out = stateroot(config.path(), home.path(), project.path())
        .args(["handoff", "show"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains(&narrative[..80]));
}

fn seed_codex_session(home: &Path, project: &Path, session_id: &str, start: &str, end: &str) {
    write_rollout(
        home,
        &format!("rollout-{session_id}.jsonl"),
        &[
            meta(session_id, project, start),
            json!({"timestamp":start,"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"codex observed objective"}]}}),
            json!({"timestamp":end,"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"codex progress on continuity"}]}}),
            json!({"timestamp":end,"type":"compacted","payload":{"message":"Codex verified compaction after stale handoff."}}),
            json!({"timestamp":end,"type":"event_msg","payload":{"type":"task_complete","turn_id":"done"}}),
        ],
    );
}

fn patch_handoff_written_at(project: &Path, written_at: &str) {
    let path = project.join(".stateroot/handoffs/current.json");
    let mut packet: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("current")).expect("json");
    packet["written_at"] = json!(written_at);
    packet["created_at"] = json!(written_at);
    std::fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&packet).expect("json")),
    )
    .expect("patch");
}

#[test]
fn write_without_to_leaves_routing_null_and_skips_next_actions_requirement() {
    let (config, home, project) = project();
    let input = write_json(
        project.path(),
        "continuity-only.json",
        &json!({
            "objective":"Continuity goal",
            "task":"Finish local module",
            "context_summary":"Detailed continuity without cross-harness routing.",
            "failures":[]
        }),
    );
    stateroot(config.path(), home.path(), project.path())
        .args(["handoff", "write", "--from", "codex", "--input", &input])
        .assert()
        .success();
    let packet = current(project.path());
    assert!(packet["recommended_next_harness"].is_null());
    assert_eq!(packet["last_harness"], "codex");
}

#[test]
fn cross_harness_write_sets_routing_and_warns_on_empty_next_actions() {
    let (config, home, project) = project();
    let missing = write_json(
        project.path(),
        "no-actions.json",
        &json!({
            "objective":"goal",
            "task":"task",
            "context_summary":"summary for routing test",
            "next_actions":[]
        }),
    );
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff", "write", "--from", "codex", "--to", "cursor", "--input", &missing,
        ])
        .assert()
        .success()
        .stderr(predicates::str::contains("next_actions empty"));

    let valid = write_json(
        project.path(),
        "with-actions.json",
        &json!({
            "objective":"goal",
            "task":"task",
            "context_summary":"summary for routing test",
            "next_actions":["Continue in cursor"]
        }),
    );
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff", "write", "--from", "codex", "--to", "cursor", "--input", &valid,
        ])
        .assert()
        .success();
    let packet = current(project.path());
    assert_eq!(packet["recommended_next_harness"], "cursor");
}

#[test]
fn finalize_after_stale_handoff_increments_seq_without_routing() {
    let (config, home, project) = project();
    let input = write_json(
        project.path(),
        "claude-handoff.json",
        &json!({
            "objective":"Ship continuity",
            "task":"Hand off to codex",
            "context_summary":"Claude finished planning; codex should implement.",
            "next_actions":["Implement in codex"],
            "failures":[]
        }),
    );
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff", "write", "--from", "claude", "--to", "codex", "--input", &input,
        ])
        .assert()
        .success();
    patch_handoff_written_at(project.path(), "2026-08-12T08:00:00Z");
    seed_codex_session(
        home.path(),
        project.path(),
        "after-stale",
        "2026-08-12T11:00:00Z",
        "2026-08-12T12:00:00Z",
    );

    stateroot(config.path(), home.path(), project.path())
        .args(["handoff", "finalize", "--from", "codex"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "finalized from verified transcript",
        ));

    let packet = current(project.path());
    assert_eq!(packet["seq"], 2);
    assert!(packet["recommended_next_harness"].is_null());
    assert_eq!(packet["created_by_harness"], "codex");
    assert_eq!(packet["last_harness"], "codex");
    assert!(packet["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .any(|item| item
            .as_str()
            .is_some_and(|text| text.contains("finalized from verified transcript"))));
    assert_eq!(
        packet["context_summary"],
        "Codex verified compaction after stale handoff."
    );
}

#[test]
fn explicit_write_blocks_finalize_over_same_seq() {
    let (config, home, project) = project();
    let input = write_json(
        project.path(),
        "stale.json",
        &json!({
            "objective":"goal",
            "task":"stale boundary",
            "context_summary":"Stale formal handoff before codex work.",
            "next_actions":["continue"],
            "failures":[]
        }),
    );
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff", "write", "--from", "claude", "--to", "codex", "--input", &input,
        ])
        .assert()
        .success();
    patch_handoff_written_at(project.path(), "2026-08-12T08:00:00Z");
    seed_codex_session(
        home.path(),
        project.path(),
        "blocked",
        "2026-08-12T11:00:00Z",
        "2026-08-12T12:00:00Z",
    );

    let explicit = write_json(
        project.path(),
        "explicit-codex.json",
        &json!({
            "objective":"Author codex objective",
            "task":"Author codex task",
            "context_summary":"Explicit author-written codex handoff.",
            "next_actions":["Author next"],
            "failures":[]
        }),
    );
    stateroot(config.path(), home.path(), project.path())
        .args(["handoff", "write", "--from", "codex", "--input", &explicit])
        .assert()
        .success();
    let before = current(project.path());
    assert_eq!(before["seq"], 2);
    assert_eq!(before["task"], "Author codex task");

    stateroot(config.path(), home.path(), project.path())
        .args(["handoff", "finalize", "--from", "codex"])
        .assert()
        .success()
        .stdout(predicates::str::contains("nothing to finalize"));
    assert_eq!(current(project.path())["task"], "Author codex task");
}

#[test]
fn resume_overlay_when_finalize_missed_and_no_overlay_after_finalize() {
    let (config, home, project) = project();
    let input = write_json(
        project.path(),
        "stale.json",
        &json!({
            "objective":"goal",
            "task":"stale boundary",
            "context_summary":"Stale formal handoff before codex work.",
            "next_actions":["continue"],
            "failures":[]
        }),
    );
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff", "write", "--from", "claude", "--to", "codex", "--input", &input,
        ])
        .assert()
        .success();
    patch_handoff_written_at(project.path(), "2026-08-12T08:00:00Z");
    seed_codex_session(
        home.path(),
        project.path(),
        "overlay",
        "2026-08-12T11:00:00Z",
        "2026-08-12T12:00:00Z",
    );

    let overlay = stateroot(config.path(), home.path(), project.path())
        .args(["resume", "--harness", "cursor", "--force"])
        .assert()
        .success();
    let overlay_stdout = String::from_utf8(overlay.get_output().stdout.clone()).expect("utf8");
    assert!(overlay_stdout.contains("Work since handoff #1 (observed — codex)"));
    assert!(overlay_stdout.contains("NOT a formal handoff packet"));
    assert_eq!(current(project.path())["seq"], 1);

    stateroot(config.path(), home.path(), project.path())
        .args(["handoff", "finalize", "--from", "codex"])
        .assert()
        .success();

    let after = stateroot(config.path(), home.path(), project.path())
        .args(["resume", "--harness", "cursor", "--force"])
        .assert()
        .success();
    let after_stdout = String::from_utf8(after.get_output().stdout.clone()).expect("utf8");
    assert!(!after_stdout.contains("Work since handoff #1 (observed — codex)"));
    assert_eq!(current(project.path())["seq"], 2);
}

#[test]
fn flag_only_cross_harness_write_sets_routing_from_next_flags() {
    let (config, home, project) = project();
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "claude",
            "--to",
            "cursor",
            "--objective",
            "Ship the feature",
            "--task",
            "Resume verification in the receiving harness",
            "--context-summary",
            "Verified local state is ready; cross-harness routing test.",
            "--next",
            "Run smoke tests",
            "--next",
            "Fix remaining blockers",
        ])
        .assert()
        .success();
    let packet = current(project.path());
    assert_eq!(packet["recommended_next_harness"], "cursor");
    assert_eq!(
        packet["next_actions"],
        json!(["Run smoke tests", "Fix remaining blockers"])
    );
    assert_eq!(
        packet["task"],
        "Resume verification in the receiving harness"
    );
}

#[test]
fn flag_only_continuity_write_has_null_routing() {
    let (config, home, project) = project();
    stateroot(config.path(), home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "claude",
            "--objective",
            "Durable goal",
            "--task",
            "Continue current work",
            "--context-summary",
            "Continuity-only flag write without routing.",
        ])
        .assert()
        .success();
    let packet = current(project.path());
    assert!(packet["recommended_next_harness"].is_null());
}

#[test]
fn input_immediate_task_alias_accepted_on_write() {
    let (config, home, project) = project();
    let path = write_json(
        project.path(),
        "alias.json",
        &json!({
            "immediate_task": "Alias boundary task",
            "objective": "Alias objective",
            "context_summary": "Alias summary for continuity."
        }),
    );
    stateroot(config.path(), home.path(), project.path())
        .args(["handoff", "write", "--from", "claude", "--input", &path])
        .assert()
        .success();
    assert_eq!(current(project.path())["task"], "Alias boundary task");
}

#[test]
fn input_decision_objects_coerced_on_write() {
    let (config, home, project) = project();
    let path = write_json(
        project.path(),
        "decisions.json",
        &json!({
            "objective": "goal",
            "task": "task",
            "context_summary": "summary",
            "decisions": [{"decision": "Pick Postgres", "rationale": "Team standard"}]
        }),
    );
    stateroot(config.path(), home.path(), project.path())
        .args(["handoff", "write", "--from", "claude", "--input", &path])
        .assert()
        .success();
    assert_eq!(
        current(project.path())["decisions"],
        json!(["Pick Postgres — Team standard"])
    );
}

#[test]
fn input_unknown_key_error_names_key_and_allowed_fields() {
    let (config, home, project) = project();
    let path = write_json(
        project.path(),
        "bad.json",
        &json!({
            "surprise": true,
            "objective": "goal",
            "task": "task",
            "context_summary": "summary"
        }),
    );
    let failure = stateroot(config.path(), home.path(), project.path())
        .args(["handoff", "write", "--from", "claude", "--input", &path])
        .assert()
        .failure();
    let stderr = String::from_utf8(failure.get_output().stderr.clone()).expect("utf8");
    assert!(
        stderr.contains("unknown handoff input key(s): surprise"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("Allowed content keys"), "stderr: {stderr}");
    assert!(!project
        .path()
        .join(".stateroot/handoffs/current.json")
        .exists());
}
