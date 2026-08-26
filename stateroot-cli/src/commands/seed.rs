//! Init seeding — deterministic application plus opt-in LLM enrichment.
//!
//! The deterministic pass always runs at `stateroot init` and writes only
//! into placeholder/empty slots (never overwrites user content). The opt-in
//! `--synthesize [--synthesize-with <backend>]` pass asks a local harness CLI
//! (registry delegation spec, piped stdout) or the DeepSeek/OpenAI API path
//! for a richer seed; its output is labeled `synthesized — unverified` and
//! may replace same-origin init-seed fields. Synthesis problems never fail
//! init: note + keep the deterministic seed + exit 0.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};
use stateroot_core::context_pack;
use stateroot_core::local_store::{self, now_rfc3339};
use stateroot_core::seed::SeedDraft;
use stateroot_core::skill_federation::{
    binary_probe, load_registry, normalize_harness, DelegationSpec,
};

use super::{compiler, harness_cli, note, Ctx};

const PLACEHOLDER_OBJECTIVES: &str =
    "# Objectives\n\nDescribe the project goal and success criteria.";
const PLACEHOLDER_MEMORY: &str = "# Project Memory\n\nCurated long-term memory for this project.";
const HARNESS_TIMEOUT: Duration = Duration::from_secs(120);

/// Auto backend preference: local harness CLIs first (registry order here),
/// then the DeepSeek/OpenAI API keys as fallback.
const HARNESS_PREFERENCE: &[&str] = &[
    "claude",
    "codex",
    "kimi",
    "gemini",
    "opencode",
    "openclaw",
    "hermes",
    "pi",
    "grok",
    "zero",
    "antigravity",
    "omp",
    "devin",
];

const INIT_SYNTH_SYSTEM: &str = "You are the StateRoot init synthesizer. Read the observed context pack. Produce STRICT JSON with keys: objective (string), context_summary (string), next_actions (array of strings), memory_facts (array of strings). Use only substance present in the input. Never invent. Empty stays empty. No prose outside the JSON.";

/// A resolved synthesis backend.
#[derive(Debug)]
enum SynthesisBackend {
    /// Local harness CLI rendered from its registry delegation spec.
    HarnessCli { id: String, spec: DelegationSpec },
    /// DeepSeek/OpenAI chat-completions endpoint.
    Api(compiler::SynthesisEndpoint),
}

/// Seed strings an [`apply`] pass wrote, so the opt-in synthesized pass may
/// replace same-origin init-seed fields (never user content).
#[derive(Debug, Default)]
pub struct AppliedSeed {
    objective: Option<String>,
    objectives_body: Option<String>,
    memory_section: Option<String>,
}

