//! Dual-mode context compiler — agentic when `DEEPSEEK_API_KEY` or
//! `OPENAI_API_KEY` is set, otherwise a full uncapped deterministic digest
//! (including the local observed context pack).
//!
//! Provider order: DeepSeek (`deepseek-v4-flash`) wins when `DEEPSEEK_API_KEY`
//! is set; otherwise OpenAI (`gpt-5.6-luna`) when `OPENAI_API_KEY` is set.
//! `STATEROOT_SYNTHESIS_API_BASE` overrides the chat-completions origin (tests).
//!
//! Failure falls back to deterministic. Never truncates.

use anyhow::Result;
use serde_json::{json, Value};
use sha2::Digest as _;
use stateroot_core::local_store::{self, now_rfc3339};

use super::Ctx;

const DEEPSEEK_BASE: &str = "https://api.deepseek.com/v1";
const DEEPSEEK_MODEL: &str = "deepseek-v4-flash";
const OPENAI_BASE: &str = "https://api.openai.com/v1";
const OPENAI_MODEL: &str = "gpt-5.6-luna";

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

/// Resolved chat-completions endpoint for the optional compiler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynthesisEndpoint {
    /// `deepseek` or `openai`.
    pub provider: &'static str,
    /// Bearer token.
    pub api_key: String,
    /// `{base}/chat/completions`.
    pub base_url: String,
    /// Model id sent in the request.
    pub model: String,
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Build the endpoint from explicit values (tests).
pub fn endpoint_from(
    deepseek_key: Option<&str>,
    openai_key: Option<&str>,
    base_override: Option<&str>,
) -> Option<SynthesisEndpoint> {
    let override_base = base_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_string());
    if let Some(api_key) = deepseek_key.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(SynthesisEndpoint {
            provider: "deepseek",
            api_key: api_key.to_string(),
            base_url: override_base.unwrap_or_else(|| DEEPSEEK_BASE.into()),
            model: DEEPSEEK_MODEL.into(),
        });
    }
    if let Some(api_key) = openai_key.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(SynthesisEndpoint {
            provider: "openai",
            api_key: api_key.to_string(),
            base_url: override_base.unwrap_or_else(|| OPENAI_BASE.into()),
            model: OPENAI_MODEL.into(),
        });
    }
    None
}

/// Resolve DeepSeek-or-OpenAI from the environment.
pub fn resolved_endpoint() -> Option<SynthesisEndpoint> {
    endpoint_from(
        nonempty_env("DEEPSEEK_API_KEY").as_deref(),
        nonempty_env("OPENAI_API_KEY").as_deref(),
        nonempty_env("STATEROOT_SYNTHESIS_API_BASE").as_deref(),
    )
}

/// Choose agentic vs deterministic.
pub fn mode(ctx: &Ctx) -> CompilerMode {
    if !ctx.config.synthesis.enabled {
        return CompilerMode::Deterministic;
    }
    if resolved_endpoint().is_some() {
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
    let system = "You are the StateRoot synthesizer. Read the observed context pack and any session bundle. Produce a STRICT JSON object with exactly these keys: progress_report (array of strings), decisions_and_amendments (array of strings), residual_work (array of strings), resolutions (array of strings). Use only substance present in the input. If there are no sessions, summarize the observed repo docs as product context. Never invent files, decisions, or history. Empty stays empty. No prose outside the JSON.";
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
    if local_store::read_handoff_local(&ctx.cwd)
        .ok()
        .flatten()
        .is_none()
    {
        let project_id = local_store::read_manifest(&ctx.cwd)
            .ok()
            .flatten()
            .and_then(|m| {
                m.get("project_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "local".into());
        let shell = json!({
            "schema_version": local_store::SCHEMA_HANDOFF_V1,
            "project_id": project_id,
            "seq": 1,
            "from": "cli",
            "created_by_harness": "cli",
            "created_at": now_rfc3339(),
            "objective": "",
            "task": "",
            "context_summary": "",
            "next_actions": [],
        });
        local_store::write_handoff_local(&ctx.cwd, &shell).map_err(|e| anyhow::anyhow!(e))?;
    }
    let path = local_store::root(&ctx.cwd).join(local_store::HANDOFF_CURRENT_PATH);
    let text = std::fs::read_to_string(&path)?;
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
    let Some(endpoint) = resolved_endpoint() else {
        return Ok(AgenticOutcome::Deterministic);
    };
    let home = match stateroot_core::harness_install::home_dir() {
        Ok(h) => h,
        Err(_) => return Ok(AgenticOutcome::Deterministic),
    };
    let bundles =
        stateroot_core::transcripts::bundle::build_bundles(&home, &ctx.cwd, None, usize::MAX);
    let pack = stateroot_core::context_pack::build(&ctx.cwd);
    if bundles.is_empty() && pack.is_empty() {
        return Ok(AgenticOutcome::Deterministic);
    }
    let sessions = Value::Array(bundles);
    let bundle_json = serde_json::to_string(&json!({
        "schema_version": "stateroot.synth_bundle.v1",
        "sessions": sessions,
        "context_pack": pack.to_synth_value(),
    }))?;
    let bundle_sha = format!("{:x}", sha2::Sha256::digest(bundle_json.as_bytes()));
    let mut gov = load_governance(ctx);
    if !force && gov.last_bundle_sha256 == bundle_sha {
        return Ok(AgenticOutcome::Unchanged);
    }
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

    let content = match call_local_provider(
        ctx,
        &endpoint.base_url,
        &endpoint.api_key,
        &endpoint.model,
        &bundle_json,
    )
    .await
    {
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
            "model": endpoint.model,
            "provider": endpoint.provider,
            "generated_at": now_rfc3339(),
            "labeled": "synthesized — not verified",
            "backend": endpoint.provider,
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
        let api_key = resolved_endpoint();
        if let Some(endpoint) = api_key {
            if let Ok(content) = call_local_ingest(
                ctx,
                &endpoint.base_url,
                &endpoint.api_key,
                &endpoint.model,
                &payload,
            )
            .await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_wins_over_openai() {
        let ep = endpoint_from(Some("ds"), Some("oa"), None).expect("endpoint");
        assert_eq!(ep.provider, "deepseek");
        assert_eq!(ep.model, DEEPSEEK_MODEL);
        assert_eq!(ep.base_url, DEEPSEEK_BASE);
        assert_eq!(ep.api_key, "ds");
    }

    #[test]
    fn openai_luna_when_no_deepseek() {
        let ep = endpoint_from(None, Some("oa"), None).expect("endpoint");
        assert_eq!(ep.provider, "openai");
        assert_eq!(ep.model, OPENAI_MODEL);
        assert_eq!(ep.base_url, OPENAI_BASE);
    }

    #[test]
    fn blank_keys_are_absent() {
        assert!(endpoint_from(Some("  "), Some(""), None).is_none());
        assert!(endpoint_from(None, None, Some("http://x")).is_none());
    }

    #[test]
    fn base_override_strips_trailing_slash() {
        let ep = endpoint_from(Some("ds"), None, Some("http://127.0.0.1:9/")).expect("endpoint");
        assert_eq!(ep.base_url, "http://127.0.0.1:9");
        assert_eq!(ep.model, DEEPSEEK_MODEL);
    }
}
