//! `stateroot plan` tests — central plan artifacts, lifecycle, digest
//! directives, and handoff plan_ref. Hermetic homes/projects only.

// Each test binary includes this module but uses a subset of its helpers.
#[path = "support/mod.rs"]
mod support;

use std::path::Path;

use support::{homes, stateroot};

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    std::fs::write(project.join("README.md"), "# Demo\n\nA demo project.\n").expect("readme");
    support::init_project(config_home, user_home, project);
}

/// Record a plan from a file; returns the plan id parsed from stdout.
fn record_plan(
    config_home: &Path,
    user_home: &Path,
    project: &Path,
    title: &str,
    body: &str,
) -> String {
    let plan_file = project.join("plan-input.md");
    std::fs::write(&plan_file, body).expect("plan file");
    let out = stateroot(config_home, user_home, project)
        .args([
            "plan",
            "record",
            "--file",
            "plan-input.md",
            "--title",
            title,
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("(draft)"), "stdout: {stdout}");
    stdout
        .strip_prefix("recorded plan ")
        .and_then(|rest| rest.split(' ').next())
        .expect("plan id in stdout")
        .to_string()
}

fn episodic(project: &Path) -> String {
    std::fs::read_to_string(project.join(".stateroot/memories/episodic.jsonl")).expect("episodic")
}

#[test]
fn candidate_verification_survives_restage_publication_cleanup_and_gc() {
    let (config, home) = homes();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    let id = record_plan(
        config.path(),
        home.path(),
        project.path(),
        "Candidate checks",
        "# Check candidate source\n",
    );
    stateroot(config.path(), home.path(), project.path())
        .args(["plan", "approve", &id])
        .assert()
        .success();
    stateroot_core::todo_federation::record_binding(
        project.path(),
        "codex",
        "shared-session",
        &id,
        "receiver",
    )
    .unwrap();
    stateroot_core::wiki::append_inbox_bullets(
        project.path(),
        &["baseline retained inbox note".into()],
    )
    .unwrap();
    stateroot_core::wiki::append_log(project.path(), "baseline retained event").unwrap();
    std::fs::write(
        project.path().join(".stateroot/ingest-gov.json"),
        "{\"last_input_sha256\":\"receiver\"}",
    )
    .unwrap();
    let routing = std::fs::read(
        project
            .path()
            .join(".stateroot/plan-bindings/codex/shared-session.json"),
    )
    .unwrap();
    stateroot(config.path(), home.path(), project.path())
        .args(["snap", "--reason", "base"])
        .assert()
        .success();
    let base = stateroot_core::roots::latest_root(project.path())
        .unwrap()
        .unwrap();
    let checkout = tempfile::tempdir().unwrap();
    let mut acknowledgements = Vec::new();
    for name in ["candidate", "candidate-b"] {
        let fork = checkout.path().join(name);
        stateroot(config.path(), home.path(), project.path())
            .args(["fork", &base, "--name", name, "--worktree"])
            .arg(&fork)
            .assert()
            .success();
        std::fs::write(fork.join(format!("{name}.txt")), name).unwrap();
        if name == "candidate" {
            std::fs::write(fork.join("Cargo.lock"), "result lockfile").unwrap();
        }
        stateroot_core::todo_federation::record_binding(
            &fork,
            "codex",
            "shared-session",
            &id,
            name,
        )
        .unwrap();
        stateroot_core::wiki::append_inbox_bullets(&fork, &[format!("{name} retained inbox note")])
            .unwrap();
        stateroot_core::wiki::append_log(&fork, &format!("{name} retained event")).unwrap();
        std::fs::write(
            fork.join(".stateroot/ingest-gov.json"),
            serde_json::json!({"last_input_sha256":name}).to_string(),
        )
        .unwrap();
        let session = format!("capture-{name}");
        stateroot(config.path(), home.path(), &fork).args(["hook", "post_tool_use", "--harness", "codex"])
            .write_stdin(serde_json::json!({"session_id":session,"tool_name":"Bash","tool_use_id":name,"tool_response":format!("{name} durable tool result")}).to_string()).assert().success();
        let watermark =
            match stateroot_core::observations::session_watermark(&fork, "codex", &session) {
                stateroot_core::observations::SessionWatermark::Present(value) => value,
                other => panic!("{other:?}"),
            };
        let key = uuid::Uuid::now_v7().to_string();
        let path = stateroot_core::observations::acknowledge_frontier(
            &fork, "codex", &session, &key, &watermark,
        )
        .unwrap();
        acknowledgements.push((session, key, watermark, std::fs::read(path).unwrap()));
        stateroot(config.path(), home.path(), &fork)
            .args(["snap", "--reason", "fork"])
            .assert()
            .success();
    }
    let prepared = stateroot(config.path(), home.path(), project.path())
        .args(["merge", "--prepare", "candidate", "candidate-b", "--json"])
        .assert()
        .success();
    let attempt: serde_json::Value = serde_json::from_slice(&prepared.get_output().stdout).unwrap();
    let attempt_id = attempt["id"].as_str().unwrap();
    let staged = stateroot(config.path(), home.path(), project.path())
        .args(["merge", "--stage", attempt_id, "--json"])
        .assert()
        .success();
    let staged: serde_json::Value = serde_json::from_slice(&staged.get_output().stdout).unwrap();
    let candidate = std::path::PathBuf::from(staged["worktree"].as_str().unwrap());
    let before = stateroot_core::roots::latest_root(project.path()).unwrap();
    let owner = git2::Repository::open(project.path()).unwrap();
    let head = || {
        let head = owner.find_reference("HEAD").unwrap();
        (head.symbolic_target().map(str::to_owned), head.target())
    };
    let head_before = head();
    let frozen_forks = || {
        ["candidate", "candidate-b"].map(|name| {
            owner
                .refname_to_id(&format!("refs/stateroot/forks/{name}"))
                .unwrap()
        })
    };
    let fork_before = frozen_forks();
    let verified = stateroot(config.path(), home.path(), &candidate)
        .args(["plan", "verify", &id, "--"])
        .arg(assert_cmd::cargo::cargo_bin("stateroot"))
        .arg("--version")
        .assert()
        .success();
    let output = String::from_utf8_lossy(&verified.get_output().stdout);
    let observation = output
        .lines()
        .find_map(|line| {
            line.strip_prefix("verification ")
                .and_then(|line| line.split(':').next())
        })
        .unwrap()
        .to_string();
    stateroot(config.path(), home.path(), project.path())
        .args(["merge", "--stage", attempt_id, "--json"])
        .assert()
        .success();
    assert!(
        !candidate.join(".git").exists(),
        "verification must use the recorded owner repository"
    );
    assert_eq!(
        stateroot_core::roots::latest_root(project.path()).unwrap(),
        before
    );
    assert_eq!(head(), head_before);
    assert_eq!(frozen_forks(), fork_before);
    let record = stateroot_core::verification::load(&candidate, &observation).unwrap();
    stateroot(config.path(), home.path(), project.path())
        .args(["merge", "--continue", attempt_id, "--json"])
        .assert()
        .success();
    assert!(!candidate.exists(), "publication removes the candidate");
    assert_eq!(
        std::fs::read(
            project
                .path()
                .join(".stateroot/plan-bindings/codex/shared-session.json")
        )
        .unwrap(),
        routing
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join(".stateroot/ingest-gov.json")).unwrap(),
        "{\"last_input_sha256\":\"receiver\"}"
    );
    assert!(
        !project
            .path()
            .join(".stateroot/spool/current.json")
            .exists(),
        "foreign projections never become current"
    );
    let inbox = stateroot_core::wiki::show(project.path(), "_inbox").unwrap();
    let log = stateroot_core::wiki::show(project.path(), "wiki/log.md").unwrap();
    for name in ["candidate", "candidate-b"] {
        assert!(inbox.contains(&format!("{name} retained inbox note")));
        assert!(log.contains(&format!("{name} retained event")));
    }
    let gc = std::process::Command::new("git")
        .args(["gc", "--prune=now"])
        .current_dir(project.path())
        .status()
        .unwrap();
    assert!(gc.success());
    for (session, key, watermark, bytes) in acknowledgements {
        let path = stateroot_core::observations::acknowledge_frontier(
            project.path(),
            "codex",
            &session,
            &key,
            &watermark,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(path).unwrap(),
            bytes,
            "replay retains exact acknowledged frontier"
        );
        assert!(std::fs::read_to_string(
            project
                .path()
                .join(".stateroot/spool/segments")
                .join(watermark.segment.unwrap())
        )
        .unwrap()
        .contains("durable tool result"));
    }
    let repo = git2::Repository::open(project.path()).unwrap();
    let published = stateroot_core::roots::latest_root(project.path())
        .unwrap()
        .unwrap();
    let published = repo
        .find_commit(published.parse().unwrap())
        .unwrap()
        .tree()
        .unwrap();
    let declaration = published
        .get_path(Path::new(stateroot_core::fidelity::MANIFEST_PATH))
        .unwrap();
    let declaration: serde_json::Value =
        serde_json::from_slice(repo.find_blob(declaration.id()).unwrap().content()).unwrap();
    assert_eq!(declaration["publication"]["operation"], "merge");
    assert!(declaration["lockfiles"]["Cargo.lock"].is_string());
    let lockfile = published.get_path(Path::new("Cargo.lock")).unwrap();
    assert_eq!(
        repo.find_blob(lockfile.id()).unwrap().content(),
        b"result lockfile"
    );
    repo.find_tree(record.product_tree.parse().unwrap())
        .expect("retained verification source tree after GC");
    let retained =
        stateroot_core::verification::validate(project.path(), &id, &observation).unwrap();
    assert_eq!(retained.product_tree, record.product_tree);
    for (suffix, expected) in ["stdout", "stderr"].into_iter().zip(&record.output_digests) {
        let path = project
            .path()
            .join(".stateroot/local/verification")
            .join(format!("{observation}.{suffix}"));
        assert_eq!(
            &stateroot_core::verification::output_digest(&path).unwrap(),
            expected
        );
    }
}