/// Deterministic seed + optional `--synthesize` enrichment for `init`.
pub async fn run(
    ctx: &Ctx,
    dir: &Path,
    project_id: &str,
    synthesize: bool,
    synthesize_with: Option<&str>,
) -> Result<()> {
    // The pack must be built before later init steps (convenience layer,
    // projections) add their own files to the tree: empty stays empty.
    let pack = context_pack::build(dir);
    let draft = stateroot_core::seed::extract(dir, &pack);
    let mut prior = AppliedSeed::default();
    if draft.is_empty() {
        println!("  nothing to seed (no repo docs)");
    } else {
        prior = apply(dir, project_id, &draft, "observed", None)?;
    }
    if !synthesize {
        return Ok(());
    }

    let candidates: Vec<SynthesisBackend> = match synthesize_with {
        Some(name) => match resolve_named(name)? {
            Some(backend) => vec![backend],
            None => return Ok(()),
        },
        None => {
            let candidates = auto_candidates();
            if candidates.is_empty() {
                note!(
                    "  synthesis skipped — no harness CLI or API key available; deterministic seed intact"
                );
                return Ok(());
            }
            candidates
        }
    };

    let user = serde_json::to_string(&pack.to_synth_value())?;
    let mut synthesized: Option<(SeedDraft, String)> = None;
    for backend in &candidates {
        match attempt(ctx, dir, backend, &user).await {
            Ok(outcome) => {
                synthesized = Some(outcome);
                break;
            }
            Err(err) => match backend {
                SynthesisBackend::HarnessCli { id, .. } => {
                    note!("  harness {id} gave no usable output ({err:#})")
                }
                SynthesisBackend::Api(endpoint) => {
                    note!("  synthesis via {} failed ({err:#})", endpoint.provider)
                }
            },
        }
    }
    let Some((sdraft, label)) = synthesized else {
        note!("  synthesis unavailable — deterministic seed intact");
        return Ok(());
    };
    if sdraft.is_empty() {
        note!("  synthesis via {label} returned an empty draft — deterministic seed intact");
        return Ok(());
    }

    let provenance = format!("synthesized — unverified ({label})");
    apply(dir, project_id, &sdraft, &provenance, Some(&prior))?;

    // Merge through the existing handoff path so resume's
    // "(synthesized — unverified)" rendering covers the init seed.
    let mut sections = serde_json::Map::new();
    if let Some(summary) = &sdraft.context_summary {
        sections.insert("progress_report".into(), json!(summary));
    }
    if !sdraft.memory_facts.is_empty() {
        sections.insert(
            "decisions_and_amendments".into(),
            json!(sdraft.memory_facts),
        );
    }
    if !sdraft.next_actions.is_empty() {
        sections.insert("residual_work".into(), json!(sdraft.next_actions));
    }
    if !sections.is_empty() {
        let flat = compiler::flatten_synthesized(
            &Value::Object(sections),
            json!({
                "labeled": "synthesized — not verified",
                "backend": label,
                "origin": "init-seed",
                "generated_at": now_rfc3339(),
            }),
        );
        compiler::merge_into_handoff_at(dir, &flat)?;
    }
    println!("  synthesized seed via {label} (unverified)");
    Ok(())
}

