//! `stateroot resume` — compact markdown digest for agent contexts.
//!
//! Output goes to stdout and is designed to be piped straight into a harness
//! prompt: current handoff highlights, hot-apex memory files and a
//! server-built context pack. Diagnostics go to stderr.

use serde_json::{json, Value};
use stateroot_core::digest_delivery::{self, DeliveryChannel, DeliveryIntent};
use stateroot_core::local_store;
use std::path::Path;

use super::{note, truncate, Ctx};

/// Delivery-deduplication key when `--harness` is absent. This is local marker
/// bookkeeping only and must never be recorded as an observed harness actor.
const UNATTRIBUTED_CALLER: &str = "unattributed";

/// Footer appended to resume output AND the hook digest — identical wording
/// in both (plan P4.2).
pub const NO_REFETCH_FOOTER: &str = "This content IS the handoff — do NOT re-fetch it via tools";

/// The digest route line for one discovered skill.
fn skill_route(skill: &stateroot_core::skill_federation::DiscoveredSkill) -> String {
    match skill.lifecycle.as_str() {
        "reference_only" => format!("delegate to {}", skill.native_harness),
        "external_only" => format!("external-only via {}", skill.native_harness),
        _ => format!("portable from {}", skill.harness),
    }
}

/// Dedupe the federated skill list for the digest: the same package
/// discovered from several scopes lists once (key: slug + route +
/// description), discovery order preserved.
fn dedup_skills(
    skills: &[stateroot_core::skill_federation::DiscoveredSkill],
) -> Vec<&stateroot_core::skill_federation::DiscoveredSkill> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for skill in skills {
        let key = (
            skill.slug.clone(),
            skill_route(skill),
            skill.description.clone(),
        );
        if seen.insert(key) {
            out.push(skill);
        }
    }
    out
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
    render_handoff_digest_full(packet, deterministic, &[], None, None)
}

/// The central-plan digest section (authoritative tier): pointer + directive,
/// never the plan body. Shared by the handoff digest and the no-handoff arm —
/// a planner/executor split must surface even before any handoff exists.
pub(crate) fn central_plan_section(project_dir: Option<&Path>) -> Option<String> {
    let (plan, _path) = project_dir.and_then(stateroot_core::plans::current)?;
    let mut section = String::from("## Active Plan\n\n");
    section.push_str(&format!(
        "**{}** ({}) — planned by {}",
        plan.title, plan.status, plan.created_by_harness
    ));
    if let Some(root) = &plan.root_ref {
        let short: String = root.chars().take(12).collect();
        section.push_str(&format!(" · root `{short}`"));
    }
    section.push('\n');
    match plan.status() {
        stateroot_core::plans::PlanStatus::Approved | stateroot_core::plans::PlanStatus::Active => {
            section.push_str(&format!(
                "\nAn {} plan exists at `.stateroot/plans/{}.md`. Execute it as written; do not re-plan or re-explore.\n\n",
                plan.status, plan.id
            ));
        }
        _ => {
            section.push_str(&format!(
                "\nA draft plan is being authored at `.stateroot/plans/{}.md` — refine the plan file; do not implement yet.\n\n",
                plan.id
            ));
        }
    }
    Some(section)
}

/// "## Shared Capabilities" — the pooled reference-only capabilities another
/// harness owns (imagegen → codex, automate → cursor, …). The pool already
/// exists on disk; without this section an agent asked "can you do X"
/// answers from its own tool list and refuses (the claude imagegen trial:
/// the pool held imagegen, the agent never looked). Bounded: 8 entries plus
/// a "+N more" tail; empty pool → no section (empty stays empty).
pub(crate) fn shared_capabilities_section(project_dir: &Path) -> Option<String> {
    let pooled = stateroot_core::skill_federation::discover_all(project_dir, None).ok()?;
    let mut refs: Vec<(String, String)> = pooled
        .iter()
        .filter(|s| s.lifecycle == "reference_only")
        .map(|s| (s.slug.clone(), s.native_harness.clone()))
        .collect();
    refs.sort();
    refs.dedup();
    render_shared_capabilities(&refs)
}