#[test]
fn command_observation_completion_is_optional_source_bound_and_preserves_history() {
    let (config, home) = homes();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    let id = record_plan(
        config.path(),
        home.path(),
        project.path(),
        "Observed command",
        "# Verify the declared source",
    );
    stateroot(config.path(), home.path(), project.path())
        .args(["plan", "approve", &id])
        .assert()
        .success();
    std::fs::write(project.path().join("code.txt"), "original").unwrap();
    let binary = assert_cmd::cargo::cargo_bin("stateroot");
    let output = stateroot(config.path(), home.path(), project.path())
        .args(["plan", "verify", &id, "--"])
        .arg(&binary)
        .arg("--version")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output = String::from_utf8(output).unwrap();
    let verification = output
        .lines()
        .find_map(|line| {
            line.strip_prefix("verification ")
                .and_then(|line| line.split(':').next())
        })
        .unwrap()
        .to_string();
    let dir = project.path().join(".stateroot/plans/verification");
    let path = dir.join(format!("{verification}.finished.json"));
    let before = std::fs::read(&path).unwrap();
    let observed: serde_json::Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(observed["exit_code"], 0);
    assert_eq!(
        Path::new(observed["command"][0].as_str().unwrap()),
        binary.as_path()
    );
    assert!(observed["status"]
        .as_str()
        .unwrap()
        .contains("semantic acceptance not established"));
    assert_eq!(observed["output_digests"].as_array().unwrap().len(), 2);
    assert!(project
        .path()
        .join(".stateroot/local/verification")
        .join(format!("{verification}.stdout"))
        .is_file());
    std::fs::write(project.path().join("code.txt"), "changed").unwrap();
    let root_before = stateroot_core::roots::latest_root(project.path()).unwrap();
    stateroot(config.path(), home.path(), project.path())
        .args([
            "plan",
            "done",
            &id,
            "--evidence",
            "caller description",
            "--verification",
            &verification,
        ])
        .assert()
        .failure();
    assert_eq!(
        stateroot_core::roots::latest_root(project.path()).unwrap(),
        root_before
    );
    assert_eq!(
        stateroot_core::plans::load(project.path(), &id)
            .unwrap()
            .0
            .status,
        "approved"
    );
    std::fs::write(project.path().join("code.txt"), "original").unwrap();
    stateroot(config.path(), home.path(), project.path())
        .args([
            "plan",
            "done",
            &id,
            "--evidence",
            "command exited zero; acceptance assessed by caller",
            "--verification",
            &verification,
        ])
        .assert()
        .success();
    let completed = stateroot_core::plans::load(project.path(), &id).unwrap().0;
    assert_eq!(
        completed.completion_receipt.unwrap().verification_ids,
        vec![verification]
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn failed_command_observation_is_retained_without_completing_the_plan() {
    let (config, home) = homes();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    let id = record_plan(
        config.path(),
        home.path(),
        project.path(),
        "Failure",
        "# Retain failure",
    );
    let binary = assert_cmd::cargo::cargo_bin("stateroot");
    stateroot(config.path(), home.path(), project.path())
        .args(["plan", "verify", &id, "--"])
        .arg(binary)
        .arg("not-a-command")
        .assert()
        .failure();
    let directory = project.path().join(".stateroot/plans/verification");
    let finished = std::fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .ends_with(".finished.json")
        })
        .unwrap()
        .path();
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(finished).unwrap()).unwrap();
    assert_eq!(record["status"], "observed command failure");
    assert_eq!(
        stateroot_core::plans::load(project.path(), &id)
            .unwrap()
            .0
            .status,
        "draft"
    );
}

