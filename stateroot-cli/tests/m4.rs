//! M4 integration tests — stdio MCP server round-trips, federation
//! pull→project flow, instruction block content, install registration.
//! Offline (the stdio server is a child process, no network).

// Each test binary includes this module but uses a subset of its helpers.
#[path = "support/mod.rs"]
mod support;

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::{Command as ProcCommand, Stdio};

use serde_json::{json, Value};
use support::{init_project, stateroot};

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

#[test]
fn native_session_inspection_is_passive_and_rejects_foreign_or_unsupported_receipts() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    let archive_id = "a".repeat(64);
    let operation_id = "8b69e8dd-af53-4a38-8efd-087543660781";
    let operations =
        stateroot_core::sessions::codex_native::archive_dir(project.path()).join("operations");
    std::fs::create_dir_all(&operations).unwrap();
    let path = operations.join(format!("{operation_id}.json"));
    let receipt = json!({
        "schema":"stateroot.codex-restore.v1", "operation_id":operation_id,
        "archive_id":archive_id, "source_thread_id":"source", "stage":"prefix_verified",
        "destination_thread_id":"destination", "destination_session_id":null,
        "destination_path":null, "prefix_digest":"prefix", "native_turns_digest":null,
        "effective_config_digest":null, "effective_envelope":null, "observed_continuation":null
    });
    let original = serde_json::to_vec(&receipt).unwrap();
    std::fs::write(&path, &original).unwrap();
    let mut client = mcp_client(config.path(), home.path(), project.path());
    client.call(
        "initialize",
        json!({"clientInfo":{"name":"cursor","version":"1"}}),
    );
    let inspect = |client: &mut McpClient, arguments: Value| {
        client.call(
            "tools/call",
            json!({"name":"native_session_inspect","arguments":arguments}),
        )
    };
    let response = inspect(
        &mut client,
        json!({"archive_id":archive_id,"operation_id":operation_id}),
    );
    let value = McpClient::tool_text(&response);
    assert_eq!(value["native_archive_verified"], false);
    assert_eq!(value["historical_prefix_verified"], false);
    assert!(value["unavailable"].is_string());
    assert!(value["observation"]
        .as_str()
        .unwrap()
        .contains("not reverified"));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let foreign = inspect(
        &mut client,
        json!({"archive_id":"b".repeat(64),"operation_id":operation_id}),
    );
    assert_eq!(foreign["result"]["isError"], true);
    let missing = inspect(
        &mut client,
        json!({"archive_id":archive_id,"operation_id":"37bde2d0-7186-4bf4-8a92-e44815bac604"}),
    );
    assert_eq!(missing["result"]["isError"], true);
    let mut unsupported = receipt;
    unsupported["schema"] = json!("unsupported");
    std::fs::write(&path, serde_json::to_vec(&unsupported).unwrap()).unwrap();
    let response = inspect(
        &mut client,
        json!({"archive_id":archive_id,"operation_id":operation_id}),
    );
    assert_eq!(response["result"]["isError"], true);
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
fn structured_inspection_and_skill_authoring_roundtrip_over_stdio() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    std::fs::write(project.path().join("example.txt"), "first").unwrap();
    let snap_context = stateroot_core::snap_context::SnapContext {
        home: home.path().to_path_buf(),
        components: vec![stateroot_core::fidelity::EnvironmentComponent {
            name: "stateroot-cli".into(),
            status: "available".into(),
            observed_version: None,
        }],
        ..Default::default()
    };
    let (root, transition) =
        stateroot_core::roots::create_root(project.path(), "cli", "test", Some(&snap_context))
            .unwrap();
    let mut client = mcp_client(config.path(), home.path(), project.path());
    client.call(
        "initialize",
        json!({"clientInfo":{"name":"cursor","version":"1"}}),
    );
    let list = client.call("tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    for name in [
        "root_inspect",
        "native_session_inspect",
        "root_compare",
        "transition_inspect",
        "handoff_inspect",
        "handoff_list",
        "verification_inspect",
        "plan_inspect",
        "plan_graph",
        "skill_inspect",
        "skill_propose",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == name),
            "missing {name}"
        );
    }
    let call = |client: &mut McpClient, name: &str, args: Value| {
        client.call("tools/call", json!({"name":name,"arguments":args}))
    };
    let root_reply = call(&mut client, "root_inspect", json!({"root":root.id}));
    let value = McpClient::tool_text(&root_reply);
    assert_eq!(value["root"]["id"], root.id);
    assert_eq!(
        value["root"],
        serde_json::to_value(stateroot_core::roots::get_root(project.path(), &root.id).unwrap())
            .unwrap()
    );
    let cli_fidelity = ProcCommand::new(assert_cmd::cargo::cargo_bin("stateroot"))
        .args(["show", &root.id, "--fidelity", "--json"])
        .env("STATEROOT_HOME", config.path())
        .env("STATEROOT_TEST_HOME", home.path())
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(
        cli_fidelity.status.success(),
        "{}",
        String::from_utf8_lossy(&cli_fidelity.stderr)
    );
    let cli_fidelity: Value = serde_json::from_slice(&cli_fidelity.stdout).unwrap();
    assert_eq!(value["fidelity"], cli_fidelity);
    assert!(value["fidelity"]["components"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["current"].is_object()));
    let diff = McpClient::tool_text(&call(
        &mut client,
        "root_compare",
        json!({"from":root.id,"to":root.id}),
    ));
    assert_eq!(diff["from_root"], root.id);
    let receipt = McpClient::tool_text(&call(
        &mut client,
        "transition_inspect",
        json!({"id":transition.id}),
    ));
    assert_eq!(
        receipt,
        stateroot_core::roots::receipt_projection(project.path(), &transition.id).unwrap()
    );
    let missing = call(&mut client, "verification_inspect", json!({"id":"missing"}));
    assert_eq!(missing["result"]["isError"], true);
    let graph = McpClient::tool_text(&call(&mut client, "plan_graph", json!({})));
    assert_eq!(
        graph,
        serde_json::to_value(stateroot_core::plans::graph(project.path())).unwrap()
    );
    let write = call(
        &mut client,
        "skill_propose",
        json!({"slug":"demo","skill_md":"# Demo\nUseful procedure.","files_base64":{"references/example.txt":"aGVsbG8="},"evidence":[{"kind":"root","reference":root.id}]}),
    );
    assert_ne!(write["result"]["isError"], true, "{write}");
    assert_eq!(McpClient::tool_text(&write)["lifecycle"], "active");
    let inspect = McpClient::tool_text(&call(
        &mut client,
        "skill_inspect",
        json!({"selector":"demo","file":"references/example.txt"}),
    ));
    assert_eq!(inspect["content_base64"], "aGVsbG8=");
    assert!(!inspect["evidence_assessments"]
        .as_array()
        .unwrap()
        .is_empty());
    let reference = home.path().join(".cursor/skills-cursor/native-only");
    std::fs::create_dir_all(&reference).unwrap();
    std::fs::write(
        reference.join("SKILL.md"),
        "---\nname: native-only\ndescription: native capability\n---\n# Native\n",
    )
    .unwrap();
    let native = McpClient::tool_text(&call(
        &mut client,
        "skill_inspect",
        json!({"selector":"native-only","file":"SKILL.md"}),
    ));
    assert_eq!(native["availability"], "reference_only");
    assert_eq!(native["route"]["native_harness"], "cursor");
    assert!(native["content_base64"].is_null());
    let invalid = call(
        &mut client,
        "skill_propose",
        json!({"slug":"../invalid","skill_md":"bad"}),
    );
    assert_eq!(invalid["result"]["isError"], true);
}

