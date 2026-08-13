//! Dual-mode context compiler — agentic when a local synthesis API key is
//! present, otherwise a full uncapped deterministic digest.
//!
//! Agentic backend: local OpenAI-compatible key
//! (`STATEROOT_SYNTHESIS_API_KEY` / `[synthesis].api_key`).
//!
//! Failure falls back to deterministic. Never truncates.

use anyhow::Result;
use serde_json::{json, Value};
use sha2::Digest as _;
use stateroot_core::local_store::{self, now_rfc3339};

use super::Ctx;

const GOV_PATH: &str = "synthesis-gov.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilerMode {
    /// Local LLM synthesis available.
    Agentic,
    /// Full local digest only.
    Deterministic,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Governance {
    last_bundle_sha256: String,
    last_run_at: String,
    day: String,
    runs_today: i64,
}

fn load_governance(ctx: &Ctx) -> Governance {
    let path = local_store::root(&ctx.cwd).join(GOV_PATH);
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_governance(ctx: &Ctx, gov: &Governance) -> Result<()> {
    let path = local_store::root(&ctx.cwd).join(GOV_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(gov)?)?;
    Ok(())
}

/// Resolve the local OpenAI-compatible synthesis key (env wins).
pub fn resolved_key(ctx: &Ctx) -> String {
    if let Ok(env) = std::env::var("STATEROOT_SYNTHESIS_API_KEY") {
        let env = env.trim().to_string();
        if !env.is_empty() {
            return env;
        }
    }
    ctx.config.synthesis.api_key.clone()
}

/// Choose agentic vs deterministic.
pub fn mode(ctx: &Ctx) -> CompilerMode {
    if !ctx.config.synthesis.enabled {
        return CompilerMode::Deterministic;
    }
    if !resolved_key(ctx).is_empty() {
        CompilerMode::Agentic
    } else {
        CompilerMode::Deterministic
    }
}

/// Flatten synthesize sections into the resume-readable shape:
/// `{progress_report: string|array, decisions_and_amendments: [...], …, provenance}`.
pub fn flatten_synthesized(sections: &Value, provenance: Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in [
        "progress_report",
        "decisions_and_amendments",
        "residual_work",
        "resolutions",
    ] {
        if let Some(value) = sections.get(key) {
            // Resume treats progress_report as a string when present.
            if key == "progress_report" {
                match value {
                    Value::String(s) => {
                        out.insert(key.into(), Value::String(s.clone()));
                    }
                    Value::Array(arr) => {
                        let joined: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                        out.insert(key.into(), Value::String(joined.join("\n")));
                    }
                    other => {
                        out.insert(key.into(), other.clone());
                    }
                }
            } else {
                out.insert(key.into(), value.clone());
            }
        }
    }
    out.insert("provenance".into(), provenance);
    Value::Object(out)
}

fn parse_sections(content: &str) -> Result<Value> {
    let trimmed = content.trim();
    let json_text = if trimmed.starts_with("```") {
        trimmed
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim()
    } else {
        trimmed
    };
    let parsed: Value = serde_json::from_str(json_text)
        .map_err(|e| anyhow::anyhow!("synthesis output is not strict JSON ({e})"))?;
    // Already nested under "sections"?
    if let Some(sections) = parsed.get("sections") {
        return Ok(sections.clone());
    }
    let mut sections = serde_json::Map::new();
    for key in [
        "progress_report",
        "decisions_and_amendments",
        "residual_work",
        "resolutions",
    ] {
        if let Some(value) = parsed.get(key) {
            sections.insert(key.to_string(), value.clone());
        }
    }
    if sections.is_empty() {
        anyhow::bail!("synthesis output had no recognized sections");
    }
    Ok(Value::Object(sections))
}

async fn call_local_provider(
    ctx: &Ctx,
    base_url: &str,
    api_key: &str,
    model: &str,
    bundle_json: &str,
) -> Result<String> {
    let system = "You are the StateRoot synthesizer. Read the session bundle and produce a STRICT JSON object with exactly these keys: progress_report (array of strings), decisions_and_amendments (array of strings), residual_work (array of strings), resolutions (array of strings). No prose outside the JSON.";
    let mut body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": bundle_json},
        ],
    });
    if let Some(extra) = ctx.config.synthesis.extra_body.as_object() {
        for (key, value) in extra {
            body[key] = value.clone();
        }
    }
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base_url}/chat/completions"))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!(
            "synthesis provider returned {status}: {}",
            &text[..text.len().min(300)]
        );
    }
    let parsed: Value = serde_json::from_str(&text)?;
    let content = parsed
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("provider response has no choices[0].message.content"))?;
    Ok(content.to_string())
}