/// Write a seed draft into placeholder/empty slots only. When `prior` is
/// given (synthesized pass), slots still holding the deterministic init-seed
/// content may be replaced — both are init seed, neither is user content.
fn apply(
    project_dir: &Path,
    project_id: &str,
    draft: &SeedDraft,
    provenance: &str,
    prior: Option<&AppliedSeed>,
) -> Result<AppliedSeed> {
    let root = local_store::root(project_dir);
    let mut applied = AppliedSeed::default();

    // project/state.json — objective only; current_phase stays "init".
    if let Some(objective) = &draft.objective {
        let state_path = root.join(local_store::STATE_PATH);
        if let Ok(text) = std::fs::read_to_string(&state_path) {
            if let Ok(mut state) = serde_json::from_str::<Value>(&text) {
                let current = state
                    .get("objective")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let may_write = current.is_empty()
                    || prior.and_then(|p| p.objective.as_deref()) == Some(current);
                if may_write {
                    state["objective"] = Value::String(objective.clone());
                    std::fs::write(
                        &state_path,
                        format!("{}\n", serde_json::to_string_pretty(&state)?),
                    )?;
                    applied.objective = Some(objective.clone());
                    let source = if provenance == "observed" {
                        " from README.md"
                    } else {
                        ""
                    };
                    println!("  seeded objective{source} ({provenance})");
                }
            }
        }
    }

    // project/objectives.md — placeholder or same-origin init seed only.
    if let Some(body) = render_objectives(draft, provenance) {
        let path = root.join("project/objectives.md");
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        let may_write = current.trim() == PLACEHOLDER_OBJECTIVES.trim()
            || prior.and_then(|p| p.objectives_body.as_deref()) == Some(current.as_str());
        if may_write {
            std::fs::write(&path, &body)?;
            applied.objectives_body = Some(body);
            println!("  seeded project/objectives.md ({provenance})");
        }
    }

    // memories/MEMORY.md — append under a Seed heading; the synthesized pass
    // replaces the deterministic section when it is still verbatim.
    if !draft.memory_facts.is_empty() {
        let section = render_memory_section(draft, provenance);
        let path = root.join(local_store::MEMORY_CORE_PATH);
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current.trim() == PLACEHOLDER_MEMORY.trim() {
            let mut body = current.trim_end().to_string();
            body.push_str(&section);
            std::fs::write(&path, body)?;
            applied.memory_section = Some(section);
            println!("  seeded memories/MEMORY.md ({provenance})");
        } else if let Some(prior_section) = prior.and_then(|p| p.memory_section.as_deref()) {
            if current.contains(prior_section) {
                std::fs::write(&path, current.replacen(prior_section, &section, 1))?;
                applied.memory_section = Some(section);
                println!("  seeded memories/MEMORY.md ({provenance})");
            }
        }
    }

    // handoffs/current.json — seq-1 init-seed shell when absent; the
    // synthesized pass updates a same-origin init-seed handoff in place.
    let has_handoff_content = draft.objective.is_some()
        || draft.context_summary.is_some()
        || !draft.next_actions.is_empty();
    let handoff_path = root.join(local_store::HANDOFF_CURRENT_PATH);
    if has_handoff_content && !handoff_path.exists() {
        let shell = json!({
            "schema_version": local_store::SCHEMA_HANDOFF_V1,
            "project_id": project_id,
            "seq": 1,
            "from": "cli",
            "created_by_harness": "cli",
            "created_at": now_rfc3339(),
            "objective": draft.objective.clone().unwrap_or_default(),
            "task": "",
            "context_summary": draft.context_summary.clone().unwrap_or_default(),
            "next_actions": draft.next_actions,
            "origin": "init-seed",
            "provenance": provenance,
        });
        local_store::write_handoff_local(project_dir, &shell).map_err(|e| anyhow::anyhow!(e))?;
        println!("  seeded handoffs/current.json ({provenance})");
    } else if has_handoff_content && prior.is_some() {
        let is_init_seed = local_store::read_handoff_local(project_dir)
            .ok()
            .flatten()
            .and_then(|p| p.get("origin").and_then(|v| v.as_str()).map(str::to_string))
            .as_deref()
            == Some("init-seed");
        if is_init_seed {
            let updated = local_store::update_handoff_current(project_dir, |packet| {
                packet["objective"] = json!(draft.objective.clone().unwrap_or_default());
                packet["context_summary"] =
                    json!(draft.context_summary.clone().unwrap_or_default());
                packet["next_actions"] = json!(draft.next_actions);
                packet["provenance"] = json!(provenance);
                true
            })
            .map_err(|e| anyhow::anyhow!(e))?;
            if updated {
                println!("  seeded handoffs/current.json ({provenance})");
            }
        }
    }

    Ok(applied)
}

fn render_objectives(draft: &SeedDraft, provenance: &str) -> Option<String> {
    if draft.objective.is_none() && draft.next_actions.is_empty() {
        return None;
    }
    let mut out = String::from("# Objectives\n");
    if let Some(objective) = &draft.objective {
        out.push_str(&format!("\n{objective}\n"));
    }
    if !draft.next_actions.is_empty() {
        out.push_str(&format!("\n## Next actions ({provenance})\n"));
        for action in &draft.next_actions {
            out.push_str(&format!("\n- {action}"));
        }
        out.push('\n');
    }
    Some(out)
}

fn render_memory_section(draft: &SeedDraft, provenance: &str) -> String {
    let mut out = format!("\n\n## Seed ({provenance} at init)\n");
    for fact in &draft.memory_facts {
        out.push_str(&format!("\n- {fact}"));
    }
    out.push('\n');
    out
}