#[test]
fn structured_receipts_report_ambiguity_and_stale_verification() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    let meta =
        stateroot_core::plans::record(project.path(), "wire", "cli", None, "plan body").unwrap();
    stateroot_core::plans::transition(
        project.path(),
        &meta.id,
        stateroot_core::plans::PlanStatus::Approved,
    )
    .unwrap();
    let record =
        stateroot_core::verification::start(project.path(), &meta.id, "cli", &["example".into()])
            .unwrap();
    let record =
        stateroot_core::verification::finish(project.path(), record, Some(0), None, vec![])
            .unwrap();
    stateroot_core::plans::complete_with_verification(
        project.path(),
        &meta.id,
        "checked",
        "cli",
        None,
        std::slice::from_ref(&record.id),
    )
    .unwrap();
    let first = json!({"seq":77,"created_at":"2026-01-01T00:00:00Z","created_by_harness":"cli","task":"first"});
    let second = json!({"seq":77,"created_at":"2026-01-01T00:00:01Z","created_by_harness":"codex","task":"second"});
    stateroot_core::local_store::write_handoff_local(project.path(), &first).unwrap();
    stateroot_core::local_store::write_handoff_local(project.path(), &second).unwrap();
    let mut client = mcp_client(config.path(), home.path(), project.path());
    client.call(
        "initialize",
        json!({"clientInfo":{"name":"cursor","version":"1"}}),
    );
    let call = |client: &mut McpClient, name: &str, args: Value| {
        client.call("tools/call", json!({"name":name,"arguments":args}))
    };
    let plan = McpClient::tool_text(&call(&mut client, "plan_inspect", json!({"id":meta.id})));
    assert_eq!(plan["plan"]["id"], meta.id);
    assert_eq!(plan["plan"]["status"], "done");
    assert_eq!(
        plan["plan"]["completion_receipt"]["verification_ids"][0],
        record.id
    );
    let observed = McpClient::tool_text(&call(
        &mut client,
        "verification_inspect",
        json!({"id":record.id}),
    ));
    assert_eq!(observed["inputs_match"], true);
    assert_eq!(observed["semantic_acceptance"], "not established");
    std::fs::write(project.path().join("changed.txt"), "changed").unwrap();
    let stale = McpClient::tool_text(&call(
        &mut client,
        "verification_inspect",
        json!({"id":record.id}),
    ));
    assert_eq!(stale["inputs_match"], false);
    let ambiguous = call(&mut client, "handoff_inspect", json!({"seq":77}));
    assert_eq!(ambiguous["result"]["isError"], true);
    assert!(McpClient::tool_text(&ambiguous)["error"]
        .as_str()
        .unwrap()
        .contains("ambiguous"));
    let page = McpClient::tool_text(&call(&mut client, "handoff_list", json!({"limit":1})));
    assert_eq!(page["total"], 2);
    assert_eq!(page["next_offset"], 1);
    let id = page["entries"][0]["id"].as_str().unwrap();
    let exact = McpClient::tool_text(&call(&mut client, "handoff_inspect", json!({"id":id})));
    assert_eq!(exact["packet"]["task"], "second");
    let next = McpClient::tool_text(&call(
        &mut client,
        "handoff_list",
        json!({"limit":1,"offset":1}),
    ));
    assert!(next["next_offset"].is_null());
    let exact = McpClient::tool_text(&call(
        &mut client,
        "handoff_inspect",
        json!({"id":next["entries"][0]["id"]}),
    ));
    assert_eq!(exact["packet"]["task"], "first");
    let bad_page = call(&mut client, "handoff_list", json!({"limit":0}));
    assert_eq!(bad_page["result"]["isError"], true);
    let current = McpClient::tool_text(&call(&mut client, "handoff_inspect", json!({})));
    assert_eq!(current["packet"]["task"], "second");
    stateroot_core::local_store::write_handoff_local(project.path(), &first).unwrap();
    let restored = McpClient::tool_text(&call(&mut client, "handoff_list", json!({"limit":1})));
    assert_eq!(restored["entries"][0]["is_current"], true);
    assert_eq!(restored["entries"][0]["id"], restored["current_id"]);
    let current = McpClient::tool_text(&call(&mut client, "handoff_inspect", json!({})));
    assert_eq!(current["packet"]["task"], "first");
    assert_eq!(current["id"], restored["current_id"]);
    let malformed = call(&mut client, "handoff_inspect", json!({"seq":"77"}));
    assert_eq!(malformed["result"]["isError"], true);
    assert_eq!(
        McpClient::tool_text(&malformed)["error"],
        "seq must be an integer"
    );
    let unknown = call(&mut client, "root_inspect", json!({"root":"not-a-root"}));
    assert_eq!(unknown["result"]["isError"], true);
}