fn merge_into_handoff(ctx: &Ctx, synthesized: &Value) -> Result<()> {
    let path = local_store::root(&ctx.cwd).join(local_store::HANDOFF_CURRENT_PATH);
    let text = std::fs::read_to_string(&path).map_err(|_| {
        anyhow::anyhow!(
            "no local handoff to merge into — write one first (`stateroot handoff write` or `stateroot import`)"
        )
    })?;
    let mut packet: Value = serde_json::from_str(&text)?;
    packet["synthesized"] = synthesized.clone();
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&packet)?),
    )?;
    Ok(())
}

/// Outcome of an agentic compiler attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgenticOutcome {
    /// Synthesized sections were freshly merged into the handoff.
    Merged,
    /// Bundle hash unchanged — skipped LLM call (idempotent).
    Unchanged,
    /// Fell back to deterministic (no key, empty bundles, network failure).
    Deterministic,
}

/// Run the agentic compiler when available.
pub async fn try_agentic(ctx: &Ctx, force: bool) -> Result<AgenticOutcome> {
    if mode(ctx) != CompilerMode::Agentic {
        return Ok(AgenticOutcome::Deterministic);
    }
    let home = match stateroot_core::harness_install::home_dir() {
        Ok(h) => h,
        Err(_) => return Ok(AgenticOutcome::Deterministic),
    };
    // Uncapped bundle — no char budget for the compiler.
    let bundles =
        stateroot_core::transcripts::bundle::build_bundles(&home, &ctx.cwd, None, usize::MAX);
    if bundles.is_empty() {
        return Ok(AgenticOutcome::Deterministic);
    }
    let sessions = Value::Array(bundles);
    let bundle_json = serde_json::to_string(&json!({
        "schema_version": "stateroot.synth_bundle.v1",
        "sessions": sessions,
    }))?;
    let bundle_sha = format!("{:x}", sha2::Sha256::digest(bundle_json.as_bytes()));
    let mut gov = load_governance(ctx);
    if !force && gov.last_bundle_sha256 == bundle_sha {
        return Ok(AgenticOutcome::Unchanged);
    }
    // Rate governance only when caps are > 0 (default 0 = uncapped).
    let now = now_rfc3339();
    let today = &now[..10.min(now.len())];
    if gov.day != today {
        gov.day = today.to_string();
        gov.runs_today = 0;
    }
    let daily_cap = ctx.config.synthesis.daily_cap;
    let min_interval = ctx.config.synthesis.min_interval_seconds;
    if !force && daily_cap > 0 && gov.runs_today >= daily_cap {
        return Ok(AgenticOutcome::Deterministic);
    }
    if !force && min_interval > 0 && !gov.last_run_at.is_empty() {
        if let Ok(last) = chrono::DateTime::parse_from_rfc3339(&gov.last_run_at) {
            let elapsed = chrono::Utc::now() - last.with_timezone(&chrono::Utc);
            if elapsed.num_seconds() < min_interval {
                return Ok(AgenticOutcome::Deterministic);
            }
        }
    }

    let api_key = resolved_key(ctx);
    if api_key.is_empty() {
        return Ok(AgenticOutcome::Deterministic);
    }
    let base_url = if ctx.config.synthesis.base_url.trim().is_empty() {
        ctx.config
            .synthesis
            .api_url
            .trim_end_matches('/')
            .to_string()
    } else {
        ctx.config
            .synthesis
            .base_url
            .trim_end_matches('/')
            .to_string()
    };
    let model = ctx.config.synthesis.model.clone();
    let content = match call_local_provider(ctx, &base_url, &api_key, &model, &bundle_json).await {
        Ok(c) => c,
        Err(_) => return Ok(AgenticOutcome::Deterministic),
    };
    let sections = match parse_sections(&content) {
        Ok(s) => s,
        Err(_) => return Ok(AgenticOutcome::Deterministic),
    };
    let flat = flatten_synthesized(
        &sections,
        json!({
            "bundle_sha256": bundle_sha,
            "model": model,
            "generated_at": now_rfc3339(),
            "labeled": "synthesized — not verified",
            "backend": "local",
        }),
    );

    if merge_into_handoff(ctx, &flat).is_err() {
        return Ok(AgenticOutcome::Deterministic);
    }
    gov.last_bundle_sha256 = bundle_sha;
    gov.last_run_at = now;
    gov.runs_today += 1;
    let _ = save_governance(ctx, &gov);
    Ok(AgenticOutcome::Merged)
}

