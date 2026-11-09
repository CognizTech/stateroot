//! Offline integration tests for the M2 git-plumbing roots surface.

// Each test binary includes this module but uses a subset of its helpers.
#[path = "support/mod.rs"]
mod support;

use std::path::Path;

use support::{init_project, stateroot};

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn root_hash(stdout: &str) -> String {
    stdout
        .lines()
        .find(|l| l.starts_with("root "))
        .expect("root line")
        .trim_start_matches("root ")
        .trim()
        .to_string()
}

#[test]
fn five_pillar_cli_relay_historical_branch_and_materialized_restore() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    let run = |cwd: &Path, args: &[&str], input: Option<String>| {
        let mut command = stateroot(config.path(), home.path(), cwd);
        command
            .env("STATEROOT_NO_AUTO_UPDATE", "1")
            .env("STATEROOT_TEST_CMD_PROBES", "cursor,claude")
            .args(args);
        if let Some(input) = input {
            command.write_stdin(input);
        }
        let result = command.assert().success();
        String::from_utf8(result.get_output().stdout.clone()).unwrap()
    };
    run(
        project.path(),
        &["soul", "propose", "--stdin"],
        Some("# Persona\n\nFixture human relationship.\n".into()),
    );
    write(
        project.path(),
        ".stateroot/soul/OVERLAY.md",
        "# Persona\n\nProject persona A.\n",
    );
    let plan_output = run(
        project.path(),
        &[
            "plan", "record", "--stdin", "--title", "relay", "--from", "codex",
        ],
        Some("# Relay\n\n- [ ] fixture work\n".into()),
    );
    let plan_id = plan_output.split_whitespace().nth(2).unwrap().to_string();
    run(project.path(), &["plan", "approve", &plan_id], None);
    run(project.path(), &["plan", "activate", &plan_id], None);
    write(
        project.path(),
        ".claude/skills/relay/SKILL.md",
        "---\nname: relay\ndescription: Fixture skill\n---\n\nSkill A.\n",
    );
    write(
        project.path(),
        ".claude/skills/relay/scripts/helper.sh",
        "printf 'fixture A'\n",
    );
    run(project.path(), &["skill", "sync", "--pull"], None);
    let skill = run(project.path(), &["skill", "show", "relay"], None);
    assert!(skill.contains("Skill A"));
    write(
        project.path(),
        ".vscode/mcp.json",
        &serde_json::json!({"servers":{"relay-tool":{"type":"stdio","command":"echo","args":["fixture A"]}}})
            .to_string(),
    );
    run(project.path(), &["mcp", "sync", "--pull"], None);
    run(project.path(), &["memory", "add", "memory A"], None);
    run(
        project.path(),
        &["learn", "record", "Prefer fixture A over fixture B."],
        None,
    );
    let learning_rows: serde_json::Value =
        serde_json::from_str(&run(project.path(), &["learnings", "list", "--json"], None)).unwrap();
    let learning_id = learning_rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["statement"] == "Prefer fixture A over fixture B.")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    write(project.path(), "work.txt", "A");
    let native = home
        .path()
        .join(".claude/projects/relay/relay-session.jsonl");
    std::fs::create_dir_all(native.parent().unwrap()).unwrap();
    std::fs::write(&native,serde_json::json!({"type":"user","sessionId":"relay-session","cwd":project.path(),"timestamp":"2026-10-09T01:00:00Z","message":{"role":"user","content":"Exact fixture context A"}}).to_string()).unwrap();
    let payload=serde_json::json!({"session_id":"relay-session","cwd":project.path(),"transcript_path":native,"tool_name":"Edit","message":"Exact fixture context A"}).to_string();
    run(
        project.path(),
        &["hook", "post_tool_use", "--harness", "claude"],
        Some(payload.clone()),
    );
    run(
        project.path(),
        &["hook", "session_end", "--harness", "claude"],
        Some(payload),
    );
    run(project.path(), &["_drain-finalize"], None);
    let a = stateroot_core::roots::latest_root(project.path())
        .unwrap()
        .unwrap();
    let receipt_a: serde_json::Value =
        serde_json::from_str(&run(project.path(), &["receipt", &a, "--json"], None)).unwrap();
    assert_eq!(receipt_a["activity"]["body"]["session_id"], "relay-session");
    assert_eq!(receipt_a["activity"]["provenance"], "native_observation");
    write(project.path(), "work.txt", "B");
    write(
        project.path(),
        ".stateroot/soul/OVERLAY.md",
        "# Persona\n\nProject persona B.\n",
    );
    run(
        project.path(),
        &["memory", "replace", "memory B", "--old", "memory A"],
        None,
    );
    run(
        project.path(),
        &[
            "learnings",
            "edit",
            &learning_id,
            "--statement",
            "Prefer fixture B over fixture A.",
        ],
        None,
    );
    write(
        project.path(),
        ".vscode/mcp.json",
        &serde_json::json!({"servers":{"relay-tool":{"type":"stdio","command":"echo","args":["fixture B"]}}})
            .to_string(),
    );
    run(project.path(), &["mcp", "sync", "--pull"], None);
    write(
        project.path(),
        ".claude/skills/relay/SKILL.md",
        "---\nname: relay\ndescription: Fixture skill\n---\n\nSkill B.\n",
    );
    run(project.path(), &["skill", "sync", "--pull"], None);
    let b = root_hash(&run(
        project.path(),
        &["snap", "--harness", "codex", "--reason", "B"],
        None,
    ));
    let branch = home.path().join("historical-C");
    run(
        project.path(),
        &[
            "fork",
            &a,
            "--branch",
            "historical-C",
            "--worktree",
            branch.to_str().unwrap(),
        ],
        None,
    );
    assert!(
        std::fs::read_to_string(branch.join(".stateroot/memories/MEMORY.md"))
            .unwrap()
            .contains("memory A")
    );
    assert!(
        std::fs::read_to_string(branch.join(".stateroot/soul/OVERLAY.md"))
            .unwrap()
            .contains("persona A")
    );
    write(&branch, "work.txt", "C");
    let c = root_hash(&run(
        &branch,
        &["snap", "--harness", "codex", "--reason", "C"],
        None,
    ));
    run(project.path(), &["compare", &b, &c], None);
    run(&branch, &["compare", &a, &c], None);
    run(&branch, &["receipt", &c, "--json"], None);
    run(&branch, &["show", &a, "--fidelity", "--json"], None);
    let repo = git2::Repository::open(project.path()).unwrap();
    let index_before = std::fs::read(repo.path().join("index")).ok();
    write(project.path(), ".staterootignore", "private.txt\n");
    write(project.path(), "private.txt", "private current bytes");
    run(project.path(), &["revert", &a, "--yes"], None);
    assert_eq!(
        std::fs::read_to_string(project.path().join("work.txt")).unwrap(),
        "A"
    );
    assert!(
        std::fs::read_to_string(project.path().join(".stateroot/memories/MEMORY.md"))
            .unwrap()
            .contains("memory A")
    );
    assert!(
        std::fs::read_to_string(project.path().join(".stateroot/soul/OVERLAY.md"))
            .unwrap()
            .contains("persona A")
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("private.txt")).unwrap(),
        "private current bytes"
    );
    assert_eq!(std::fs::read(repo.path().join("index")).ok(), index_before);
    assert!(run(project.path(), &["skill", "show", "relay"], None).contains("Skill A"));
    assert!(
        run(project.path(), &["learnings", "show", &learning_id], None)
            .contains("Prefer fixture A over fixture B.")
    );
    let tool_state = run(project.path(), &["mcp", "status", "--json"], None);
    assert!(
        tool_state.contains("relay-tool"),
        "tool status after restore: {tool_state}"
    );
    let tools = std::fs::read_to_string(project.path().join(".stateroot/tools/mcp.json")).unwrap();
    assert!(tools.contains("fixture A"));
    assert!(!tools.contains("fixture B"));
    let native_gone = native.with_extension("gone");
    std::fs::rename(&native, &native_gone).unwrap();
    let source_missing: serde_json::Value =
        serde_json::from_str(&run(project.path(), &["receipt", &a, "--json"], None)).unwrap();
    assert_eq!(
        source_missing["source"]["native_status"],
        "unavailable; exact native history is not pinned"
    );
    let captured = source_missing["source"]["event_id"].as_str().unwrap();
    assert!(
        run(project.path(), &["observations", "show", captured], None)
            .contains("Exact fixture context A")
    );
    let handoff = run(
        project.path(),
        &[
            "handoff",
            "write",
            "--from",
            "codex",
            "--no-plan",
            "--objective",
            "fixture relay",
            "--task",
            "inspect completion",
            "--context-summary",
            "No unrelated work is assigned by this fixture.",
        ],
        None,
    );
    assert!(handoff.contains("handoff"));
    let digest = run(project.path(), &["handoff", "show"], None);
    assert!(!digest.contains("## Assigned Plan"));
    assert!(digest.contains("not an execution instruction"));
}

