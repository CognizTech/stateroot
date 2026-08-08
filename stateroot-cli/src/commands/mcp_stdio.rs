//! `stateroot mcp-stdio` — the local stdio MCP server (M4).
//!
//! Line-delimited JSON-RPC over stdin/stdout, backed ENTIRELY by local
//! stores (memory files, learnings, proposals, soul). No HTTP anywhere.
//! The six tools mirror the server's W8 surface with identical
//! names/semantics; writes from external harnesses are quarantined
//! (session-candidate/private) until a human approves.

use serde_json::{json, Value};

use super::Ctx;

/// Tool definitions shared by `tools/list` and `stateroot mcp tools`
/// (name, description for agent consumption, input schema).
pub const TOOL_DEFS: &[(&str, &str, &str)] = &[
    (
        "memory_save",
        "Save a durable fact for future sessions. Call when the user states a fact worth remembering (deadline, version, preference of record). External-harness writes are quarantined (session-candidate, private) until approved.",
        r#"{"type":"object","properties":{"content":{"type":"string"},"scope":{"type":"string","enum":["user","project"]},"visibility":{"type":"string","enum":["shared","private"]}},"required":["content"]}"#,
    ),
    (
        "memory_recall",
        "Recall durable facts relevant to the current task. Call before answering from memory: returns only what your harness is permitted to see (scoped gates).",
        r#"{"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer"}},"required":["query"]}"#,
    ),
    (
        "learn_record",
        "Record a correction or lesson into the learning loop. Call after the user corrects you or when a procedure worked. Files a proposal — never a direct write.",
        r#"{"type":"object","properties":{"note":{"type":"string"},"as_kind":{"type":"string","enum":["soul","memory","skill","learning"]}},"required":["note"]}"#,
    ),
    (
        "skill_propose",
        "Propose a reusable skill from a procedure that worked end-to-end. Creates a quarantined candidate + approval proposal; never activates anything.",
        r#"{"type":"object","properties":{"slug":{"type":"string"},"name":{"type":"string"},"skill_md":{"type":"string"},"rationale":{"type":"string"}},"required":["slug","skill_md"]}"#,
    ),
    (
        "soul_read",
        "Read the working-relationship projection for YOUR harness (tone/principles/boundaries). Call at session start to orient.",
        r#"{"type":"object","properties":{}}"#,
    ),
    (
        "learnings_list",
        "List learnings (durable preferences, corrections) for self-orientation. Candidates surface nowhere until approved.",
        r#"{"type":"object","properties":{"scope":{"type":"string","enum":["user","project"]},"status":{"type":"string"},"limit":{"type":"integer"}}}"#,
    ),
];

