//! M4 integration tests — stdio MCP server round-trips, federation
//! pull→project flow, instruction block content, install registration.
//! Offline (the stdio server is a child process, no network).

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::{Command as ProcCommand, Stdio};

use assert_cmd::Command;
use serde_json::{json, Value};

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

/// Spawn the stdio server as a child; returns (stdin, stdout lines, child).
fn mcp_client(config_home: &Path, user_home: &Path, cwd: &Path) -> McpClient {
    let bin = assert_cmd::cargo::cargo_bin("stateroot");
    let mut child = ProcCommand::new(bin)
        .arg("mcp-stdio")
        .env("STATEROOT_HOME", config_home)
        .env("STATEROOT_TEST_HOME", user_home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_BASE")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp-stdio");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = BufReader::new(child.stdout.take().expect("stdout"));
    McpClient {
        stdin,
        stdout,
        child,
        next_id: 1,
    }
}

struct McpClient {
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    child: std::process::Child,
    next_id: i64,
}

impl McpClient {
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        writeln!(self.stdin, "{request}").expect("write request");
        self.stdin.flush().expect("flush");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read reply");
        serde_json::from_str(line.trim()).expect("reply json")
    }

    fn tool_text(reply: &Value) -> Value {
        let text = reply
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .expect("tool text");
        serde_json::from_str(text).expect("tool json")
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn stdio_round_trips_all_six_tools() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    // a canonical soul for soul_read
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["soul", "generate", "--yes", "--apply"])
        .assert()
        .success();
    // seed memory: one shared, one private (legacy memory.md migrates into MEMORY.md)
    let memory = project.path().join(".stateroot/memory.md");
    std::fs::write(
        &memory,
        "- the deploy port is 9060\n- salary figure <!-- visibility: private -->\n",
    )
    .expect("memory");

    let mut client = mcp_client(config_home.path(), user_home.path(), project.path());
    let init = client.call(
        "initialize",
        json!({"clientInfo": {"name": "cursor", "version": "1.0"}}),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], json!("stateroot"));

    let tools = client.call("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
        .collect();
    for expected in [
        "memory_save",
        "memory_recall",
        "memory",
        "wiki_show",
        "learn_record",
        "skill_propose",
        "soul_read",
        "learnings_list",
    ] {
        assert!(
            names.contains(&expected),
            "missing tool {expected}: {names:?}"
        );
    }

    // memory_save writes curated MEMORY.md (no quarantine)
    let saved = client.call(
        "tools/call",
        json!({"name": "memory_save", "arguments": {"content": "release train is fridays", "scope": "user", "visibility": "shared"}}),
    );
    let saved = McpClient::tool_text(&saved);
    assert_eq!(saved["quarantined"], json!(false), "{saved}");
    assert_eq!(saved["success"], json!(true), "{saved}");
    assert!(
        project
            .path()
            .join(".stateroot/memories/MEMORY.md")
            .is_file(),
        "MEMORY.md must exist after save"
    );

    // memory_recall (external): shared hit visible, private invisible
    let recall = client.call(
        "tools/call",
        json!({"name": "memory_recall", "arguments": {"query": "port"}}),
    );
    let recall = McpClient::tool_text(&recall);
    let hits = recall["hits"].as_array().expect("hits");
    assert!(
        hits.iter()
            .any(|h| h["note"].as_str().unwrap_or("").contains("9060")),
        "{recall}"
    );
    assert!(
        !hits
            .iter()
            .any(|h| h["note"].as_str().unwrap_or("").contains("salary")),
        "private must not leak: {recall}"
    );

    // learn_record → always a learning (no keyword reroute)
    let learned = client.call(
        "tools/call",
        json!({"name": "learn_record", "arguments": {"note": "actually the build uses --locked"}}),
    );
    let learned = McpClient::tool_text(&learned);
    assert_eq!(learned["kind"], json!("learning"), "{learned}");
    assert_eq!(learned["status"], json!("active"), "{learned}");
    assert!(learned["id"].as_str().is_some(), "{learned}");

    // skill_propose → activates and projects immediately
    let proposed = client.call(
        "tools/call",
        json!({"name": "skill_propose", "arguments": {"slug": "pdf-fu", "name": "Pdf Fu", "skill_md": "---\nname: pdf-fu\n---\n# Pdf Fu\n"}}),
    );
    let proposed = McpClient::tool_text(&proposed);
    assert_eq!(proposed["quarantined"], json!(false), "{proposed}");
    assert_eq!(proposed["lifecycle"], json!("active"), "{proposed}");
    let sidecar = std::fs::read_to_string(
        project
            .path()
            .join(".stateroot/skills/pdf-fu/skill.federation.json"),
    )
    .expect("sidecar");
    assert!(sidecar.contains("\"lifecycle\": \"active\""), "{sidecar}");

    // soul_read returns the caller-harness projection
    let soul = client.call("tools/call", json!({"name": "soul_read", "arguments": {}}));
    let soul = McpClient::tool_text(&soul);
    assert_eq!(soul["harness"], json!("cursor"), "{soul}");
    assert!(
        soul["projection"]
            .as_str()
            .unwrap_or("")
            .contains("## Working relationship"),
        "{soul}"
    );

    // learnings_list: all statuses visible
    let listed = client.call(
        "tools/call",
        json!({"name": "learnings_list", "arguments": {"scope": "project"}}),
    );
    let listed = McpClient::tool_text(&listed);
    assert_eq!(listed["gates"], json!("all"), "{listed}");
}

