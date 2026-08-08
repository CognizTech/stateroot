//! `stateroot synthesize [--force]` — local synthesis (M3).
//!
//! Bundle from transcripts (lifted bundle builder) → direct OpenAI-compatible
//! chat call (user's own provider/key via `[synthesis]` in config.toml;
//! DeepSeek/OpenAI/Ollama/litellm all work; `extra_body` is merged verbatim
//! for non-thinking passthrough) → strict-JSON sections merged into the
//! local handoff with provenance.
//!
//! Governance (persisted in `.stateroot/synthesis-gov.json`): hash-idempotent
//! skip, min-interval, daily cap. Honest unavailability: no key → a note and
//! exit 0 — synthesis never blocks resume or import.

use anyhow::Result;
use serde_json::{json, Value};
use sha2::Digest as _;
use stateroot_core::local_store::{self, now_rfc3339};

use super::Ctx;

const GOV_PATH: &str = "synthesis-gov.json";
const MAX_BUNDLE_CHARS: usize = 120_000;

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

fn resolved_key(ctx: &Ctx) -> String {
    if let Ok(env) = std::env::var("STATEROOT_SYNTHESIS_API_KEY") {
        let env = env.trim().to_string();
        if !env.is_empty() {
            return env;
        }
    }
    ctx.config.synthesis.api_key.clone()
}

/// `stateroot synthesize [--force]`
pub async fn run(ctx: &Ctx, force: bool) -> Result<()> {
    ctx.require_project()?;
    if !ctx.config.synthesis.enabled {
        println!("synthesis disabled (synthesis.enabled=false in config.toml) — deterministic pack intact");
        return Ok(());
    }
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))?;

    // Bundle + hash-idempotence.
    let bundles =
        stateroot_core::transcripts::bundle::build_bundles(&home, &ctx.cwd, None, MAX_BUNDLE_CHARS);
    if bundles.is_empty() {
        println!("no sessions to synthesize (transcript stores are empty)");
        return Ok(());
    }
    let bundle_json = serde_json::to_string(&json!({
        "schema_version": "stateroot.synth_bundle.v1",
        "sessions": bundles,
    }))?;
    let bundle_sha = format!("{:x}", sha2::Sha256::digest(bundle_json.as_bytes()));
    let mut gov = load_governance(ctx);
    if !force && gov.last_bundle_sha256 == bundle_sha {
        println!(
            "synthesis skipped — bundle unchanged since the last run (pass --force to re-run)"
        );
        return Ok(());
    }

    // Rate governance.
    let now = now_rfc3339();
    let today = &now[..10];
    if gov.day != today {
        gov.day = today.to_string();
        gov.runs_today = 0;
    }
    if !force && gov.runs_today >= ctx.config.synthesis.daily_cap {
        println!(
            "synthesis skipped — daily cap ({}) reached; deterministic pack intact",
            ctx.config.synthesis.daily_cap
        );
        return Ok(());
    }
    if !force && !gov.last_run_at.is_empty() {
        if let Ok(last) = chrono::DateTime::parse_from_rfc3339(&gov.last_run_at) {
            let elapsed = chrono::Utc::now() - last.with_timezone(&chrono::Utc);
            if elapsed.num_seconds() < ctx.config.synthesis.min_interval_seconds {
                println!(
                    "synthesis skipped — min interval ({}s) not elapsed ({}s)",
                    ctx.config.synthesis.min_interval_seconds,
                    elapsed.num_seconds()
                );
                return Ok(());
            }
        }
    }

    let api_key = resolved_key(ctx);
    if api_key.is_empty() {
        println!(
            "synthesis unavailable — no API key (set synthesis.api_key in config.toml or STATEROOT_SYNTHESIS_API_KEY); deterministic pack intact"
        );
        return Ok(());
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

    let content = call_provider(ctx, &base_url, &api_key, &model, &bundle_json).await?;
    let sections = parse_sections(&content)?;
    let synthesized = json!({
        "sections": sections,
        "provenance": {
            "bundle_sha256": bundle_sha,
            "model": model,
            "generated_at": now_rfc3339(),
            "labeled": "synthesized — not verified",
        }
    });
    merge_into_handoff(ctx, &synthesized)?;

    gov.last_bundle_sha256 = bundle_sha;
    gov.last_run_at = now;
    gov.runs_today += 1;
    save_governance(ctx, &gov)?;
    println!(
        "synthesis merged into the local handoff ({} section(s); labeled synthesized, never verified)",
        sections.as_object().map(|m| m.len()).unwrap_or(0)
    );
    Ok(())
}

async fn call_provider(
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
    // extra_body passthrough (non-thinking flags, temperature, vendor opts).
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

/// Parse the strict-JSON sections, tolerating a fenced reply.
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

fn merge_into_handoff(ctx: &Ctx, synthesized: &Value) -> Result<()> {
    let path = local_store::root(&ctx.cwd).join(local_store::HANDOFF_CURRENT_PATH);
    let text = std::fs::read_to_string(&path)
        .map_err(|_| anyhow::anyhow!("no local handoff to merge into — write one first (`stateroot handoff write` or `stateroot import`)"))?;
    let mut packet: Value = serde_json::from_str(&text)?;
    packet["synthesized"] = synthesized.clone();
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&packet)?),
    )?;
    Ok(())
}