#[test]
fn plan_lifecycle_walk_and_episodic_notes() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let id = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Ship the Parser",
        "# Ship the Parser\n\n1. tokenize\n2. parse\n",
    );
    assert!(id.starts_with("plan_"));
    assert!(project
        .path()
        .join(format!(".stateroot/plans/{id}.md"))
        .is_file());
    assert!(project
        .path()
        .join(format!(".stateroot/plans/{id}.json"))
        .is_file());

    // show prints the verbatim body.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "show", &id])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("# Ship the Parser"), "show: {stdout}");
    assert!(stdout.contains("2. parse"), "show: {stdout}");

    // list shows id/title/status/harness.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains(&format!("{id} · Ship the Parser · draft · cli")),
        "list: {stdout}"
    );

    // approve → activate → done (done requires evidence).
    for (action, status) in [
        ("approve", "approved"),
        ("activate", "active"),
        ("done", "done"),
    ] {
        let mut cmd = stateroot(config_home.path(), user_home.path(), project.path());
        cmd.args(["plan", action, &id]);
        if action == "done" {
            cmd.args(["--evidence", "acceptance gate green"]);
        }
        let out = cmd.assert().success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
        assert!(
            stdout.contains(&format!("plan {id} → {status}")),
            "{action}: {stdout}"
        );
    }
    let episodic = episodic(project.path());
    for status in ["approved", "active", "done"] {
        assert!(
            episodic.contains(&format!("plan {id} {status} by cli")),
            "episodic: {episodic}"
        );
    }
    assert!(
        episodic.contains(&format!("plan {id} recorded: Ship the Parser")),
        "{episodic}"
    );
}

