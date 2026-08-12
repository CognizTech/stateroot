//! `stateroot resume` — compact markdown digest for agent contexts.
//!
//! Output goes to stdout and is designed to be piped straight into a harness
//! prompt: current handoff highlights, hot-apex memory files and a
//! server-built context pack. Diagnostics go to stderr.

use serde_json::{json, Value};
use stateroot_core::local_store;
use stateroot_core::local_store::now_rfc3339;
use std::path::Path;

use super::{note, truncate, Ctx};

/// Maximum characters pulled from each hot-apex memory file.
const HOT_APEX_BUDGET: usize = 1500;

/// Delivery-deduplication key when `--harness` is absent. This is local marker
/// bookkeeping only and must never be recorded as an observed harness actor.
const UNATTRIBUTED_CALLER: &str = "unattributed";

/// Footer appended to resume output AND the hook digest — identical wording
/// in both (plan P4.2).
pub const NO_REFETCH_FOOTER: &str = "This content IS the handoff — do NOT re-fetch it via tools";

/// Session marker: suppress a second full resume for the same handoff seq.
const RESUME_DELIVERED_MARKER: &str = "resume-delivered.json";

/// Per-item cap for actionable/bug lines (rich resume — was 200).
const ITEM_BUDGET: usize = 800;

fn resume_marker_path(project_dir: &Path) -> std::path::PathBuf {
    local_store::root(project_dir).join(RESUME_DELIVERED_MARKER)
}

fn local_handoff_seq(project_dir: &Path) -> Option<i64> {
    local_store::read_handoff_local(project_dir)
        .ok()
        .flatten()
        .and_then(|handoff| handoff.get("seq").and_then(|v| v.as_i64()))
}

fn resume_already_delivered(project_dir: &Path, harness: &str, seq: i64) -> bool {
    let Ok(text) = std::fs::read_to_string(resume_marker_path(project_dir)) else {
        return false;
    };
    let Ok(marker) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    marker.get("harness").and_then(|v| v.as_str()) == Some(harness)
        && marker.get("handoff_seq").and_then(|v| v.as_i64()) == Some(seq)
}

