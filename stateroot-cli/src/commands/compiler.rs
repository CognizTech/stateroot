//! Dual-mode context compiler — agentic when `DEEPSEEK_API_KEY` or
//! `OPENAI_API_KEY` is set, otherwise a full uncapped deterministic digest
//! (including the local observed context pack).
//!
//! Provider order: DeepSeek (`deepseek-v4-flash`) wins when `DEEPSEEK_API_KEY`
//! is set; otherwise OpenAI (`gpt-5.6-luna`) when `OPENAI_API_KEY` is set.
//! `STATEROOT_SYNTHESIS_API_BASE` overrides the chat-completions origin (tests).
//!
//! Failure falls back to deterministic. Never truncates.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};
use sha2::Digest as _;
use stateroot_core::local_store::{self, now_rfc3339};

use super::Ctx;

const DEEPSEEK_BASE: &str = "https://api.deepseek.com/v1";
const DEEPSEEK_MODEL: &str = "deepseek-v4-flash";
const OPENAI_BASE: &str = "https://api.openai.com/v1";
const OPENAI_MODEL: &str = "gpt-5.6-luna";

const SYNTHESIS_SYSTEM: &str = "You are the StateRoot synthesizer. Read the observed context pack and any session bundle. Produce a STRICT JSON object with exactly these keys: progress_report (array of strings), decisions_and_amendments (array of strings), residual_work (array of strings), resolutions (array of strings). Use only substance present in the input. If there are no sessions, summarize the observed repo docs as product context. Never invent files, decisions, or history. Empty stays empty. No prose outside the JSON.";

const INGEST_SYSTEM: &str = "You are the StateRoot wiki ingest compiler. Read mined notes and produce STRICT JSON with keys: pages (array of {slug, body, summary, kind}), memory_facts (array of {action: add|replace|remove, content, old?}). File durable project knowledge into pages; put only still-true load-bearing facts into memory_facts (respect that MEMORY.md is a small curated apex). Never invent verified lineage. Never write soul or USER identity. No prose outside JSON.";

const GOV_PATH: &str = "synthesis-gov.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilerMode {
    /// Local LLM synthesis available.
    Agentic,
    /// Full local digest only.
    Deterministic,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Governance {
    last_bundle_sha256: String,
    last_run_at: String,
    day: String,
    runs_today: i64,
    attempts: u64,
    last_attempt_sha256: String,
    last_attempt_status: String,
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
    stateroot_core::safe_io::atomic_replace(&path, &serde_json::to_vec_pretty(gov)?)?;
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

/// Non-empty trimmed environment variable, or `None`.
pub fn nonempty_env(name: &str) -> Option<String> {
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

/// Redirect ceiling for one provider request. Gateway path moves need ≤2
/// hops; 5 gives headroom while making redirect loops fail fast.
const PROVIDER_MAX_REDIRECTS: usize = 5;

/// Hard cap on a provider response body. Chat-completions JSON for the
/// compiler is a few KB; 1 MiB is ~100× headroom. Larger bodies are
/// pathological and reported, never read whole into memory.
const PROVIDER_MAX_BODY_BYTES: usize = 1024 * 1024;

/// Chars of a provider error body quoted in diagnostics. Counted in chars,
/// not bytes: the old 300-byte slice panicked when the index landed
/// mid-codepoint.
const PROVIDER_ERROR_EXCERPT_CHARS: usize = 300;

/// Whether a redirect from `previous` to `next` stays inside the configured
/// provider origin. Only exact-origin hops (scheme + host + port) are
/// followed: anything else would carry the prompt body — and on 307/308 the
/// request — to an origin the operator never configured, and an HTTPS→HTTP
/// hop would strip TLS.
fn redirect_follow_allowed(previous: &reqwest::Url, next: &reqwest::Url) -> Result<(), String> {
    if previous.scheme() == "https" && next.scheme() == "http" {
        return Err(format!("refusing HTTPS→HTTP downgrade redirect to {next}"));
    }
    let same_origin = previous.scheme() == next.scheme()
        && previous.host_str() == next.host_str()
        && previous.port_or_known_default() == next.port_or_known_default();
    if !same_origin {
        return Err(format!("refusing cross-origin provider redirect to {next}"));
    }
    Ok(())
}

/// HTTP client for the optional provider calls: exact-origin redirects only,
/// capped, so a misbehaving endpoint or gateway cannot loop the CLI or walk
/// the request body onto another origin.
fn provider_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= PROVIDER_MAX_REDIRECTS {
                return attempt.error(format!(
                    "provider redirect limit ({PROVIDER_MAX_REDIRECTS}) exceeded — possible loop"
                ));
            }
            let previous = attempt
                .previous()
                .last()
                .expect("a redirect attempt always has a previous URL");
            if let Err(reason) = redirect_follow_allowed(previous, attempt.url()) {
                return attempt.error(reason);
            }
            attempt.follow()
        }))
        .build()
}