/// Pure renderer (unit-tested): reference-only (slug, harness) pairs → the
/// bounded section, or None when the pool is empty.
fn render_shared_capabilities(refs: &[(String, String)]) -> Option<String> {
    if refs.is_empty() {
        return None;
    }
    const MAX: usize = 8;
    let mut section = String::from(
        "## Shared Capabilities (pooled — delegate, never refuse)\n\n\
         Asked for a capability you lack natively? Another harness owns it — name the path and offer to delegate instead of answering \"I can't\".\n",
    );
    for (slug, harness) in refs.iter().take(MAX) {
        section.push_str(&format!(
            "- {slug} → {harness}: `stateroot delegate --to {harness} --skill {slug} --task \"…\"`\n"
        ));
    }
    if refs.len() > MAX {
        section.push_str(&format!(
            "- … +{} more (`stateroot skill list`)\n",
            refs.len() - MAX
        ));
    }
    section.push('\n');
    Some(section)
}

/// "## Latest Activity" — the newest observed activity anywhere (checkpoint
/// or root), with harness + timestamp. A long-running session never writes a
/// formal handoff; without this line the next harness anchors on the older
/// formal writer and the live session is invisible (the claude-code incident:
/// codex named as last actor while kimi was still working). When activity
/// postdates the handoff boundary, the section says so explicitly.
pub(crate) fn latest_activity_section(project_dir: &Path) -> Option<String> {
    let activity = latest_activity(project_dir)?;
    let mut section = format!(
        "## Latest Activity\n\n- {} · {} · {}\n",
        activity.harness, activity.kind, activity.at
    );
    let handoff = stateroot_core::local_store::read_handoff_local(project_dir)
        .ok()
        .flatten();
    if let Some(packet) = handoff {
        let boundary = packet
            .get("written_at")
            .and_then(|v| v.as_str())
            .filter(|t| !t.is_empty())
            .or_else(|| packet.get("created_at").and_then(|v| v.as_str()))
            .unwrap_or("");
        if !boundary.is_empty() && ts_newer(&activity.at, boundary) {
            let seq = packet.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
            let author = packet
                .get("created_by_harness")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            section.push_str(&format!(
                "- activity continues after formal handoff #{seq} by {author} — the formal handoff is stale; Recent Checkpoints and observed evidence carry the work since.\n"
            ));
        }
    }
    section.push('\n');
    Some(section)
}

struct Activity {
    harness: String,
    kind: &'static str,
    at: String,
}