/// Run the stdio server until stdin closes.
pub async fn run(ctx: &Ctx) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))?;
    let mut caller_harness = "skillsagent".to_string();
    let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            break; // EOF — client closed the transport
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(err) => {
                let fallback = error_fallback(Value::Null, -32700, &format!("parse error: {err}"));
                stdout.write_all(format!("{fallback}\n").as_bytes()).await?;
                stdout.flush().await?;
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let is_notification = request.get("id").is_none();

        let reply = match method {
            "initialize" => {
                if let Some(name) = request
                    .pointer("/params/clientInfo/name")
                    .and_then(|v| v.as_str())
                {
                    caller_harness = stateroot_core::skill_federation::normalize_harness(name);
                }
                Some(ok(
                    id,
                    json!({
                        "protocolVersion": "2024-11-05",
                        "serverInfo": {"name": "stateroot", "version": env!("CARGO_PKG_VERSION")},
                        "capabilities": {"tools": {}},
                    }),
                ))
            }
            "ping" => Some(ok(id, json!({}))),
            "tools/list" => Some(ok(id, json!({"tools": tool_list()}))),
            "tools/call" => {
                let name = request
                    .pointer("/params/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or(json!({}));
                let external = caller_harness != "skillsagent" && caller_harness != "cli";
                Some(call_tool(
                    ctx,
                    &home,
                    &caller_harness,
                    external,
                    id,
                    name,
                    &args,
                ))
            }
            _ if is_notification => None, // notifications get no reply
            _ => Some(error_fallback(
                id,
                -32601,
                &format!("method not found: {method}"),
            )),
        };
        if let Some(reply) = reply {
            stdout.write_all(format!("{reply}\n").as_bytes()).await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

fn ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_fallback(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": format!("stateroot mcp-stdio: {message}")}
    })
}

fn tool_list() -> Vec<Value> {
    TOOL_DEFS
        .iter()
        .map(|(name, description, schema)| {
            json!({
                "name": name,
                "description": description,
                "inputSchema": serde_json::from_str::<Value>(schema).unwrap_or(json!({"type":"object"})),
            })
        })
        .collect()
}

fn call_tool(
    ctx: &Ctx,
    home: &std::path::Path,
    caller: &str,
    external: bool,
    id: Value,
    name: &str,
    args: &Value,
) -> Value {
    let text = match name {
        "memory_save" => memory_save(ctx, home, external, args),
        "memory_recall" => memory_recall(ctx, home, external, args),
        "learn_record" => learn_record(ctx, args),
        "skill_propose" => skill_propose(ctx, home, caller, args),
        "soul_read" => soul_read(home, caller),
        "learnings_list" => learnings_list(ctx, home, external, args),
        _ => return error_fallback(id, -32602, &format!("unknown tool: {name}")),
    };
    ok(id, json!({"content": [{"type": "text", "text": text}]}))
}

fn memory_save(ctx: &Ctx, home: &std::path::Path, external: bool, args: &Value) -> String {
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if content.is_empty() {
        return json!({"error": "content is required"}).to_string();
    }
    let (scope, visibility, quarantined) = if external {
        // Foreign writes are always quarantined — recorded honestly.
        ("session_candidate", "private", true)
    } else {
        (
            args.get("scope")
                .and_then(|v| v.as_str())
                .unwrap_or("project"),
            args.get("visibility")
                .and_then(|v| v.as_str())
                .unwrap_or("shared"),
            false,
        )
    };
    let path = if quarantined {
        let dir = stateroot_core::local_store::root(&ctx.cwd).join("learnings/_candidates");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("memory.md")
    } else if scope == "user" {
        home.join(".stateroot/memory.md")
    } else {
        stateroot_core::local_store::root(&ctx.cwd).join("memory.md")
    };
    let marker = if visibility == "private" || quarantined {
        " <!-- visibility: private -->"
    } else {
        ""
    };
    let mut body = std::fs::read_to_string(&path).unwrap_or_default();
    if !body.ends_with('\n') && !body.is_empty() {
        body.push('\n');
    }
    body.push_str(&format!("- {content}{marker}\n"));
    match std::fs::write(&path, body) {
        Ok(()) => json!({
            "saved": true,
            "quarantined": quarantined,
            "scope": scope,
            "visibility": visibility,
            "path": path.display().to_string(),
        })
        .to_string(),
        Err(err) => json!({"error": format!("write failed: {err}")}).to_string(),
    }
}

fn memory_recall(ctx: &Ctx, home: &std::path::Path, external: bool, args: &Value) -> String {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
    let terms: Vec<&str> = query.split_whitespace().filter(|t| t.len() > 2).collect();
    let mut hits: Vec<Value> = Vec::new();
    let mut scan = |path: &std::path::Path, scope: &str| {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        for line in text.lines() {
            let line = line.trim();
            if !line.starts_with('-') {
                continue;
            }
            let private = line.contains("<!-- visibility: private -->");
            if external && private {
                continue; // foreign harnesses never see private notes
            }
            let lower = line.to_lowercase();
            let score = terms.iter().filter(|t| lower.contains(*t)).count();
            if score > 0 || terms.is_empty() {
                hits.push(json!({
                    "note": line.trim_start_matches('-').trim(),
                    "scope": scope,
                    "visibility": if private { "private" } else { "shared" },
                    "score": score,
                }));
            }
        }
    };
    scan(
        &stateroot_core::local_store::root(&ctx.cwd).join("memory.md"),
        "project",
    );
    scan(&home.join(".stateroot/memory.md"), "user");
    hits.sort_by(|a, b| {
        b.get("score")
            .and_then(|v| v.as_u64())
            .cmp(&a.get("score").and_then(|v| v.as_u64()))
    });
    hits.truncate(limit);
    json!({"hits": hits, "gates": if external { "shared only" } else { "owner" }}).to_string()
}

fn learn_record(ctx: &Ctx, args: &Value) -> String {
    let note = args
        .get("note")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if note.is_empty() {
        return json!({"error": "note is required"}).to_string();
    }
    let forced = args.get("as_kind").and_then(|v| v.as_str());
    let class = match forced {
        Some(kind) => stateroot_core::learnings::Classification {
            kind: kind.to_string(),
            category: match kind {
                "soul" => "identity",
                "skill" => "procedures",
                "memory" => "facts",
                _ => "general",
            }
            .to_string(),
        },
        None => stateroot_core::learnings::classify_note(note),
    };
    let payload = match class.kind.as_str() {
        "learning" => {
            let candidate = stateroot_core::learnings::Learning::candidate(
                note,
                &class.category,
                0.45,
                "mcp learn_record",
                "project",
            );
            json!({
                "id": candidate.id,
                "statement": candidate.statement,
                "category": candidate.category,
                "confidence": candidate.confidence,
                "label": candidate.label,
                "sources": candidate.sources,
                "scope": candidate.scope,
            })
        }
        _ => json!({"content": note, "scope": "project", "origin": "mcp learn_record"}),
    };
    match stateroot_core::proposals::create(
        &ctx.cwd,
        &class.kind,
        &format!(
            "{}: {}",
            class.kind,
            note.chars().take(60).collect::<String>()
        ),
        &format!(
            "classified as {} ({}) via mcp learn_record",
            class.kind, class.category
        ),
        payload,
        json!({"route": "mcp learn_record"}),
    ) {
        Ok(proposal) => json!({
            "classification": {"kind": class.kind, "category": class.category},
            "proposal_id": proposal.id,
            "status": "pending",
        })
        .to_string(),
        Err(err) => json!({"error": format!("proposal failed: {err}")}).to_string(),
    }
}

fn skill_propose(ctx: &Ctx, home: &std::path::Path, caller: &str, args: &Value) -> String {
    let slug = args
        .get("slug")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let skill_md = args
        .get("skill_md")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let name = args.get("name").and_then(|v| v.as_str()).unwrap_or(slug);
    let rationale = args
        .get("rationale")
        .and_then(|v| v.as_str())
        .unwrap_or("mcp skill_propose");
    if slug.is_empty() || skill_md.is_empty() {
        return json!({"error": "slug and skill_md are required"}).to_string();
    }
    // Candidate package (quarantined — lifecycle candidate, never projected).
    let root = stateroot_core::local_store::root(&ctx.cwd)
        .join("skills")
        .join(slug);
    if let Err(err) = std::fs::create_dir_all(&root) {
        return json!({"error": format!("create candidate dir: {err}")}).to_string();
    }
    if let Err(err) = std::fs::write(root.join("SKILL.md"), skill_md) {
        return json!({"error": format!("write SKILL.md: {err}")}).to_string();
    }
    let meta = json!({
        "schema_version": "stateroot.skill_package.v1",
        "slug": slug,
        "scope": "project",
        "lifecycle": "candidate",
        "origin": {"harness": caller, "source_kind": "mcp_proposal"},
        "ownership_class": "harness_authored",
        "native_harness": caller,
    });
    if let Err(err) = std::fs::write(
        root.join("skill.federation.json"),
        serde_json::to_string_pretty(&meta).unwrap(),
    ) {
        return json!({"error": format!("write sidecar: {err}")}).to_string();
    }
    let _ = home;
    match stateroot_core::proposals::create(
        &ctx.cwd,
        "skill",
        &format!("skill candidate: {name}"),
        rationale,
        json!({"slug": slug, "name": name, "scope": "project"}),
        json!({"route": "mcp skill_propose"}),
    ) {
        Ok(proposal) => json!({
            "candidate": slug,
            "proposal_id": proposal.id,
            "quarantined": true,
            "activates": "never — approve with `stateroot proposals approve`",
        })
        .to_string(),
        Err(err) => json!({"error": format!("proposal failed: {err}")}).to_string(),
    }
}

fn soul_read(home: &std::path::Path, caller: &str) -> String {
    match stateroot_core::soul::read_canonical(home) {
        Some(soul) => {
            let harness = if caller == "cli" { None } else { Some(caller) };
            json!({
                "harness": caller,
                "projection": stateroot_core::soul::render_projection(&soul, harness),
            })
            .to_string()
        }
        None => {
            json!({"projection": "", "note": "no canonical soul yet (stateroot soul generate)"})
                .to_string()
        }
    }
}

fn learnings_list(ctx: &Ctx, home: &std::path::Path, external: bool, args: &Value) -> String {
    let scope = args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("project");
    let status = args.get("status").and_then(|v| v.as_str());
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let learnings = stateroot_core::learnings::read_scope(&ctx.cwd, home, scope);
    let rows: Vec<Value> = learnings
        .iter()
        .filter(|l| {
            // Foreign harnesses see only active learnings (candidates surface
            // nowhere); the owner may filter freely.
            let gate_ok = !external || l.status == "active";
            gate_ok && status.map(|s| l.status == s).unwrap_or(true)
        })
        .take(limit)
        .map(|l| {
            json!({
                "id": l.id,
                "statement": l.statement,
                "category": l.category,
                "confidence": l.confidence,
                "status": l.status,
                "scope": l.scope,
            })
        })
        .collect();
    json!({"learnings": rows, "scope": scope, "gates": if external { "active only" } else { "owner" }})
        .to_string()
}