#[test]
fn federation_foreign_pull_projects_immediately() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // foreign skill in a harness root
    let skill_dir = user_home.path().join(".hermes/skills/tools/dep-lint");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: dep-lint\ndescription: Lint dependencies\n---\n# Dep Lint\n",
    )
    .expect("skill md");

    // pull → active + projected
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["skill", "sync"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        !stdout.contains("candidate_quarantined"),
        "sync must not quarantine: {stdout}"
    );
    let sidecar = std::fs::read_to_string(
        user_home
            .path()
            .join(".stateroot/skills/dep-lint/skill.federation.json"),
    )
    .expect("sidecar");
    assert!(sidecar.contains("\"lifecycle\": \"active\""), "{sidecar}");
    assert!(
        user_home
            .path()
            .join(".agents/skills/dep-lint/SKILL.md")
            .is_file(),
        "foreign skill must project immediately"
    );

    // promote is idempotent activation
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["skill", "promote", "dep-lint"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("activated"), "promote: {stdout}");
}

#[test]
fn instruction_block_and_install_register_stdio_mcp() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    std::fs::create_dir_all(user_home.path().join(".codex")).expect(".codex");
    std::fs::create_dir_all(user_home.path().join(".cursor")).expect(".cursor");
    std::fs::create_dir_all(user_home.path().join(".openclaw")).expect(".openclaw");

    stateroot(config_home.path(), user_home.path(), project.path())
        .arg("install")
        .assert()
        .success();

    // instruction block carries the self-improvement guidance
    let block =
        std::fs::read_to_string(user_home.path().join(".codex/AGENTS.md")).expect("AGENTS.md");
    assert!(block.contains("learn_record"), "block: {block}");
    assert!(block.contains("memory_save"), "block: {block}");
    assert!(block.contains("skill_propose"), "block: {block}");
    assert!(
        block.contains("Do not put taste in memory"),
        "block: {block}"
    );
    assert!(block.contains("product-intent"), "block: {block}");
    assert!(
        block.contains("no approve gate")
            || block.contains("activates immediately")
            || !block.contains("classify→approve"),
        "block must not invent an approval story: {block}"
    );

    // the harness MCP config registers the local stdio server (cursor has an
    // MCP target; codex's adapter deliberately registers none upstream)
    let cursor_mcp = user_home.path().join(".cursor/mcp.json");
    let contents = std::fs::read_to_string(&cursor_mcp).expect("cursor mcp.json");
    assert!(contents.contains("stateroot"), "{contents}");
    assert!(contents.contains("mcp-stdio"), "{contents}");

    // openclaw extension registration still points at the right place
    // (extensions/, never the invisible plugins/ path)
    let ext = user_home.path().join(".openclaw/extensions/stateroot");
    assert!(ext.is_dir(), "openclaw extension dir: {}", ext.display());
    let index = std::fs::read_to_string(ext.join("index.ts")).expect("extension index.ts");
    assert!(index.contains("stateroot"), "index.ts mentions stateroot");
    assert!(
        !user_home
            .path()
            .join(".openclaw/plugins/stateroot")
            .exists(),
        "no legacy plugins/ debris"
    );

    // `mcp tools` lists the local surface
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["mcp", "tools"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("memory_save"), "tools: {stdout}");
    assert!(stdout.contains("soul_read"), "tools: {stdout}");
}