const INGEST_GOV_PATH: &str = "ingest-gov.json";

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct IngestGovernance {
    last_input_sha256: String,
    last_run_at: String,
}

fn load_ingest_gov(ctx: &Ctx) -> IngestGovernance {
    let path = local_store::root(&ctx.cwd).join(INGEST_GOV_PATH);
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_ingest_gov(ctx: &Ctx, gov: &IngestGovernance) -> Result<()> {
    let path = local_store::root(&ctx.cwd).join(INGEST_GOV_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(gov)?)?;
    Ok(())
}

fn deterministic_ingest(ctx: &Ctx, home: &std::path::Path) -> Result<String> {
    let added = stateroot_core::learnings::distill_to_inbox(&ctx.cwd, home)
        .map_err(|e| anyhow::anyhow!(e))?;
    let _ = stateroot_core::wiki::ensure_layout(&ctx.cwd);
    if added > 0 {
        let _ = stateroot_core::wiki::append_log(
            &ctx.cwd,
            &format!("deterministic ingest: {added} bullet(s) → memories/pages/_inbox.md"),
        );
    }
    let _ = stateroot_core::memory_index::rebuild_if_needed(&ctx.cwd, home);
    Ok(format!("deterministic ingest: {added} new inbox bullet(s)"))
}

fn apply_agentic_ingest(ctx: &Ctx, home: &std::path::Path, parsed: &Value) -> Result<String> {
    let mut pages = 0usize;
    let mut facts = 0usize;
    if let Some(arr) = parsed.get("pages").and_then(|v| v.as_array()) {
        for page in arr {
            let slug = page.get("slug").and_then(|v| v.as_str()).unwrap_or("");
            let body = page.get("body").and_then(|v| v.as_str()).unwrap_or("");
            let summary = page.get("summary").and_then(|v| v.as_str()).unwrap_or(slug);
            let kind = page
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("concept");
            if slug.is_empty() || body.is_empty() {
                continue;
            }
            // Never write soul from ingest.
            if slug.eq_ignore_ascii_case("soul") || slug.contains("SOUL") {
                continue;
            }
            let _ = stateroot_core::wiki::write_page(&ctx.cwd, slug, body, summary, kind)?;
            pages += 1;
        }
    }
    if let Some(arr) = parsed.get("memory_facts").and_then(|v| v.as_array()) {
        for fact in arr {
            let content = fact
                .get("content")
                .or_else(|| fact.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if content.is_empty() {
                continue;
            }
            let action = fact.get("action").and_then(|v| v.as_str()).unwrap_or("add");
            let result = match action {
                "replace" => {
                    let old = fact.get("old").and_then(|v| v.as_str()).unwrap_or("");
                    stateroot_core::hot_apex::replace(&ctx.cwd, home, "memory", old, content, false)
                }
                "remove" => stateroot_core::hot_apex::remove(&ctx.cwd, home, "memory", content),
                _ => stateroot_core::hot_apex::add(&ctx.cwd, home, "memory", content, false),
            };
            if result.map(|r| r.success).unwrap_or(false) {
                facts += 1;
            }
        }
    }
    let _ = stateroot_core::wiki::append_log(
        &ctx.cwd,
        &format!("agentic ingest: pages={pages} memory_facts={facts}"),
    );
    let _ = stateroot_core::memory_index::rebuild_if_needed(&ctx.cwd, home);
    Ok(format!(
        "agentic ingest: {pages} page(s), {facts} memory fact(s)"
    ))
}

async fn call_local_ingest(
    ctx: &Ctx,
    base_url: &str,
    api_key: &str,
    model: &str,
    payload: &str,
) -> Result<String> {
    let system = "You are the StateRoot wiki ingest compiler. Read mined notes and produce STRICT JSON with keys: pages (array of {slug, body, summary, kind}), memory_facts (array of {action: add|replace|remove, content, old?}). File durable project knowledge into pages; put only still-true load-bearing facts into memory_facts (respect that MEMORY.md is a small curated apex). Never invent verified lineage. Never write soul or USER identity. No prose outside JSON.";
    let mut body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": payload},
        ],
    });
    if let Some(extra) = ctx.config.synthesis.extra_body.as_object() {
        for (key, value) in extra {
            body[key] = value.clone();
        }
    }
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base_url}/chat/completions"))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!(
            "ingest provider returned {status}: {}",
            &text[..text.len().min(300)]
        );
    }
    let parsed: Value = serde_json::from_str(&text)?;
    let content = parsed
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("provider response has no choices[0].message.content"))?;
    Ok(content.to_string())
}