/// The newest observed activity: last checkpoint vs latest root, newest wins.
fn latest_activity(project_dir: &Path) -> Option<Activity> {
    let mut best: Option<Activity> = stateroot_core::local_store::recent_episodic(project_dir, 1)
        .into_iter()
        .next()
        .map(|rec| Activity {
            harness: rec
                .get("harness")
                .and_then(|v| v.as_str())
                .unwrap_or("cli")
                .to_string(),
            kind: "checkpoint",
            at: rec
                .get("ts")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .filter(|a| !a.at.is_empty());
    if let Ok(Some(hash)) = stateroot_core::roots::latest_root(project_dir) {
        if let Ok(manifest) = stateroot_core::roots::get_root(project_dir, &hash) {
            let candidate = Activity {
                harness: manifest.created_by_harness.clone(),
                kind: "root",
                at: manifest.created_at.clone(),
            };
            let replace = match (&best, candidate.at.is_empty()) {
                (None, false) => true,
                (Some(current), false) => ts_newer(&candidate.at, &current.at),
                _ => false,
            };
            if replace {
                best = Some(candidate);
            }
        }
    }
    best
}

/// Strict RFC3339 comparison; unparseable sides stay honest (no claim).
fn ts_newer(a: &str, b: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => a > b,
        _ => false,
    }
}

/// "## Recent Checkpoints" — the freshest structured lineage: the last five
/// episodic checkpoint notes, oldest-of-kept first. Cheap strings, never
/// invented; absent when the log is empty.
pub(crate) fn recent_checkpoints_section(project_dir: &Path) -> Option<String> {
    let records = stateroot_core::local_store::recent_episodic(project_dir, 5);
    if records.is_empty() {
        return None;
    }
    let mut out = String::from("## Recent Checkpoints\n\n");
    for record in &records {
        let ts = record.get("ts").and_then(|v| v.as_str()).unwrap_or("");
        let short_ts: String = ts.chars().take(16).collect();
        let note = record.get("note").and_then(|v| v.as_str()).unwrap_or("");
        let capped: String = note.chars().take(200).collect();
        let ellipsis = if note.chars().count() > 200 {
            "…"
        } else {
            ""
        };
        out.push_str(&format!("- [{short_ts}] {capped}{ellipsis}\n"));
    }
    out.push('\n');
    Some(out)
}

/// "## Recent Delegations" — the last few delegations with live status
/// (running|completed|failed|lost), so a parent harness learns
/// asynchronously that its labor finished — the same shape as kimi's
/// background subagent notifications. Absent when the store is empty.
pub(crate) fn recent_delegations_section(project_dir: &Path) -> Option<String> {
    let records = super::delegate::recent_delegations(project_dir, 5);
    if records.is_empty() {
        return None;
    }
    let mut out = String::from("## Recent Delegations\n\n");
    for (short_ts, status, task) in &records {
        out.push_str(&format!("- [{short_ts}] {status} · {task}\n"));
    }
    out.push('\n');
    Some(out)
}

/// Full digest: deterministic switch + durable learnings + active goal (both
/// from synced local files), rendered after Plan State.
pub fn render_handoff_digest_full(
    packet: &Value,
    deterministic: bool,
    durable: &[super::learnings_reader::Learning],
    active_goal: Option<&Value>,
    project_dir: Option<&Path>,
) -> String {
    let mut out = String::new();
    let get_str = |key: &str| packet.get(key).and_then(|v| v.as_str()).unwrap_or("");

    let objective = get_str("objective");
    if !objective.is_empty() {
        out.push_str(&format!("## Objective\n\n{objective}\n\n"));
    }
    let lineage = project_dir
        .map(stateroot_core::roots::compose_digest_section)
        .unwrap_or_default();
    if !lineage.is_empty() {
        out.push_str(&lineage);
    } else if let Some(root) = packet
        .get("latest_root")
        .and_then(|v| v.as_str())
        .filter(|value| !value.is_empty())
    {
        let short: String = root.chars().take(12).collect();
        out.push_str(&format!("Continuing from root `{short}`.\n\n"));
    }
    if let (Some(project_dir), Ok(home)) =
        (project_dir, stateroot_core::harness_install::home_dir())
    {
        if let Some(highlight) = stateroot_core::learnings::highlight_for_digest(project_dir, &home)
        {
            out.push_str(&format!("{highlight}\n\n"));
        }
    }
    let phase = get_str("current_phase");
    if !phase.is_empty() {
        out.push_str(&format!("## Current Phase\n\n{phase}\n\n"));
    }
    if let Some(section) = project_dir.and_then(latest_activity_section) {
        out.push_str(&section);
    }
    // The authoritative plan tier: the central plan store as pointer +
    // directive (NEVER the body — the executor reads one file; the token
    // razor stays). The packet's transcript-derived Plan State below is the
    // fallback tier and is suppressed whenever a central plan exists (the
    // dedup rule: the store section wins).
    let central_plan = central_plan_section(project_dir);
    if let Some(section) = &central_plan {
        out.push_str(section);
    }
    // The residual-work view: latest plan snapshot with status markers.
    if central_plan.is_none() {
        if let Some(items) = packet.get("plan_state").and_then(|v| v.as_array()) {
            if !items.is_empty() {
                out.push_str("## Plan State\n\n");
                for item in items {
                    let step = item.get("step").and_then(|v| v.as_str()).unwrap_or("");
                    let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("");
                    out.push_str(&format!("- [{status}] {step}\n"));
                }
                out.push('\n');
            }
        }
    }
    // Durable preferences (all active learnings) — after Plan State.
    if !durable.is_empty() {
        let mut section = String::new();
        for learning in durable {
            section.push_str(&format!(
                "- {} ({:.2})\n",
                learning.statement, learning.confidence
            ));
        }
        out.push_str("## Durable Preferences\n\n");
        out.push_str(&section);
        out.push('\n');
    }
    // The freshest structured lineage: recent episodic checkpoint notes.
    if let Some(section) = project_dir.and_then(recent_checkpoints_section) {
        out.push_str(&section);
    }
    // Delegation outcomes surface asynchronously here (async-only delegate).
    if let Some(section) = project_dir.and_then(recent_delegations_section) {
        out.push_str(&section);
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
            out.push_str(&format!("next: {next_step}\n"));
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
                            out.push_str(&format!("- {text}\n"));
                        }
                        out.push('\n');
                    }
                }
            }
        }
    }
    // Failures and bugs are separate authoring channels but one reader-facing
    // section. Preserve first-seen wording and avoid duplicate rendering.
    let mut failures = Vec::new();
    for key in ["failures", "bugs_found"] {
        if let Some(items) = packet.get(key).and_then(|v| v.as_array()) {
            for item in items {
                let text = match item {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                if !text.trim().is_empty() && !failures.contains(&text) {
                    failures.push(text);
                }
            }
        }
    }
    if !failures.is_empty() {
        out.push_str("## Failed Approaches / Bugs\n\n");
        for text in failures {
            out.push_str(&format!("- {text}\n"));
        }
        out.push('\n');
    }
    // Actionables first.
    for (key, title) in [
        ("next_actions", "Next Actions"),
        ("open_questions", "Open Questions"),
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
                    out.push_str(&format!("- {text}\n"));
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
                history.push_str(&format!("- {text}\n"));
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
                history.push_str(&format!("**{role}:** {text}\n\n"));
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
    if (lowered == "failures" || lowered.starts_with("failures ["))
        && packet
            .get("failures")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
    {
        return true;
    }
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
    Some(text.to_string())
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
/// observed context pack from the repo, durable preferences/goals from local
/// docs, skills from federation discovery.
pub async fn run(
    ctx: &Ctx,
    harness: Option<&str>,
    no_accept: bool,
    force: bool,
    deterministic: bool,
) -> anyhow::Result<()> {
    let project = ctx.require_project()?;

    // Dual-mode compiler: try agentic merge before rendering (non-fatal).
    if !deterministic {
        let _ = super::compiler::try_agentic(ctx, false).await;
    }

    // An explicit resume harness is direct local evidence. Persist it before
    // duplicate-delivery suppression so even an early return refreshes the
    // active actor marker.
    let recorded_harness = harness
        .map(|id| super::active_harness::record(&ctx.cwd, id))
        .transpose()?;
    let caller = recorded_harness.as_deref().unwrap_or(UNATTRIBUTED_CALLER);
    let content_fp = digest_delivery::content_fingerprint(&ctx.cwd);
    if !force {
        let decision = digest_delivery::should_deliver(
            &ctx.cwd,
            caller,
            DeliveryIntent::Session,
            DeliveryChannel::Resume,
            &json!({}),
            &content_fp,
            false,
        );
        if !decision.deliver {
            let seq = digest_delivery::handoff_seq(&ctx.cwd);
            println!(
                "(StateRoot resume already delivered this session for handoff seq {seq} — \
skipping duplicate. Pass --force to reprint.)\n\n{NO_REFETCH_FOOTER}"
            );
            return Ok(());
        }
    }

    let (handoff, _handoff_source) = fetch_handoff(&ctx.cwd);

    let root = local_store::root(&ctx.cwd);
    let memory_md = read_hot_apex(&root, local_store::MEMORY_CORE_PATH);

    // --- digest (stdout only) ---
    let mut out = String::new();
    let name = if project.name.is_empty() {
        project.project_id.as_str()
    } else {
        project.name.as_str()
    };
    out.push_str(&format!("# StateRoot Resume — {name}\n\n"));

    // Update nudge (cache-only, never network here): agents act on what they
    // see, and the skill tells them what to do with this line.
    if let Some(notice) = super::update::update_notice(&ctx.config_dir) {
        out.push_str(&notice);
    }
    if let Ok(home) = stateroot_core::harness_install::home_dir() {
        if let Some(notice) = super::soul::soul_sync_notice(&home) {
            out.push_str(&notice);
        }
    }

    // Persona (global; project overlay overrides when present).
    if let Some(persona) = super::persona::resolve_in_project(&ctx.config_dir, Some(&ctx.cwd), None)
    {
        out.push_str(super::persona::IDENTITY_ACTIVATION);
        out.push_str("\n\n");
        out.push_str(&persona);
        out.push_str("\n\n---\n\n");
    }

    let user_md = stateroot_core::harness_install::home_dir()
        .ok()
        .and_then(|home| stateroot_core::user_profile::read(&home))
        .filter(|text| !text.trim().is_empty());
    if let Some(user) = user_md.as_ref() {
        out.push_str("### USER.md\n\n");
        out.push_str(user);
        out.push_str("\n\n---\n\n");
    }

    if let Ok(home) = stateroot_core::harness_install::home_dir() {
        let status = stateroot_core::learnings::bootstrap_status(&ctx.cwd, &home);
        out.push_str(&stateroot_core::learnings::compose_instruction(&status));
        out.push_str("\n---\n\n");
        let _ = stateroot_core::rules::ensure_product_intent(&home);
        out.push_str(&stateroot_core::rules::compose_section(&ctx.cwd, &home));
        out.push_str("\n---\n\n");
        stateroot_core::hot_apex::ensure_migrated(&ctx.cwd, &home);
        out.push_str(&stateroot_core::wiki::compose_digest_section(&ctx.cwd));
        out.push_str("\n---\n\n");
    }
    let pack_md = stateroot_core::context_pack::build(&ctx.cwd).render_markdown();
    if !pack_md.trim().is_empty() {
        out.push_str(&pack_md);
        out.push_str("---\n\n");
    }

    // Durable preferences: all active learnings (project + user + workspace + bound domain).
    let durable: Vec<super::learnings_reader::Learning> =
        stateroot_core::harness_install::home_dir()
            .ok()
            .map(|home| {
                stateroot_core::learnings::collect_active_for_digest(&ctx.cwd, &home)
                    .into_iter()
                    .map(|l| super::learnings_reader::Learning {
                        id: l.id,
                        statement: l.statement,
                        category: l.category,
                        confidence: l.confidence,
                        label: l.label,
                        sources: l.sources,
                        scope: l.scope,
                        status: l.status,
                    })
                    .collect()
            })
            .unwrap_or_default();
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
                Some(&ctx.cwd),
            ));
            if !out.ends_with('\n') {
                out.push('\n');
            }
            if let Ok(home) = stateroot_core::harness_install::home_dir() {
                if let Some(gap) =
                    stateroot_core::handoff_continuity::overlay_for_handoff(&home, &ctx.cwd, packet)
                {
                    out.push_str(
                        &stateroot_core::handoff_continuity::compose_since_handoff_overlay(
                            &ctx.cwd, packet, &gap,
                        ),
                    );
                    out.push('\n');
                }
            }
        }
        None => {
            out.push_str("(no handoff yet — write one with `stateroot handoff write`)\n");
            // A plan may exist before any handoff (plan/implement split):
            // the planner/executor directive must still surface.
            if let Some(section) = central_plan_section(Some(&ctx.cwd)) {
                out.push('\n');
                out.push_str(&section);
            }
            if let Some(section) = shared_capabilities_section(&ctx.cwd) {
                out.push('\n');
                out.push_str(&section);
            }
            if let Some(section) = latest_activity_section(&ctx.cwd) {
                out.push('\n');
                out.push_str(&section);
            }
            if let Some(section) = recent_checkpoints_section(&ctx.cwd) {
                out.push('\n');
                out.push_str(&section);
            }
            if let Some(section) = recent_delegations_section(&ctx.cwd) {
                out.push('\n');
                out.push_str(&section);
            }
        }
    }

    if memory_md.is_some() {
        out.push_str("\n## Memory (hot apex)\n");
        if let Ok(home) = stateroot_core::harness_install::home_dir() {
            if let Some(block) =
                stateroot_core::hot_apex::render_for_digest(&ctx.cwd, &home, "memory")
            {
                out.push_str(&format!("\n{block}\n"));
            } else if let Some(memory) = memory_md {
                out.push_str(&format!("\n### MEMORY.md\n\n{memory}\n"));
            }
        } else if let Some(memory) = memory_md {
            out.push_str(&format!("\n### MEMORY.md\n\n{memory}\n"));
        }
    }

    // Federated skill index: native origins + user-global and project
    // portable packages. Managed `.agents/skills` projections are skipped by
    // discovery to avoid loops. The same package discovered from several
    // scopes lists ONCE (deduped by slug + route + description); the header
    // count and the 40-line cap apply to the deduped list.
    let skills = stateroot_core::skill_federation::discover_all(&ctx.cwd, None).unwrap_or_default();
    let skills = dedup_skills(&skills);
    if !skills.is_empty() {
        out.push_str(&format!("\n## Federated Skills ({})\n\n", skills.len()));
        for skill in skills.iter().take(40) {
            let route = skill_route(skill);
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
    digest_delivery::mark_delivered(
        &ctx.cwd,
        caller,
        DeliveryIntent::Session,
        DeliveryChannel::Resume,
        "resume",
        &json!({}),
        &content_fp,
    );
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
    fn shared_capabilities_render_bounded_and_honest() {
        assert!(
            render_shared_capabilities(&[]).is_none(),
            "empty pool → no section (empty stays empty)"
        );
        let refs: Vec<(String, String)> = (0..10)
            .map(|i| (format!("cap-{i:02}"), "codex".to_string()))
            .collect();
        let section = render_shared_capabilities(&refs).expect("section");
        assert!(section.contains("delegate, never refuse"));
        assert!(section.contains("cap-00 → codex"));
        assert!(section.contains("stateroot delegate --to codex --skill cap-00"));
        assert!(!section.contains("cap-08 → codex"), "bounded at 8");
        assert!(section.contains("+2 more"), "tail names the remainder");
        let one = render_shared_capabilities(&[("imagegen".into(), "codex".into())]).expect("one");
        assert!(one.contains("imagegen → codex"));
        assert!(!one.contains("more (`stateroot skill list`)"));
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
    fn digest_omits_recommended_next_when_null() {
        let mut packet = rich_packet();
        packet["recommended_next_harness"] = Value::Null;
        let out = render_handoff_digest(&packet);
        assert!(!out.contains("Recommended next harness"), "out: {out}");
    }

    #[test]
    fn latest_activity_names_the_freshest_actor_and_flags_stale_handoffs() {
        let dir = tempfile::tempdir().expect("dir");
        stateroot_core::local_store::init_skeleton(dir.path(), "p", "proj", "local")
            .expect("skeleton");
        assert!(
            latest_activity_section(dir.path()).is_none(),
            "empty project"
        );

        stateroot_core::local_store::append_episodic(
            dir.path(),
            &json!({"ts": "2026-08-25T09:10:32Z", "harness": "kimi", "note": "work", "files": []}),
        )
        .expect("episodic");
        let section = latest_activity_section(dir.path()).expect("section");
        assert!(
            section.contains("kimi · checkpoint · 2026-08-25T09:10:32Z"),
            "{section}"
        );
        assert!(
            !section.contains("stale"),
            "no handoff yet, no stale claim: {section}"
        );

        // Older formal handoff from another harness → the stale note fires.
        stateroot_core::local_store::write_handoff_local(
            dir.path(),
            &json!({
                "schema_version": stateroot_core::local_store::SCHEMA_HANDOFF_V1,
                "project_id": "p", "seq": 2, "created_by_harness": "codex",
                "created_at": "2026-08-24T10:00:00Z", "objective": "o", "task": "t",
                "context_summary": "", "next_actions": []
            }),
        )
        .expect("handoff");
        let section = latest_activity_section(dir.path()).expect("section");
        assert!(
            section.contains("after formal handoff #2 by codex"),
            "stale note: {section}"
        );
        // And it lands in the full digest.
        let out = render_handoff_digest_full(
            &json!({"objective": "o"}),
            true,
            &[],
            None,
            Some(dir.path()),
        );
        assert!(out.contains("## Latest Activity"), "out: {out}");
    }

    #[test]
    fn recent_checkpoints_section_renders_and_stays_absent_when_empty() {
        let dir = tempfile::tempdir().expect("dir");
        assert!(
            recent_checkpoints_section(dir.path()).is_none(),
            "empty log"
        );
        stateroot_core::local_store::append_episodic(
            dir.path(),
            &json!({"ts": "2026-08-25T07:31:01Z", "note": "wired the bridge"}),
        )
        .expect("append");
        let section = recent_checkpoints_section(dir.path()).expect("section");
        assert!(section.contains("## Recent Checkpoints"), "{section}");
        assert!(section.contains("wired the bridge"), "{section}");
        assert!(section.contains("2026-08-25T07:31"), "{section}");
        // And it lands in the full digest.
        let packet = json!({"objective": "obj"});
        let out = render_handoff_digest_full(&packet, true, &[], None, Some(dir.path()));
        assert!(out.contains("## Recent Checkpoints"), "out: {out}");
    }

    #[test]
    fn active_plan_section_supersedes_transcript_plan_state() {
        let dir = tempfile::tempdir().expect("dir");
        let packet = json!({
            "objective": "obj",
            "plan_state": [{"step": "residual step", "status": "pending"}],
        });
        // No central plan → the transcript Plan State fallback renders.
        let out = render_handoff_digest_full(&packet, true, &[], None, Some(dir.path()));
        assert!(out.contains("## Plan State"), "out: {out}");
        assert!(!out.contains("## Active Plan"), "out: {out}");

        // A central plan wins: pointer + directive, Plan State suppressed,
        // and the plan body never enters the digest.
        let meta = stateroot_core::plans::record(
            dir.path(),
            "Ship It",
            "claude",
            None,
            "# Ship It\n\nBODY-SECRET-NEVER-IN-DIGEST\n",
        )
        .expect("record");
        stateroot_core::plans::transition(
            dir.path(),
            &meta.id,
            stateroot_core::plans::PlanStatus::Approved,
        )
        .expect("approve");
        let out = render_handoff_digest_full(&packet, true, &[], None, Some(dir.path()));
        assert!(out.contains("## Active Plan"), "out: {out}");
        assert!(
            out.contains("**Ship It** (approved) — planned by claude"),
            "out: {out}"
        );
        assert!(
            out.contains("Execute it as written; do not re-plan or re-explore"),
            "out: {out}"
        );
        assert!(
            out.contains(&format!(".stateroot/plans/{}.md", meta.id)),
            "out: {out}"
        );
        assert!(!out.contains("## Plan State"), "out: {out}");
        assert!(!out.contains("BODY-SECRET-NEVER-IN-DIGEST"), "out: {out}");

        // Draft-only → the planner directive instead.
        let draft_dir = tempfile::tempdir().expect("dir2");
        let draft = stateroot_core::plans::record(
            draft_dir.path(),
            "Rough Draft",
            "codex",
            None,
            "# Rough Draft\n\nbody\n",
        )
        .expect("record draft");
        let out = render_handoff_digest_full(&packet, true, &[], None, Some(draft_dir.path()));
        assert!(
            out.contains("refine the plan file; do not implement yet"),
            "out: {out}"
        );
        assert!(
            out.contains(&format!(".stateroot/plans/{}.md", draft.id)),
            "out: {out}"
        );
        assert!(!out.contains("Execute it as written"), "out: {out}");
        assert!(!out.contains("## Plan State"), "out: {out}");
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

    #[test]
    fn hot_apex_preserves_full_memory() {
        let home = tempfile::tempdir().expect("home");
        let long_user = format!("Fellow Daoist Han — {}", "u".repeat(2000));
        std::fs::create_dir_all(home.path().join(".stateroot/user")).expect("user dir");
        std::fs::write(home.path().join(".stateroot/user/USER.md"), &long_user).expect("user");

        let user_md =
            stateroot_core::user_profile::read(home.path()).filter(|text| !text.trim().is_empty());
        let user = user_md.expect("user profile");
        assert_eq!(user.len(), long_user.len());
        assert!(user.contains(&"u".repeat(1800)));

        let project = tempfile::tempdir().expect("project");
        let root = local_store::root(project.path());
        std::fs::create_dir_all(root.join("memories")).expect("mem dir");
        let long_memory = "m".repeat(2500);
        std::fs::write(root.join("memories/MEMORY.md"), &long_memory).expect("memory");
        let memory_md = read_hot_apex(&root, local_store::MEMORY_CORE_PATH).expect("memory");
        assert_eq!(memory_md, long_memory);
    }

    #[test]
    fn persona_resolve_stays_full_for_resume() {
        let config = tempfile::tempdir().expect("config");
        let persona_lines: String = (0..25)
            .map(|i| format!("Resume persona voice line {i}: remain in character"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(config.path().join("persona.md"), persona_lines).expect("persona");
        let persona = crate::commands::persona::resolve(config.path()).expect("persona");
        assert!(persona.contains("Resume persona voice line 24: remain in character"));
    }

    #[test]
    fn federated_skills_dedupe_by_slug_route_and_description() {
        use stateroot_core::skill_federation::DiscoveredSkill;
        fn skill(
            slug: &str,
            harness: &str,
            lifecycle: &str,
            native: &str,
            desc: &str,
        ) -> DiscoveredSkill {
            DiscoveredSkill {
                identity_key: format!("k-{slug}-{harness}"),
                slug: slug.into(),
                name: slug.into(),
                description: desc.into(),
                harness: harness.into(),
                source_path: String::new(),
                scope: "global".into(),
                ownership_class: "user_installed".into(),
                lifecycle: lifecycle.into(),
                visibility: String::new(),
                package_digest: String::new(),
                files: Default::default(),
                source_kind: "user_installed".into(),
                license: None,
                native_harness: native.into(),
                native_invocation: String::new(),
                compatibility: Value::Null,
                hash_exclusions: Vec::new(),
            }
        }
        let skills = vec![
            skill("demo", "claude", "active", "claude", "Does demo"),
            // Literal duplicate discovered from a second scope — deduped.
            skill("demo", "claude", "active", "claude", "Does demo"),
            // Same slug but a different route → a distinct entry.
            skill("demo", "codex", "active", "codex", "Does demo"),
            skill("other", "pi", "reference_only", "pi", "Ref"),
        ];
        let deduped = dedup_skills(&skills);
        assert_eq!(deduped.len(), 3, "deduped: {:?}", deduped.len());
        assert_eq!(deduped[0].harness, "claude");
        assert_eq!(deduped[1].harness, "codex");
        assert_eq!(deduped[2].slug, "other");
        // No duplicates → identity.
        let unique = vec![skill("a", "claude", "active", "claude", "A")];
        assert_eq!(dedup_skills(&unique).len(), 1);
    }
}