#[test]
fn init_auto_inits_git_and_snap_log_show_flow() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // M2: non-git folder got a silent repo at init.
    assert!(project.path().join(".git").is_dir(), "auto git init");

    // state_only on a bare project (init's convenience layer files would
    // otherwise count; the bare manifest case is the honest empty tree).
    let bare = tempfile::tempdir().expect("bare project");
    std::fs::create_dir_all(bare.path().join(".stateroot")).expect("bare stateroot");
    std::fs::write(
        bare.path().join(".stateroot/manifest.json"),
        r#"{"schema_version":"stateroot.manifest.v1","project_id":"ws-bare","name":"bare","created_at":"2026-08-07T00:00:00Z"}"#,
    )
    .expect("bare manifest");
    let out = stateroot(config_home.path(), user_home.path(), bare.path())
        .args(["snap", "--reason", "genesis"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("coverage: state-only"), "snap: {stdout}");

    // full coverage first root (init wrote convenience-layer files).
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "genesis"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("coverage: files:"), "snap1: {stdout}");
    let first = root_hash(&stdout);

    // full coverage second root, parented on the first.
    write(project.path(), "src/main.rs", "fn main() {}\n");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "add main"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("coverage: files: 6 pinned"),
        "snap2: {stdout}"
    );
    let second = root_hash(&stdout);

    // log: lineage with coverage lines.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("log")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("## Roots (2)"), "log: {stdout}");
    assert!(stdout.contains(&second[..12]), "log: {stdout}");
    assert!(
        stdout.contains("[files: 5]"),
        "genesis coverage line: {stdout}"
    );

    // show by prefix.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["show", &second[..12]])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains(&format!("parents: {}", &first[..12])),
        "show: {stdout}"
    );
    assert!(stdout.contains("reason: add main"), "show: {stdout}");
}