#[test]
fn degraded_memory_recall_preserves_visible_limit_over_stdio() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    init_project(config.path(), home.path(), project.path());
    let entries = vec![
        "VisibilityCanary private first <!-- visibility: private -->".to_string(),
        "VisibilityCanary shared second".to_string(),
    ];
    std::fs::write(
        project.path().join(".stateroot/memories/MEMORY.md"),
        stateroot_core::hot_apex::join_entries(&entries),
    )
    .unwrap();
    let db = project.path().join(".stateroot/local/memory.sqlite");
    if db.is_file() {
        std::fs::remove_file(&db).unwrap();
    }
    std::fs::create_dir_all(db).unwrap();
    for (caller, expected, visibility) in [
        ("cursor", "shared second", "shared"),
        ("cli", "private first", "private"),
    ] {
        let mut client = mcp_client(config.path(), home.path(), project.path());
        client.call(
            "initialize",
            json!({"clientInfo": {"name": caller, "version": "1.0"}}),
        );
        let reply = client.call(
            "tools/call",
            json!({"name":"memory_recall", "arguments":{"query":"VisibilityCanary", "limit":1}}),
        );
        let result = McpClient::tool_text(&reply);
        let hits = result["hits"].as_array().expect("fallback hits");
        assert_eq!(hits.len(), 1, "{caller}: {result}");
        assert!(
            hits[0]["note"].as_str().unwrap().contains(expected),
            "{caller}: {result}"
        );
        assert_eq!(hits[0]["visibility"], visibility);
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

    // memory_save honors scope: user writes global MEMORY.md, never project memory.
    let saved = client.call(
        "tools/call",
        json!({"name": "memory_save", "arguments": {"content": "release train is fridays", "scope": "user", "visibility": "shared"}}),
    );
    let saved = McpClient::tool_text(&saved);
    assert_eq!(saved["quarantined"], json!(false), "{saved}");
    assert_eq!(saved["success"], json!(true), "{saved}");
    assert_eq!(saved["scope"], json!("user"), "{saved}");
    let global_memory = user_home.path().join(".stateroot/memories/MEMORY.md");
    assert!(
        global_memory.is_file(),
        "user-scoped MEMORY.md must exist after save"
    );
    let global_text = std::fs::read_to_string(&global_memory).expect("global MEMORY.md");
    assert!(
        global_text.contains("release train is fridays"),
        "{global_text}"
    );
    let project_text =
        std::fs::read_to_string(project.path().join(".stateroot/memories/MEMORY.md"))
            .expect("project MEMORY.md");
    assert!(
        !project_text.contains("release train is fridays"),
        "user-scoped fact leaked into project memory: {project_text}"
    );

    let project_saved = client.call(
        "tools/call",
        json!({"name": "memory_save", "arguments": {"content": "project deploys from main", "scope": "project"}}),
    );
    let project_saved = McpClient::tool_text(&project_saved);
    assert_eq!(project_saved["success"], json!(true), "{project_saved}");
    assert_eq!(project_saved["scope"], json!("project"), "{project_saved}");
    let project_text =
        std::fs::read_to_string(project.path().join(".stateroot/memories/MEMORY.md"))
            .expect("project MEMORY.md");
    assert!(
        project_text.contains("project deploys from main"),
        "{project_text}"
    );

    // User-global shared memory is indexed for recall in every project.
    let global_recall = client.call(
        "tools/call",
        json!({"name": "memory_recall", "arguments": {"query": "release train"}}),
    );
    let global_recall = McpClient::tool_text(&global_recall);
    assert!(
        global_recall["hits"]
            .as_array()
            .is_some_and(|hits| hits.iter().any(|hit| hit["scope"] == "user"
                && hit["note"]
                    .as_str()
                    .unwrap_or("")
                    .contains("release train is fridays"))),
        "{global_recall}"
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
    let id = listed["learnings"].as_array().unwrap().first().unwrap()["id"]
        .as_str()
        .unwrap();
    let shown = client.call(
        "tools/call",
        json!({"name":"learnings_show", "arguments":{"id":id,"scope":"project"}}),
    );
    let shown = McpClient::tool_text(&shown);
    assert_eq!(shown["id"], id);
    assert!(shown["sources"].is_string());
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
    std::fs::create_dir_all(user_home.path().join(".pi/agent")).expect(".pi");

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
        block.contains("unpiped") || block.contains("| head"),
        "instruction block must forbid truncating resume: {block}"
    );
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

    let pi_plugin = user_home.path().join(".pi/agent/extensions/stateroot.ts");
    let pi_src = std::fs::read_to_string(&pi_plugin).expect("pi plugin");
    assert!(
        pi_src.contains("customType: \"stateroot\""),
        "pi plugin must inject a session message: {pi_src}"
    );
    assert!(
        pi_src.contains("before_agent_start"),
        "pi plugin must listen for before_agent_start: {pi_src}"
    );
    assert!(
        !pi_src.contains("prependContext"),
        "pi must not use the OMP prependContext return: {pi_src}"
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
