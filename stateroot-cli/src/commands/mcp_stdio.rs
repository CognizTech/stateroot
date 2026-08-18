//! `stateroot mcp-stdio` — the local stdio MCP server (M4).
//!
//! Line-delimited JSON-RPC over stdin/stdout, backed ENTIRELY by local
//! stores (memory files, learnings, proposals, soul). No HTTP anywhere.

use serde_json::{json, Value};

use super::Ctx;

/// Tool definitions shared by `tools/list` and `stateroot mcp tools`.
pub const TOOL_DEFS: &[(&str, &str, &str)] = &[
    (
        "memory_save",
        "Save a durable fact into curated MEMORY.md (add alias). Prefer the `memory` tool for replace/remove. Not taste — those go to learn_record.",
        r#"{"type":"object","properties":{"content":{"type":"string"},"scope":{"type":"string","enum":["user","project"]},"visibility":{"type":"string","enum":["shared","private"]},"target":{"type":"string","enum":["memory","user"]}},"required":["content"]}"#,
    ),
    (
        "memory",
        "Curate hot-apex MEMORY.md or USER.md. Actions: add, replace, remove, show. Caps: MEMORY 8000 / USER 4000 chars — overflow errors so you consolidate. Never writes soul.",
        r#"{"type":"object","properties":{"action":{"type":"string","enum":["add","replace","remove","show"]},"target":{"type":"string","enum":["memory","user"]},"content":{"type":"string"},"old_text":{"type":"string"},"visibility":{"type":"string","enum":["shared","private"]}},"required":["action"]}"#,
    ),
    (
        "memory_recall",
        "Recall durable facts from curated memory, wiki pages, episodic, and transcripts (FTS). Call before answering from memory.",
        r#"{"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer"}},"required":["query"]}"#,
    ),
    (
        "wiki_show",
        "Read one compiled wiki page body (path like memories/pages/auth.md or a slug).",
        r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
    ),
    (
        "learn_record",
        "Record one durable preference (taste). Format: '<prefer X over Y / never Z>. <when…>.' scope=user|workspace|project|domain:<slug>. Activates immediately. Facts go to memory_save / memory.",
        r#"{"type":"object","properties":{"note":{"type":"string"},"scope":{"type":"string"}},"required":["note"]}"#,
    ),
    (
        "skill_propose",
        "Propose a reusable skill from a procedure that worked end-to-end. Activates and projects immediately.",
        r#"{"type":"object","properties":{"slug":{"type":"string"},"name":{"type":"string"},"skill_md":{"type":"string"},"rationale":{"type":"string"}},"required":["slug","skill_md"]}"#,
    ),
    (
        "soul_read",
        "Read the working-relationship projection for YOUR harness (tone/principles/boundaries). Call at session start to orient.",
        r#"{"type":"object","properties":{}}"#,
    ),
    (
        "learnings_list",
        "List learnings (durable preferences, corrections) for self-orientation. Active notes are inherited by every harness.",
        r#"{"type":"object","properties":{"scope":{"type":"string"},"status":{"type":"string"},"limit":{"type":"integer"}}}"#,
    ),
    (
        "observations_list",
        "Read-only audit of raw hook-captured observations from .stateroot/spool/observations.jsonl. Provenance/debug only — not primary memory.",
        r#"{"type":"object","properties":{"kind":{"type":"string"},"harness":{"type":"string"},"query":{"type":"string"},"limit":{"type":"integer"}}}"#,
    ),
];

/// Run the stdio server until stdin closes.
pub async fn run(ctx: &Ctx) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))?;
    stateroot_core::hot_apex::ensure_migrated(&ctx.cwd, &home);
    let _ = stateroot_core::wiki::ensure_layout(&ctx.cwd);
    let _ = stateroot_core::memory_index::rebuild(&ctx.cwd, &home);
    let mut caller_harness = "statesmith".to_string();
    let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            break;
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
                        "serverInfo": {"name": "stateroot", "version": crate::cli::BUILD_VERSION},
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
                let external = caller_harness != "statesmith" && caller_harness != "cli";
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
            _ if is_notification => None,
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
        "memory_save" => memory_save(ctx, home, args),
        "memory" => memory_tool(ctx, home, args),
        "memory_recall" => memory_recall(ctx, home, external, args),
        "wiki_show" => wiki_show(ctx, args),
        "learn_record" => learn_record(ctx, home, args),
        "skill_propose" => skill_propose(ctx, home, caller, args),
        "soul_read" => soul_read(home, caller),
        "learnings_list" => learnings_list(ctx, home, external, args),
        "observations_list" => observations_list(ctx, args),
        _ => return error_fallback(id, -32602, &format!("unknown tool: {name}")),
    };
    ok(id, json!({"content": [{"type": "text", "text": text}]}))
}