#[test]
fn resume_names_source_drift_without_falsifying_the_historical_fact() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    write(
        project.path(),
        "spec.txt",
        "batch size is 32 (token HISTORICAL-RESULT-42)\n",
    );
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "memory",
            "add",
            "Historical measurement HISTORICAL-RESULT-42: batch size was 32 when spec.txt said so.",
        ])
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "historical spec"])
        .assert()
        .success();

    write(
        project.path(),
        "spec.txt",
        "batch size is 64 (token CURRENT-RESULT-64)\n",
    );
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "current spec"])
        .assert()
        .success();

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--force"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("HISTORICAL-RESULT-42"),
        "historical fact must remain: {stdout}"
    );
    assert!(
        !stdout.contains("HISTORICAL-RESULT-42 is false"),
        "a changed source must not falsify the fact: {stdout}"
    );
    assert!(
        stdout.contains("older records stay historical, not false"),
        "drift label missing: {stdout}"
    );
    assert!(
        stdout.contains("M spec.txt"),
        "changed path missing: {stdout}"
    );
    assert!(
        stdout.contains("stateroot diff") && stdout.contains("--content"),
        "exact-byte reader missing: {stdout}"
    );
}

#[test]
fn diff_content_revert_and_fork() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    write(project.path(), "a.txt", "one\ntwo\n");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "v1"])
        .assert()
        .success();
    let first = root_hash(&String::from_utf8(out.get_output().stdout.clone()).expect("utf8"));

    write(project.path(), "a.txt", "one\nTWO\n");
    write(project.path(), "b.txt", "new file\n");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "v2"])
        .assert()
        .success();
    let second = root_hash(&String::from_utf8(out.get_output().stdout.clone()).expect("utf8"));

    // names + status
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["diff", &first, &second])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("M a.txt"), "diff: {stdout}");
    assert!(stdout.contains("A b.txt"), "diff: {stdout}");

    // unified content
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["diff", &first, &second, "--content"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("### a.txt"), "content: {stdout}");
    assert!(stdout.contains("-two"), "content: {stdout}");
    assert!(stdout.contains("+TWO"), "content: {stdout}");

    // receipt for the second transition (verified tier = git delta).
    let transitions_dir = project.path().join(".stateroot/transitions");
    let tids: Vec<String> = std::fs::read_dir(&transitions_dir)
        .expect("transitions")
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".json").map(str::to_string))
        })
        .collect();
    assert_eq!(tids.len(), 2, "one transition per snap");
    let latest_tid = tids.iter().max().expect("two transitions");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["receipt", latest_tid])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("# Transition receipt"), "receipt: {stdout}");
    assert!(
        stdout.contains("## Verified (git diff)"),
        "receipt: {stdout}"
    );
    assert!(stdout.contains("M a.txt"), "receipt: {stdout}");

    // fork: branch ref + report (worktree materialization is opt-in).
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["fork", &first, "--branch", "claude-line"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("fork claude-line"), "fork: {stdout}");
    assert!(
        stdout.contains(&format!(
            "materialize with: stateroot fork {first} --worktree"
        )),
        "fork: {stdout}"
    );

    // append-only revert: new root with the v1 tree; v2 still listed.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["revert", &first[..12], "--yes"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("reverted to"), "revert: {stdout}");
    let a_txt = std::fs::read_to_string(project.path().join("a.txt")).expect("a.txt");
    assert_eq!(
        a_txt, "one\ntwo\n",
        "historical project bytes must actually be materialized"
    );
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .arg("log")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("## Roots (4)"),
        "append-only roots include pre-restore recovery: {stdout}"
    );
    assert!(
        stdout.contains(&second[..12]),
        "v2 root still present: {stdout}"
    );

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["compare", &first, &second])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("# Root compare"), "compare: {stdout}");
    assert!(
        stdout.contains("Verified diff (files)"),
        "compare: {stdout}"
    );
}

