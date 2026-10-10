//! Offline integration tests for the M2 git-plumbing roots surface.

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

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
}

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
    assert!(stdout.contains("--worktree <path>"), "fork: {stdout}");

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
    write(&worktree, "src/lib.rs", "pub fn merged() {}\n");
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