/// Hidden test seam (mirrors `STATEROOT_TEST_HOME`): when
/// `STATEROOT_TEST_CMD_PROBES` is set, harness binary detection answers from
/// this comma-separated allowlist instead of probing the host PATH.
fn test_cmd_probes() -> Option<Vec<String>> {
    std::env::var("STATEROOT_TEST_CMD_PROBES").ok().map(|raw| {
        raw.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// Auto candidates: probed harness CLIs in preference order, then the API
/// endpoint when a key is set.
fn auto_candidates() -> Vec<SynthesisBackend> {
    let mut candidates = harness_candidates(test_cmd_probes().as_deref());
    if let Some(endpoint) = compiler::resolved_endpoint() {
        candidates.push(SynthesisBackend::Api(endpoint));
    }
    candidates
}

fn harness_candidates(allowlist: Option<&[String]>) -> Vec<SynthesisBackend> {
    let Ok(registry) = load_registry() else {
        return Vec::new();
    };
    let probe = binary_probe(allowlist);
    HARNESS_PREFERENCE
        .iter()
        .filter_map(|pref| {
            let entry = registry.harnesses.iter().find(|e| e.id == *pref)?;
            if entry.delegation.mode != "cli" {
                return None;
            }
            let command = entry.delegation.command.clone()?;
            if !probe(&command) {
                return None;
            }
            Some(SynthesisBackend::HarnessCli {
                id: entry.id.clone(),
                spec: entry.delegation.clone(),
            })
        })
        .collect()
}

/// Resolve an explicit `--synthesize-with <name>`: `deepseek`/`openai` force
/// that API endpoint, a harness id forces that CLI. Unknown names bail with
/// the valid list; unavailable backends note and stand down.
fn resolve_named(name: &str) -> Result<Option<SynthesisBackend>> {
    let key = name.trim().to_ascii_lowercase();
    match key.as_str() {
        "deepseek" | "openai" => {
            let key_env = if key == "deepseek" {
                "DEEPSEEK_API_KEY"
            } else {
                "OPENAI_API_KEY"
            };
            let (deepseek, openai) = if key == "deepseek" {
                (compiler::nonempty_env("DEEPSEEK_API_KEY"), None)
            } else {
                (None, compiler::nonempty_env("OPENAI_API_KEY"))
            };
            let endpoint = compiler::endpoint_from(
                deepseek.as_deref(),
                openai.as_deref(),
                compiler::nonempty_env("STATEROOT_SYNTHESIS_API_BASE").as_deref(),
            );
            match endpoint {
                Some(endpoint) => Ok(Some(SynthesisBackend::Api(endpoint))),
                None => {
                    note!(
                        "  synthesis skipped — backend '{key}' selected but {key_env} is not set; deterministic seed intact"
                    );
                    Ok(None)
                }
            }
        }
        _ => {
            let id = normalize_harness(&key);
            let registry = load_registry().map_err(|e| anyhow::anyhow!(e))?;
            let Some(entry) = registry.harnesses.iter().find(|e| e.id == id) else {
                anyhow::bail!(
                    "unknown synthesis backend '{name}' — valid backends: {}, deepseek, openai",
                    HARNESS_PREFERENCE.join(", ")
                );
            };
            let spec = &entry.delegation;
            let command = spec.command.clone().filter(|_| spec.mode == "cli");
            let Some(command) = command else {
                note!(
                    "  synthesis skipped — harness '{id}' has no CLI delegation; deterministic seed intact"
                );
                return Ok(None);
            };
            if !binary_probe(test_cmd_probes().as_deref())(&command) {
                note!(
                    "  synthesis skipped — harness '{id}' binary '{command}' not found on PATH; deterministic seed intact"
                );
                return Ok(None);
            }
            Ok(Some(SynthesisBackend::HarnessCli {
                id: entry.id.clone(),
                spec: spec.clone(),
            }))
        }
    }
}

/// Run one backend and parse its strict-JSON seed draft.
async fn attempt(
    ctx: &Ctx,
    dir: &Path,
    backend: &SynthesisBackend,
    user: &str,
) -> Result<(SeedDraft, String)> {
    match backend {
        SynthesisBackend::HarnessCli { id, spec } => {
            let prompt = format!("{INIT_SYNTH_SYSTEM}\n\n{user}");
            let content = run_harness_cli(dir, id, spec, &prompt)?;
            Ok((parse_seed_draft(&content)?, id.clone()))
        }
        SynthesisBackend::Api(endpoint) => {
            let content = compiler::call_provider(ctx, endpoint, INIT_SYNTH_SYSTEM, user).await?;
            Ok((parse_seed_draft(&content)?, endpoint.provider.to_string()))
        }
    }
}

/// Launch a harness CLI with piped stdout (unlike `harness run`, which
/// inherits stdio) and capture its response. 120s cap; pty-marked rows may
/// misbehave when piped — the caller notes and falls through honestly.
fn run_harness_cli(dir: &Path, id: &str, spec: &DelegationSpec, prompt: &str) -> Result<String> {
    let output = harness_cli::run_capture(
        dir,
        id,
        spec,
        prompt,
        &harness_cli::LaunchPolicy::default(),
        Some(HARNESS_TIMEOUT),
    )?;
    if output.timed_out {
        anyhow::bail!("timed out after {}s", HARNESS_TIMEOUT.as_secs());
    }
    if !output.status.success() {
        anyhow::bail!("exited with {}", output.status);
    }
    if output.stdout.is_empty() {
        anyhow::bail!("empty stdout");
    }
    Ok(output.stdout)
}

/// Parse a backend response into a seed draft — strict JSON with the same
/// ```json fence-stripping leniency as the compiler.
fn parse_seed_draft(content: &str) -> Result<SeedDraft> {
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
    let string_field = |key: &str| {
        parsed
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let list_field = |key: &str| {
        parsed
            .get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok(SeedDraft {
        objective: string_field("objective"),
        context_summary: string_field("context_summary"),
        next_actions: list_field("next_actions"),
        memory_facts: list_field("memory_facts"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use stateroot_core::skill_federation::build_launch_argv_from_spec;

    #[test]
    fn harness_candidates_follow_preference_order_not_probe_order() {
        let candidates = harness_candidates(Some(&["kimi".into(), "claude".into()]));
        let ids: Vec<String> = candidates
            .iter()
            .map(|c| match c {
                SynthesisBackend::HarnessCli { id, .. } => id.clone(),
                SynthesisBackend::Api(_) => "api".to_string(),
            })
            .collect();
        assert_eq!(ids, ["claude", "kimi"]);
    }

    #[test]
    fn registry_argv_renders_the_prompt_without_skills() {
        let candidates = harness_candidates(Some(&["claude".into()]));
        let SynthesisBackend::HarnessCli { spec, .. } = &candidates[0] else {
            panic!("claude must be a harness CLI candidate");
        };
        assert_eq!(
            build_launch_argv_from_spec(spec, Some("do it"), &[], false),
            Some(vec![
                "claude".into(),
                "--print".into(),
                "--permission-mode".into(),
                "bypassPermissions".into(),
                "do it".into(),
            ])
        );
    }

    #[test]
    fn unprobed_binaries_are_not_candidates() {
        assert!(harness_candidates(Some(&[])).is_empty());
    }

    #[test]
    fn parse_seed_draft_strips_fences_and_stays_strict() {
        let draft = parse_seed_draft(
            "```json\n{\"objective\":\"obj\",\"context_summary\":\"ctx\",\"next_actions\":[\"a\"],\"memory_facts\":[\"f\"]}\n```",
        )
        .expect("fenced json");
        assert_eq!(draft.objective.as_deref(), Some("obj"));
        assert_eq!(draft.context_summary.as_deref(), Some("ctx"));
        assert_eq!(draft.next_actions, ["a"]);
        assert_eq!(draft.memory_facts, ["f"]);
        assert!(parse_seed_draft("not json").is_err());
        let empty = parse_seed_draft("{}").expect("empty object");
        assert!(empty.is_empty());
    }
}