fn parse_ingest_json(content: &str) -> Result<Value> {
    let trimmed = content.trim();
    let json_text = if trimmed.starts_with("```") {
        trimmed
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim()
    } else {
        trimmed
    };
    Ok(serde_json::from_str(json_text)?)
}

/// Dual-mode wiki/memory ingest (session_end / pre_compact / `wiki compile`).
///
/// Deterministic floor always mines into `_inbox.md`. When agentic, also asks
/// the model to file pages + curated MEMORY facts. Never writes soul.
pub async fn try_ingest(ctx: &Ctx, force: bool) -> Result<String> {
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))?;
    stateroot_core::hot_apex::ensure_migrated(&ctx.cwd, &home);
    let _ = stateroot_core::wiki::ensure_layout(&ctx.cwd);

    let input_hash = stateroot_core::wiki::compile_input_hash(&ctx.cwd);
    let mut gov = load_ingest_gov(ctx);
    if !force && gov.last_input_sha256 == input_hash && !input_hash.is_empty() {
        let _ = stateroot_core::memory_index::rebuild_if_needed(&ctx.cwd, &home);
        return Ok("ingest unchanged (hash match)".into());
    }

    // Always run deterministic floor first.
    let mut summary = deterministic_ingest(ctx, &home)?;

    if mode(ctx) == CompilerMode::Agentic {
        let stmts = stateroot_core::learnings::distill_statements(&ctx.cwd, &home);
        let inbox =
            stateroot_core::wiki::show(&ctx.cwd, "memories/pages/_inbox.md").unwrap_or_default();
        let payload = serde_json::to_string_pretty(&json!({
            "schema_version": "stateroot.ingest.v1",
            "mined": stmts.iter().map(|(s, src, c)| json!({
                "statement": s, "sources": src, "confidence": c
            })).collect::<Vec<_>>(),
            "inbox": inbox,
            "index": stateroot_core::wiki::read_index(&ctx.cwd),
        }))?;
        let api_key = resolved_key(ctx);
        if !api_key.is_empty() {
            let base_url = if ctx.config.synthesis.base_url.trim().is_empty() {
                ctx.config
                    .synthesis
                    .api_url
                    .trim_end_matches('/')
                    .to_string()
            } else {
                ctx.config
                    .synthesis
                    .base_url
                    .trim_end_matches('/')
                    .to_string()
            };
            let model = ctx.config.synthesis.model.clone();
            if let Ok(content) = call_local_ingest(ctx, &base_url, &api_key, &model, &payload).await
            {
                if let Ok(parsed) = parse_ingest_json(&content) {
                    if let Ok(agentic) = apply_agentic_ingest(ctx, &home, &parsed) {
                        summary = format!("{summary}; {agentic}");
                    }
                }
            }
        }
    }

    gov.last_input_sha256 = input_hash;
    gov.last_run_at = now_rfc3339();
    let _ = save_ingest_gov(ctx, &gov);
    Ok(summary)
}