/// Read a provider response body with a hard size cap. An honest
/// Content-Length over the cap fails before body bytes are consumed.
async fn read_body_bounded(resp: reqwest::Response) -> Result<String> {
    if let Some(len) = resp.content_length() {
        if len > PROVIDER_MAX_BODY_BYTES as u64 {
            anyhow::bail!(
                "provider response body exceeds the {PROVIDER_MAX_BODY_BYTES} byte cap (content-length {len})"
            );
        }
    }
    let mut resp = resp;
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if buf.len() + chunk.len() > PROVIDER_MAX_BODY_BYTES {
            anyhow::bail!("provider response body exceeds the {PROVIDER_MAX_BODY_BYTES} byte cap");
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// First chars of a provider error body for diagnostics — char-boundary
/// safe, with a truncation marker when the body ran longer.
fn error_excerpt(text: &str) -> String {
    let excerpt: String = text.chars().take(PROVIDER_ERROR_EXCERPT_CHARS).collect();
    if excerpt.len() < text.len() {
        format!("{excerpt}…[truncated]")
    } else {
        excerpt
    }
}

/// Shared chat-completions POST for every API-backed synthesis caller.
///
/// Merges `synthesis.extra_body` config into the request, then returns the
/// first choice's message content.
pub async fn call_provider(
    ctx: &Ctx,
    endpoint: &SynthesisEndpoint,
    system: &str,
    user: &str,
) -> Result<String> {
    let mut body = json!({
        "model": endpoint.model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
    });
    if let Some(extra) = ctx.config.synthesis.extra_body.as_object() {
        for (key, value) in extra {
            body[key] = value.clone();
        }
    }
    let client = provider_client()?;
    let resp = client
        .post(format!("{}/chat/completions", endpoint.base_url))
        .bearer_auth(&endpoint.api_key)
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = read_body_bounded(resp).await?;
    if !status.is_success() {
        anyhow::bail!(
            "synthesis provider returned {status}: {}",
            error_excerpt(&text)
        );
    }
    let parsed: Value = serde_json::from_str(&text)?;
    let content = parsed
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("provider response has no choices[0].message.content"))?;
    Ok(content.to_string())
}

async fn call_local_provider(
    ctx: &Ctx,
    base_url: &str,
    api_key: &str,
    model: &str,
    bundle_json: &str,
) -> Result<String> {
    let endpoint = SynthesisEndpoint {
        provider: "api",
        api_key: api_key.to_string(),
        base_url: base_url.to_string(),
        model: model.to_string(),
    };
    call_provider(ctx, &endpoint, SYNTHESIS_SYSTEM, bundle_json).await
}

fn meaningful_handoff(packet: Option<&Value>) -> Option<Value> {
    packet.map(|packet| {
        let mut packet = packet.clone();
        if let Some(object) = packet.as_object_mut() {
            for field in ["last_activity", "accepted_by", "accepted_at"] {
                object.remove(field);
            }
        }
        packet
    })
}

/// Merge advisory output through the ordinary current-handoff owner. A new
/// shell uses the existing formal history funnel, with compare/create under
/// its lock. Authored fields and incidental activity remain receiver-owned.
pub fn merge_into_handoff_at(project_dir: &Path, synthesized: &Value) -> Result<()> {
    let expected = local_store::read_handoff_local(project_dir)?;
    anyhow::ensure!(
        merge_into_handoff_if_current(project_dir, synthesized, expected.as_ref())?,
        "handoff changed before synthesis publication"
    );
    Ok(())
}

fn merge_into_handoff_if_current(
    project_dir: &Path,
    synthesized: &Value,
    expected: Option<&Value>,
) -> Result<bool> {
    let shell = if expected.is_none() {
        let project_id = local_store::read_manifest(project_dir)?
            .and_then(|m| {
                m.get("project_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "local".into());
        Some(
            json!({"schema_version": local_store::SCHEMA_HANDOFF_V1, "project_id": project_id,
            "seq": stateroot_core::handoff_continuity::reserve_sequence(project_dir).map_err(anyhow::Error::msg)?,
            "from": "cli", "created_by_harness": "cli", "created_at": now_rfc3339(),
            "objective": "", "task": "", "context_summary": "", "next_actions": []}),
        )
    } else {
        None
    };
    Ok(local_store::update_or_create_handoff_current(
        project_dir,
        |current| {
            if meaningful_handoff(current) != meaningful_handoff(expected) {
                return None;
            }
            let mut packet = current.cloned().or(shell)?;
            packet["synthesized"] = synthesized.clone();
            Some(packet)
        },
    )?)
}

fn synthesis_bundle(ctx: &Ctx, home: &Path) -> Result<String> {
    let bundles =
        stateroot_core::transcripts::bundle::build_bundles(home, &ctx.cwd, None, usize::MAX);
    let pack = stateroot_core::context_pack::build(&ctx.cwd);
    Ok(serde_json::to_string(
        &json!({"schema_version": "stateroot.synth_bundle.v1",
        "sessions": bundles, "context_pack": pack.to_synth_value()}),
    )?)
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
    let _guard = stateroot_core::safe_io::ResourceLock::acquire(
        local_store::root(&ctx.cwd).join("local/locks/synthesis.lock"),
    )
    .map_err(anyhow::Error::msg)?;
    let bundle_json = synthesis_bundle(ctx, &home)?;
    let bundle: Value = serde_json::from_str(&bundle_json)?;
    if bundle["sessions"].as_array().is_some_and(Vec::is_empty)
        && stateroot_core::context_pack::build(&ctx.cwd).is_empty()
    {
        return Ok(AgenticOutcome::Deterministic);
    }
    let bundle_sha = digest(
        &json!({"bundle": bundle_json, "config": endpoint_identity(ctx, &endpoint, SYNTHESIS_SYSTEM)?}),
    )?;
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

    let expected_handoff = local_store::read_handoff_local(&ctx.cwd)?;
    gov.attempts += 1;
    gov.last_attempt_sha256 = bundle_sha.clone();
    gov.last_attempt_status = "generating".into();
    save_governance(ctx, &gov)?;
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
        Err(_) => {
            gov.last_attempt_status = "generation_failed".into();
            save_governance(ctx, &gov)?;
            return Ok(AgenticOutcome::Deterministic);
        }
    };
    let sections = match parse_sections(&content) {
        Ok(s) => s,
        Err(_) => {
            gov.last_attempt_status = "generation_failed".into();
            save_governance(ctx, &gov)?;
            return Ok(AgenticOutcome::Deterministic);
        }
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

    if synthesis_bundle(ctx, &home)? != bundle_json
        || !merge_into_handoff_if_current(&ctx.cwd, &flat, expected_handoff.as_ref())?
    {
        gov.last_attempt_status = "stale".into();
        save_governance(ctx, &gov)?;
        return Ok(AgenticOutcome::Deterministic);
    }
    gov.last_attempt_status = "applied".into();
    gov.last_bundle_sha256 = bundle_sha;
    gov.last_run_at = now;
    gov.runs_today += 1;
    let _ = save_governance(ctx, &gov);
    Ok(AgenticOutcome::Merged)
}

const INGEST_GOV_PATH: &str = "ingest-gov.json";

/// v1 fields remain readable, but are not proof of optional success.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct IngestGovernance {
    last_input_sha256: String,
    last_run_at: String,
    floor_sha256: String,
    floor_runs: u64,
    optional_success_sha256: String,
    optional_attempts: u64,
    optional_status: String,
    optional_detail: String,
    last_success: Option<Value>,
    generated: Option<GeneratedIngest>,
    pending: Option<PreparedIngest>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct GeneratedIngest {
    input_sha256: String,
    sources_sha256: String,
    parsed: Value,
    output_versions: BTreeMap<String, String>,
    input_files: BTreeMap<String, String>,
    generation_ms: u128,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PreparedWrite {
    before_sha256: String,
    after_sha256: String,
    after: String,
    append: bool,
    #[serde(default)]
    before_len: usize,
}

/// Exact prepared bytes survive a process exit between publication and the
/// success checkpoint. This is part of existing ingest governance, not a
/// second compiler store. Failed/stale attempts retain the prior success key.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PreparedIngest {
    input_sha256: String,
    success_sha256: String,
    sources_sha256: String,
    input_files: BTreeMap<String, String>,
    config_sha256: String,
    dependencies: Value,
    dependencies_sha256: String,
    materialization: Value,
    writes: BTreeMap<String, PreparedWrite>,
    summary: String,
    #[serde(default)]
    learning_sha256: String,
    #[serde(default)]
    publication_started: bool,
}

fn load_ingest_gov(ctx: &Ctx) -> Result<IngestGovernance> {
    let path = local_store::root(&ctx.cwd).join(INGEST_GOV_PATH);
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(serde_json::from_str(&text)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(IngestGovernance::default()),
        Err(err) => Err(err.into()),
    }
}

fn save_ingest_gov(ctx: &Ctx, gov: &IngestGovernance) -> Result<()> {
    let path = local_store::root(&ctx.cwd).join(INGEST_GOV_PATH);
    stateroot_core::safe_io::atomic_replace(&path, &serde_json::to_vec_pretty(gov)?)?;
    Ok(())
}

fn digest(value: &impl serde::Serialize) -> Result<String> {
    Ok(format!(
        "{:x}",
        sha2::Sha256::digest(serde_json::to_vec(value)?)
    ))
}

fn optional_text(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn learning_identity(ctx: &Ctx, home: &Path) -> Result<String> {
    let mut scopes = vec![
        "user".to_string(),
        "workspace".into(),
        "project".into(),
        "domain".into(),
    ];
    if let Some(domain) = stateroot_core::learnings::bound_domain(&ctx.cwd) {
        scopes.push(format!("domain:{domain}"));
    }
    let statements: Vec<_> = scopes
        .iter()
        .flat_map(|scope| {
            stateroot_core::learnings::read_scope(&ctx.cwd, home, scope)
                .into_iter()
                .map(|learning| learning.statement)
        })
        .collect();
    digest(&statements)
}

fn floor_sources(ctx: &Ctx, home: &Path) -> Result<String> {
    digest(
        &json!({"miner": "distill-v3", "sources": stateroot_core::wiki::compile_input_hash(&ctx.cwd),
        "learnings": learning_identity(ctx, home)?}),
    )
}

fn floor_identity(ctx: &Ctx, home: &Path) -> Result<String> {
    digest(
        &json!({"sources": floor_sources(ctx, home)?, "inbox": optional_text(&local_store::root(&ctx.cwd).join("wiki/pages/_inbox.md"))?}),
    )
}

fn endpoint_identity(ctx: &Ctx, endpoint: &SynthesisEndpoint, system: &str) -> Result<String> {
    // The key is not retained. A credential rotation can repair a failed
    // request, and must invalidate that attempt's configuration identity.
    digest(
        &json!({"compiler": env!("CARGO_PKG_VERSION"), "contract": "ingest-v2",
        "provider": endpoint.provider, "model": endpoint.model, "base": endpoint.base_url,
        "system_sha256": digest(&system)?, "credential_sha256": digest(&endpoint.api_key)?, "extra_body": ctx.config.synthesis.extra_body,
        "enabled": ctx.config.synthesis.enabled}),
    )
}

fn relative_dependency(project: &Path, raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);
    anyhow::ensure!(
        !raw.is_empty()
            && !raw.contains('\\')
            && !raw.contains(':')
            && path
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
        "dependency must be project-relative: {raw}"
    );
    let joined = project.join(path);
    let mut existing = joined.as_path();
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| anyhow::anyhow!("missing dependency parent: {raw}"))?;
    }
    anyhow::ensure!(
        existing
            .canonicalize()?
            .starts_with(project.canonicalize()?),
        "dependency leaves project: {raw}"
    );
    Ok(joined)
}

/// Resolve only native-authored references. Merely having a Skill installed
/// does not make it an input, and no catalog relevance selection occurs here.
fn resolve_dependencies(ctx: &Ctx, home: &Path, declared: &Value) -> Result<Value> {
    let references = declared
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("compile_dependencies must be an array"))?;
    let mut resolved = Vec::new();
    for reference in references {
        match reference
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("file")
        {
            "file" | "source" | "instruction" | "tool_config" => {
                let raw = reference
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("file dependency requires path"))?;
                let path = relative_dependency(&ctx.cwd, raw)?;
                let content = optional_text(&path)?;
                resolved.push(json!({"reference": reference, "content": content}));
            }
            "skill" => {
                let selector = reference
                    .get("selector")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("skill dependency requires selector"))?;
                let file = reference
                    .get("file")
                    .and_then(Value::as_str)
                    .unwrap_or("SKILL.md");
                let inspection = stateroot_core::skill_federation::inspect_package(
                    &ctx.cwd,
                    Some(home),
                    Some(selector),
                    None,
                    Some(file),
                    None,
                )
                .map_err(anyhow::Error::msg)?;
                resolved.push(json!({"reference": reference, "availability": inspection.availability,
                    "metadata_digest": inspection.metadata_digest, "metadata": inspection.package_metadata,
                    "packages": inspection.packages.iter().map(|p| json!({"identity": p.identity_key,
                        "digest": p.package_digest, "files": p.files, "exclusions": p.hash_exclusions,
                        "compatibility": p.compatibility, "native_harness": p.native_harness,
                        "native_invocation": p.native_invocation})).collect::<Vec<_>>(),
                    "content_base64": inspection.content_base64}));
            }
            kind => anyhow::bail!("unsupported compile dependency kind: {kind}"),
        }
    }
    Ok(Value::Array(resolved))
}