fn mutation_json(r: stateroot_core::hot_apex::MutationResult) -> Value {
    json!({
        "success": r.success,
        "saved": r.success,
        "noop": r.noop,
        "error": r.error,
        "usage": r.usage,
        "path": r.path.as_ref().map(|p| p.display().to_string()),
        "current_entries": r.current_entries,
        "quarantined": false,
    })
}

fn memory_save(ctx: &Ctx, home: &std::path::Path, args: &Value) -> String {
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if content.is_empty() {
        return json!({"error": "content is required"}).to_string();
    }
    // Facts always land in curated MEMORY.md (not USER.md / soul).
    let target = args
        .get("target")
        .and_then(|v| v.as_str())
        .unwrap_or("memory");
    let target = if target == "user" { "user" } else { "memory" };
    let private = args.get("visibility").and_then(|v| v.as_str()) == Some("private");
    let scope = args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("project");
    stateroot_core::hot_apex::ensure_migrated(&ctx.cwd, home);
    // Legacy scope=user wrote ~/.stateroot/memory.md — that migrates into project
    // MEMORY. New writes with target=memory always hit MEMORY.md.
    let write_target = if target == "user" { "user" } else { "memory" };
    match stateroot_core::hot_apex::add(&ctx.cwd, home, write_target, content, private) {
        Ok(r) => {
            let _ = stateroot_core::memory_index::rebuild_if_needed(&ctx.cwd, home);
            let mut v = mutation_json(r);
            if let Some(obj) = v.as_object_mut() {
                obj.insert("scope".into(), json!(scope));
                obj.insert(
                    "visibility".into(),
                    json!(if private { "private" } else { "shared" }),
                );
            }
            v.to_string()
        }
        Err(err) => json!({"error": format!("{err}")}).to_string(),
    }
}