#[test]
fn log_json_projects_parallel_lineage_from_refs_and_records() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    write(project.path(), "work.txt", "base\n");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "base"])
        .assert()
        .success();
    let root = root_hash(&String::from_utf8(out.get_output().stdout.clone()).expect("utf8"));

    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["fork", &root, "--branch", "parallel-ui"])
        .assert()
        .success();

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["log", "--json"])
        .assert()
        .success();
    let projection: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("lineage json");
    assert_eq!(
        projection["schema_version"].as_str(),
        Some("stateroot.lineage.v1")
    );
    assert_eq!(projection["trunk"]["tip"].as_str(), Some(root.as_str()));
    let fork = projection["forks"]
        .as_array()
        .and_then(|forks| forks.iter().find(|fork| fork["name"] == "parallel-ui"))
        .expect("fork projection");
    assert_eq!(
        fork["ref"].as_str(),
        Some("refs/stateroot/forks/parallel-ui")
    );
    assert_eq!(fork["tip"].as_str(), Some(root.as_str()));
    assert_eq!(fork["base_root"].as_str(), Some(root.as_str()));
    assert_eq!(fork["contained"].as_bool(), Some(true));
    let root_entry = projection["roots"]
        .as_array()
        .and_then(|roots| roots.iter().find(|entry| entry["id"] == root))
        .expect("root projection");
    assert_eq!(root_entry["parents"].as_array().map(Vec::len), Some(0));
}

#[test]
fn merge_cleanup_json_and_human_contracts() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    write(project.path(), "src/main.rs", "fn main() {}\n");
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "base"])
        .assert()
        .success();
    let root = root_hash(&String::from_utf8(out.get_output().stdout.clone()).expect("utf8"));

    let worktree_tmp = tempfile::tempdir().expect("worktree tmp");
    let worktree = worktree_tmp.path().join("checkout");
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "fork",
            &root,
            "--name",
            "fork-a",
            "--worktree",
            &worktree.to_string_lossy(),
        ])
        .assert()
        .success();
    // Historical repair is ref-only: inspecting and removing a proven legacy
    // leak must retain the active detached checkout and its lineage.
    let repo = stateroot_core::roots::ensure_repo(project.path()).unwrap();
    let temporary = "refs/heads/stateroot-forktmp-fork-a";
    // Reconstruct the old writer's single creation event, rather than a new
    // branch inheriting the fixed writer's prior operation history.
    repo.reflog_delete(temporary).unwrap();
    repo.reference(
        temporary,
        git2::Oid::from_str(&root).unwrap(),
        false,
        "fork tmp branch",
    )
    .unwrap();
    let inspect = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["fork", "--cleanup", "--json"])
        .assert()
        .success();
    let inspection: serde_json::Value =
        serde_json::from_slice(&inspect.get_output().stdout).unwrap();
    assert_eq!(inspection["refs"][0]["state"], "eligible", "{inspection}");
    assert!(repo.find_reference(temporary).is_ok());
    let apply = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["fork", "--cleanup", "--apply", "--json"])
        .assert()
        .success();
    let applied: serde_json::Value = serde_json::from_slice(&apply.get_output().stdout).unwrap();
    assert_eq!(applied["schema_version"], "stateroot.fork-ref-cleanup.v1");
    assert_eq!(applied["refs"][0]["state"], "removed");
    assert!(repo.find_reference(temporary).is_err());
    assert!(worktree.exists());
    assert_eq!(
        stateroot_core::roots::latest_root(project.path())
            .unwrap()
            .as_deref(),
        Some(root.as_str())
    );
    write(&worktree, "src/lib.rs", "pub fn merged() {}\n");
    let capture = serde_json::json!({"cwd":worktree,"session_id":"retirement-session","tool_name":"Bash","tool_use_id":"retirement-tool-1","tool_input":{"command":"fixture observation"},"tool_response":"retained fixture output"});
    stateroot(config_home.path(), user_home.path(), &worktree)
        .args(["hook", "post_tool_use", "--harness", "claude"])
        .write_stdin(capture.to_string())
        .assert()
        .success();
    let park = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["fork", "--park", "fork-a", "--json"])
        .assert()
        .success();
    let parked: serde_json::Value = serde_json::from_slice(&park.get_output().stdout).unwrap();
    assert_eq!(parked["state"], "parked");
    assert!(!worktree.exists());
    let reopen = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["fork", "--reopen", "fork-a", "--json"])
        .assert()
        .success();
    let reopened: serde_json::Value = serde_json::from_slice(&reopen.get_output().stdout).unwrap();
    assert_eq!(reopened["fork"], "fork-a");
    assert_eq!(reopened["state"], "active");
    assert_eq!(
        std::fs::read_to_string(worktree.join("src/lib.rs")).unwrap(),
        "pub fn merged() {}\n"
    );
    let inspected = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["fork", "--inspect", "fork-a", "--json"])
        .assert()
        .success();
    let inspected: serde_json::Value =
        serde_json::from_slice(&inspected.get_output().stdout).unwrap();
    assert_eq!(inspected["retention"]["manifest"]["phase"], "retained");
    stateroot(config_home.path(), user_home.path(), &worktree)
        .args(["snap", "--reason", "fork work"])
        .assert()
        .success();

    let merge_out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["merge", "fork-a"])
        .assert()
        .success();
    let merge_stdout = String::from_utf8(merge_out.get_output().stdout.clone()).expect("utf8");
    assert!(
        merge_stdout.contains("stateroot merge --cleanup fork-a"),
        "human merge names the cleanup command: {merge_stdout}"
    );
    assert!(
        worktree.exists(),
        "merge must return without deleting the worktree"
    );

    let json_out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["merge", "--cleanup", "fork-a", "--json"])
        .assert()
        .success();
    let payload: serde_json::Value =
        serde_json::from_slice(&json_out.get_output().stdout).expect("cleanup json");
    assert_eq!(
        payload["schema_version"].as_str(),
        Some("stateroot.merge.cleanup.v1")
    );
    assert_eq!(payload["forks"][0]["name"].as_str(), Some("fork-a"));
    assert_eq!(payload["forks"][0]["cleaned"].as_bool(), Some(true));
    assert!(
        !worktree.exists(),
        "explicit cleanup removes the deferred worktree"
    );
}

