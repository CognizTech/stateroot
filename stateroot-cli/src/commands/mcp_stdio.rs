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
        "Save a durable fact into curated MEMORY.md (add alias). scope=project writes the current project's memory; scope=user writes ~/.stateroot/memories/MEMORY.md. target=user writes USER.md. Prefer the `memory` tool for replace/remove. Not taste — those go to learn_record.",
        r#"{"type":"object","properties":{"content":{"type":"string"},"scope":{"type":"string","enum":["user","project"]},"visibility":{"type":"string","enum":["shared","private"]},"target":{"type":"string","enum":["memory","user"]}},"required":["content"]}"#,
    ),
    (
        "memory",
        "Curate project/user-scoped MEMORY.md or global USER.md. Actions: add, replace, remove, show. scope=user + target=memory writes global memory. Caps: MEMORY 8000 / USER 4000 chars — overflow errors so you consolidate. Never writes soul.",
        r#"{"type":"object","properties":{"action":{"type":"string","enum":["add","replace","remove","show"]},"scope":{"type":"string","enum":["user","project"]},"target":{"type":"string","enum":["memory","user"]},"content":{"type":"string"},"old_text":{"type":"string"},"visibility":{"type":"string","enum":["shared","private"]}},"required":["action"]}"#,
    ),
    (
        "memory_recall",
        "Recall durable facts from curated memory, wiki pages, episodic, and transcripts (FTS). Call before answering from memory.",
        r#"{"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer"}},"required":["query"]}"#,
    ),
    (
        "wiki_show",
        "Read one compiled wiki page (path like wiki/pages/auth.md or a slug).",
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
        r#"{"type":"object","properties":{"slug":{"type":"string"},"name":{"type":"string"},"skill_md":{"type":"string"},"rationale":{"type":"string"},"files_base64":{"type":"object","additionalProperties":{"type":"string"}},"evidence":{"type":"array","items":{"type":"object"}}},"required":["slug","skill_md"]}"#,
    ),
    ("root_inspect", "Inspect a retained root and its restoration fidelity; omit root for current lineage tip.", r#"{"type":"object","properties":{"root":{"type":"string"}}}"#),
    ("native_session_inspect", "Inspect a retained native session archive and optional saved recovery receipt. Reports the last recorded observation; does not contact the native harness or refresh the receipt.", r#"{"type":"object","properties":{"archive_id":{"type":"string"},"operation_id":{"type":"string"}},"required":["archive_id"]}"#),
    ("root_compare", "Compare two retained roots using the canonical structured diff.", r#"{"type":"object","properties":{"from":{"type":"string"},"to":{"type":"string"}},"required":["from","to"]}"#),
    ("transition_inspect", "Inspect a transition receipt with its full identity and evidence.", r#"{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}"#),
    ("handoff_inspect", "Read the current handoff or one history packet by exact id or unambiguous sequence.", r#"{"type":"object","properties":{"id":{"type":"string"},"seq":{"type":"integer"}}}"#),
    ("handoff_list", "List distinct local history packet IDs and provenance, current first; use offset for later pages.", r#"{"type":"object","properties":{"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":200}}}"#),
    ("verification_inspect", "Read an observed command and report whether its project, plan and source still match; this is not semantic acceptance.", r#"{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}"#),
    ("plan_inspect", "Inspect recorded plan metadata and its declared completion receipt.", r#"{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}"#),
    ("plan_graph", "Inspect the canonical dependency graph and readiness, without dispatching work.", r#"{"type":"object","properties":{}}"#),
    ("skill_inspect", "Inspect the Skill catalog, exact package, resource or native route without asserting it was used.", r#"{"type":"object","properties":{"selector":{"type":"string"},"digest":{"type":"string"},"file":{"type":"string"},"harness":{"type":"string"}}}"#),
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
        "learnings_show",
        "Read a full saved learning with its scope, provenance, and lifecycle state.",
        r#"{"type":"object","properties":{"id":{"type":"string"},"scope":{"type":"string"}},"required":["id"]}"#,
    ),
    (
        "observations_list",
        "Read-only audit of raw hook-captured observations from .stateroot/spool/observations.jsonl. Provenance/debug only — not primary memory.",
        r#"{"type":"object","properties":{"kind":{"type":"string"},"harness":{"type":"string"},"query":{"type":"string"},"limit":{"type":"integer"}}}"#,
    ),
    (
        "projects_list",
        "List every initialized StateRoot project on this machine (name, path, phase, objective, active plan, last root, on-disk). For fixed-workspace agents: discover projects here, then work the one requested.",
        r#"{"type":"object","properties":{}}"#,
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
    if matches!(
        name,
        "root_inspect"
            | "native_session_inspect"
            | "root_compare"
            | "transition_inspect"
            | "handoff_inspect"
            | "handoff_list"
            | "verification_inspect"
            | "plan_inspect"
            | "plan_graph"
            | "skill_inspect"
            | "skill_propose"
    ) {
        return match structured_tool(ctx, home, caller, name, args) {
            Ok(value) => ok(
                id,
                json!({"content":[{"type":"text","text":value.to_string()}]}),
            ),
            Err(error) => ok(
                id,
                json!({"isError":true,"content":[{"type":"text","text":json!({"error":error}).to_string()}]}),
            ),
        };
    }
    let text = match name {
        "memory_save" => memory_save(ctx, home, args),
        "memory" => memory_tool(ctx, home, args),
        "memory_recall" => memory_recall(ctx, home, external, args),
        "wiki_show" => wiki_show(ctx, args),
        "learn_record" => learn_record(ctx, home, args),
        "soul_read" => soul_read(home, caller),
        "learnings_list" => learnings_list(ctx, home, external, args),
        "learnings_show" => {
            let scope = args["scope"].as_str().unwrap_or("project");
            let id = args["id"].as_str().unwrap_or("");
            match stateroot_core::learnings::read_scope(&ctx.cwd, home, scope)
                .into_iter()
                .find(|learning| learning.id == id)
            {
                Some(learning) => serde_json::to_string(&learning)
                    .unwrap_or_else(|error| json!({"error": error.to_string()}).to_string()),
                None => {
                    json!({"error": "learning unavailable", "id": id, "scope": scope}).to_string()
                }
            }
        }
        "observations_list" => observations_list(ctx, args),
        "projects_list" => super::projects::collect(ctx)
            .map(|v| v.to_string())
            .unwrap_or_else(|e| json!({"error": format!("{e:#}")}).to_string()),
        _ => return error_fallback(id, -32602, &format!("unknown tool: {name}")),
    };
    ok(id, json!({"content": [{"type": "text", "text": text}]}))
}

fn structured_tool(
    ctx: &Ctx,
    home: &std::path::Path,
    caller: &str,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    use stateroot_core::{
        fidelity, local_store, plans, roots, skill_federation as skills, verification,
    };
    ctx.require_project().map_err(|e| e.to_string())?;
    if !args.is_object() {
        return Err("tool arguments must be an object".into());
    }
    let string_fields: &[&str] = match name {
        "root_inspect" => &["root"],
        "native_session_inspect" => &["archive_id", "operation_id"],
        "root_compare" => &["from", "to"],
        "transition_inspect" | "verification_inspect" | "plan_inspect" => &["id"],
        "handoff_inspect" => &["id"],
        "skill_inspect" => &["selector", "digest", "file", "harness"],
        _ => &[],
    };
    for field in string_fields {
        if args.get(field).is_some_and(|value| !value.is_string()) {
            return Err(format!("{field} must be a string"));
        }
    }
    if name == "handoff_inspect"
        && args
            .get("seq")
            .is_some_and(|value| value.as_i64().is_none())
    {
        return Err("seq must be an integer".into());
    }
    if name == "handoff_list" {
        for field in ["offset", "limit"] {
            if args
                .get(field)
                .is_some_and(|value| value.as_u64().is_none())
            {
                return Err(format!("{field} must be a nonnegative integer"));
            }
        }
    }
    let required = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("{key} is required"))
    };
    match name {
        "native_session_inspect" => {
            let archive_id = required("archive_id")?;
            let operation = match args.get("operation_id").and_then(Value::as_str) {
                Some("") => return Err("operation_id must be nonempty".into()),
                Some(id) => {
                    let receipt = stateroot_core::sessions::codex_native::receipt(&ctx.cwd, id)?;
                    if receipt.archive_id != archive_id {
                        return Err("native operation belongs to a different archive".into());
                    }
                    Some(receipt)
                }
                None => None,
            };
            let mut report =
                fidelity::native_session_report(&ctx.cwd, archive_id, operation.as_ref());
            report["observation"] = json!("last recorded receipt; native harness not contacted and current state not reverified");
            Ok(report)
        }
        "root_inspect" => {
            let id = match args.get("root").and_then(Value::as_str) {
                Some(id) if !id.is_empty() => id.to_owned(),
                Some(_) => return Err("root must be nonempty".into()),
                None => roots::latest_root(&ctx.cwd)
                    .map_err(|e| e.to_string())?
                    .ok_or("no current root")?,
            };
            let root = roots::get_root(&ctx.cwd, &id).map_err(|e| e.to_string())?;
            let fidelity = fidelity::report_with_components(
                &ctx.cwd,
                &root.id,
                home,
                &super::roots::integration_environment(ctx, home),
            )
            .map_err(|e| e.to_string())?;
            Ok(json!({"root":root,"fidelity":fidelity}))
        }
        "root_compare" => {
            roots::diff_roots(&ctx.cwd, required("from")?, required("to")?, false, 0, 0)
                .map_err(|e| e.to_string())
        }
        "transition_inspect" => {
            roots::receipt_projection(&ctx.cwd, required("id")?).map_err(|e| e.to_string())
        }
        "handoff_list" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50);
            if !(1..=200).contains(&limit) {
                return Err("limit must be between 1 and 200".into());
            }
            let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
            let offset = usize::try_from(offset).map_err(|_| "offset is too large")?;
            let mut packets = super::handoff::distinct_history_packets(
                local_store::list_handoffs_local(&ctx.cwd).map_err(|e| e.to_string())?,
            );
            let (current_id, current_status) = match local_store::read_handoff_local(&ctx.cwd) {
                Ok(Some(packet)) => (
                    Some(super::handoff::handoff_body_sha256(&packet)),
                    "available",
                ),
                Ok(None) => (None, "absent"),
                Err(_) => (None, "unavailable"),
            };
            if let Some(id) = &current_id {
                if let Some(position) = packets.iter().position(|(packet_id, _)| packet_id == id) {
                    let current = packets.remove(position);
                    packets.insert(0, current);
                }
            }
            let total = packets.len();
            let entries: Vec<_> = packets
                .into_iter()
                .skip(offset)
                .take(limit as usize)
                .map(|(id, packet)| {
                    json!({
                        "is_current":current_id.as_ref() == Some(&id), "id":id, "seq":packet.get("seq"), "created_at":packet.get("created_at"),
                        "created_by_harness":packet.get("created_by_harness"),
                        "recommended_next_harness":packet.get("recommended_next_harness"),
                        "fork_id":packet.get("fork_id"), "current_phase":packet.get("current_phase")
                    })
                })
                .collect();
            let end = offset.saturating_add(entries.len());
            Ok(
                json!({"entries":entries,"total":total,"offset":offset,"current_id":current_id,"current_status":current_status,"next_offset":if end < total {Some(end)} else {None}}),
            )
        }
        "handoff_inspect" => {
            let id = args.get("id").and_then(Value::as_str);
            let seq = args.get("seq").and_then(Value::as_i64);
            if id.is_some() && seq.is_some() {
                return Err("select either id or seq".into());
            }
            if id.is_none() && seq.is_none() {
                return local_store::read_handoff_local(&ctx.cwd)
                    .map_err(|e| e.to_string())?
                    .map(|packet| json!({"id":super::handoff::handoff_body_sha256(&packet),"packet":packet,"source":"current"}))
                    .ok_or("no current handoff".into());
            }
            if let Some(id) = id {
                if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("id must be an exact packet SHA256".into());
                }
            }
            let packets = super::handoff::distinct_history_packets(
                local_store::list_handoffs_local(&ctx.cwd).map_err(|e| e.to_string())?,
            );
            let matches: Vec<_> = packets
                .into_iter()
                .filter(|(key, packet)| {
                    id.is_some_and(|id| key.eq_ignore_ascii_case(id))
                        || seq.is_some_and(|seq| {
                            packet.get("seq").and_then(Value::as_i64) == Some(seq)
                        })
                })
                .collect();
            if matches.is_empty() {
                return Err("handoff not found".into());
            }
            if matches.len() != 1 {
                return Err(format!(
                    "ambiguous handoff sequence; exact packet ids: {}",
                    matches
                        .iter()
                        .map(|(id, _)| id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            let (id, packet) = matches.into_iter().next().unwrap();
            Ok(json!({"id":id,"packet":packet,"source":"history"}))
        }
        "verification_inspect" => {
            let record = verification::load(&ctx.cwd, required("id")?)?;
            let matches = verification::inputs_match(&ctx.cwd, &record.plan_id, &record);
            Ok(
                json!({"observation":record,"inputs_match":matches.is_ok(),"input_status":matches.err(),"semantic_acceptance":"not established"}),
            )
        }
        "plan_inspect" => {
            let (meta, _) =
                plans::load(&ctx.cwd, required("id")?).ok_or("plan unavailable or unsupported")?;
            Ok(json!({"plan":meta}))
        }
        "plan_graph" => Ok(json!(plans::graph(&ctx.cwd))),
        "skill_inspect" => {
            let field = |key: &str| args.get(key).and_then(Value::as_str);
            let result = skills::inspect_package(
                &ctx.cwd,
                Some(home),
                field("selector"),
                field("digest"),
                field("file"),
                field("harness"),
            )?;
            serde_json::to_value(result).map_err(|e| e.to_string())
        }
        "skill_propose" => {
            let request: skills::SkillWriteRequest =
                serde_json::from_value(args.clone()).map_err(|e| e.to_string())?;
            let result = skills::author_skill(
                &ctx.cwd,
                home,
                caller,
                &request,
                &skills::SyncOptions::default(),
            )?;
            let _ = stateroot_core::proposals::create(
                &ctx.cwd,
                "skill",
                &format!(
                    "skill activated: {}",
                    request.name.as_deref().unwrap_or(&request.slug)
                ),
                request.rationale.as_deref().unwrap_or("mcp skill_propose"),
                json!({"slug":request.slug,"scope":"project"}),
                json!({"route":"mcp skill_propose","status":"active"}),
            );
            Ok(
                json!({"candidate":request.slug,"lifecycle":result.lifecycle,"quarantined":false,"activates":result.activates,"package":result.package,"actions":result.actions}),
            )
        }
        _ => Err("unknown tool".into()),
    }
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

/// Resolve the public (`scope`, `target`) pair to one hot-apex store.
///
/// `target=user` is the global USER.md identity/profile document. A memory
/// target honors scope: project MEMORY.md or user-global MEMORY.md.
fn memory_route(args: &Value) -> Result<(&'static str, &'static str), &'static str> {
    let scope = args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("project");
    let target = args
        .get("target")
        .and_then(|v| v.as_str())
        .unwrap_or("memory");
    if !matches!(scope, "project" | "user") {
        return Err("scope must be project|user");
    }
    match target {
        "memory" if scope == "user" => Ok(("global_memory", "user")),
        "memory" => Ok(("memory", "project")),
        "user" => Ok(("user", "user")),
        _ => Err("target must be memory|user"),
    }
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
    let (write_target, scope) = match memory_route(args) {
        Ok(route) => route,
        Err(error) => return json!({"error": error}).to_string(),
    };
    let private = args.get("visibility").and_then(|v| v.as_str()) == Some("private");
    stateroot_core::hot_apex::ensure_migrated(&ctx.cwd, home);
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
    let (target, scope) = match memory_route(args) {
        Ok(route) => route,
        Err(error) => return json!({"error": error}).to_string(),
    };
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
                Ok(text) => json!({"text": text, "scope": scope}).to_string(),
                Err(err) => json!({"error": format!("{err}")}).to_string(),
            };
        }
        _ => return json!({"error": "action must be add|replace|remove|show"}).to_string(),
    };
    match result {
        Ok(r) => {
            let _ = stateroot_core::memory_index::rebuild_if_needed(&ctx.cwd, home);
            let mut value = mutation_json(r);
            if let Some(object) = value.as_object_mut() {
                object.insert("scope".into(), json!(scope));
            }
            value.to_string()
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
                        "scope": if matches!(h.kind.as_str(), "user" | "memory_user") {
                            "user"
                        } else {
                            "project"
                        },
                        "visibility": if h.private { "private" } else { "shared" },
                        "score": h.score,
                        "source": h.source,
                        "stale": h.stale,
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
                "sources": l.sources,
                "label": l.label,
                "active_at_root": l.active_at_root,
                "superseded_by": l.superseded_by,
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