#[test]
fn plan_one_active_demotes_with_note_and_illegal_transitions_error() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let first = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "First",
        "# First\n",
    );
    let second = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Second",
        "# Second\n",
    );

    // done on a draft is illegal (even with evidence); unknown id errors.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "done", &first, "--evidence", "premature"])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(stderr.contains("cannot move to done"), "stderr: {stderr}");
    // done without evidence is a usage error at every status.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "done", &first])
        .assert()
        .failure();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "approve", "no-such-plan"])
        .assert()
        .failure();

    for id in [&first, &second] {
        stateroot(config_home.path(), user_home.path(), project.path())
            .args(["plan", "approve", id])
            .assert()
            .success();
    }
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "activate", &first])
        .assert()
        .success();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "activate", &second])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains(&format!("{first} demoted to approved")),
        "stdout: {stdout}"
    );

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains(&format!("{first} · First · approved")),
        "list: {stdout}"
    );
    assert!(
        stdout.contains(&format!("{second} · Second · active")),
        "list: {stdout}"
    );

    // abandon works from a non-terminal state.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "abandon", &first])
        .assert()
        .success();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains(&format!("{first} · First · abandoned")),
        "list: {stdout}"
    );
}

#[test]
fn plan_digest_directives_and_body_omission() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // Draft-only → planner directive.
    let id = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Refactor Auth",
        "# Refactor Auth\n\nTHE-PLAN-BODY-MUST-NOT-APPEAR\n",
    );
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("## Pending Plan"), "digest: {stdout}");
    assert!(!stdout.contains("## Active Plan"), "digest: {stdout}");
    assert!(
        stdout.contains("refine the plan file; do not implement yet"),
        "digest: {stdout}"
    );
    assert!(
        stdout.contains(&format!(".stateroot/plans/{id}.md")),
        "digest: {stdout}"
    );
    assert!(
        !stdout.contains("THE-PLAN-BODY-MUST-NOT-APPEAR"),
        "digest: {stdout}"
    );

    // Approved with no executor → the assignment directive.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "approve", &id])
        .assert()
        .success();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("## Pending Plan"), "digest: {stdout}");
    assert!(!stdout.contains("## Active Plan"), "digest: {stdout}");
    assert!(
        stdout.contains("assign or claim execution"),
        "digest: {stdout}"
    );
    assert!(
        stdout.contains("**Refactor Auth** (approved) — planned by cli"),
        "digest: {stdout}"
    );
    assert!(
        !stdout.contains("THE-PLAN-BODY-MUST-NOT-APPEAR"),
        "digest: {stdout}"
    );
    // The fallback transcript Plan State is suppressed while a central plan exists
    // (nothing seeds plan_state here, so it must simply stay absent).
    assert!(!stdout.contains("## Plan State"), "digest: {stdout}");

    // Activated → the executor directive.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "activate", &id])
        .assert()
        .success();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("## Active Plan"), "digest: {stdout}");
    assert!(
        stdout.contains("Execute it as written; do not re-plan or re-explore"),
        "digest: {stdout}"
    );
}