fn ingest_payload(ctx: &Ctx, dependencies: Value) -> Result<Value> {
    Ok(json!({"schema_version": "stateroot.ingest.v2", "mined": [],
        "inbox": stateroot_core::wiki::show(&ctx.cwd, "wiki/pages/_inbox.md")?,
        "index": stateroot_core::wiki::read_index(&ctx.cwd), "dependencies": dependencies}))
}

fn output_snapshot(ctx: &Ctx) -> Result<BTreeMap<String, String>> {
    let root = local_store::root(&ctx.cwd);
    let mut paths = stateroot_core::wiki::list_pages(&ctx.cwd);
    paths.extend(
        [
            "wiki/index.md",
            "wiki/log.md",
            "wiki/SCHEMA.md",
            local_store::MEMORY_CORE_PATH,
            local_store::EPISODIC_PATH,
        ]
        .into_iter()
        .map(str::to_string),
    );
    paths
        .into_iter()
        .map(|path| Ok((path.clone(), digest(&optional_text(&root.join(path))?)?)))
        .collect()
}

struct MaterializedIngest {
    writes: BTreeMap<String, PreparedWrite>,
    summary: String,
    expected: Value,
    copied_bytes: usize,
}

fn prepare_ingest(
    ctx: &Ctx,
    home: &Path,
    parsed: &Value,
    actor: &str,
    versions: &BTreeMap<String, String>,
) -> Result<MaterializedIngest> {
    // Materialize only the proposed pages and existing mutators' fixed
    // outputs. No unrelated page bodies or episodic history are copied.
    let mut paths = vec![
        "wiki/index.md".to_string(),
        "wiki/log.md".into(),
        "wiki/SCHEMA.md".into(),
        "wiki/pages/_inbox.md".into(),
        "wiki/pages/memory/apex-archive.md".into(),
        local_store::MEMORY_CORE_PATH.into(),
    ];
    if let Some(pages) = parsed.get("pages").and_then(Value::as_array) {
        for page in pages {
            if let Some(slug) = page.get("slug").and_then(Value::as_str) {
                let rel = format!("wiki/pages/{}.md", slug.trim().trim_end_matches(".md"));
                relative_dependency(&local_store::root(&ctx.cwd), &rel)?;
                paths.push(rel);
            }
        }
    }
    paths.sort();
    paths.dedup();
    let stage = tempfile::tempdir()?;
    let mut before = BTreeMap::new();
    let mut copied_bytes = 0;
    let absent = digest(&Option::<String>::None)?;
    for rel in paths {
        let text = optional_text(&local_store::root(&ctx.cwd).join(&rel))?;
        anyhow::ensure!(
            digest(&text)? == *versions.get(&rel).unwrap_or(&absent),
            "ingest output conflict: {rel}"
        );
        if let Some(text) = &text {
            copied_bytes += text.len();
            let path = local_store::root(stage.path()).join(&rel);
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(path, text)?;
        }
        before.insert(rel, text);
    }
    let staged_ctx = Ctx {
        cwd: stage.path().into(),
        config_dir: ctx.config_dir.clone(),
        config: ctx.config.clone(),
    };
    let summary = apply_agentic_ingest(&staged_ctx, home, parsed, Some(actor))?;
    let after = output_snapshot(&staged_ctx)?;
    let mut writes = BTreeMap::new();
    for (rel, _) in after {
        let Some(mut after) = optional_text(&local_store::root(stage.path()).join(&rel))? else {
            continue;
        };
        let append = rel == local_store::EPISODIC_PATH;
        if append {
            let operation = uuid::Uuid::new_v4().to_string();
            after = after
                .lines()
                .enumerate()
                .map(|(n, line)| {
                    let mut record: Value = serde_json::from_str(line)?;
                    record["ingest_operation"] = json!(format!("{operation}:{n}"));
                    Ok(format!("{}\n", serde_json::to_string(&record)?))
                })
                .collect::<Result<String>>()?;
        }
        let old = if append {
            optional_text(&local_store::root(&ctx.cwd).join(&rel))?
        } else {
            before.get(&rel).cloned().flatten()
        };
        let before_sha256 = digest(&old)?;
        anyhow::ensure!(
            before_sha256 == *versions.get(&rel).unwrap_or(&absent),
            "ingest output conflict: {rel}"
        );
        let full_after = if append {
            format!("{}{after}", old.as_deref().unwrap_or(""))
        } else {
            after.clone()
        };
        let after_sha256 = digest(&Some(full_after))?;
        if before_sha256 != after_sha256 {
            writes.insert(
                rel,
                PreparedWrite {
                    before_sha256,
                    after_sha256,
                    after,
                    append,
                    before_len: old.as_ref().map_or(0, String::len),
                },
            );
        }
    }
    let expected = json!({"index": stateroot_core::wiki::read_index(stage.path()),
        "inbox": stateroot_core::wiki::show(stage.path(), "wiki/pages/_inbox.md")?});
    Ok(MaterializedIngest {
        writes,
        summary,
        expected,
        copied_bytes,
    })
}