#[test]
fn merge_prepare_status_continue_and_abort_contracts() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    write(project.path(), "src/main.rs", "fn main() {}\n");
    let root = root_hash(
        &String::from_utf8(
            stateroot(config_home.path(), user_home.path(), project.path())
                .args(["snap", "--reason", "base"])
                .assert()
                .success()
                .get_output()
                .stdout
                .clone(),
        )
        .expect("utf8"),
    );
    let temp = tempfile::tempdir().expect("worktree temp");
    let worktree = temp.path().join("fork");
    stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "fork",
            &root,
            "--name",
            "fork-a",
            "--worktree",
            &worktree.to_string_lossy(),
        ])
        .assert()
        .success();
    write(&worktree, "src/lib.rs", "pub fn merged() {}\n");
    stateroot(config_home.path(), user_home.path(), &worktree)
        .args(["snap", "--reason", "fork work"])
        .assert()
        .success();

    let prepared = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["merge", "--prepare", "fork-a", "--json"])
        .assert()
        .success();
    let attempt: serde_json::Value =
        serde_json::from_slice(&prepared.get_output().stdout).expect("attempt json");
    assert_eq!(
        attempt["schema_version"].as_str(),
        Some("stateroot.merge-attempt.v1")
    );
    assert_eq!(attempt["state"].as_str(), Some("ready"));
    let id = attempt["id"].as_str().expect("attempt id");

    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["merge", "--status", id, "--json"])
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["merge", "--continue", id, "--json"])
        .assert()
        .success();
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["merge", "--status", id])
        .assert()
        .failure();
}

#[test]
fn merge_conflicted_attempt_reconciles_through_worktree() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    write(project.path(), "src/main.rs", "fn main() {}\n");
    let root = root_hash(
        &String::from_utf8(
            stateroot(config_home.path(), user_home.path(), project.path())
                .args(["snap", "--reason", "base"])
                .assert()
                .success()
                .get_output()
                .stdout
                .clone(),
        )
        .expect("utf8"),
    );
    let temp = tempfile::tempdir().expect("worktree temp");
    for (name, body) in [
        ("left", "fn main() { left(); }\n"),
        ("right", "fn main() { right(); }\n"),
    ] {
        let worktree = temp.path().join(name);
        stateroot(config_home.path(), user_home.path(), project.path())
            .args([
                "fork",
                &root,
                "--name",
                name,
                "--worktree",
                &worktree.to_string_lossy(),
            ])
            .assert()
            .success();
        write(&worktree, "src/main.rs", body);
        stateroot(config_home.path(), user_home.path(), &worktree)
            .args(["snap", "--reason", "fork work"])
            .assert()
            .success();
    }

    let prepared = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["merge", "--prepare", "left", "right", "--json"])
        .assert()
        .success();
    let attempt: serde_json::Value =
        serde_json::from_slice(&prepared.get_output().stdout).expect("attempt json");
    assert_eq!(attempt["state"].as_str(), Some("attention"));
    assert_eq!(
        attempt["conflicts"][0]["kind"].as_str(),
        Some("both_modified")
    );
    assert_eq!(attempt["pending_forks"][0]["name"].as_str(), Some("right"));
    let id = attempt["id"].as_str().expect("attempt id").to_string();
    let worktree = attempt["worktree"].as_str().expect("worktree path");
    let rendered = std::fs::read_to_string(Path::new(worktree).join("src/main.rs"))
        .expect("rendered conflict");
    assert!(rendered.contains("<<<<<<<"), "markers: {rendered}");

    // Continuing with markers still in place fails closed.
    let blocked = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["merge", "--continue", &id])
        .assert()
        .failure();
    let stderr = String::from_utf8(blocked.get_output().stderr.clone()).expect("utf8");
    assert!(
        stderr.contains("conflict markers remain"),
        "marker gate: {stderr}"
    );

    // The appointed agent resolves in the worktree and continues with
    // evidence; the published trunk carries the reconciled bytes.
    write(
        Path::new(worktree),
        "src/main.rs",
        "fn main() { left(); right(); }\n",
    );
    let done = stateroot(config_home.path(), user_home.path(), project.path())
        .args([
            "merge",
            "--continue",
            &id,
            "--evidence",
            "cargo test",
            "--json",
        ])
        .assert()
        .success();
    let payload: serde_json::Value =
        serde_json::from_slice(&done.get_output().stdout).expect("continue json");
    assert_eq!(payload["merged_forks"][0]["name"].as_str(), Some("left"));
    assert_eq!(payload["merged_forks"][1]["name"].as_str(), Some("right"));
    let trunk = std::fs::read_to_string(project.path().join("src/main.rs")).expect("trunk");
    assert_eq!(trunk, "fn main() { left(); right(); }\n");
    assert!(!Path::new(worktree).exists(), "worktree consumed");
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["merge", "--status", &id])
        .assert()
        .failure();
}