#[test]
fn plan_handoff_write_attaches_plan_ref() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    std::fs::write(
        project.path().join("handoff-input.json"),
        r#"{"objective":"continue","task":"continue","context_summary":"state captured","next_actions":["continue"]}"#,
    )
    .expect("handoff input");

    // No plan → no plan_ref.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--input",
            "handoff-input.json",
        ])
        .assert()
        .success();
    let packet: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.path().join(".stateroot/handoffs/current.json"))
            .expect("handoff"),
    )
    .expect("json");
    assert!(packet.get("plan_ref").is_none(), "packet: {packet}");

    // Approved plan → plan_ref attached.
    let id = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Pack It",
        "# Pack It\n",
    );
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "approve", &id])
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--input",
            "handoff-input.json",
        ])
        .assert()
        .success();
    let packet: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.path().join(".stateroot/handoffs/current.json"))
            .expect("handoff"),
    )
    .expect("json");
    assert_eq!(packet["plan_ref"]["id"], serde_json::json!(id));
    assert_eq!(packet["plan_ref"]["title"], "Pack It");
    assert_eq!(packet["plan_ref"]["status"], "approved");

    // Activated → plan_ref follows the lifecycle.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "activate", &id])
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "handoff",
            "write",
            "--from",
            "codex",
            "--input",
            "handoff-input.json",
        ])
        .assert()
        .success();
    let packet: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.path().join(".stateroot/handoffs/current.json"))
            .expect("handoff"),
    )
    .expect("json");
    assert_eq!(packet["plan_ref"]["status"], "active");
}