fn mark_resume_delivered(project_dir: &Path, harness: &str, seq: i64) {
    let path = resume_marker_path(project_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let marker = json!({
        "harness": harness,
        "handoff_seq": seq,
        "delivered_at": now_rfc3339(),
    });
    let _ = std::fs::write(path, serde_json::to_string(&marker).unwrap_or_default());
}

/// Render a handoff packet (`stateroot.handoff.v1`) as digest markdown,
/// actionables-first: Next Actions → Open Questions / Failed Approaches →
/// summary → files touched. Rich pack fields (plan_state,
/// progress_summaries, conversation_tail — additive HandoffV1 optionals)
/// render as their own sections when present.
pub fn render_handoff_digest(packet: &Value) -> String {
    render_handoff_digest_with(packet, false)
}

/// [`render_handoff_digest`] with a `deterministic` switch: when true, the
/// LLM-synthesized sections are omitted (everything else identical).
pub fn render_handoff_digest_with(packet: &Value, deterministic: bool) -> String {
    render_handoff_digest_full(packet, deterministic, &[], None)
}

/// Full digest: deterministic switch + durable learnings + active goal (both
/// from synced local files), rendered after Plan State.
pub fn render_handoff_digest_full(
    packet: &Value,
    deterministic: bool,
    durable: &[super::learnings_reader::Learning],
    active_goal: Option<&Value>,
) -> String {
    let mut out = String::new();
    let get_str = |key: &str| packet.get(key).and_then(|v| v.as_str()).unwrap_or("");

    let objective = get_str("objective");
    if !objective.is_empty() {
        out.push_str(&format!("## Objective\n\n{objective}\n\n"));
    }
    let phase = get_str("current_phase");
    if !phase.is_empty() {
        out.push_str(&format!("## Current Phase\n\n{phase}\n\n"));
    }
    // The residual-work view: latest plan snapshot with status markers.
    if let Some(items) = packet.get("plan_state").and_then(|v| v.as_array()) {
        if !items.is_empty() {
            out.push_str("## Plan State\n\n");
            for item in items {
                let step = item.get("step").and_then(|v| v.as_str()).unwrap_or("");
                let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("");
                out.push_str(&format!("- [{}] {}\n", status, truncate(step, ITEM_BUDGET)));
            }
            out.push('\n');
        }
    }
    // Durable preferences (learnings synced to local files, confidence ≥
    // the surface threshold) — after Plan State, before the synthesized tier.
    if !durable.is_empty() {
        let mut section = String::new();
        for learning in durable {
            section.push_str(&format!(
                "- {} ({:.2})\n",
                truncate(&learning.statement, 200),
                learning.confidence
            ));
        }
        out.push_str("## Durable Preferences\n\n");
        out.push_str(&section);
        out.push('\n');
    }
    // Active goal (synced goal docs) — after Durable Preferences, before
    // the synthesized tier.
    if let Some(goal) = active_goal {
        out.push_str("## Active Goal\n\n");
        let objective = goal.get("objective").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!("{objective}\n\n"));
        if let Some(criteria) = goal.get("completion_criteria").and_then(|v| v.as_array()) {
            if let Some(first) = criteria.first() {
                let surface = first
                    .get("verification_surface")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let check = first.get("check").and_then(|v| v.as_str()).unwrap_or("");
                out.push_str(&format!("done-when [{surface}]: {check}\n"));
            }
        }
        let completed = goal
            .get("steps_completed")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let pending = goal
            .get("steps_pending")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let next_step = goal
            .get("plan")
            .and_then(|v| v.as_array())
            .and_then(|plan| {
                plan.iter()
                    .find(|s| s.get("status").and_then(|v| v.as_str()) == Some("in_progress"))
                    .or_else(|| {
                        plan.iter()
                            .find(|s| s.get("status").and_then(|v| v.as_str()) == Some("pending"))
                    })
            })
            .and_then(|s| s.get("step").and_then(|v| v.as_str()))
            .unwrap_or("");
        if !next_step.is_empty() {
            out.push_str(&format!("next: {}\n", truncate(next_step, 200)));
        }
        out.push_str(&format!(
            "steps: {completed} completed, {pending} pending\n"
        ));
        if let Some(budget) = goal.get("budget").and_then(|v| v.as_object()) {
            if !budget.is_empty() {
                let parts: Vec<String> = budget
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect();
                out.push_str(&format!("budget: {}\n", parts.join(" ")));
            }
        }
        out.push('\n');
    }
    // LLM-synthesized sections (labeled tier — "synthesized — unverified";
    // rendered when the handoff carries them, after Plan State).
    if !deterministic {
        if let Some(synthesized) = packet.get("synthesized") {
            let report = synthesized
                .get("progress_report")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !report.trim().is_empty() {
                out.push_str(&format!(
                    "## Progress Report (synthesized — unverified)\n\n{report}\n\n"
                ));
            }
            for (key, title) in [
                (
                    "decisions_and_amendments",
                    "Decisions & Amendments (synthesized)",
                ),
                ("residual_work", "Residual Work (synthesized)"),
                ("resolutions", "Resolutions (synthesized)"),
            ] {
                if let Some(items) = synthesized.get(key).and_then(|v| v.as_array()) {
                    let texts: Vec<String> = items
                        .iter()
                        .map(|item| match item {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .filter(|text| !text.trim().is_empty())
                        .collect();
                    if !texts.is_empty() {
                        out.push_str(&format!("## {title}\n\n"));
                        for text in texts {
                            out.push_str(&format!("- {}\n", truncate(&text, ITEM_BUDGET)));
                        }
                        out.push('\n');
                    }
                }
            }
        }
    }
    // Actionables first.
    for (key, title) in [
        ("next_actions", "Next Actions"),
        ("open_questions", "Open Questions"),
        ("bugs_found", "Failed Approaches / Bugs"),
        ("blockers", "Blockers"),
        ("warnings", "Warnings"),
    ] {
        if let Some(items) = packet.get(key).and_then(|v| v.as_array()) {
            if !items.is_empty() {
                out.push_str(&format!("## {title}\n\n"));
                for item in items {
                    let text = match item {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    out.push_str(&format!("- {}\n", truncate(&text, ITEM_BUDGET)));
                }
                out.push('\n');
            }
        }
    }
    // Summary after the actionables — plus the full progress narrative when
    // the handoff carries compacted summaries (the context_summary of an
    // imported handoff IS the newest one; rendering both would duplicate).
    let summaries: Vec<&str> = packet
        .get("progress_summaries")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|s| s.as_str()).collect())
        .unwrap_or_default();
    let summary = get_str("context_summary");
    if !summary.is_empty() && summaries.first() != Some(&summary) {
        out.push_str(&format!("## Context Summary\n\n{summary}\n\n"));
    }
    // Transcript-sourced sections (Progress Narrative / Milestones /
    // Conversation Tail). B3: once the project has captured state, they are
    // HISTORY — grouped under an "Adoption History" banner after the
    // captured-state content. Before any captured state, they ARE the state
    // and render unmarked.
    let mut history = String::new();
    if !summaries.is_empty() {
        let total = summaries.len();
        history.push_str("## Progress Narrative\n\n");
        for (index, text) in summaries.iter().enumerate() {
            history.push_str(&format!("### [{}/{}]\n\n{}\n\n", index + 1, total, text));
        }
    }
    // Milestones: per-task accomplishment summaries (own heading — the
    // provenance differs from the compacted narrative; truth contract).
    if let Some(items) = packet.get("milestones").and_then(|v| v.as_array()) {
        let texts: Vec<&str> = items.iter().filter_map(|s| s.as_str()).collect();
        if !texts.is_empty() {
            history.push_str("## Milestones\n\n");
            for text in texts {
                history.push_str(&format!("- {}\n", truncate(text, ITEM_BUDGET)));
            }
            history.push('\n');
        }
    }
    // Conversation tail (last message pairs, roles preserved).
    if let Some(items) = packet.get("conversation_tail").and_then(|v| v.as_array()) {
        if !items.is_empty() {
            history.push_str("## Conversation Tail\n\n");
            for item in items {
                let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");
                let text = item.get("text").and_then(|v| v.as_str()).unwrap_or("");
                history.push_str(&format!("**{role}:** {}\n\n", truncate(text, ITEM_BUDGET)));
            }
        }
    }
    if has_captured_state(packet) && !history.is_empty() {
        out.push_str("## Adoption History\n\n");
        out.push_str("_(transcript-imported history — captured state takes precedence)_\n\n");
    }
    out.push_str(&history);
    // Files touched last (full paths — they are short).
    if let Some(items) = packet.get("changed_files").and_then(|v| v.as_array()) {
        if !items.is_empty() {
            out.push_str("## Files Touched\n\n");
            for item in items {
                let text = match item {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                out.push_str(&format!("- {text}\n"));
            }
            out.push('\n');
        }
    }
    let next = get_str("recommended_next_harness");
    if !next.is_empty() {
        out.push_str(&format!("_Recommended next harness: {next}_\n"));
    }
    // Acceptance trail (local-only field; shown when present).
    if let Some(accepted) = packet.get("accepted_by").and_then(|v| v.as_array()) {
        if !accepted.is_empty() {
            let names: Vec<String> = accepted
                .iter()
                .filter_map(|a| a.as_str().map(|s| s.to_string()))
                .collect();
            out.push_str(&format!("_Accepted by: {}_\n", names.join(", ")));
        }
    }
    out.push_str(&format!("\n{NO_REFETCH_FOOTER}\n"));
    out
}

/// Pack-section titles that duplicate handoff-rendered content, mapped to
/// the handoff key whose non-empty presence makes the pack section
/// redundant (dedupe rule 4: the pack's added value is Project State, repo
/// docs, framing/instructions, and anything the handoff lacks). Titles
/// match exactly (case-insensitive) or as a prefix followed by ` [` (the
/// pack emits `Milestones [i/N] (observed task completions)`).
#[allow(dead_code)]
const PACK_DUP_TITLES: &[(&str, &str)] = &[
    ("Plan State", "/plan_state"),
    ("Next Actions", "/next_actions"),
    ("Conversation Tail", "/conversation_tail"),
    ("Changed Files", "/changed_files"),
    ("Objectives", "/objective"),
    ("Milestones", "/milestones"),
    ("Failures", "/bugs_found"),
    ("Handoff Summary", "/context_summary"),
    (
        "Progress Report (synthesized — unverified)",
        "/synthesized/progress_report",
    ),
    (
        "Decisions & Amendments (synthesized)",
        "/synthesized/decisions_and_amendments",
    ),
    ("Residual Work (synthesized)", "/synthesized/residual_work"),
    ("Resolutions (synthesized)", "/synthesized/resolutions"),
];

/// B3 captured-state heuristic (local, honest): the project has real
/// (non-transcript) state once a handoff has moved past seq 1 OR any harness
/// has accepted a handoff. Before that, transcript content IS the state.
fn has_captured_state(packet: &Value) -> bool {
    let seq = packet.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
    if seq > 1 {
        return true;
    }
    packet
        .get("accepted_by")
        .and_then(|v| v.as_array())
        .map(|arr| !arr.is_empty())
        .unwrap_or(false)
}

/// Sections of a context-pack response, defensive about the envelope shape:
/// the live server returns `data = {cached, json_path, md_path, rev, body}`
/// with the pack at `data.body` — try `body.sections` first, then top-level
/// `sections` (older shape).
#[allow(dead_code)]
pub fn pack_sections(pack: &Value) -> Vec<&Value> {
    pack.get("body")
        .and_then(|b| b.get("sections"))
        .or_else(|| pack.get("sections"))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().collect())
        .unwrap_or_default()
}

/// True when a pack section duplicates content the handoff already rendered.
#[allow(dead_code)]
fn is_dup_section(title: &str, handoff: Option<&Value>) -> bool {
    let Some(packet) = handoff else {
        return false;
    };
    let lowered = title.to_lowercase();
    PACK_DUP_TITLES.iter().any(|(dup_title, key)| {
        let dup_lowered = dup_title.to_lowercase();
        let title_matches =
            lowered == dup_lowered || lowered.starts_with(&format!("{dup_lowered} ["));
        title_matches
            && packet
                .pointer(key)
                .map(|v| match v {
                    Value::Array(arr) => !arr.is_empty(),
                    Value::String(s) => !s.trim().is_empty(),
                    other => !other.is_null(),
                })
                .unwrap_or(false)
    })
}

fn read_hot_apex(root: &std::path::Path, rel: &str) -> Option<String> {
    let path = root.join(rel);
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(truncate(text, HOT_APEX_BUDGET))
}

/// Mark the local handoff as accepted by `harness` (deduped, local-only —
/// the server schema is `extra="forbid"` and has no accepted_by field).
/// Returns the accepted count after the update.
pub fn accept_handoff_local(project_dir: &std::path::Path, harness: &str) -> anyhow::Result<usize> {
    let harness = harness.to_string();
    let mut count = 0usize;
    local_store::update_handoff_current(project_dir, |packet| {
        let accepted = packet.as_object_mut().map(|obj| {
            obj.entry("accepted_by")
                .or_insert_with(|| Value::Array(vec![]))
        });
        if let Some(Value::Array(arr)) = accepted {
            if !arr.iter().any(|a| a.as_str() == Some(harness.as_str())) {
                arr.push(Value::String(harness.clone()));
                count = arr.len();
                true
            } else {
                count = arr.len();
                false
            }
        } else {
            false
        }
    })?;
    Ok(count)
}

/// Compact one-line digest composed from the local handoff (objective +
/// top-3 next_actions + seq + accepted count). `None` when no handoff exists.
pub fn digest_footer(project_dir: &std::path::Path) -> Option<String> {
    let packet = local_store::read_handoff_local(project_dir)
        .ok()
        .flatten()?;
    let get_str = |key: &str| packet.get(key).and_then(|v| v.as_str()).unwrap_or("");
    let objective = get_str("objective");
    let next: Vec<String> = packet
        .get("next_actions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .take(3)
                .filter_map(|i| i.as_str().map(|s| truncate(s, 60)))
                .collect()
        })
        .unwrap_or_default();
    let seq = packet.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
    let accepted = packet
        .get("accepted_by")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let mut parts = Vec::new();
    if !objective.is_empty() {
        parts.push(truncate(objective, 80));
    }
    if !next.is_empty() {
        parts.push(format!("next: {}", next.join(" · ")));
    }
    parts.push(format!("seq {seq}"));
    parts.push(format!("accepted {accepted}"));
    Some(format!("digest: {}", parts.join(" · ")))
}

