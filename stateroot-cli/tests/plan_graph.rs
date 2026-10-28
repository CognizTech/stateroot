//! Real CLI, hook/service and controlled worker fixtures; isolated homes only.
#[path = "support/mod.rs"]
mod support;
use serde_json::{json, Value};
use stateroot_core::plans;
use std::path::Path;

fn cli(config: &Path, home: &Path, project: &Path) -> assert_cmd::Command {
    let mut command = support::stateroot(config, home, project);
    command.env_remove("STATEROOT_DELEGATION_DEPTH");
    command
}

fn make_plan(project: &Path, title: &str, approved: bool) -> plans::PlanMeta {
    let p = plans::record(project, title, "codex", None, "# Exact retained body").unwrap();
    if approved {
        plans::transition(project, &p.id, plans::PlanStatus::Approved)
            .unwrap()
            .0
    } else {
        p
    }
}

fn declare(config: &Path, home: &Path, project: &Path, id: &str, declaration: Value) {
    let rev = plans::revision(project, id).unwrap();
    let out = cli(config, home, project)
        .args([
            "--actor",
            "codex",
            "plan",
            "declare",
            id,
            "--revision",
            &rev,
        ])
        .write_stdin(declaration.to_string())
        .assert()
        .success();
    let value: Value =
        serde_json::from_slice(&out.get_output().stdout).expect("stdout is one JSON document");
    assert_eq!(value["schema_version"], "stateroot.plan.v2");
}

#[test]
fn cli_graph_roundtrip_cas_and_legacy_workflow() {
    let (config, home) = support::homes();
    let project = tempfile::tempdir().unwrap();
    support::init_project(config.path(), home.path(), project.path());
    let p = make_plan(project.path(), "legacy", true);
    let original = std::fs::read(plans::body_path(project.path(), &p.id)).unwrap();
    let revision = plans::revision(project.path(), &p.id).unwrap();
    let out = cli(config.path(), home.path(), project.path())
        .args(["plan", "graph", "--json"])
        .assert()
        .success();
    let graph: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(graph["nodes"][0]["readiness"], "dependencies_not_declared");
    declare(
        config.path(),
        home.path(),
        project.path(),
        &p.id,
        json!({"dependencies":[],"owned_paths":["contract.rs"]}),
    );
    cli(config.path(), home.path(), project.path())
        .args(["plan", "declare", &p.id, "--revision", &revision])
        .write_stdin("{\"dependencies\":[]}")
        .assert()
        .failure();
    let out = cli(config.path(), home.path(), project.path())
        .args(["plan", "ready", "--json"])
        .assert()
        .success();
    let graph: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(graph["ready"], json!([p.id]));
    cli(config.path(), home.path(), project.path())
        .args(["plan", "activate", &p.id])
        .assert()
        .success();
    cli(config.path(), home.path(), project.path())
        .args([
            "plan",
            "done",
            &p.id,
            "--evidence",
            "real CLI fixture passed",
        ])
        .assert()
        .success();
    let loaded = plans::load(project.path(), &p.id).unwrap().0;
    assert_eq!(
        loaded.execution.unwrap().declaration.owned_paths,
        ["contract.rs"]
    );
    assert!(loaded.completion_receipt.is_some());
    assert_eq!(
        std::fs::read(plans::body_path(project.path(), &p.id)).unwrap(),
        original
    );
}

#[cfg(unix)]
fn command(config: &Path, home: &Path, project: &Path, path: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("stateroot"));
    cmd.current_dir(project)
        .env("STATEROOT_HOME", config)
        .env("STATEROOT_TEST_HOME", home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env("PATH", path)
        .env_remove("STATEROOT_DELEGATION_DEPTH")
        .env_remove("OPENAI_API_KEY")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_BASE")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd
}

#[cfg(unix)]
fn fake_harness() -> (tempfile::TempDir, String) {
    use std::os::unix::fs::PermissionsExt;
    let bin = tempfile::tempdir().unwrap();
    let executable = bin.path().join("claude");
    std::fs::write(&executable, "#!/bin/sh\necho launch >> .stateroot/local/launch-count\nprintf '%s\\n' \"$@\" > .stateroot/local/worker-argv\nwhile [ -d .stateroot/local ] && [ ! -f .stateroot/local/release-worker ]; do sleep 0.05; done\necho controlled-worker-done\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap()
    );
    (bin, path)
}