fn ingest_provenance(ctx: &Ctx, prepared: &PreparedIngest) -> Result<Value> {
    Ok(
        json!({"input_sha256": prepared.input_sha256, "sources_sha256": prepared.sources_sha256,
        "config_sha256": prepared.config_sha256, "dependencies": prepared.dependencies,
        "dependencies_sha256": prepared.dependencies_sha256, "learning_sha256": prepared.learning_sha256,
        "applied_at": now_rfc3339(),
        "materialization": prepared.materialization,
        "outputs": prepared.writes.iter().map(|(path, write)| Ok(json!({"path": path,
            "before_sha256": write.before_sha256, "prepared_after_sha256": write.after_sha256,
            "observed_after_sha256": digest(&optional_text(&local_store::root(&ctx.cwd).join(path))?)?,
            "prepared_bytes": write.after.len(), "append": write.append})))
            .collect::<Result<Vec<_>>>()?, "label": "synthesized; application observed, semantic correctness unverified"}),
    )
}

fn same_prepared_inputs(ctx: &Ctx, prepared: &PreparedIngest) -> bool {
    prepared.input_files.iter().all(|(rel, before)| {
        optional_text(&local_store::root(&ctx.cwd).join(rel))
            .and_then(|current| digest(&current))
            .is_ok_and(|current| {
                &current == before
                    || prepared
                        .writes
                        .get(rel)
                        .is_some_and(|write| current == write.after_sha256)
            })
    })
}

fn append_prefix_matches(current: Option<&str>, write: &PreparedWrite) -> Result<bool> {
    if write.before_sha256 == digest(&Option::<String>::None)? {
        return Ok(true);
    }
    let prefix = current.and_then(|text| text.get(..write.before_len));
    Ok(digest(&prefix)? == write.before_sha256)
}