#[test]
fn managed_fork_cli_keeps_local_work_when_project_removal_is_requested() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    write(project.path(), "work.txt", "base\n");
    let out = stateroot(config.path(), home.path(), project.path())
        .args(["snap", "--reason", "base"])
        .assert()
        .success();
    let root = root_hash(&String::from_utf8_lossy(&out.get_output().stdout));
    stateroot(config.path(), home.path(), project.path())
        .args(["fork", &root, "--name", "managed", "--worktree"])
        .assert()
        .success();
    let worktree = project.path().join(".stateroot/local/worktrees/managed");
    assert_eq!(std::fs::read(worktree.join("work.txt")).unwrap(), b"base\n");
    write(&worktree, "uncommitted.txt", "retain me\n");
    let owner = git2::Repository::open(project.path()).unwrap();
    let before = owner.refname_to_id("refs/stateroot/latest").unwrap();
    stateroot(config.path(), home.path(), project.path())
        .args(["remove", "--yes"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("managed fork worktrees remain"));
    assert_eq!(
        std::fs::read(worktree.join("uncommitted.txt")).unwrap(),
        b"retain me\n"
    );
    assert_eq!(
        owner.refname_to_id("refs/stateroot/latest").unwrap(),
        before
    );
    assert!(git2::Repository::open(&worktree)
        .unwrap()
        .head_detached()
        .unwrap());
}