/// Fetch the current handoff packet: server first, local fallback.
/// Local handoff fetch (the fork has no server):
/// `.stateroot/handoffs/current.json` is the only source.
pub fn fetch_handoff(cwd: &std::path::Path) -> (Option<Value>, &'static str) {
    match local_store::read_handoff_local(cwd) {
        Ok(packet) => (packet, "local"),
        Err(err) => {
            note!("warning: could not read local handoff: {err}");
            (None, "none")
        }
    }
}

/// Run `stateroot resume` — fully local: handoff from `.stateroot/`,
/// durable preferences/goals from local docs, skills from federation
/// discovery. There is no server projection or context pack in this variant.
pub fn run(
    ctx: &Ctx,
    harness: Option<&str>,
    no_accept: bool,
    force: bool,
    deterministic: bool,
) -> anyhow::Result<()> {
    let project = ctx.require_project()?;

    // An explicit resume harness is direct local evidence. Persist it before
    // duplicate-delivery suppression so even an early return refreshes the
    // active actor marker.
    let recorded_harness = harness
        .map(|id| super::active_harness::record(&ctx.cwd, id))
        .transpose()?;
    let caller = recorded_harness.as_deref().unwrap_or(UNATTRIBUTED_CALLER);
    let handoff_seq = local_handoff_seq(&ctx.cwd);
    if !force {
        if let Some(seq) = handoff_seq {
            if resume_already_delivered(&ctx.cwd, caller, seq) {
                println!(
                    "(StateRoot resume already delivered this session for handoff seq {seq} — \
skipping duplicate. Pass --force to reprint.)\n\n{NO_REFETCH_FOOTER}"
                );
                return Ok(());
            }
        }
    }

    let (handoff, _handoff_source) = fetch_handoff(&ctx.cwd);

    let root = local_store::root(&ctx.cwd);
    let user_md = stateroot_core::harness_install::home_dir()
        .ok()
        .and_then(|home| stateroot_core::user_profile::read(&home))
        .map(|text| truncate(&text, HOT_APEX_BUDGET));
    let memory_md = read_hot_apex(&root, local_store::MEMORY_CORE_PATH);

    // --- digest (stdout only) ---
    let mut out = String::new();
    let name = if project.name.is_empty() {
        project.project_id.as_str()
    } else {
        project.name.as_str()
    };
    out.push_str(&format!("# StateRoot Resume — {name}\n\n"));

    // Persona (local cache).
    if let Some(persona) = super::persona::resolve(&ctx.config_dir) {
        out.push_str(&persona);
        out.push_str("\n\n---\n\n");
    }

    // Durable preferences from the local learnings files (confidence ≥
    // threshold) — propagated into the digest AND used to dedupe sections.
    // Durable preferences: project + user scopes (user scope landed with
    // M3's `~/.stateroot/learnings`); candidates surface nowhere.
    let mut durable: Vec<super::learnings_reader::Learning> =
        super::learnings_reader::read_local_learnings(&ctx.cwd)
            .into_iter()
            .filter(|l| {
                l.status == "active"
                    && l.scope != "session_candidate"
                    && l.confidence >= super::learnings_reader::SURFACE_THRESHOLD
            })
            .collect();
    if let Ok(home) = stateroot_core::harness_install::home_dir() {
        let mut seen: std::collections::BTreeSet<String> =
            durable.iter().map(|l| l.id.clone()).collect();
        for learning in stateroot_core::learnings::read_scope(&ctx.cwd, &home, "user") {
            if learning.status == "active"
                && learning.confidence >= super::learnings_reader::SURFACE_THRESHOLD
                && seen.insert(learning.id.clone())
            {
                durable.push(super::learnings_reader::Learning {
                    id: learning.id,
                    statement: learning.statement,
                    category: learning.category,
                    confidence: learning.confidence,
                    label: learning.label,
                    sources: learning.sources,
                    scope: learning.scope,
                    status: learning.status,
                });
            }
        }
    }
    // Active goal from the local goal docs.
    let active_goal = super::learnings_reader::read_local_goals(&ctx.cwd)
        .into_iter()
        .find(|g| g.get("lifecycle").and_then(|v| v.as_str()) == Some("active"));

    match handoff {
        Some(ref packet) => {
            out.push_str(&render_handoff_digest_full(
                packet,
                deterministic,
                &durable,
                active_goal.as_ref(),
            ));
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
        None => {
            out.push_str("(no handoff yet — write one with `stateroot handoff write`)\n");
        }
    }

    if user_md.is_some() || memory_md.is_some() {
        out.push_str("\n## Memory (hot apex)\n");
        if let Some(memory) = memory_md {
            out.push_str(&format!("\n### MEMORY.md\n\n{memory}\n"));
        }
        if let Some(user) = user_md {
            out.push_str(&format!("\n### USER.md\n\n{user}\n"));
        }
    }

    // Federated skill index: native origins + user-global and project
    // portable packages. Managed `.agents/skills` projections are skipped by
    // discovery to avoid loops.
    let skills = stateroot_core::skill_federation::discover_all(&ctx.cwd, None).unwrap_or_default();
    if !skills.is_empty() {
        out.push_str(&format!("\n## Federated Skills ({})\n\n", skills.len()));
        for skill in skills.iter().take(40) {
            let route = match skill.lifecycle.as_str() {
                "reference_only" => format!("delegate to {}", skill.native_harness),
                "external_only" => format!("external-only via {}", skill.native_harness),
                _ => format!("portable from {}", skill.harness),
            };
            if skill.description.is_empty() {
                out.push_str(&format!("- `{}` — {} ({route})\n", skill.slug, skill.name));
            } else {
                out.push_str(&format!(
                    "- `{}` — {} ({route})\n",
                    skill.slug, skill.description
                ));
            }
        }
        if skills.len() > 40 {
            out.push_str(&format!(
                "- … {} more; run `stateroot skill list`\n",
                skills.len() - 40
            ));
        }
        out.push_str("\nPortable skills are directly available under `.agents/skills`. ");
        out.push_str("For the full index and routes: `stateroot skill list`.\n");
    }

    // Acceptance mark (unless --no-accept) + compact digest footer.
    if !no_accept {
        if let Some(caller) = recorded_harness.as_deref() {
            match accept_handoff_local(&ctx.cwd, caller) {
                Ok(count) if count > 0 => {
                    let _ = count;
                }
                Ok(_) => {}
                Err(err) => note!("warning: could not mark acceptance: {err}"),
            }
        }
    }
    if let Some(footer) = digest_footer(&ctx.cwd) {
        out.push_str(&format!("\n---\n\n{footer}\n"));
    }

    print!("{out}");
    if let Some(seq) = handoff_seq {
        mark_resume_delivered(&ctx.cwd, caller, seq);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rich_packet() -> Value {
        json!({
            "schema_version": "stateroot.handoff.v1",
            "project_id": "ws-x",
            "seq": 1,
            "objective": "implement the marketplace",
            "next_actions": ["wire the API"],
            "plan_state": [
                {"step": "schema done", "status": "completed"},
                {"step": "wire the API", "status": "in_progress"},
                {"step": "polish", "status": "pending"}
            ],
            "progress_summaries": [
                "s6 newest",
                "s5",
                "s4",
                "s3",
                "s2",
                "s1 oldest"
            ],
            "conversation_tail": [
                {"role": "user", "text": "implement this plan"},
                {"role": "assistant", "text": "schema landed"}
            ],
            "milestones": [
                "Milestone: schema migrated and all endpoints verified against staging.",
                "Milestone: API wired end to end."
            ],
            "changed_files": ["src/api.rs"],
            "created_at": "2026-07-26T00:00:00Z",
            "created_by_harness": "codex"
        })
    }

    #[test]
    fn digest_renders_rich_handoff_fields() {
        let out = render_handoff_digest(&rich_packet());
        // Plan State with status markers.
        assert!(out.contains("## Plan State"), "out: {out}");
        assert!(out.contains("- [completed] schema done"), "out: {out}");
        assert!(out.contains("- [in_progress] wire the API"), "out: {out}");
        assert!(out.contains("- [pending] polish"), "out: {out}");
        // All 6 summaries, newest-first with [i/N] subheadings.
        assert!(out.contains("## Progress Narrative"), "out: {out}");
        for (index, marker) in ["s6 newest", "s5", "s4", "s3", "s2", "s1 oldest"]
            .iter()
            .enumerate()
        {
            assert!(out.contains(marker), "missing {marker}: {out}");
            assert!(
                out.contains(&format!("### [{}/6]", index + 1)),
                "out: {out}"
            );
        }
        // No separate Context Summary when it equals the newest summary.
        assert!(!out.contains("## Context Summary"), "out: {out}");
        // Conversation tail with roles.
        assert!(out.contains("## Conversation Tail"), "out: {out}");
        assert!(out.contains("**user:** implement this plan"), "out: {out}");
        assert!(out.contains("**assistant:** schema landed"), "out: {out}");
        // Milestones with their own provenance-honest heading.
        assert!(out.contains("## Milestones"), "out: {out}");
        assert!(out.contains("- Milestone: schema migrated"), "out: {out}");
        assert!(
            out.contains("- Milestone: API wired end to end."),
            "out: {out}"
        );
        // No captured state (seq 1, no acceptance) → transcript content IS
        // the state, no Adoption History banner.
        assert!(!out.contains("## Adoption History"), "out: {out}");
        // Full file paths.
        assert!(out.contains("- src/api.rs"), "out: {out}");
    }

    #[test]
    fn captured_state_groups_transcript_sections_under_adoption_history() {
        let mut packet = rich_packet();
        packet["seq"] = json!(3);
        packet["progress_summaries"] = json!(["newest summary", "older"]);
        packet["conversation_tail"] = json!([{"role": "user", "text": "hi"}]);
        packet["milestones"] = json!(["Milestone: something real happened here"]);
        let out = render_handoff_digest(&packet);
        assert!(out.contains("## Adoption History"), "out: {out}");
        // Banner precedes the transcript-sourced sections.
        let banner_at = out.find("## Adoption History").expect("banner");
        for section in [
            "## Progress Narrative",
            "## Milestones",
            "## Conversation Tail",
        ] {
            let at = out.find(section).expect(section);
            assert!(banner_at < at, "banner must precede {section}: {out}");
        }

        // Acceptance alone also marks captured state.
        let mut packet = rich_packet();
        packet["accepted_by"] = json!(["cursor"]);
        packet["milestones"] = json!(["Milestone: another real thing happened"]);
        let out = render_handoff_digest(&packet);
        assert!(out.contains("## Adoption History"), "out: {out}");
    }

    #[test]
    fn digest_keeps_distinct_context_summary() {
        let mut packet = rich_packet();
        packet["context_summary"] = Value::String("a DIFFERENT summary".to_string());
        let out = render_handoff_digest(&packet);
        assert!(
            out.contains("## Context Summary\n\na DIFFERENT summary"),
            "out: {out}"
        );
        assert!(out.contains("## Progress Narrative"), "out: {out}");
    }

    #[test]
    fn digest_renders_synthesized_sections_and_dedupes_pack_titles() {
        let mut packet = rich_packet();
        packet["synthesized"] = json!({
            "progress_report": "Solid two-session arc.",
            "decisions_and_amendments": ["chose asyncpg", "amended: pool per loop"],
            "residual_work": ["wire the API"],
            "resolutions": ["celery loop bug resolved"],
            "provenance": {"model": "synthesis", "generated_at": "…", "source_sessions": ["s-1"], "bundle_chars": 1, "bundle_sha256": "…"}
        });
        let out = render_handoff_digest(&packet);
        assert!(
            out.contains("## Progress Report (synthesized — unverified)"),
            "out: {out}"
        );
        assert!(out.contains("Solid two-session arc."), "out: {out}");
        assert!(
            out.contains("## Decisions & Amendments (synthesized)"),
            "out: {out}"
        );
        assert!(out.contains("- chose asyncpg"), "out: {out}");
        assert!(out.contains("## Residual Work (synthesized)"), "out: {out}");
        assert!(out.contains("## Resolutions (synthesized)"), "out: {out}");
        // Placement: after Plan State, before Milestones.
        let plan_at = out.find("## Plan State").expect("plan");
        let synth_at = out.find("## Progress Report").expect("synth");
        let miles_at = out.find("## Milestones").expect("milestones");
        assert!(plan_at < synth_at && synth_at < miles_at, "order: {out}");
        // Pack dedupe: synthesized pack sections skip when rendered.
        assert!(is_dup_section(
            "Progress Report (synthesized — unverified)",
            Some(&packet)
        ));
        assert!(is_dup_section(
            "Decisions & Amendments (synthesized)",
            Some(&packet)
        ));
        assert!(is_dup_section("Residual Work (synthesized)", Some(&packet)));
        assert!(is_dup_section("Resolutions (synthesized)", Some(&packet)));
        // …and NOT skipped when the handoff lacks synthesized content.
        assert!(!is_dup_section(
            "Progress Report (synthesized — unverified)",
            Some(&rich_packet())
        ));
    }

    #[test]
    fn deterministic_render_omits_only_the_synthesized_sections() {
        let mut packet = rich_packet();
        packet["synthesized"] = json!({
            "progress_report": "Solid two-session arc.",
            "decisions_and_amendments": ["chose asyncpg"],
            "residual_work": ["wire the API"],
            "resolutions": ["celery loop bug resolved"]
        });
        let full = render_handoff_digest_with(&packet, false);
        let deterministic = render_handoff_digest_with(&packet, true);
        assert!(full.contains("## Progress Report (synthesized — unverified)"));
        assert!(!deterministic.contains("synthesized"));
        assert!(!deterministic.contains("Solid two-session arc."));
        // Everything else identical.
        assert!(deterministic.contains("## Plan State"));
        assert!(deterministic.contains("## Progress Narrative"));
        assert!(deterministic.contains("## Milestones"));
        assert!(deterministic.contains("## Conversation Tail"));
    }

    #[test]
    fn pack_sections_reads_body_first_then_top_level() {
        // Live server shape: data = {cached, json_path, md_path, rev, body}.
        let live = json!({
            "cached": false,
            "json_path": "/pack.json",
            "md_path": "/pack.md",
            "rev": 3,
            "body": {
                "schema_version": "stateroot.context_pack.v1",
                "sections": [{"title": "Project State", "content": "…"}],
                "truncated": false
            }
        });
        let sections = pack_sections(&live);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0]["title"], "Project State");
        // Legacy top-level shape still works.
        let legacy = json!({"sections": [{"title": "Old", "content": "…"}]});
        assert_eq!(pack_sections(&legacy).len(), 1);
        // Neither → empty.
        assert!(pack_sections(&json!({"cached": true})).is_empty());
    }

    #[test]
    fn dup_section_matches_only_rendered_handoff_content() {
        let packet = rich_packet();
        assert!(is_dup_section("Plan State", Some(&packet)));
        assert!(is_dup_section("Conversation Tail", Some(&packet)));
        assert!(is_dup_section("Changed Files", Some(&packet)));
        assert!(is_dup_section("Objectives", Some(&packet)));
        assert!(is_dup_section("Next Actions", Some(&packet)));
        // New dedupe additions: Milestones (pack titles carry an [i/N]
        // suffix), Failures (bugs_found), Handoff Summary (context_summary).
        let mut packet = rich_packet();
        packet["bugs_found"] = json!(["one failed approach"]);
        packet["context_summary"] = json!("a real summary");
        assert!(is_dup_section("Milestones", Some(&packet)));
        assert!(is_dup_section(
            "Milestones [1/2] (observed task completions)",
            Some(&packet)
        ));
        assert!(is_dup_section("Failures", Some(&packet)));
        assert!(is_dup_section("Handoff Summary", Some(&packet)));
        // Not duplicated when the handoff lacks the content.
        let thin = json!({"objective": "", "plan_state": [], "changed_files": [], "bugs_found": [], "context_summary": "", "milestones": []});
        assert!(!is_dup_section("Plan State", Some(&thin)));
        assert!(!is_dup_section("Changed Files", Some(&thin)));
        assert!(!is_dup_section("Objectives", Some(&thin)));
        assert!(!is_dup_section(
            "Milestones [1/2] (observed task completions)",
            Some(&thin)
        ));
        assert!(!is_dup_section("Failures", Some(&thin)));
        assert!(!is_dup_section("Handoff Summary", Some(&thin)));
        // No handoff at all → nothing is a duplicate.
        assert!(!is_dup_section("Plan State", None));
        // Unrelated titles never dedupe.
        assert!(!is_dup_section("Soul", Some(&packet)));
        assert!(!is_dup_section("Project State", Some(&packet)));
    }
}