fn write_is_current_or_prepared(current: Option<&str>, write: &PreparedWrite) -> Result<bool> {
    let identity = digest(&current)?;
    Ok(identity == write.before_sha256
        || identity == write.after_sha256
        || (write.append && append_prefix_matches(current, write)?))
}

fn publication_order(rel: &str) -> u8 {
    // Preserve the existing compactor's durable archive -> history -> apex
    // ordering. Losing an interrupted journal can never make demotion precede
    // preservation, and recovery retains the journal until every write lands.
    if rel == "wiki/pages/memory/apex-archive.md" {
        0
    } else if rel.starts_with("wiki/pages/") {
        1
    } else if rel == local_store::EPISODIC_PATH {
        3
    } else if rel == local_store::MEMORY_CORE_PATH {
        4
    } else {
        2
    }
}

fn apply_prepared(
    ctx: &Ctx,
    home: &Path,
    prepared: &PreparedIngest,
    gov: &mut IngestGovernance,
) -> Result<Option<String>> {
    let root = local_store::root(&ctx.cwd);
    // Lock ordering is unchanged. Publication ordering is archive before apex.
    let _memory = stateroot_core::hot_apex::write_guard(&ctx.cwd)?;
    let _wiki = stateroot_core::wiki::write_guard(&ctx.cwd)?;
    let _episodic = local_store::episodic_guard(&ctx.cwd)?;
    if !prepared.publication_started {
        let declared = stateroot_core::wiki::compile_dependencies(&ctx.cwd)?;
        anyhow::ensure!(
            declared == prepared.dependencies,
            "ingest dependency declarations changed"
        );
        anyhow::ensure!(
            same_prepared_inputs(ctx, prepared),
            "ingest input changed before publication"
        );
        anyhow::ensure!(
            learning_identity(ctx, home)? == prepared.learning_sha256,
            "ingest learning inputs changed before publication"
        );
        anyhow::ensure!(
            digest(&resolve_dependencies(ctx, home, &declared)?)? == prepared.dependencies_sha256,
            "ingest dependency bytes changed"
        );
    }
    for (rel, write) in &prepared.writes {
        relative_dependency(&root, rel)?;
        if rel == "wiki/log.md" {
            stateroot_core::wiki::prepared_log_needs_append(
                optional_text(&root.join(rel))?.as_deref(),
                &digest(&(prepared.input_sha256.as_str(), write.after_sha256.as_str()))?,
                &write.before_sha256,
                &write.after,
            )?;
        } else {
            anyhow::ensure!(
                write_is_current_or_prepared(optional_text(&root.join(rel))?.as_deref(), write)?,
                "ingest output conflict: {rel}"
            );
        }
    }
    // Durable commit intent precedes the first authoritative byte. From here
    // source/config changes belong to the next assessment, not abandonment of
    // an already-started mutation. Conflicting authored outputs still stop it.
    if !prepared.publication_started {
        gov.pending
            .as_mut()
            .expect("saved pending apply")
            .publication_started = true;
        save_ingest_gov(ctx, gov)?;
    }
    let mut writes: Vec<_> = prepared.writes.iter().collect();
    writes.sort_by_key(|(rel, _)| (publication_order(rel), *rel));
    for (n, (rel, write)) in writes.into_iter().enumerate() {
        if rel == "wiki/log.md" {
            continue;
        }
        let path = root.join(rel);
        let current = optional_text(&path)?;
        if digest(&current)? == write.after_sha256
            || (write.append
                && current
                    .as_deref()
                    .is_some_and(|text| text.contains(&write.after)))
        {
            continue;
        }
        anyhow::ensure!(
            write_is_current_or_prepared(current.as_deref(), write)?,
            "ingest output conflict: {rel}"
        );
        let after = if write.append {
            format!("{}{}", current.as_deref().unwrap_or(""), write.after)
        } else {
            write.after.clone()
        };
        stateroot_core::safe_io::atomic_replace(&path, after.as_bytes())?;
        local_store::report_written(&ctx.cwd, rel);
        if std::env::var_os("STATEROOT_TEST_CMD_PROBES").is_some()
            && std::env::var("STATEROOT_TEST_INGEST_INTERRUPT_AFTER")
                .ok()
                .is_some_and(|point| {
                    point == (n + 1).to_string()
                        || (point == "memory" && rel == local_store::MEMORY_CORE_PATH)
                        || (point == "episodic" && rel == local_store::EPISODIC_PATH)
                })
        {
            anyhow::bail!("test interruption after ingest output {}", n + 1);
        }
    }
    // Own generated writes may advance the floor baseline; newly authored
    // notes or changed learning scopes must actually pass through mining.
    let floor = if same_prepared_inputs(ctx, prepared)
        && learning_identity(ctx, home)? == prepared.learning_sha256
    {
        Some(floor_identity(ctx, home)?)
    } else {
        None
    };
    drop(_episodic);
    drop(_wiki);
    drop(_memory);
    if let Some(log) = prepared.writes.get("wiki/log.md") {
        stateroot_core::wiki::append_prepared_log_once(
            &ctx.cwd,
            &digest(&(prepared.input_sha256.as_str(), log.after_sha256.as_str()))?,
            &log.before_sha256,
            &log.after,
        )?;
        if std::env::var_os("STATEROOT_TEST_CMD_PROBES").is_some()
            && std::env::var("STATEROOT_TEST_INGEST_INTERRUPT_AFTER")
                .ok()
                .as_deref()
                == Some("log")
        {
            anyhow::bail!("test interruption after ingest log append");
        }
    }
    Ok(floor)
}

fn deterministic_ingest(ctx: &Ctx, home: &std::path::Path) -> Result<String> {
    let added = stateroot_core::learnings::distill_to_inbox(&ctx.cwd, home)
        .map_err(|e| anyhow::anyhow!(e))?;
    let _ = stateroot_core::wiki::ensure_layout(&ctx.cwd);
    if added > 0 {
        let _ = stateroot_core::wiki::append_log(
            &ctx.cwd,
            &format!("deterministic ingest: {added} bullet(s) → wiki/pages/_inbox.md"),
        );
    }
    let _ = stateroot_core::memory_index::rebuild_if_needed(&ctx.cwd, home);
    Ok(format!("deterministic ingest: {added} new inbox bullet(s)"))
}