#[test]
fn exact_revision_offer_review_publish_and_cleanup_preserve_source_identity() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    let run = |cwd: &Path, args: &[&str]| {
        let output = stateroot(config_home.path(), user_home.path(), cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stateroot {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let snapshot = |cwd: &Path, reason: &str| root_hash(&run(cwd, &["snap", "--reason", reason]));
    let prepare = |attempt: &str| {
        let output = run(project.path(), &["merge", "--prepare", attempt, "--json"]);
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        (value["id"].as_str().unwrap().to_string(), value)
    };
    let stage = |attempt: &str| {
        let output = run(project.path(), &["merge", "--stage", attempt, "--json"]);
        serde_json::from_str::<serde_json::Value>(&output).unwrap()
    };
    let verify = |candidate: &Path, plan: &str| {
        let output = stateroot(config_home.path(), user_home.path(), candidate)
            .args(["plan", "verify", plan, "--"])
            .arg(assert_cmd::cargo::cargo_bin("stateroot"))
            .arg("--version")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "candidate verification failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .find_map(|line| {
                line.strip_prefix("verification ")
                    .and_then(|line| line.split(':').next())
            })
            .unwrap()
            .to_string()
    };

    let recorded = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["plan", "record", "--stdin", "--title", "Review B"])
        .write_stdin("# Review exact B\n")
        .output()
        .unwrap();
    assert!(recorded.status.success());
    let recorded = String::from_utf8(recorded.stdout).unwrap();
    let plan = recorded
        .strip_prefix("recorded plan ")
        .and_then(|line| line.split(' ').next())
        .unwrap()
        .to_string();
    run(project.path(), &["plan", "approve", &plan]);
    write(project.path(), "README.md", "target T\n");
    let target = snapshot(project.path(), "target T");
    let checkouts = tempfile::tempdir().expect("checkouts");
    let contributor = checkouts.path().join("contributor");
    run(
        project.path(),
        &[
            "fork",
            &target,
            "--name",
            "contributor",
            "--worktree",
            &contributor.to_string_lossy(),
        ],
    );
    write(&contributor, "contribution.txt", "offered revision B\n");
    let offered = snapshot(&contributor, "offered B");
    run(
        &contributor,
        &[
            "handoff",
            "write",
            "--from",
            "codex",
            "--to",
            "kimi",
            "--no-plan",
            "--objective",
            "Review exactly B",
            "--task",
            &format!("Review root B={offered} against target T={target}; do not substitute later D"),
            "--context-summary",
            "Offer immutable root B and its exact packet identity; local review is not Cloud approval.",
            "--next",
            "Pin B in a separate submission fork and review it against the explicit target",
        ],
    );
    let handoffs = run(&contributor, &["handoff", "list"]);
    let offer = handoffs
        .lines()
        .find(|line| line.contains("codex") && line.contains("kimi"))
        .expect("offer handoff");
    let offer_seq = offer.split_whitespace().next().unwrap().to_string();
    let offer_packet = offer
        .split_whitespace()
        .nth(4)
        .expect("offer packet id")
        .to_string();

    let submission = checkouts.path().join("submission-b");
    run(
        project.path(),
        &[
            "fork",
            &offered,
            "--name",
            "submission-b",
            "--worktree",
            &submission.to_string_lossy(),
        ],
    );
    write(&contributor, "contribution.txt", "later revision D\n");
    write(&contributor, "later-d.txt", "not reviewed\n");
    let later = snapshot(&contributor, "later D");
    assert_ne!(offered, later);
    let repo = git2::Repository::open(project.path()).unwrap();
    assert_eq!(
        repo.refname_to_id("refs/stateroot/forks/submission-b")
            .unwrap()
            .to_string(),
        offered
    );
    assert_eq!(
        repo.refname_to_id("refs/stateroot/forks/contributor")
            .unwrap()
            .to_string(),
        later
    );

    let (stale_attempt, prepared) = prepare("submission-b");
    assert_eq!(prepared["trunk_tip"], target);
    assert_eq!(prepared["forks"][0]["tip"], offered);
    stage(&stale_attempt);
    write(project.path(), "target-t2.txt", "target T2\n");
    let target_two = snapshot(project.path(), "target T2");
    let stale = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["merge", "--continue", &stale_attempt, "--json"])
        .output()
        .unwrap();
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stderr).contains("stale: trunk moved"));
    assert_eq!(
        repo.refname_to_id("refs/stateroot/forks/submission-b")
            .unwrap()
            .to_string(),
        offered
    );
    run(project.path(), &["merge", "--abort", &stale_attempt]);

    let (attempt, prepared) = prepare("submission-b");
    assert_eq!(prepared["trunk_tip"], target_two);
    assert_eq!(prepared["forks"][0]["tip"], offered);
    let staged = stage(&attempt);
    let candidate = staged["worktree"].as_str().unwrap().to_string();
    let candidate = Path::new(&candidate);
    assert_eq!(
        std::fs::read_to_string(candidate.join("contribution.txt")).unwrap(),
        "offered revision B\n"
    );
    assert_eq!(
        std::fs::read_to_string(candidate.join("target-t2.txt")).unwrap(),
        "target T2\n"
    );
    assert!(!candidate.join("later-d.txt").exists());

    verify(candidate, &plan);
    write(candidate, "contribution.txt", "post-check edit\n");
    let edited = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["merge", "--continue", &attempt, "--json"])
        .output()
        .unwrap();
    assert!(!edited.status.success());
    assert!(String::from_utf8_lossy(&edited.stderr)
        .contains("candidate differs from sealed staged tree"));
    let doctor = run(candidate, &["doctor", "--json"]);
    let doctor: serde_json::Value = serde_json::from_str(&doctor).unwrap();
    assert_eq!(doctor["authoritative_state"]["verification"]["stale"], 1);

    write(candidate, "contribution.txt", "offered revision B\n");
    stage(&attempt);
    let verification = verify(candidate, &plan);
    stage(&attempt);
    let offer_evidence = format!("offer handoff #{offer_seq} packet {offer_packet} at {offered}");
    let verification_evidence =
        format!("verification {verification} checked the restored B+T2 candidate");
    let published = run(
        project.path(),
        &[
            "merge",
            "--continue",
            &attempt,
            "--evidence",
            &offer_evidence,
            "--evidence",
            &verification_evidence,
            "--json",
        ],
    );
    let published: serde_json::Value = serde_json::from_str(&published).unwrap();
    let merged = published["trunk_root"].as_str().unwrap().to_string();
    let merge_root = stateroot_core::roots::get_root(project.path(), &merged).unwrap();
    assert_eq!(
        merge_root.parents,
        vec![target_two.clone(), offered.clone()]
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("contribution.txt")).unwrap(),
        "offered revision B\n"
    );
    assert!(!project.path().join("later-d.txt").exists());
    assert!(!candidate.exists());

    let cleanup = run(
        project.path(),
        &["merge", "--cleanup", "submission-b", "--json"],
    );
    let cleanup: serde_json::Value = serde_json::from_str(&cleanup).unwrap();
    assert_eq!(cleanup["forks"][0]["cleaned"], true);
    assert!(!submission.exists());
    let sealed_tip = repo
        .refname_to_id("refs/stateroot/forks/submission-b")
        .unwrap()
        .to_string();
    let sealed = stateroot_core::roots::get_root(project.path(), &sealed_tip).unwrap();
    assert_eq!(sealed.metadata_status, "recorded");
    assert_eq!(
        sealed.metadata_availability,
        Some(stateroot_core::roots::RootMetadataAvailability::CheckoutSidecar)
    );
    for root in [&offered, &later] {
        let manifest = stateroot_core::roots::get_root(project.path(), root).unwrap();
        assert_eq!(manifest.metadata_status, "legacy_unknown");
        assert_eq!(
            manifest.metadata_availability,
            Some(stateroot_core::roots::RootMetadataAvailability::CheckoutSidecarAbsentGitRetained)
        );
    }
    let lineage = stateroot_core::roots::lineage_projection(project.path()).unwrap();
    let offered_projection = lineage["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|root| root["id"] == offered)
        .unwrap();
    assert_eq!(offered_projection["metadata_status"], "legacy_unknown");
    assert_eq!(
        offered_projection["metadata_availability"],
        "checkout_sidecar_absent_git_retained"
    );
    let historical = run(&contributor, &["diff", &offered, &later, "--content"]);
    assert!(historical.contains("later revision D"));

    let shown = run(&contributor, &["handoff", "show", &offer_seq]);
    let offered_short: String = offered.chars().take(12).collect();
    let later_short: String = later.chars().take(12).collect();
    assert!(
        shown.contains(&format!("Handoff root: `{offered_short}`")),
        "{shown}"
    );
    assert!(
        shown.contains(&format!("Current checkout root: `{later_short}`")),
        "{shown}"
    );
    let done = run(
        project.path(),
        &[
            "plan",
            "done",
            &plan,
            "--verification",
            &verification,
            "--evidence",
            "reviewed exact B against T2; D excluded; merge and verification receipts inspected",
        ],
    );
    assert!(done.contains(&merged[..12]), "{done}");
    run(
        project.path(),
        &[
            "handoff",
            "write",
            "--from",
            "kimi",
            "--no-plan",
            "--objective",
            "Exact B integrated",
            "--task",
            &format!("Inspect merge {merged} and offer packet {offer_packet}; D={later} excluded"),
            "--context-summary",
            &format!("offer={offer_packet}; B={offered}; T={target}; T2={target_two}; merge={merged}; verification={verification}; submission cleanup retained source and receipts"),
        ],
    );
    let result = run(project.path(), &["handoff", "show"]);
    assert!(result.contains(&offer_packet));
    assert!(result.contains(&merged));
}