#[test]
fn plan_record_from_stdin_and_title_from_heading() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "record", "--stdin"])
        .write_stdin("# Streamed Plan\n\nvia stdin\n")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("recorded plan plan_"), "stdout: {stdout}");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("Streamed Plan"), "list: {stdout}");

    // Empty stdin is refused.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "record", "--stdin"])
        .write_stdin("  \n")
        .assert()
        .failure();
}

#[test]
fn plan_list_is_newest_first_with_active_pinned() {
    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let first = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "aaa-first",
        "# aaa-first\n\nDo the first thing.\n",
    );
    let second = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "zzz-second",
        "# zzz-second\n\nDo the second thing.\n",
    );
    // Activate the OLDER plan: the pin must outrank pure recency.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "approve", &first])
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "activate", &first])
        .assert()
        .success();

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "list: {stdout}");
    assert!(
        lines[0].contains(&first) && lines[0].contains("active"),
        "active plan pinned on top: {stdout}"
    );
    assert!(lines[1].contains(&second), "newest draft next: {stdout}");

    // Done with the active plan: the remaining list is strictly newest-first.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "done", &first, "--evidence", "work verified"])
        .assert()
        .success();
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines[0].contains(&second),
        "after done, newest first: {stdout}"
    );
    assert!(lines[1].contains(&first), "oldest last: {stdout}");
}

#[test]
fn plan_approval_pins_digest_and_body_drift_warns() {
    use sha2::Digest as _;

    let (config_home, user_home) = homes();
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    let id = record_plan(
        config_home.path(),
        user_home.path(),
        project.path(),
        "Pinned Plan",
        "# Pinned Plan\n\n1. do the approved thing\n",
    );
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "approve", &id])
        .assert()
        .success();

    // The sidecar pins sha256 of the approved body.
    let sidecar_path = project.path().join(format!(".stateroot/plans/{id}.json"));
    let body_path = project.path().join(format!(".stateroot/plans/{id}.md"));
    let sidecar: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar_path).expect("sidecar"))
            .expect("sidecar json");
    let digest = sidecar["approved_digest"]
        .as_str()
        .expect("approved_digest")
        .to_string();
    let body = std::fs::read(&body_path).expect("body");
    assert_eq!(digest, format!("{:x}", sha2::Sha256::digest(&body)));

    // No drift → no warning on show or in the digest.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "show", &id])
        .assert()
        .success();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(
        !stderr.contains("plan body changed since approval"),
        "stderr: {stderr}"
    );

    // An edit after approval: show warns (stderr), the digest warns in the
    // Pending Plan section (approval does not claim execution).
    std::fs::write(&body_path, "# Pinned Plan\n\nEDITED AFTER APPROVAL\n").expect("edit");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "show", &id])
        .assert()
        .success();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(
        stderr.contains("plan body changed since approval"),
        "stderr: {stderr}"
    );
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("## Pending Plan"), "digest: {stdout}");
    assert!(!stdout.contains("## Active Plan"), "digest: {stdout}");
    assert!(
        stdout.contains("plan body changed since approval"),
        "digest: {stdout}"
    );

    // A pre-existing sidecar without approved_digest makes no drift claim,
    // even though the body no longer matches anything on record.
    let mut sidecar: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar_path).expect("sidecar"))
            .expect("sidecar json");
    sidecar
        .as_object_mut()
        .expect("object")
        .remove("approved_digest");
    std::fs::write(
        &sidecar_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&sidecar).expect("json")
        ),
    )
    .expect("legacy sidecar");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "show", &id])
        .assert()
        .success();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(
        !stderr.contains("plan body changed since approval"),
        "legacy stderr: {stderr}"
    );
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        !stdout.contains("plan body changed since approval"),
        "legacy digest: {stdout}"
    );
}