fn apply_agentic_ingest(
    ctx: &Ctx,
    home: &std::path::Path,
    parsed: &Value,
    actor: Option<&str>,
) -> Result<String> {
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
            // Model output is a page slug, never an arbitrary filesystem path.
            let relative = format!("{}.md", slug.trim().trim_end_matches(".md"));
            relative_dependency(&stateroot_core::wiki::pages_dir(&ctx.cwd), &relative)?;
            let _ = stateroot_core::wiki::write_page(&ctx.cwd, slug, body, summary, kind, actor)?;
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
            let result = result?;
            anyhow::ensure!(
                result.success,
                "memory apply failed: {}",
                result.error.unwrap_or_default()
            );
            facts += 1;
        }
    }
    let _ = stateroot_core::wiki::append_log(
        &ctx.cwd,
        &format!("agentic ingest: pages={pages} memory_facts={facts}"),
    );
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
    let mut body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": INGEST_SYSTEM},
            {"role": "user", "content": payload},
        ],
    });
    if let Some(extra) = ctx.config.synthesis.extra_body.as_object() {
        for (key, value) in extra {
            body[key] = value.clone();
        }
    }
    let client = provider_client()?;
    let resp = client
        .post(format!("{base_url}/chat/completions"))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = read_body_bounded(resp).await?;
    if !status.is_success() {
        anyhow::bail!(
            "ingest provider returned {status}: {}",
            error_excerpt(&text)
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
    let parsed: Value = serde_json::from_str(json_text)?;
    anyhow::ensure!(
        parsed.is_object()
            && parsed.get("pages").is_some_and(Value::is_array)
            && parsed.get("memory_facts").is_some_and(Value::is_array),
        "ingest requires pages and memory_facts arrays"
    );
    Ok(parsed)
}

/// The keyless stage remains independent even while optional publication
/// awaits output reconciliation. Its writes use the existing Wiki owner and
/// must never be rolled back to satisfy an older optional journal.
fn advance_deterministic_floor(
    ctx: &Ctx,
    home: &Path,
    gov: &mut IngestGovernance,
) -> Result<(String, bool)> {
    let floor = floor_identity(ctx, home)?;
    if gov.floor_sha256 == floor {
        return Ok(("deterministic ingest unchanged".into(), true));
    }
    let before = floor_sources(ctx, home)?;
    let summary = deterministic_ingest(ctx, home)?;
    if before != floor_sources(ctx, home)? {
        return Ok((
            format!("{summary}; source changed during deterministic ingest; retry pending"),
            false,
        ));
    }
    gov.floor_sha256 = floor_identity(ctx, home)?;
    gov.floor_runs += 1;
    gov.last_run_at = now_rfc3339();
    save_ingest_gov(ctx, gov)?;
    Ok((summary, true))
}

/// Dual-mode wiki/memory ingest (session_end / pre_compact / `wiki compile`).
///
/// Deterministic floor always mines into `_inbox.md`. When agentic, also asks
/// the model to file pages + curated MEMORY facts. Never writes soul.
pub async fn try_ingest(ctx: &Ctx, force: bool) -> Result<String> {
    let home = stateroot_core::harness_install::home_dir().map_err(anyhow::Error::msg)?;
    let _guard = stateroot_core::safe_io::ResourceLock::acquire(
        local_store::root(&ctx.cwd).join("local/locks/ingest.lock"),
    )
    .map_err(anyhow::Error::msg)?;
    stateroot_core::hot_apex::ensure_migrated(&ctx.cwd, &home);
    stateroot_core::wiki::ensure_layout(&ctx.cwd)?;
    let mut gov = load_ingest_gov(ctx)?;
    let mut summary = "deterministic ingest unchanged".to_string();
    // A started publication is a durable mutation, recoverable even without
    // credentials. Never discard its recovery bytes on a new boundary.
    if force
        && gov.optional_detail.contains("output conflict")
        && gov
            .pending
            .as_ref()
            .is_some_and(|pending| !pending.publication_started)
    {
        gov.pending = None;
    }
    if let Some(pending) = gov.pending.clone() {
        let unchanged = if pending.publication_started {
            true
        } else if mode(ctx) == CompilerMode::Agentic {
            let endpoint = resolved_endpoint().expect("agentic endpoint");
            let declared = stateroot_core::wiki::compile_dependencies(&ctx.cwd)?;
            same_prepared_inputs(ctx, &pending)
                && pending.learning_sha256 == learning_identity(ctx, &home)?
                && pending.config_sha256 == endpoint_identity(ctx, &endpoint, INGEST_SYSTEM)?
                && declared == pending.dependencies
                && digest(&resolve_dependencies(ctx, &home, &declared)?)?
                    == pending.dependencies_sha256
        } else {
            false
        };
        if unchanged {
            match apply_prepared(ctx, &home, &pending, &mut gov) {
                Ok(floor_after) => {
                    gov.last_success = Some(ingest_provenance(ctx, &pending)?);
                    gov.pending = None;
                    gov.optional_status = "applied".into();
                    gov.optional_detail.clear();
                    if let Some(floor_after) = floor_after {
                        gov.floor_sha256 = floor_after;
                    }
                    gov.optional_success_sha256 = pending.success_sha256.clone();
                    save_ingest_gov(ctx, &gov)?;
                    let _ = stateroot_core::memory_index::rebuild_if_needed(&ctx.cwd, &home);
                    summary = format!("{} (resumed)", pending.summary);
                }
                Err(err) => {
                    gov.optional_status = "apply_pending".into();
                    gov.optional_detail = err.to_string();
                    save_ingest_gov(ctx, &gov)?;
                    let (floor_summary, _) = if same_prepared_inputs(ctx, &pending)
                        && learning_identity(ctx, &home)? == pending.learning_sha256
                    {
                        ("deterministic ingest unchanged".into(), true)
                    } else {
                        advance_deterministic_floor(ctx, &home, &mut gov)?
                    };
                    return Ok(format!(
                        "{summary}; {floor_summary}; optional ingest apply pending: {err}"
                    ));
                }
            }
        } else {
            gov.pending = None;
            gov.optional_status = "stale".into();
            gov.optional_detail = "unpublished source or configuration changed".into();
            save_ingest_gov(ctx, &gov)?;
        }
    }
    let (floor_summary, floor_ready) = advance_deterministic_floor(ctx, &home, &mut gov)?;
    summary = format!("{summary}; {floor_summary}");
    if !floor_ready {
        return Ok(summary);
    }
    if mode(ctx) != CompilerMode::Agentic {
        return Ok(summary);
    }
    let endpoint = resolved_endpoint().expect("agentic endpoint");
    let configuration = endpoint_identity(ctx, &endpoint, INGEST_SYSTEM)?;
    let declared = stateroot_core::wiki::compile_dependencies(&ctx.cwd)?;
    let dependencies = resolve_dependencies(ctx, &home, &declared)?;
    let learning = learning_identity(ctx, &home)?;
    let sources = floor_sources(ctx, &home)?;
    let payload = ingest_payload(ctx, dependencies.clone())?;
    let input = digest(&json!({"payload": payload, "configuration": configuration}))?;
    if !force && gov.pending.is_none() && gov.optional_success_sha256 == input {
        return Ok(format!("{summary}; optional ingest unchanged"));
    }

    if force
        || gov.generated.as_ref().is_some_and(|generated| {
            generated.input_sha256 != input || generated.sources_sha256 != sources
        })
    {
        gov.generated = None;
    }
    let generated = if let Some(generated) = gov.generated.clone() {
        generated
    } else {
        let output_versions = output_snapshot(ctx)?;
        let input_files = [
            local_store::EPISODIC_PATH,
            "spool/observations.jsonl",
            "wiki/pages/_inbox.md",
            "wiki/index.md",
        ]
        .into_iter()
        .map(|rel| {
            Ok((
                rel.to_string(),
                digest(&optional_text(&local_store::root(&ctx.cwd).join(rel))?)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
        gov.optional_attempts += 1;
        gov.optional_status = "generating".into();
        save_ingest_gov(ctx, &gov)?;
        let started = std::time::Instant::now();
        let parsed = match call_local_ingest(
            ctx,
            &endpoint.base_url,
            &endpoint.api_key,
            &endpoint.model,
            &serde_json::to_string_pretty(&payload)?,
        )
        .await
        .and_then(|content| parse_ingest_json(&content))
        {
            Ok(parsed) => parsed,
            Err(err) => {
                gov.optional_status = "generation_failed".into();
                gov.optional_detail = err.to_string();
                save_ingest_gov(ctx, &gov)?;
                return Ok(format!("{summary}; optional ingest failed: {err}"));
            }
        };
        let generated = GeneratedIngest {
            input_sha256: input.clone(),
            sources_sha256: sources.clone(),
            parsed,
            output_versions,
            input_files,
            generation_ms: started.elapsed().as_millis(),
        };
        gov.generated = Some(generated.clone());
        gov.optional_status = "generated".into();
        save_ingest_gov(ctx, &gov)?;
        generated
    };
    let prepare_started = std::time::Instant::now();
    let prepared = match prepare_ingest(
        ctx,
        &home,
        &generated.parsed,
        &endpoint.model,
        &generated.output_versions,
    ) {
        Ok(prepared) => prepared,
        Err(err) => {
            gov.optional_status = "prepare_pending".into();
            gov.optional_detail = err.to_string();
            save_ingest_gov(ctx, &gov)?;
            return Ok(format!("{summary}; optional ingest prepare pending: {err}"));
        }
    };
    if sources != floor_sources(ctx, &home)?
        || learning != learning_identity(ctx, &home)?
        || dependencies != resolve_dependencies(ctx, &home, &declared)?
        || payload != ingest_payload(ctx, dependencies.clone())?
    {
        gov.generated = None;
        gov.optional_status = "stale".into();
        gov.optional_detail = "source changed during generation".into();
        save_ingest_gov(ctx, &gov)?;
        return Ok(format!(
            "{summary}; optional ingest stale: source changed during generation"
        ));
    }
    let mut expected_payload = payload;
    expected_payload["index"] = prepared.expected["index"].clone();
    expected_payload["inbox"] = prepared.expected["inbox"].clone();
    let success_sha256 =
        digest(&json!({"payload": expected_payload, "configuration": configuration}))?;
    let pending = PreparedIngest {
        input_sha256: input,
        success_sha256,
        sources_sha256: sources,
        input_files: generated.input_files,
        config_sha256: configuration.clone(),
        dependencies: declared,
        dependencies_sha256: digest(&dependencies)?,
        materialization: json!({"copied_bytes": prepared.copied_bytes, "prepared_bytes": prepared.writes.values().map(|w| w.after.len()).sum::<usize>(),
            "prepare_ms": prepare_started.elapsed().as_millis(), "generation_ms": generated.generation_ms,
            "payload_bytes": serde_json::to_vec(&expected_payload)?.len()}),
        writes: prepared.writes,
        summary: prepared.summary,
        learning_sha256: learning,
        publication_started: false,
    };
    gov.generated = None;
    gov.pending = Some(pending.clone());
    gov.optional_status = "apply_pending".into();
    save_ingest_gov(ctx, &gov)?;
    let floor_after = match apply_prepared(ctx, &home, &pending, &mut gov) {
        Ok(floor_after) => floor_after,
        Err(err) => {
            gov.optional_detail = err.to_string();
            save_ingest_gov(ctx, &gov)?;
            return Ok(format!("{summary}; optional ingest apply pending: {err}"));
        }
    };
    gov.last_success = Some(ingest_provenance(ctx, &pending)?);
    gov.pending = None;
    gov.optional_status = "applied".into();
    gov.optional_detail.clear();
    if let Some(floor_after) = floor_after {
        gov.floor_sha256 = floor_after;
    }
    gov.optional_success_sha256 = pending.success_sha256.clone();
    save_ingest_gov(ctx, &gov)?;
    let _ = stateroot_core::memory_index::rebuild_if_needed(&ctx.cwd, &home);
    Ok(format!("{summary}; {}", pending.summary))
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

    // --- provider transport guards (plan_2026-10-20T07-10-54Z) -----------

    fn test_ctx() -> Ctx {
        Ctx {
            cwd: std::env::temp_dir(),
            config_dir: std::env::temp_dir(),
            config: stateroot_core::config::AppConfig::default(),
        }
    }

    fn endpoint_to(base_url: &str) -> SynthesisEndpoint {
        SynthesisEndpoint {
            provider: "deepseek",
            api_key: "test-key".into(),
            base_url: base_url.into(),
            model: "deepseek-v4-flash".into(),
        }
    }

    #[test]
    fn error_excerpt_short_text_passes_through() {
        assert_eq!(error_excerpt("plain failure"), "plain failure");
        assert_eq!(error_excerpt(""), "");
    }

    #[test]
    fn error_excerpt_truncates_on_char_boundaries() {
        // 299 ASCII bytes then a 3-byte codepoint: byte 300 is mid-char —
        // the old byte slice panicked on exactly this shape.
        let mut text = "x".repeat(299);
        text.push('界');
        text.push_str(" tail");
        let excerpt = error_excerpt(&text);
        assert!(excerpt.starts_with(&"x".repeat(299)));
        assert!(excerpt.contains('界'), "wide char kept whole: {excerpt:?}");
        assert!(excerpt.ends_with("…[truncated]"));
        // Exactly 300 chars: no marker.
        let exact = "y".repeat(PROVIDER_ERROR_EXCERPT_CHARS);
        assert_eq!(error_excerpt(&exact), exact);
    }

    #[test]
    fn redirect_rules_follow_only_exact_origin() {
        let prev = reqwest::Url::parse("https://api.example.com/v1/chat/completions").unwrap();
        // Same origin, new path (gateway move): allowed.
        assert!(redirect_follow_allowed(
            &prev,
            &reqwest::Url::parse("https://api.example.com/v2/chat/completions").unwrap()
        )
        .is_ok());
        // Explicit default port is still the same origin.
        assert!(redirect_follow_allowed(
            &prev,
            &reqwest::Url::parse("https://api.example.com:443/v2/chat/completions").unwrap()
        )
        .is_ok());
        // HTTPS→HTTP downgrade: refused even on the same host+port.
        let down = redirect_follow_allowed(
            &prev,
            &reqwest::Url::parse("http://api.example.com/v1/chat/completions").unwrap(),
        )
        .expect_err("downgrade must be refused");
        assert!(down.contains("downgrade"), "{down}");
        // Other port, host or scheme: refused as cross-origin.
        for next in [
            "https://api.example.com:8443/v1/chat/completions",
            "https://other.example.com/v1/chat/completions",
            "http://api.example.com:8080/v1/chat/completions",
        ] {
            let err = redirect_follow_allowed(&prev, &reqwest::Url::parse(next).unwrap())
                .expect_err("cross-origin must be refused");
            assert!(
                err.contains("cross-origin") || err.contains("downgrade"),
                "{err}"
            );
        }
    }

    /// Error body whose byte 300 lands mid-codepoint ("界" at bytes 299..302).
    fn straddling_error_body() -> Vec<u8> {
        let mut body = b"error-code-teapot ".to_vec();
        body.resize(299, b'x');
        body.extend_from_slice("界".as_bytes());
        body.extend_from_slice(b" detail after the wide char");
        body
    }

    #[tokio::test]
    async fn call_provider_unicode_error_renders_without_panic() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(500).set_body_bytes(straddling_error_body()),
            )
            .mount(&server)
            .await;
        let err = call_provider(&test_ctx(), &endpoint_to(&server.uri()), "sys", "user")
            .await
            .expect_err("500 must error");
        let msg = format!("{err:#}");
        assert!(msg.contains("500"), "{msg}");
        assert!(
            msg.contains("error-code-teapot"),
            "content preserved: {msg}"
        );
        assert!(!msg.contains("test-key"), "token never rendered: {msg}");
    }

    #[tokio::test]
    async fn call_local_ingest_unicode_error_renders_without_panic() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(500).set_body_bytes(straddling_error_body()),
            )
            .mount(&server)
            .await;
        let err = call_local_ingest(&test_ctx(), &server.uri(), "test-key", "model", "payload")
            .await
            .expect_err("500 must error");
        let msg = format!("{err:#}");
        assert!(msg.contains("500"), "{msg}");
        assert!(
            msg.contains("error-code-teapot"),
            "content preserved: {msg}"
        );
        assert!(!msg.contains("test-key"), "token never rendered: {msg}");
    }

    #[tokio::test]
    async fn call_local_ingest_direct_endpoint_and_oversize_guard() {
        // Direct configured endpoint keeps working.
        let server = wiremock::MockServer::start().await;
        let content = serde_json::json!({"pages": [], "memory_facts": []}).to_string();
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"choices": [{"message": {"content": content}}]}),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        let got = call_local_ingest(&test_ctx(), &server.uri(), "test-key", "model", "payload")
            .await
            .expect("direct endpoint works");
        assert!(got.contains("memory_facts"), "{got}");
        server.verify().await;

        // A body over the cap errors honestly instead of being read whole.
        let big = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(vec![
                b' ';
                PROVIDER_MAX_BODY_BYTES
                    + 1
            ]))
            .mount(&big)
            .await;
        let err = call_local_ingest(&test_ctx(), &big.uri(), "test-key", "model", "payload")
            .await
            .expect_err("oversize must error");
        assert!(format!("{err:#}").contains("byte cap"), "{err:#}");
    }

    #[tokio::test]
    async fn provider_client_refuses_cross_origin_and_follows_same_origin() {
        let client = provider_client().expect("client builds");
        // Cross-origin: the request fails at the entry hop, target untouched.
        let target = wiremock::MockServer::start().await;
        let entry = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/start-cross"))
            .respond_with(
                wiremock::ResponseTemplate::new(307)
                    .insert_header("location", format!("{}/moved", target.uri())),
            )
            .mount(&entry)
            .await;
        let err = client
            .get(format!("{}/start-cross", entry.uri()))
            .send()
            .await
            .expect_err("cross-origin redirect must error");
        // reqwest's top-level Display is generic; the refusal reason lives
        // in the source chain.
        let mut msg = err.to_string();
        let mut src = std::error::Error::source(&err);
        while let Some(e) = src {
            msg.push_str(&format!(": {e}"));
            src = e.source();
        }
        assert!(msg.contains("cross-origin"), "{msg}");
        assert!(
            target
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "cross-origin target contacted"
        );
        // Same origin path move: followed.
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/start"))
            .respond_with(
                wiremock::ResponseTemplate::new(307)
                    .insert_header("location", format!("{}/v2/start", entry.uri())),
            )
            .mount(&entry)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/v2/start"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&entry)
            .await;
        let resp = client
            .get(format!("{}/start", entry.uri()))
            .send()
            .await
            .expect("same-origin redirect follows");
        assert_eq!(resp.status(), 200);
    }
}