#[cfg(target_os = "linux")]
#[test]
fn inspected_legacy_migration_uses_the_installed_fork_command() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    write(project.path(), ".gitignore", "target/\n.stateroot/\n");
    write(
        project.path(),
        "Cargo.toml",
        "[package]\nname='migration_cli_fixture'\nversion='0.1.0'\nedition='2021'\n",
    );
    write(project.path(), "src/lib.rs", "pub fn value()->u32{7}\n");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let build = |cwd: &Path, args: &[&str]| {
        let result = std::process::Command::new(&cargo)
            .args(args)
            .current_dir(cwd)
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET_DIR")
            .env_remove("CARGO_BUILD_BUILD_DIR")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    build(project.path(), &["generate-lockfile", "--offline"]);
    let snapped = stateroot(config.path(), home.path(), project.path())
        .args(["snap"])
        .assert()
        .success();
    let root = root_hash(&String::from_utf8_lossy(&snapped.get_output().stdout));
    stateroot(config.path(), home.path(), project.path())
        .args(["fork", &root, "--name", "legacy", "--worktree"])
        .assert()
        .success();
    let checkout = project.path().join(".stateroot/local/worktrees/legacy");
    build(&checkout, &["build", "--locked", "--offline"]);
    write(&checkout, "uncommitted.txt", "keep this source\n");
    let observed = stateroot(config.path(), home.path(), project.path())
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .env_remove("CARGO_BUILD_BUILD_DIR")
        .args(["fork", "--migrate-output", "legacy", "--json"])
        .assert()
        .success();
    let inspection: serde_json::Value =
        serde_json::from_slice(&observed.get_output().stdout).unwrap();
    let mut payload = serde_json::json!({"fork":"legacy","expected_tip":"1111111111111111111111111111111111111111",
        "expected_inventory":inspection["inventory"],"reason":"inspected disposable Cargo build fixture"});
    stateroot(config.path(), home.path(), project.path())
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .env_remove("CARGO_BUILD_BUILD_DIR")
        .args([
            "fork",
            "--migrate-output",
            "legacy",
            "--input",
            "-",
            "--json",
        ])
        .write_stdin(payload.to_string())
        .assert()
        .failure();
    assert!(checkout.join("target/CACHEDIR.TAG").exists());
    payload["expected_tip"] = inspection["tip"].clone();
    stateroot(config.path(), home.path(), project.path())
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .env_remove("CARGO_BUILD_BUILD_DIR")
        .env(
            "STATEROOT_BUILD_CACHE_ROOT",
            project.path().join(".stateroot/local/migration-cache"),
        )
        .args([
            "fork",
            "--migrate-output",
            "legacy",
            "--input",
            "-",
            "--json",
        ])
        .write_stdin(payload.to_string())
        .assert()
        .success();
    assert!(!checkout.join("target").exists());
    assert_eq!(
        std::fs::read(checkout.join("uncommitted.txt")).unwrap(),
        b"keep this source\n"
    );
    assert_eq!(
        std::fs::read(checkout.join("src/lib.rs")).unwrap(),
        b"pub fn value()->u32{7}\n"
    );
    let repo = git2::Repository::open(project.path()).unwrap();
    assert_eq!(
        repo.refname_to_id("refs/stateroot/forks/legacy")
            .unwrap()
            .to_string(),
        inspection["tip"]
    );
}