fn memory_tool(ctx: &Ctx, home: &std::path::Path, args: &Value) -> String {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let target = args
        .get("target")
        .and_then(|v| v.as_str())
        .unwrap_or("memory");
    let private = args.get("visibility").and_then(|v| v.as_str()) == Some("private");
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let old = args
        .get("old_text")
        .or_else(|| args.get("old"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    stateroot_core::hot_apex::ensure_migrated(&ctx.cwd, home);
    let result = match action {
        "add" => stateroot_core::hot_apex::add(&ctx.cwd, home, target, content, private),
        "replace" => {
            stateroot_core::hot_apex::replace(&ctx.cwd, home, target, old, content, private)
        }
        "remove" => stateroot_core::hot_apex::remove(&ctx.cwd, home, target, old),
        "show" => {
            return match stateroot_core::hot_apex::show(&ctx.cwd, home, target) {
                Ok(text) => json!({"text": text}).to_string(),
                Err(err) => json!({"error": format!("{err}")}).to_string(),
            };
        }
        _ => return json!({"error": "action must be add|replace|remove|show"}).to_string(),
    };
    match result {
        Ok(r) => {
            let _ = stateroot_core::memory_index::rebuild_if_needed(&ctx.cwd, home);
            mutation_json(r).to_string()
        }
        Err(err) => json!({"error": format!("{err}")}).to_string(),
    }
}

fn memory_recall(ctx: &Ctx, home: &std::path::Path, external: bool, args: &Value) -> String {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
    stateroot_core::hot_apex::ensure_migrated(&ctx.cwd, home);
    match stateroot_core::memory_index::search(&ctx.cwd, home, query, limit, !external) {
        Ok(hits) => {
            let hits: Vec<Value> = hits
                .into_iter()
                .map(|h| {
                    json!({
                        "note": h.text,
                        "kind": h.kind,
                        "path": h.path,
                        "scope": if h.kind == "user" { "user" } else { "project" },
                        "visibility": if h.private { "private" } else { "shared" },
                        "score": h.score,
                    })
                })
                .collect();
            json!({
                "hits": hits,
                "gates": if external { "shared only" } else { "owner" },
            })
            .to_string()
        }
        Err(err) => json!({"error": format!("{err}")}).to_string(),
    }
}

fn wiki_show(ctx: &Ctx, args: &Value) -> String {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return json!({"error": "path is required"}).to_string();
    }
    match stateroot_core::wiki::show(&ctx.cwd, path) {
        Ok(body) => json!({"path": path, "body": body}).to_string(),
        Err(err) => json!({"error": format!("{err}")}).to_string(),
    }
}

fn learn_record(ctx: &Ctx, home: &std::path::Path, args: &Value) -> String {
    let note = args
        .get("note")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if note.is_empty() {
        return json!({"error": "note is required"}).to_string();
    }
    let scope = args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("project")
        .trim();
    let scope = match scope {
        "" | "project" => "project",
        "user" => "user",
        "workspace" => "workspace",
        other if other.starts_with("domain:") => other,
        "domain" => "domain",
        other => {
            return json!({"error": format!("unknown scope '{other}' — use project, user, workspace, domain, or domain:<slug>")})
                .to_string();
        }
    };
    match stateroot_core::learnings::record_note(&ctx.cwd, home, note, scope, "mcp learn_record") {
        Ok((id, new, category)) => json!({
            "kind": "learning",
            "classification": {"kind": "learning", "category": category},
            "status": "active",
            "scope": scope,
            "id": id,
            "new": new,
            "activated": true,
        })
        .to_string(),
        Err(err) => json!({"error": format!("record failed: {err}")}).to_string(),
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
    let root = stateroot_core::local_store::root(&ctx.cwd)
        .join("skills")
        .join(slug);
    if let Err(err) = std::fs::create_dir_all(&root) {
        return json!({"error": format!("create skill dir: {err}")}).to_string();
    }
    if let Err(err) = std::fs::write(root.join("SKILL.md"), skill_md) {
        return json!({"error": format!("write SKILL.md: {err}")}).to_string();
    }
    let meta = json!({
        "schema_version": "stateroot.skill_package.v1",
        "slug": slug,
        "scope": "project",
        "lifecycle": "active",
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
    let _ = stateroot_core::skill_federation::activate_skill(&ctx.cwd, home, "project", slug);
    let options = stateroot_core::skill_federation::SyncOptions {
        dry_run: false,
        push: true,
        pull: false,
        cmd_probe: None,
    };
    let _ = stateroot_core::skill_federation::sync_project(&ctx.cwd, &options, Some(home));
    let _ = stateroot_core::proposals::create(
        &ctx.cwd,
        "skill",
        &format!("skill activated: {name}"),
        rationale,
        json!({"slug": slug, "name": name, "scope": "project"}),
        json!({"route": "mcp skill_propose", "status": "active"}),
    );
    json!({
        "candidate": slug,
        "lifecycle": "active",
        "quarantined": false,
        "activates": "immediately",
    })
    .to_string()
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

fn learnings_list(ctx: &Ctx, home: &std::path::Path, _external: bool, args: &Value) -> String {
    let scope = args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("project");
    let status = args.get("status").and_then(|v| v.as_str());
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let learnings = stateroot_core::learnings::read_scope(&ctx.cwd, home, scope);
    let rows: Vec<Value> = learnings
        .iter()
        .filter(|l| status.map(|s| l.status == s).unwrap_or(true))
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
    json!({"learnings": rows, "scope": scope, "gates": "all"}).to_string()
}

fn observations_list(ctx: &Ctx, args: &Value) -> String {
    if !stateroot_core::local_store::is_stateroot_dir(&ctx.cwd) {
        return json!({"error": "not a stateroot project — run from an initialized project root"})
            .to_string();
    }
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let rows = stateroot_core::observations::filter_spool(
        &ctx.cwd,
        &stateroot_core::observations::ObservationFilter {
            kind: args
                .get("kind")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            harness: args
                .get("harness")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            since: None,
            until: None,
            query: args
                .get("query")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            limit,
        },
    );
    let payload: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.id,
                "ts": row.ts,
                "event": row.event,
                "harness": row.harness,
                "kind_hint": row.kind_hint,
                "tool": row.tool,
                "excerpt": row.excerpt,
                "scope_status": row.scope_status,
            })
        })
        .collect();
    json!({"observations": payload, "read_only": true}).to_string()
}