#[cfg(unix)]
struct Service(std::process::Child);
#[cfg(unix)]
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
fn await_condition(mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "controlled worker condition timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(unix)]
fn assignment(operation: &str) -> Value {
    json!({"harness":"claude","dispatch":{"operation_id":operation,"task":"controlled worker task","authorization":"owner authorized this bounded fixture programme"}})
}

#[cfg(unix)]
#[test]
fn dependency_transition_hook_service_race_launches_once_and_never_launches_drafts() {
    let (config, home) = support::homes();
    let project = tempfile::tempdir().unwrap();
    support::init_project(config.path(), home.path(), project.path());
    let producer = make_plan(project.path(), "producer", true);
    let consumer = make_plan(project.path(), "consumer", true);
    let draft = make_plan(project.path(), "draft", false);
    declare(
        config.path(),
        home.path(),
        project.path(),
        &consumer.id,
        json!({
            "dependencies":[{"plan_id":producer.id,"body_digest":producer.approved_digest,"kind":"completion"}],
            "assignment":assignment("consumer-operation")
        }),
    );
    declare(
        config.path(),
        home.path(),
        project.path(),
        &draft.id,
        json!({"dependencies":[],"assignment":assignment("draft-operation")}),
    );
    let (_bin, path) = fake_harness();
    // A real native boundary delivers the blocker to the coordinator.
    let blocked = command(config.path(), home.path(), project.path(), &path)
        .args(["hook", "--harness", "codex", "session_start"])
        .output()
        .unwrap();
    assert!(
        blocked.status.success(),
        "{}",
        String::from_utf8_lossy(&blocked.stderr)
    );
    let projection: Value = serde_json::from_slice(
        &std::fs::read(
            project
                .path()
                .join(".stateroot/local/projections/continuity.v1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(projection["attention"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["kind"] == "plan_readiness"
            && a["plan_id"] == consumer.id
            && a["detail"].as_str().unwrap().contains("incomplete")));
    assert!(!project
        .path()
        .join(".stateroot/local/launch-count")
        .exists());
    cli(config.path(), home.path(), project.path())
        .args([
            "plan",
            "done",
            &producer.id,
            "--evidence",
            "producer acceptance fixture",
        ])
        .assert()
        .success();
    // Independent processes contend through the actual service + native hook
    // entrypoints, sharing the normal delegate reservation lock.
    let service = Service(
        command(config.path(), home.path(), project.path(), &path)
            .args(["service", "run"])
            .spawn()
            .unwrap(),
    );
    let hook = command(config.path(), home.path(), project.path(), &path)
        .args(["hook", "--harness", "codex", "session_start"])
        .spawn()
        .unwrap();
    let result = hook.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    await_condition(|| {
        project
            .path()
            .join(".stateroot/local/launch-count")
            .exists()
            && project.path().join(".stateroot/local/worker-argv").exists()
    });
    let graph = plans::graph(project.path());
    let row = graph.nodes.iter().find(|n| n.id == consumer.id).unwrap();
    assert_eq!(row.worker_state, "running");
    assert_eq!(row.workers.len(), 1);
    assert!(
        std::fs::read_to_string(project.path().join(".stateroot/local/worker-argv"))
            .unwrap()
            .contains(&consumer.id)
    );
    assert!(graph
        .nodes
        .iter()
        .find(|n| n.id == draft.id)
        .unwrap()
        .workers
        .is_empty());
    let key = row.dispatch_key.clone().unwrap();
    assert_eq!(
        std::fs::read_to_string(project.path().join(".stateroot/local/launch-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    drop(service);
    std::fs::write(project.path().join(".stateroot/local/release-worker"), "go").unwrap();
    let record_path = project
        .path()
        .join(format!(".stateroot/delegations/{key}.json"));
    await_condition(|| {
        std::fs::read(&record_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .is_some_and(|r| r["outcome"] == "completed")
    });
    // Restart/repeated automatic observations preserve terminal attempt identity.
    let before: Value = serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    cli(config.path(), home.path(), project.path())
        .env("PATH", &path)
        .args(["plan", "advance"])
        .assert()
        .success();
    let after: Value = serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(before["attempt"], after["attempt"]);
    assert_eq!(
        std::fs::read_to_string(project.path().join(".stateroot/local/launch-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn uncertain_starting_and_failed_records_are_not_automatic_retry_intent() {
    let (config, home) = support::homes();
    let project = tempfile::tempdir().unwrap();
    support::init_project(config.path(), home.path(), project.path());
    let p = make_plan(project.path(), "uncertain", true);
    declare(
        config.path(),
        home.path(),
        project.path(),
        &p.id,
        json!({"dependencies":[],"assignment":assignment("uncertain-operation")}),
    );
    let row = plans::graph(project.path())
        .nodes
        .into_iter()
        .find(|n| n.id == p.id)
        .unwrap();
    let key = row.dispatch_key.unwrap();
    let dir = project.path().join(".stateroot/delegations");
    std::fs::create_dir_all(&dir).unwrap();
    let record_path = dir.join(format!("{key}.json"));
    let (_bin, path) = fake_harness();
    for outcome in [None, Some("failed"), Some("lost")] {
        let mut record = json!({"schema_version":"stateroot.delegation.v2","id":key,"plan_id":p.id,
            "status":"starting","attempt":1,"fingerprint":{"harness":"claude","task":"controlled worker task","worktree":null}});
        if let Some(outcome) = outcome {
            record["outcome"] = json!(outcome);
        }
        let bytes = serde_json::to_vec(&record).unwrap();
        std::fs::write(&record_path, &bytes).unwrap();
        cli(config.path(), home.path(), project.path())
            .env("PATH", &path)
            .args(["plan", "advance"])
            .assert()
            .success();
        assert_eq!(std::fs::read(&record_path).unwrap(), bytes);
        assert!(!project
            .path()
            .join(".stateroot/local/launch-count")
            .exists());
        if outcome.is_none() {
            cli(config.path(), home.path(), project.path())
                .env("PATH", &path)
                .args([
                    "delegate",
                    "--to",
                    "claude",
                    "--task",
                    "controlled worker task",
                    "--key",
                    &key,
                ])
                .assert()
                .failure();
            assert_eq!(std::fs::read(&record_path).unwrap(), bytes);
        }
    }
}

#[cfg(unix)]
fn automatic_fork_fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    plans::PlanMeta,
) {
    let (config, home) = support::homes();
    let workspace = tempfile::tempdir().unwrap();
    let trunk = workspace.path().join("trunk");
    let fork = workspace.path().join("executor");
    support::init_project(config.path(), home.path(), &trunk);
    let plan = make_plan(&trunk, "automatic fork operation", true);
    let mut binding = assignment("one-project-operation");
    binding["fork"] = json!("executor");
    declare(
        config.path(),
        home.path(),
        &trunk,
        &plan.id,
        json!({"dependencies":[], "assignment":binding}),
    );
    let base = stateroot_core::roots::create_root(&trunk, "codex", "dispatch fixture", None)
        .unwrap()
        .0
        .id;
    stateroot_core::roots::fork_root(&trunk, &base, Some("executor"), "codex").unwrap();
    stateroot_core::roots::fork_materialize(&trunk, "executor", &fork, Some(&plan.id)).unwrap();
    plans::transition(&fork, &plan.id, plans::PlanStatus::Active).unwrap();
    let registration = stateroot_core::config::lookup_project(config.path(), &trunk)
        .unwrap()
        .unwrap();
    stateroot_core::config::register_project(config.path(), &fork, registration).unwrap();
    (config, home, workspace, trunk, fork, plan)
}

#[cfg(unix)]
#[test]
fn automatic_operation_uses_one_reservation_across_trunk_and_executor_fork() {
    for scenario in [
        "trunk_then_fork",
        "fork_then_trunk",
        "service_and_fork_hook",
    ] {
        let (config, home, _workspace, trunk, fork, plan) = automatic_fork_fixture();
        let (_bin, path) = fake_harness();
        let invoke = |project: &Path| {
            let output = command(config.path(), home.path(), project, &path)
                .args(["hook", "--harness", "codex", "session_start"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        let service = if scenario == "service_and_fork_hook" {
            let service = Service(
                command(config.path(), home.path(), &trunk, &path)
                    .args(["service", "run"])
                    .spawn()
                    .unwrap(),
            );
            invoke(&fork);
            Some(service)
        } else {
            let (first, second) = if scenario == "trunk_then_fork" {
                (&trunk, &fork)
            } else {
                (&fork, &trunk)
            };
            invoke(first);
            await_condition(|| fork.join(".stateroot/local/launch-count").exists());
            invoke(second);
            None
        };
        let key = plans::graph(&trunk)
            .nodes
            .into_iter()
            .find(|n| n.id == plan.id)
            .unwrap()
            .dispatch_key
            .unwrap();
        let owner_record = trunk.join(format!(".stateroot/delegations/{key}.json"));
        let private_record = fork.join(format!(".stateroot/delegations/{key}.json"));
        await_condition(|| fork.join(".stateroot/local/launch-count").exists());
        // If the regression makes two reservations, wait for both controlled
        // workers before asserting so this is an actual duplicate-launch proof.
        if owner_record.exists() && private_record.exists() {
            await_condition(|| {
                std::fs::read_to_string(fork.join(".stateroot/local/launch-count"))
                    .unwrap()
                    .lines()
                    .count()
                    >= 2
            });
        }
        let count = std::fs::read_to_string(fork.join(".stateroot/local/launch-count"))
            .unwrap()
            .lines()
            .count();
        let trunk_view = plans::graph(&trunk)
            .nodes
            .into_iter()
            .find(|n| n.id == plan.id)
            .unwrap();
        let fork_view = plans::graph(&fork)
            .nodes
            .into_iter()
            .find(|n| n.id == plan.id)
            .unwrap();
        drop(service);
        std::fs::write(fork.join(".stateroot/local/release-worker"), "go").unwrap();
        assert_eq!(
            count, 1,
            "{scenario}: one authorized operation launched twice"
        );
        assert!(
            owner_record.exists(),
            "{scenario}: reservation must have one shared owner"
        );
        assert!(
            !private_record.exists(),
            "{scenario}: fork must not reserve separately"
        );
        assert_eq!(trunk_view.workers, fork_view.workers, "{scenario}");
        assert_eq!(fork_view.worker_state, "running", "{scenario}");
        await_condition(|| {
            std::fs::read(&owner_record)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .is_some_and(|r| r["outcome"] == "completed")
        });
        for project in [&trunk, &fork] {
            invoke(project);
        }
        assert_eq!(
            std::fs::read_to_string(fork.join(".stateroot/local/launch-count"))
                .unwrap()
                .lines()
                .count(),
            1,
            "terminal replay from either checkout"
        );
    }
}

#[cfg(unix)]
#[test]
fn stale_executor_cannot_revive_owner_abandonment_reassignment_or_unknown_attempt() {
    for scenario in [
        "abandoned",
        "reassigned",
        "unknown",
        "different_project",
        "registration_changed",
        "missing_fork_context",
    ] {
        let (config, home, _workspace, trunk, fork, plan) = automatic_fork_fixture();
        let (_bin, path) = fake_harness();
        match scenario {
            "abandoned" => {
                plans::transition(&trunk, &plan.id, plans::PlanStatus::Abandoned).unwrap();
            }
            "reassigned" => {
                let mut declaration = plans::load(&trunk, &plan.id)
                    .unwrap()
                    .0
                    .execution
                    .unwrap()
                    .declaration;
                declaration
                    .assignment
                    .as_mut()
                    .unwrap()
                    .dispatch
                    .as_mut()
                    .unwrap()
                    .task = "changed owner task".into();
                plans::declare_execution(
                    &trunk,
                    &plan.id,
                    &plans::revision(&trunk, &plan.id).unwrap(),
                    declaration,
                    "codex",
                )
                .unwrap();
            }
            "unknown" => {
                let key = plans::graph(&trunk)
                    .nodes
                    .into_iter()
                    .find(|n| n.id == plan.id)
                    .unwrap()
                    .dispatch_key
                    .unwrap();
                let directory = trunk.join(".stateroot/delegations");
                std::fs::create_dir_all(&directory).unwrap();
                std::fs::write(directory.join(format!("{key}.json")), serde_json::to_vec(&json!({"schema_version":"stateroot.delegation.v2", "id":key,"plan_id":plan.id,"status":"starting","attempt":1})).unwrap()).unwrap();
            }
            "missing_fork_context" => {
                std::fs::remove_file(fork.join(".stateroot/local/fork-context.json")).unwrap();
            }
            "registration_changed" => {
                let registry = trunk.join(".stateroot/local/fork-worktrees.json");
                let mut value: Value =
                    serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
                value["entries"]["executor"]["path"] = json!(trunk);
                std::fs::write(registry, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            "different_project" => {
                let manifest = fork.join(".stateroot/manifest.json");
                let mut value: Value =
                    serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
                value["project_id"] = json!("independent-project");
                std::fs::write(manifest, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            _ => unreachable!(),
        }
        let output = command(config.path(), home.path(), &fork, &path)
            .args(["hook", "--harness", "codex", "session_start"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let view = plans::graph(&fork)
            .nodes
            .into_iter()
            .find(|n| n.id == plan.id)
            .unwrap();
        std::fs::write(fork.join(".stateroot/local/release-worker"), "go").unwrap();
        assert_eq!(
            view.readiness, "blocked",
            "{scenario}: stale fork must not dispatch"
        );
        if scenario == "unknown" {
            assert_eq!(view.worker_state, "unknown");
        }
        assert!(
            !fork.join(".stateroot/local/launch-count").exists(),
            "{scenario}"
        );
    }
}

#[cfg(unix)]
#[test]
fn manual_fork_delegation_keeps_its_caller_owned_record() {
    let (config, home, _workspace, trunk, fork, _plan) = automatic_fork_fixture();
    let (_bin, path) = fake_harness();
    cli(config.path(), home.path(), &fork)
        .env("PATH", &path)
        .args([
            "delegate",
            "--to",
            "claude",
            "--task",
            "manual caller task",
            "--key",
            "manual-fork-key",
        ])
        .assert()
        .success();
    let record = fork.join(".stateroot/delegations/manual-fork-key.json");
    await_condition(|| fork.join(".stateroot/local/launch-count").exists());
    assert!(record.exists());
    assert!(!trunk
        .join(".stateroot/delegations/manual-fork-key.json")
        .exists());
    std::fs::write(fork.join(".stateroot/local/release-worker"), "go").unwrap();
    await_condition(|| {
        std::fs::read(&record)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .is_some_and(|r| r["outcome"] == "completed")
    });
}
