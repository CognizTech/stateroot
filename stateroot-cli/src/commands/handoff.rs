//! `stateroot handoff write|list|show`.

use std::io::Read as _;
use std::io::Write as _;
use std::path::Path;

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Digest as _;
use stateroot_core::handoff_continuity::{self, FINALIZE_WARNING};
use stateroot_core::local_store::now_rfc3339;
use stateroot_core::local_store::{self, SCHEMA_HANDOFF_V1};
use stateroot_core::transcripts::TranscriptSession;

use super::resume::{fetch_handoff, render_handoff_digest_full};
use super::{note, truncate, Ctx};

/// Origin of a handoff write request.
///
/// `Explicit` — user/MCP/`stateroot handoff write`: may replace `handoffs/current.json`.
/// `Automatic` — hook shutdown / `run --handoff-on-exit` / TUI quit: checkpoint only;
/// never clobbers a deliberate structured handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffOrigin {
    Explicit,
    Automatic,
}

/// Apply soft quality warnings without truncating agent-facing content.
/// Returns the packet unchanged (continuity beats form-filling).
fn bound_packet(packet: Value) -> Value {
    packet
}

/// Warn on thin fields; never refuse the write (product-intent §11 / §22).
fn validate_packet(packet: &Value, handing_to_another: bool) -> anyhow::Result<()> {
    let required = |key: &str| {
        packet
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
    };
    for key in ["objective", "task", "context_summary"] {
        if required(key).is_empty() {
            note!("warning: handoff {key} is empty — writing anyway");
        }
    }
    let summary_len = required("context_summary").chars().count();
    if summary_len > stateroot_core::handoff_bounds::CONTEXT_SUMMARY_MAX {
        note!(
            "warning: handoff context_summary is {summary_len} chars — over the {} digest budget; writing anyway",
            stateroot_core::handoff_bounds::CONTEXT_SUMMARY_MAX
        );
    }
    if !required("task").is_empty()
        && !required("context_summary").is_empty()
        && required("task").eq_ignore_ascii_case(required("context_summary"))
    {
        note!("warning: task and context_summary are identical — writing anyway");
    }
    if handing_to_another
        && packet
            .get("next_actions")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
    {
        note!("warning: next_actions empty when handing off to another harness — writing anyway");
    }
    Ok(())
}

/// CLI flag overrides for `handoff write` (authoritative over `--input`).
#[derive(Debug, Default, Clone)]
pub struct HandoffWriteFlags<'a> {
    pub plan: Option<&'a str>,
    pub no_plan: bool,
    pub objective: Option<&'a str>,
    pub task: Option<&'a str>,
    pub context_summary: Option<&'a str>,
    pub next: &'a [String],
    pub decisions: &'a [String],
    pub failures: &'a [String],
    pub failed_approaches: &'a [String],
    pub context_only: &'a [String],
    pub worktree: Option<&'a str>,
}

const HANDOFF_INPUT_KEYS: &[&str] = &[
    "artifact_refs",
    "task",
    "objective",
    "current_phase",
    "implementation_status",
    "context_summary",
    "decisions",
    "changed_files",
    "tests_run",
    "failures",
    "bugs_found",
    "blockers",
    "open_questions",
    "next_actions",
    "warnings",
    "relevant_memories",
    "relevant_skills",
    "artifacts",
    "traces",
    "failed_approaches",
    "context_only",
];

const HANDOFF_INPUT_ALIASES: &[(&str, &str)] =
    &[("immediate_task", "task"), ("summary", "context_summary")];

const HANDOFF_ENVELOPE_KEYS: &[&str] = &[
    "schema_version",
    "project_id",
    "seq",
    "last_harness",
    "recommended_next_harness",
    "created_at",
    "written_at",
    "created_by_harness",
    "latest_root",
    "plan_state",
    "plan_ref",
    "progress_summaries",
    "milestones",
    "conversation_tail",
    "accepted_by",
];

/// One structured failed-approach record: what was tried, how it ended, why.
/// The outcome vocabulary is fixed so the receiver can trust the label.
#[derive(Debug, Default, Deserialize)]
struct FailedApproachInput {
    #[serde(default)]
    approach: String,
    #[serde(default)]
    outcome: String,
    #[serde(default)]
    reason: String,
}

/// Author-controlled handoff content. Envelope, provenance, transcript-rich
/// fields, and timestamps intentionally do not appear here: parsing rejects
/// them instead of allowing the input file to impersonate the CLI.
#[derive(Debug, Default, Deserialize)]
struct HandoffInput {
    artifact_refs: Option<Vec<stateroot_core::fidelity::ArtifactRef>>,
    plan: Option<String>,
    #[serde(default)]
    no_plan: bool,
    task: Option<String>,
    objective: Option<String>,
    current_phase: Option<String>,
    implementation_status: Option<String>,
    context_summary: Option<String>,
    decisions: Option<Vec<String>>,
    changed_files: Option<Vec<String>>,
    tests_run: Option<Vec<String>>,
    failures: Option<Vec<String>>,
    bugs_found: Option<Vec<String>>,
    blockers: Option<Vec<String>>,
    open_questions: Option<Vec<String>>,
    next_actions: Option<Vec<String>>,
    warnings: Option<Vec<String>>,
    relevant_memories: Option<Vec<String>>,
    relevant_skills: Option<Vec<String>>,
    artifacts: Option<Vec<String>>,
    traces: Option<Vec<String>>,
    /// Structured failed approaches (A1); additive — old packets without the
    /// key parse unchanged.
    failed_approaches: Option<Vec<FailedApproachInput>>,
    /// Authority-labeled background facts (A4): context, never instructions.
    context_only: Option<Vec<String>>,
    /// WS5: bind the receiving agent to a directory (fork worktree).
    worktree: Option<String>,
}

/// The fixed outcome vocabulary for structured failed approaches; anything
/// else is rejected so a typo never reads as a real outcome.
fn canonical_outcome(raw: &str) -> Option<String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "success" => Some("success".into()),
        "partial" => Some("partial".into()),
        "failed" => Some("failed".into()),
        _ => None,
    }
}

/// Parse one `--failed-approach "approach → outcome: reason"` flag.
fn parse_failed_approach_flag(raw: &str) -> anyhow::Result<FailedApproachInput> {
    let shape = || {
        anyhow::anyhow!(
            "invalid --failed-approach '{raw}': expected \"<approach> → <outcome>: <reason>\" with outcome success|partial|failed"
        )
    };
    let (approach, rest) = raw.split_once('→').ok_or_else(shape)?;
    let (outcome, reason) = rest.split_once(':').ok_or_else(shape)?;
    let outcome = canonical_outcome(outcome).ok_or_else(|| {
        anyhow::anyhow!(
            "invalid --failed-approach outcome '{}': expected success|partial|failed",
            outcome.trim()
        )
    })?;
    Ok(FailedApproachInput {
        approach: approach.trim().to_string(),
        outcome,
        reason: reason.trim().to_string(),
    })
}

fn coerce_decision_item(item: &Value) -> Option<String> {
    match item {
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        }
        Value::Object(map) => {
            let decision = map
                .get("decision")
                .or_else(|| map.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let rationale = map
                .get("rationale")
                .or_else(|| map.get("why"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if decision.is_empty() && rationale.is_empty() {
                return None;
            }
            if rationale.is_empty() {
                Some(decision.to_string())
            } else if decision.is_empty() {
                Some(rationale.to_string())
            } else {
                Some(format!("{decision} — {rationale}"))
            }
        }
        _ => None,
    }
}

fn normalize_handoff_input_object(obj: &mut serde_json::Map<String, Value>) -> anyhow::Result<()> {
    for (alias, canonical) in HANDOFF_INPUT_ALIASES {
        if let Some(value) = obj.remove(*alias) {
            obj.entry(canonical.to_string()).or_insert(value);
        }
    }

    if let Some(decisions) = obj.get_mut("decisions") {
        if let Some(items) = decisions.as_array() {
            let coerced: Vec<Value> = items
                .iter()
                .filter_map(coerce_decision_item)
                .map(Value::String)
                .collect();
            *decisions = Value::Array(coerced);
        }
    }

    let envelope: Vec<String> = obj
        .keys()
        .filter(|key| HANDOFF_ENVELOPE_KEYS.contains(&key.as_str()))
        .cloned()
        .collect();
    if !envelope.is_empty() {
        anyhow::bail!(
            "handoff input must not include envelope/provenance keys ({}) — the CLI owns those fields",
            envelope.join(", ")
        );
    }

    let unknown: Vec<String> = obj
        .keys()
        .filter(|key| !HANDOFF_INPUT_KEYS.contains(&key.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        anyhow::bail!(
            "unknown handoff input key(s): {}. Allowed content keys: {}",
            unknown.join(", "),
            HANDOFF_INPUT_KEYS.join(", ")
        );
    }
    Ok(())
}

fn parse_handoff_input_text(text: &str, path: &str) -> anyhow::Result<HandoffInput> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(HandoffInput::default());
    }
    let mut value: Value = serde_json::from_str(trimmed).with_context(|| {
        format!("invalid handoff JSON in '{path}': expected a JSON object of content fields")
    })?;
    let obj = value.as_object_mut().ok_or_else(|| {
        anyhow::anyhow!("invalid handoff JSON in '{path}': expected a JSON object")
    })?;
    normalize_handoff_input_object(obj)?;
    let mut input: HandoffInput = serde_json::from_value(Value::Object(std::mem::take(obj)))
        .with_context(|| {
            format!("invalid handoff input '{path}': could not parse normalized content fields")
        })?;
    if let Some(entries) = input.failed_approaches.as_mut() {
        for entry in entries.iter_mut() {
            entry.outcome = canonical_outcome(&entry.outcome).ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid handoff input '{path}': failed_approaches outcome '{}' is not success|partial|failed",
                    entry.outcome.trim()
                )
            })?;
        }
    }
    Ok(input)
}

fn read_input(path: Option<&str>) -> anyhow::Result<HandoffInput> {
    let Some(path) = path else {
        return Ok(HandoffInput::default());
    };
    let text = if path == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .context("could not read handoff JSON from stdin")?;
        text
    } else {
        std::fs::read_to_string(path).with_context(|| {
            format!(
                "could not read handoff input '{}'",
                Path::new(path).display()
            )
        })?
    };
    parse_handoff_input_text(&text, path)
}

fn apply_write_flags(
    mut input: HandoffInput,
    flags: &HandoffWriteFlags<'_>,
) -> anyhow::Result<HandoffInput> {
    if let Some(task) = flags.task.filter(|text| !text.trim().is_empty()) {
        input.task = Some(task.to_string());
    }
    if let Some(summary) = flags.context_summary.filter(|text| !text.trim().is_empty()) {
        input.context_summary = Some(summary.to_string());
    }
    if !flags.next.is_empty() {
        input.next_actions = Some(flags.next.to_vec());
    }
    if !flags.decisions.is_empty() {
        input.decisions = Some(flags.decisions.to_vec());
    }
    if !flags.failures.is_empty() {
        input.failures = Some(flags.failures.to_vec());
    }
    if !flags.failed_approaches.is_empty() {
        let mut parsed = Vec::with_capacity(flags.failed_approaches.len());
        for raw in flags.failed_approaches {
            parsed.push(parse_failed_approach_flag(raw)?);
        }
        input.failed_approaches = Some(parsed);
    }
    if !flags.context_only.is_empty() {
        input.context_only = Some(flags.context_only.to_vec());
    }
    if let Some(worktree) = flags.worktree.filter(|text| !text.trim().is_empty()) {
        input.worktree = Some(worktree.to_string());
    }
    input.plan = flags.plan.map(str::to_owned);
    input.no_plan = flags.no_plan;
    Ok(input)
}

fn nonempty(text: Option<String>) -> Option<String> {
    text.filter(|text| !text.trim().is_empty())
}

fn clean_list(items: Option<Vec<String>>) -> Vec<String> {
    let mut out = Vec::new();
    for item in items.unwrap_or_default() {
        if item.trim().is_empty() || out.iter().any(|existing| existing == &item) {
            continue;
        }
        out.push(item);
    }
    out
}

fn fill_list(target: &mut Vec<String>, observed: &[String]) {
    if target.is_empty() {
        for item in observed {
            if !item.trim().is_empty() && !target.iter().any(|existing| existing == item) {
                target.push(item.clone());
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacySection {
    CurrentState,
    Decisions,
    NextActions,
    Failures,
}

#[derive(Debug, Default)]
struct LegacyNote {
    context_summary: Option<String>,
    decisions: Vec<String>,
    next_actions: Vec<String>,
    failures: Vec<String>,
}

const LEGACY_LABELS: &[(LegacySection, &str)] = &[
    (LegacySection::CurrentState, "CURRENT STATE:"),
    (LegacySection::Decisions, "DECISIONS/WHY:"),
    (LegacySection::NextActions, "NEXT ACTIONS:"),
    (LegacySection::Failures, "FAILED APPROACHES/BUGS:"),
];

fn legacy_label_at(note: &str, start: usize) -> Option<(LegacySection, usize)> {
    let before_is_safe = note[..start]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace);
    if !before_is_safe {
        return None;
    }
    for &(section, label) in LEGACY_LABELS {
        let end = start.checked_add(label.len())?;
        let candidate = note.get(start..end)?;
        let after_is_safe = note[end..].chars().next().is_none_or(char::is_whitespace);
        if after_is_safe && candidate.eq_ignore_ascii_case(label) {
            return Some((section, end));
        }
    }
    None
}

/// Split only an entirely numbered section. Mixed prose is preserved as one
/// item so a best-effort migration cannot change its meaning.
fn conservative_numbered_items(text: &str) -> Vec<String> {
    let inline = text.trim().trim_end_matches(';');
    if inline.contains(';') && !inline.contains('\n') {
        let mut items = Vec::new();
        for segment in inline.split(';') {
            let segment = segment.trim();
            let Some(after_open) = segment.strip_prefix('(') else {
                items.clear();
                break;
            };
            let Some((number, item)) = after_open.split_once(')') else {
                items.clear();
                break;
            };
            if number.is_empty()
                || !number.chars().all(|character| character.is_ascii_digit())
                || item.trim().is_empty()
            {
                items.clear();
                break;
            }
            items.push(item.trim().to_string());
        }
        if items.len() > 1 {
            return items;
        }
    }

    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let mut items = Vec::new();
    for line in &lines {
        let trimmed = line.trim();
        let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return vec![text.trim().to_string()];
        }
        let rest = &trimmed[digits..];
        let Some(rest) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) else {
            return vec![text.trim().to_string()];
        };
        let item = rest.trim();
        if item.is_empty() {
            return vec![text.trim().to_string()];
        }
        items.push(item.to_string());
    }
    if lines.is_empty() {
        Vec::new()
    } else {
        items
    }
}

fn parse_legacy_note(note: &str) -> LegacyNote {
    let mut labels = Vec::new();
    for (start, _) in note.char_indices() {
        if let Some((section, end)) = legacy_label_at(note, start) {
            labels.push((section, start, end));
        }
    }

    // Conservative migration: only the exact four-label legacy packet,
    // anchored at the start and in canonical order, is segmented. Ordinary
    // prose containing similar words or a literal label remains one summary.
    if labels.len() != LEGACY_LABELS.len()
        || !note[..labels[0].1].trim().is_empty()
        || labels
            .iter()
            .zip(LEGACY_LABELS)
            .any(|((actual, _, _), (expected, _))| actual != expected)
    {
        return LegacyNote {
            context_summary: nonempty(Some(note.to_string())),
            ..Default::default()
        };
    }

    let mut parsed = LegacyNote::default();
    for (index, &(section, _, content_start)) in labels.iter().enumerate() {
        let content_end = labels
            .get(index + 1)
            .map_or(note.len(), |(_, start, _)| *start);
        let text = note[content_start..content_end].trim().trim();
        if text.is_empty() {
            continue;
        }
        match section {
            LegacySection::CurrentState => parsed.context_summary = Some(text.to_string()),
            LegacySection::Decisions => parsed.decisions.extend(conservative_numbered_items(text)),
            LegacySection::NextActions => {
                parsed
                    .next_actions
                    .extend(conservative_numbered_items(text));
            }
            LegacySection::Failures => parsed.failures.extend(conservative_numbered_items(text)),
        }
    }
    parsed
}

pub(crate) fn compact_tail(session: &TranscriptSession) -> Vec<Value> {
    // Full uncapped tail — product-intent forbids truncating agent-facing
    // continuity. Name kept for call sites / import path.
    session
        .conversation_tail
        .iter()
        .map(|entry| json!({"role": entry.role, "text": entry.text}))
        .collect()
}

fn transcript_digest(session: &TranscriptSession) -> String {
    let mut parts = vec![format!("Transcript outcome: {}", session.outcome.as_str())];
    parts.push(format!(
        "{} file(s) changed, {} failure(s), {} next action(s), {} tool event(s)",
        session.files_touched.len(),
        session.failed_approaches.len(),
        session.next_steps.len(),
        session.tool_events
    ));
    if let Some(milestone) = session
        .milestones
        .last()
        .filter(|text| !text.trim().is_empty())
    {
        parts.push(format!("Latest milestone: {milestone}"));
    }
    format!("{}.", parts.join("; "))
}

/// Read the project objective/phase from local state (cheap, offline-safe).
fn local_state_fields(cwd: &Path) -> anyhow::Result<(String, String)> {
    let path = local_store::root(cwd).join(local_store::STATE_PATH);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("could not read project state {}", path.display()))?;
    let state = serde_json::from_str::<Value>(&text)
        .with_context(|| format!("invalid project state JSON {}", path.display()))?;
    let objective = state
        .get("objective")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let phase = state
        .get("current_phase")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok((objective, phase))
}

struct PacketContext<'a> {
    project_dir: &'a Path,
    project_id: &'a str,
    seq: i64,
    source: &'a str,
    routing_dest: Option<&'a str>,
    note_text: Option<&'a str>,
    objective_override: Option<&'a str>,
    state_objective: String,
    state_phase: String,
    handing_to_another: bool,
}

fn assemble_packet(
    mut input: HandoffInput,
    session: Option<&TranscriptSession>,
    context: PacketContext<'_>,
) -> anyhow::Result<Value> {
    let legacy = context.note_text.map(parse_legacy_note).unwrap_or_default();
    let failures_explicit_empty = input.failures.as_ref().is_some_and(Vec::is_empty);

    let task = nonempty(input.task.take())
        .or_else(|| {
            session.and_then(|session| {
                session
                    .user_prompts
                    .iter()
                    .rev()
                    .find(|text| !text.trim().is_empty())
                    .cloned()
            })
        })
        .or_else(|| {
            session.and_then(|session| {
                session
                    .plan_state
                    .iter()
                    .find(|item| item.status != "completed" && !item.step.trim().is_empty())
                    .map(|item| item.step.clone())
            })
        })
        .unwrap_or_default();

    // The CLI flag is the final author override. Durable local state is more
    // authoritative than a transcript opener; the latter fills only a gap.
    let author_objective = match context.objective_override {
        Some(text) => nonempty(Some(text.to_string())),
        None => nonempty(input.objective.take()),
    };
    let objective = author_objective
        .or_else(|| nonempty(Some(context.state_objective)))
        .or_else(|| session.and_then(|session| nonempty(Some(session.objective.clone()))))
        .unwrap_or_default();
    let current_phase = nonempty(input.current_phase.take())
        .or_else(|| nonempty(Some(context.state_phase)))
        .unwrap_or_default();
    let implementation_status = nonempty(input.implementation_status.take())
        .or_else(|| session.map(transcript_digest))
        .unwrap_or_default();

    let mut decisions = clean_list(input.decisions.take());
    fill_list(&mut decisions, &legacy.decisions);
    let mut changed_files = clean_list(input.changed_files.take());
    let tests_run = clean_list(input.tests_run.take());
    let mut failures = clean_list(input.failures.take());
    let bugs_found = clean_list(input.bugs_found.take());
    let blockers = clean_list(input.blockers.take());
    let open_questions = clean_list(input.open_questions.take());
    let mut next_actions = clean_list(input.next_actions.take());
    let mut warnings = clean_list(input.warnings.take());
    let relevant_memories = clean_list(input.relevant_memories.take());
    let relevant_skills = clean_list(input.relevant_skills.take());
    let artifacts = clean_list(input.artifacts.take());
    let traces = clean_list(input.traces.take());
    let failed_approaches: Vec<Value> = input
        .failed_approaches
        .take()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| !entry.approach.trim().is_empty())
        .map(|entry| {
            json!({"approach": entry.approach, "outcome": entry.outcome, "reason": entry.reason})
        })
        .collect();
    let context_only = clean_list(input.context_only.take());

    if !failures_explicit_empty {
        fill_list(&mut failures, &legacy.failures);
    }
    fill_list(&mut next_actions, &legacy.next_actions);
    if let Some(session) = session {
        let observed_warning = format!(
            "transcript enrichment is observed from latest matching {} session {}",
            context.source, session.session_id
        );
        if !warnings.contains(&observed_warning) {
            warnings.push(observed_warning);
        }
        fill_list(&mut changed_files, &session.files_touched);
        if !failures_explicit_empty && failures.is_empty() && bugs_found.is_empty() {
            fill_list(&mut failures, &session.failed_approaches);
        }
        fill_list(&mut next_actions, &session.next_steps);
    } else {
        let warning = format!(
            "no matching verified {} transcript found for this project",
            context.source
        );
        if !warnings.contains(&warning) {
            warnings.push(warning);
        }
    }

    let context_summary = nonempty(input.context_summary.take())
        .or(legacy.context_summary)
        .or_else(|| {
            session.and_then(|session| {
                session
                    .progress_summaries
                    .iter()
                    .find(|text| !text.trim().is_empty())
                    .cloned()
            })
        })
        .or_else(|| session.map(transcript_digest))
        .unwrap_or_else(|| {
            format!(
                "No matching verified {} transcript was found; only author-provided and local project state are included.",
                context.source
            )
        });

    let now = now_rfc3339();
    let mut packet = json!({
        "schema_version": SCHEMA_HANDOFF_V1,
        "project_id": context.project_id,
        "seq": context.seq,
        "task": task,
        "current_phase": current_phase,
        "last_harness": context.source,
        "recommended_next_harness": if context.handing_to_another {
            json!(context.routing_dest.unwrap_or(context.source))
        } else {
            Value::Null
        },
        "objective": objective,
        "implementation_status": implementation_status,
        "decisions": decisions,
        "changed_files": changed_files,
        "tests_run": tests_run,
        "failures": failures,
        "bugs_found": bugs_found,
        "blockers": blockers,
        "open_questions": open_questions,
        "next_actions": next_actions,
        "warnings": warnings,
        "relevant_memories": relevant_memories,
        "relevant_skills": relevant_skills,
        "artifacts": artifacts,
        "traces": traces,
        "failed_approaches": failed_approaches,
        "context_only": context_only,
        "context_summary": context_summary,
        "created_at": now,
        "written_at": now,
        "created_by_harness": context.source,
    });

    if let Some(session) = session {
        if !session.plan_state.is_empty() {
            packet["plan_state"] = json!(session
                .plan_state
                .iter()
                .map(|item| json!({"step": item.step, "status": item.status}))
                .collect::<Vec<_>>());
        }
        let progress_summaries = clean_list(Some(session.progress_summaries.clone()));
        if !progress_summaries.is_empty() {
            packet["progress_summaries"] = json!(progress_summaries);
        }
        let milestones = clean_list(Some(session.milestones.clone()));
        if !milestones.is_empty() {
            packet["milestones"] = json!(milestones);
        }
        let tail = compact_tail(session);
        if !tail.is_empty() {
            packet["conversation_tail"] = Value::Array(tail);
        }
    }

    // WS5/6D: a handoff can bind the receiving agent to a fork — validated
    // as a REGISTERED worktree of this project, stored as the opaque fork
    // id (absolute paths never enter shared packets; the machine-local
    // registry resolves them).
    // A fork carries its own active-plan state.  Retain its directory only
    // locally while constructing the packet so a bound handoff points at the
    // plan actually claimed by that fork, rather than an unrelated approved
    // plan in the caller's checkout.
    let mut bound_plan = None;
    if let Some(worktree) = nonempty(input.worktree.take()) {
        let fork_ctx = stateroot_core::local_store::fork_context(Path::new(&worktree)).ok_or_else(|| {
            anyhow::anyhow!(
                "worktree {worktree} has no fork context — `handoff write --worktree` requires a fork materialized by this project"
            )
        })?;
        let registered =
            stateroot_core::roots::registered_worktree_path(context.project_dir, &fork_ctx.fork)
                .map(|p| p == Path::new(&worktree))
                .unwrap_or(false);
        if !registered {
            anyhow::bail!(
                "worktree {worktree} is not a registered fork worktree of this project (fork `{}` — registry mismatch)",
                fork_ctx.fork
            );
        }
        bound_plan = stateroot_core::plans::active(Path::new(&worktree));
        packet["fork_id"] = json!(fork_ctx.fork);
    }

    if let Ok(Some(root)) = stateroot_core::roots::latest_root(context.project_dir) {
        if !root.is_empty() {
            packet["latest_root"] = json!(root);
        }
    }

    // The central plan store is authoritative for ordinary handoffs.  A
    // fork-bound handoff instead names that fork's active plan, which is
    // independently activated in its own checkout.
    let selected_plan = if input.no_plan {
        None
    } else if let Some(id) = input.plan.as_deref() {
        Some(
            stateroot_core::plans::load(context.project_dir, id)
                .ok_or_else(|| anyhow::anyhow!("unknown explicit plan {id}"))?,
        )
    } else {
        bound_plan.or_else(|| stateroot_core::plans::active_or_approved(context.project_dir))
    };
    if let Some((plan, _)) = selected_plan {
        packet["plan_ref"] = json!({
            "id": plan.id,
            "title": plan.title,
            "status": plan.status,
        });
    }
    if input.no_plan {
        packet["plan_intent"] = json!("none");
    } else if input.plan.is_some() {
        packet["plan_intent"] = json!("explicit");
    }

    if let Some(references) = input.artifact_refs {
        packet["artifact_refs"] = json!(references);
    }
    packet = bound_packet(packet);
    validate_packet(&packet, context.handing_to_another)?;
    Ok(packet)
}

/// Durably append immutable history before replacing `current.json`.
///
/// On Unix and other non-Windows targets, current replacement uses a synced
/// same-directory temporary file and rename. Windows cannot portably rename
/// over an existing file with `std`, so it degrades to truncate/write/sync;
/// readers may observe a partial current file if that update is interrupted.
fn write_packet_durable(project_dir: &Path, packet: &Value) -> anyhow::Result<()> {
    let root = local_store::root(project_dir);
    let current = root.join(local_store::HANDOFF_CURRENT_PATH);
    let parent = current
        .parent()
        .context("handoff current path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let text = format!("{}\n", serde_json::to_string_pretty(packet)?);
    write_packet_history_durable(project_dir, packet, &text)?;

    stateroot_core::safe_io::atomic_replace(&current, text.as_bytes())?;
    Ok(())
}

/// Append immutable handoff history without replacing this checkout's local
/// current packet.  Bound handoffs use this in the caller then write their
/// deliverable current packet only inside the receiving fork.
fn write_packet_history_durable(
    project_dir: &Path,
    packet: &Value,
    text: &str,
) -> anyhow::Result<()> {
    let root = local_store::root(project_dir);
    let timestamp = packet
        .get("created_at")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .replace([':', '.'], "-");
    let harness = packet
        .get("created_by_harness")
        .and_then(Value::as_str)
        .unwrap_or("cli");
    let history_dir = root.join(local_store::HANDOFF_HISTORY_DIR);
    std::fs::create_dir_all(&history_dir)?;
    let history = history_dir.join(format!(
        "{timestamp}-{harness}-{}.json",
        uuid::Uuid::now_v7()
    ));
    let mut history_file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&history)?;
    let history_result = (|| -> anyhow::Result<()> {
        history_file.write_all(text.as_bytes())?;
        history_file.sync_all()?;
        Ok(())
    })();
    if history_result.is_err() {
        let _ = std::fs::remove_file(&history);
    }
    history_result
}

/// Explicit project-only POC: reuse the authoritative input validation, packet
/// assembly/bounds and history-first writer, without host transcript discovery,
/// routing, telemetry or continuity reconciliation.
pub(crate) fn write_project_only(project_dir: &Path, input_path: &Path) -> anyhow::Result<()> {
    let input = read_input(Some(
        input_path
            .to_str()
            .context("handoff input path must be UTF-8")?,
    ))?;
    if input.worktree.is_some() {
        anyhow::bail!("dot POC does not support worktree routing");
    }
    for (field, value) in [
        ("objective", &input.objective),
        ("task", &input.task),
        ("context_summary", &input.context_summary),
    ] {
        if value.as_deref().unwrap_or_default().trim().is_empty() {
            anyhow::bail!("handoff requires nonempty {field}");
        }
    }
    if input.next_actions.is_none() {
        anyhow::bail!("handoff requires next_actions as an array of strings");
    }
    let _lock = stateroot_core::safe_io::ResourceLock::acquire(
        local_store::root(project_dir).join("local/locks/handoff-write.lock"),
    )?;
    let manifest = local_store::read_manifest(project_dir)?.context("missing manifest")?;
    let project_id = manifest["project_id"]
        .as_str()
        .context("manifest requires project_id")?;
    // Include immutable history so restored/repaired current never reuses a seq.
    let current_seq = local_store::read_handoff_local(project_dir)?
        .into_iter()
        .chain(local_store::list_handoffs_local(project_dir)?)
        .filter_map(|packet| packet["seq"].as_i64())
        .max()
        .unwrap_or(0);
    let seq = current_seq
        .checked_add(1)
        .context("handoff sequence exhausted")?;
    let (state_objective, state_phase) = local_state_fields(project_dir)?;
    let packet = assemble_packet(
        input,
        None,
        PacketContext {
            project_dir,
            project_id,
            seq,
            source: "dot",
            routing_dest: None,
            note_text: None,
            objective_override: None,
            state_objective,
            state_phase,
            handing_to_another: false,
        },
    )?;
    write_packet_durable(project_dir, &packet)?;
    handoff_continuity::write_explicit_marker(
        project_dir,
        "dot",
        seq,
        packet["written_at"].as_str().unwrap_or_default(),
    )?;
    Ok(())
}

/// `stateroot handoff write [--from H] [--to H] [--task …] [--next …] [--input PATH]`.
///
/// Explicit origin replaces the current structured handoff. Automatic origin
/// (lifecycle hooks) records a checkpoint only and preserves any existing
/// structured handoff. `--to` is optional: omit it for continuity-only writes;
/// use it only for explicit cross-harness routing (orchestration/auto mode).
/// Prefer CLI flags near usage limits; `--input` is optional for large payloads.
pub async fn write(
    ctx: &Ctx,
    from: Option<&str>,
    to: Option<&str>,
    note_text: Option<&str>,
    input_path: Option<&str>,
    write_flags: &HandoffWriteFlags<'_>,
) -> anyhow::Result<()> {
    write_with_origin(
        ctx,
        from,
        to,
        note_text,
        input_path,
        write_flags,
        HandoffOrigin::Explicit,
    )
    .await
}

/// Same as [`write`] with an explicit/automatic origin.
pub async fn write_with_origin(
    ctx: &Ctx,
    from: Option<&str>,
    to: Option<&str>,
    note_text: Option<&str>,
    input_path: Option<&str>,
    write_flags: &HandoffWriteFlags<'_>,
    origin: HandoffOrigin,
) -> anyhow::Result<()> {
    if origin == HandoffOrigin::Automatic {
        return automatic_checkpoint_only(ctx, note_text).await;
    }

    let project = ctx.require_project()?;
    let _handoff_lock = stateroot_core::safe_io::ResourceLock::acquire(
        local_store::root(&ctx.cwd).join("local/locks/handoff-write.lock"),
    )?;
    let source = match from {
        Some(explicit) => super::active_harness::canonical_id(explicit)
            .map_err(|_| anyhow::anyhow!("unknown handoff source '{explicit}'; pass --from <harness> with a known harness id"))?,
        None => super::active_harness::read(&ctx.cwd)
            .map_err(|err| anyhow::anyhow!("cannot use active harness marker ({err}); pass --from <harness>"))?
            .ok_or_else(|| anyhow::anyhow!("handoff source is unknown; pass --from <harness>"))?,
    };
    let destination = match to {
        Some(explicit) => super::active_harness::canonical_id(explicit).map_err(|_| {
            anyhow::anyhow!(
                "unknown handoff destination '{explicit}'; pass --to <harness> with a known harness id or alias"
            )
        })?,
        None => source.clone(),
    };
    let input = apply_write_flags(read_input(input_path)?, write_flags)?;
    // Read directly so malformed state cannot silently reset the sequence.
    let current = local_store::read_handoff_local(&ctx.cwd)?;
    let current_seq = std::iter::once(current)
        .flatten()
        .chain(local_store::list_handoffs_local(&ctx.cwd)?)
        .filter_map(|packet| packet.get("seq").and_then(|value| value.as_i64()))
        .max()
        .unwrap_or(0);

    // `project/state.json` holds the objective recorded at init; nothing
    // refreshes it as work progresses, so an explicit restatement wins.
    let (state_objective, phase) = local_state_fields(&ctx.cwd)?;
    let home = super::install::home_dir()?;
    let session = handoff_continuity::latest_verified_session(&home, &ctx.cwd, &source);
    let handing_to_another = destination != source;
    let packet = assemble_packet(
        input,
        session.as_ref(),
        PacketContext {
            project_dir: &ctx.cwd,
            project_id: &project.project_id,
            seq: current_seq + 1,
            source: &source,
            routing_dest: if handing_to_another {
                Some(destination.as_str())
            } else {
                None
            },
            note_text,
            objective_override: write_flags.objective,
            state_objective,
            state_phase: phase,
            handing_to_another,
        },
    )?;

    let bound_worktree = packet
        .get("fork_id")
        .and_then(Value::as_str)
        .and_then(|fork| stateroot_core::roots::registered_worktree_path(&ctx.cwd, fork));
    let delivery_dir = if let Some(worktree) = bound_worktree {
        // A fork-bound handoff is delivery to that fork, not a replacement of
        // trunk's per-session current packet.  Keep an immutable sender-side
        // audit record and give the receiver its own current packet.
        let text = format!("{}\n", serde_json::to_string_pretty(&packet)?);
        write_packet_history_durable(&ctx.cwd, &packet, &text)?;
        write_packet_durable(&worktree, &packet)?;
        worktree
    } else {
        write_packet_durable(&ctx.cwd, &packet)?;
        ctx.cwd.clone()
    };
    let seq = packet.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
    let written_at = packet
        .get("written_at")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    handoff_continuity::write_explicit_marker(&delivery_dir, &source, seq, written_at)?;
    println!("handoff #{seq} written");
    // Compact digest footer (composed locally — no extra server calls).
    if let Some(footer) = super::resume::digest_footer(&delivery_dir) {
        println!("{footer}");
    }
    if ctx.config.continuity.enabled {
        if let Err(err) = stateroot_core::continuity::reconcile(
            &delivery_dir,
            &ctx.config_dir,
            &ctx.config.continuity,
        ) {
            note!("continuity reconcile: {err}");
        }
    }
    crate::telemetry::activity(&ctx.config_dir, &delivery_dir, Some(&source));
    Ok(())
}

fn resolve_handoff_source(ctx: &Ctx, from: Option<&str>) -> anyhow::Result<String> {
    match from {
        Some(explicit) => super::active_harness::canonical_id(explicit).map_err(|_| {
            anyhow::anyhow!(
                "unknown handoff source '{explicit}'; pass --from <harness> with a known harness id"
            )
        }),
        None => super::active_harness::read(&ctx.cwd)
            .map_err(|err| {
                anyhow::anyhow!("cannot use active harness marker ({err}); pass --from <harness>")
            })?
            .ok_or_else(|| anyhow::anyhow!("handoff source is unknown; pass --from <harness>")),
    }
}

/// Auto-finalize observed session work into `current.json` when gates pass.
pub fn try_auto_finalize(ctx: &Ctx, harness: &str) -> anyhow::Result<bool> {
    ctx.require_project()?;
    let current = local_store::read_handoff_local(&ctx.cwd)?;
    if current.is_none() {
        return Ok(false);
    }
    let home = super::install::home_dir()?;
    if !handoff_continuity::should_finalize(&ctx.cwd, &home, harness, current.as_ref()) {
        return Ok(false);
    };
    finalize_current(ctx, harness, current.as_ref(), None).map(|_| true)
}

/// The finalize phase of the session-boundary journal (repair Phase 3).
///
/// Differences from `try_auto_finalize`, by contract:
/// - The FIRST automatic handoff is created when none exists (an empty
///   store must not turn finalization into a silent no-op).
/// - The packet is bound to the boundary's exact root (`latest_root` =
///   `boundary_root`) — the handoff for a session boundary references the
///   root produced for THAT boundary.
/// - A missing verified session is an ERROR (retryable), never a quiet
///   success — the journal backs off and parks as manual_attention with
///   the error retained if the transcript never appears.
///
/// Replayable exact-session finalization. The journal key binds the immutable
/// intent and history packet, including a crash between packet and phase commit.
pub fn finalize_bound_job(
    ctx: &Ctx,
    job: &stateroot_core::finalize_journal::BoundaryJob,
) -> anyhow::Result<i64> {
    let _lock = stateroot_core::safe_io::ResourceLock::acquire(
        local_store::root(&ctx.cwd).join("local/locks/handoff-write.lock"),
    )?;
    let history = local_store::list_handoffs_local(&ctx.cwd)?;
    if let Some(packet) = history
        .iter()
        .find(|packet| packet["boundary_ingest_key"].as_str() == Some(job.ingest_key.as_str()))
    {
        let current = local_store::read_handoff_local(&ctx.cwd)?;
        if packet["boundary_publish_current"].as_bool() == Some(true)
            && current.as_ref().is_none_or(|current| {
                current["seq"].as_i64().unwrap_or(0) < packet["seq"].as_i64().unwrap_or(0)
                    && can_publish_boundary(&ctx.cwd, job, current)
            })
        {
            stateroot_core::safe_io::atomic_replace_json(
                &local_store::root(&ctx.cwd).join(local_store::HANDOFF_CURRENT_PATH),
                packet,
            )?;
        }
        return packet["seq"]
            .as_i64()
            .context("boundary result missing seq");
    }
    anyhow::ensure!(
        job.project_path.is_empty()
            || stateroot_core::transcripts::same_worktree(Path::new(&job.project_path), &ctx.cwd),
        "boundary belongs to another checkout"
    );
    anyhow::ensure!(
        stateroot_core::roots::lineage_refname(&ctx.cwd) == job.lineage_ref,
        "boundary lineage no longer matches checkout"
    );
    let home = super::install::home_dir()?;
    let session = handoff_continuity::verified_session(
        &home,
        &ctx.cwd,
        &job.harness,
        &job.session_id,
        job.transcript.as_deref(),
    )
    .ok_or_else(|| {
        anyhow::anyhow!(
            "no verified exact native session {} for {}; boundary retained",
            job.session_id,
            job.harness
        )
    })?;
    let current = local_store::read_handoff_local(&ctx.cwd)?;
    let seq = history
        .iter()
        .chain(current.iter())
        .filter_map(|packet| packet["seq"].as_i64())
        .max()
        .unwrap_or(0);
    let (objective, phase) = local_state_fields(&ctx.cwd)?;
    let project = ctx.require_project()?;
    let intent_path =
        local_store::root(&ctx.cwd).join(format!("local/finalize-intents/{}.json", job.ingest_key));
    let mut packet: Value = if intent_path.is_file() {
        let intent: Value = serde_json::from_slice(&std::fs::read(&intent_path)?)?;
        anyhow::ensure!(
            intent["schema_version"].as_str()
                == Some(stateroot_core::local_store::SCHEMA_HANDOFF_V1)
                && intent["boundary_ingest_key"].as_str() == Some(job.ingest_key.as_str())
                && intent["evidence_ref"]["session_id"].as_str() == Some(job.session_id.as_str()),
            "corrupt or unsupported finalization intent retained in place"
        );
        intent
    } else {
        let mut packet = handoff_continuity::build_finalize_packet(
            &project.project_id,
            &ctx.cwd,
            &job.harness,
            seq,
            &session,
            &objective,
            &phase,
        );
        packet["latest_root"] = serde_json::json!(job.root);
        packet["boundary_ingest_key"] = serde_json::json!(job.ingest_key);
        packet["boundary_at"] = serde_json::json!(job.enqueued_at);
        packet["boundary_job_id"] = serde_json::json!(job.id);
        packet["snapshot_timing"] = serde_json::json!(job.snapshot_timing);
        packet["boundary_source_root"] = serde_json::json!(job.boundary_source_root);
        packet["warnings"]
            .as_array_mut()
            .expect("packet warnings")
            .push(serde_json::json!(job.snapshot_timing));
        packet["capture_watermark"] = serde_json::json!(job.capture_watermark);
        if let Some(plan_ref) = &job.plan_ref {
            packet["plan_ref"] = plan_ref.clone();
        }
        let packet = bound_packet(packet);
        stateroot_core::safe_io::atomic_replace_json(&intent_path, &packet)?;
        packet
    };
    // A saved intent can be older than another writer's committed sequence.
    packet["seq"] = serde_json::json!(seq + 1);
    let publish = current
        .as_ref()
        .is_none_or(|current| can_publish_boundary(&ctx.cwd, job, current));
    packet["boundary_publish_current"] = serde_json::json!(publish);
    validate_packet(&packet, false)?;
    if publish {
        write_packet_durable(&ctx.cwd, &packet)?;
    } else {
        write_packet_history_durable(
            &ctx.cwd,
            &packet,
            &format!("{}\n", serde_json::to_string_pretty(&packet)?),
        )?;
    }
    Ok(seq + 1)
}

fn can_publish_boundary(
    project: &Path,
    job: &stateroot_core::finalize_journal::BoundaryJob,
    current: &Value,
) -> bool {
    let authored = current["seq"].as_i64().is_some_and(|current_seq| {
        handoff_continuity::any_explicit_blocks_finalize(project, current_seq)
    }) || !current["recommended_next_harness"].is_null();
    let time = current["boundary_at"]
        .as_str()
        .or_else(|| current["written_at"].as_str())
        .unwrap_or("");
    !authored
        && (time < job.enqueued_at.as_str()
            || time == job.enqueued_at.as_str()
                && current["boundary_job_id"]
                    .as_str()
                    .is_none_or(|id| id <= job.id.as_str()))
}

/// Inspect retained jobs without recovering, advancing, or rewriting them.
pub fn inspect(ctx: &Ctx, id: Option<&str>) -> anyhow::Result<()> {
    ctx.require_project()?;
    let mut jobs = stateroot_core::finalize_journal::load_all(&ctx.cwd);
    if let Some(id) = id {
        jobs.retain(|job| job.id == id);
        anyhow::ensure!(
            !jobs.is_empty(),
            "boundary job unavailable or unsupported; original bytes retained: {id}"
        );
    }
    let invalid = stateroot_core::finalize_journal::invalid_records(&ctx.cwd);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema_version": "stateroot.boundary-inspection.v1",
            "read_only": true,
            "jobs": jobs,
            "unsupported_records_retained": invalid,
        }))?
    );
    Ok(())
}

/// Explicit recovery advances the selected retained job, unlike inspection.
pub async fn recover(ctx: &Ctx, id: &str, transcript: Option<&str>) -> anyhow::Result<()> {
    let job = stateroot_core::finalize_journal::load_all(&ctx.cwd)
        .into_iter()
        .find(|job| job.id == id)
        .ok_or_else(|| anyhow::anyhow!("boundary job unavailable or quarantined: {id}"))?;
    if let Some(locator) = transcript {
        anyhow::ensure!(
            handoff_continuity::verified_session(
                &super::install::home_dir()?,
                &ctx.cwd,
                &job.harness,
                &job.session_id,
                Some(locator)
            )
            .is_some(),
            "locator does not verify this job's native session and project"
        );
    }
    super::drain_finalize::recover_one(ctx, id, transcript).await?;
    println!(
        "boundary job {id}: recovery attempted; original evidence and recovery history retained"
    );
    Ok(())
}

fn finalize_current(
    ctx: &Ctx,
    harness: &str,
    current: Option<&Value>,
    boundary_root: Option<&str>,
) -> anyhow::Result<i64> {
    let _lock = stateroot_core::safe_io::ResourceLock::acquire(
        local_store::root(&ctx.cwd).join("local/locks/handoff-write.lock"),
    )?;
    let fresh = local_store::read_handoff_local(&ctx.cwd)?;
    let current = fresh.as_ref().or(current);
    if let Some(packet) = current {
        anyhow::ensure!(
            !handoff_continuity::explicit_blocks_finalize(
                &ctx.cwd,
                harness,
                packet["seq"].as_i64().unwrap_or(0)
            ),
            "explicit authored handoff takes precedence"
        );
    }
    let home = super::install::home_dir()?;
    let session = handoff_continuity::latest_verified_session(&home, &ctx.cwd, harness)
        .ok_or_else(|| anyhow::anyhow!("no verified {harness} transcript to finalize"))?;
    let current_seq = current
        .and_then(|p| p.get("seq"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let (state_objective, phase) = local_state_fields(&ctx.cwd)?;
    let project = ctx.require_project()?;
    let mut packet = handoff_continuity::build_finalize_packet(
        &project.project_id,
        &ctx.cwd,
        harness,
        current_seq,
        &session,
        &state_objective,
        &phase,
    );
    if let Some(root) = boundary_root {
        packet["latest_root"] = Value::String(root.to_string());
    }
    packet = bound_packet(packet);
    validate_packet(&packet, false)?;
    write_packet_durable(&ctx.cwd, &packet)?;
    Ok(packet
        .get("seq")
        .and_then(|v| v.as_i64())
        .unwrap_or(current_seq + 1))
}

/// `stateroot handoff finalize [--from H]` — manual recovery when hooks missed.
pub async fn finalize(ctx: &Ctx, from: Option<&str>) -> anyhow::Result<()> {
    let source = resolve_handoff_source(ctx, from)?;
    if try_auto_finalize(ctx, &source)? {
        let seq = local_store::read_handoff_local(&ctx.cwd)?
            .and_then(|packet| packet.get("seq").and_then(|v| v.as_i64()))
            .unwrap_or(0);
        println!("handoff #{seq} finalized from verified transcript ({FINALIZE_WARNING})");
        if let Some(footer) = super::resume::digest_footer(&ctx.cwd) {
            println!("{footer}");
        }
    } else {
        println!(
            "nothing to finalize (no newer verified session or explicit handoff blocks overwrite)"
        );
    }
    Ok(())
}

/// `stateroot handoff repair` — recover a malformed current packet without
/// hand-editing `.stateroot/`.
pub async fn repair(ctx: &Ctx) -> anyhow::Result<()> {
    ctx.require_project()?;
    let outcome = local_store::repair_handoff_current(&ctx.cwd)?;
    match (outcome.quarantined, outcome.restored_from) {
        (None, None) => println!("current handoff is valid (or absent); nothing to repair"),
        (Some(quarantine), Some(history)) => {
            println!(
                "repaired current handoff from {}; corrupt bytes retained at {}",
                history.display(),
                quarantine.display()
            );
        }
        (Some(quarantine), None) => {
            println!(
                "current handoff remains corrupt; no valid history exists. Corrupt bytes retained at {}",
                quarantine.display()
            );
        }
        (None, Some(_)) => unreachable!("restoration always quarantines corrupt bytes"),
    }
    Ok(())
}

/// Lifecycle/automatic exit: checkpoint observation only — never replace
/// `handoffs/current.json` or finalize a separate handoff boundary.
async fn automatic_checkpoint_only(ctx: &Ctx, note_text: Option<&str>) -> anyhow::Result<()> {
    let note = note_text.unwrap_or("automatic session checkpoint");
    let projected =
        super::checkpoint::record_checkpoint(ctx, super::checkpoint::LOCAL_HARNESS, note, &[])
            .await?;
    if projected {
        println!("checkpoint recorded; existing structured handoff preserved");
    } else {
        println!("checkpoint queued (offline); existing structured handoff preserved");
    }
    Ok(())
}

/// Local-only acceptance bookkeeping mutates the packet in place (accept
/// marks, checkpoint activity stamps); it is not handoff content, so it is
/// stripped before hashing — otherwise every checkpoint would read as drift.
const ACCEPTANCE_BOOKKEEPING_KEYS: &[&str] = &["accepted_by", "acceptances", "last_activity"];

/// sha256 of the canonical handoff body: the packet minus local acceptance
/// bookkeeping (serde_json objects serialize with sorted keys, so the digest
/// is stable across in-place rewrites).
fn handoff_body_sha256(packet: &Value) -> String {
    let mut canonical = packet.clone();
    if let Some(obj) = canonical.as_object_mut() {
        for key in ACCEPTANCE_BOOKKEEPING_KEYS {
            obj.remove(*key);
        }
    }
    let text = serde_json::to_string(&canonical).unwrap_or_default();
    format!("{:x}", sha2::Sha256::digest(text.as_bytes()))
}

/// `stateroot handoff accept` — mark the current handoff accepted by a harness.
///
/// Fail-closed on a stale boundary (the digest's ts_newer rule): accepting a
/// handoff that newer observed activity has overtaken would anchor the
/// receiver on dead state; `--force` overrides and is recorded. Acceptances
/// are append-only records carrying the content hash at accept time, so a
/// repeated `--operation-id` is an idempotent no-op and a changed body since
/// the last acceptance reads as drift (warning, never a refusal).
pub async fn accept(
    ctx: &Ctx,
    by: &str,
    operation_id: Option<&str>,
    force: bool,
) -> anyhow::Result<()> {
    ctx.require_project()?;
    // The accepter is part of the shared record — validate it as a canonical
    // harness id (aliases resolve), with `cli` kept as the local-CLI actor.
    let by = if by.trim() == "cli" {
        "cli".to_string()
    } else {
        super::active_harness::canonical_id(by).map_err(|_| {
            anyhow::anyhow!("unknown harness '{by}'; pass --by with a known harness id or alias")
        })?
    };
    let operation_id = operation_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let Some(packet) = local_store::read_handoff_local(&ctx.cwd)? else {
        println!("no current handoff to accept");
        return Ok(());
    };
    let seq = packet.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);

    // Idempotency is checked before the staleness gate: re-issuing an
    // already-applied operation must not newly fail.
    if let Some(id) = operation_id.as_deref() {
        let already = packet
            .get("acceptances")
            .and_then(Value::as_array)
            .is_some_and(|records| {
                records
                    .iter()
                    .any(|record| record.get("operation_id").and_then(Value::as_str) == Some(id))
            });
        if already {
            println!("handoff #{seq} already accepted (operation {id}) — idempotent no-op");
            return Ok(());
        }
    }

    let boundary = packet
        .get("written_at")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .or_else(|| packet.get("created_at").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    let stale = super::resume::latest_activity(&ctx.cwd).filter(|activity| {
        !boundary.is_empty() && super::resume::ts_newer(&activity.at, &boundary)
    });
    if let Some(activity) = &stale {
        if !force {
            anyhow::bail!(
                "refusing to accept handoff #{seq}: {} by {} at {} postdates the handoff boundary {boundary} — the handoff is stale. Re-read with `stateroot resume` or pass --force to accept anyway",
                activity.kind,
                activity.harness,
                activity.at
            );
        }
        note!(
            "warning: accepting stale handoff #{seq} under --force (newer activity: {} by {} at {})",
            activity.kind,
            activity.harness,
            activity.at
        );
    }

    let body_sha256 = handoff_body_sha256(&packet);
    if let Some(prior) = packet
        .get("acceptances")
        .and_then(Value::as_array)
        .and_then(|records| records.last())
        .and_then(|record| record.get("body_sha256"))
        .and_then(Value::as_str)
    {
        if prior != body_sha256 {
            note!(
                "warning: handoff body changed since the last acceptance (sha256 {}… → {}…) — recording anyway",
                &prior[..prior.len().min(12)],
                &body_sha256[..body_sha256.len().min(12)]
            );
        }
    }

    let mut record = json!({
        "by": by,
        "at": now_rfc3339(),
        "body_sha256": body_sha256,
    });
    if let Some(id) = operation_id {
        record["operation_id"] = json!(id);
    }
    if force {
        record["forced"] = json!(true);
    }
    let mut count = 0usize;
    local_store::update_handoff_current(&ctx.cwd, |packet| {
        let Some(obj) = packet.as_object_mut() else {
            return false;
        };
        if let Value::Array(records) = obj
            .entry("acceptances")
            .or_insert_with(|| Value::Array(vec![]))
        {
            records.push(record.clone());
        }
        if let Value::Array(accepted) = obj
            .entry("accepted_by")
            .or_insert_with(|| Value::Array(vec![]))
        {
            if !accepted.iter().any(|a| a.as_str() == Some(by.as_str())) {
                accepted.push(Value::String(by.clone()));
            }
            count = accepted.len();
        }
        true
    })?;
    println!("handoff accepted by {by} ({count} acceptance(s) total)");
    crate::telemetry::activity(&ctx.config_dir, &ctx.cwd, Some(&by));
    if let Some(footer) = super::resume::digest_footer(&ctx.cwd) {
        println!("{footer}");
    }
    super::reconcile_quiet(ctx);
    Ok(())
}

/// Identical history copies can arrive through multiple forks. Collapse only
/// the read view, using the existing acceptance hash; never rewrite history.
fn distinct_history_packets(packets: Vec<Value>) -> Vec<(String, Value)> {
    let mut seen = std::collections::BTreeSet::new();
    packets
        .into_iter()
        .filter_map(|packet| {
            let id = handoff_body_sha256(&packet);
            seen.insert(id.clone()).then_some((id, packet))
        })
        .collect()
}

/// `stateroot handoff list`.
pub async fn list(ctx: &Ctx) -> anyhow::Result<()> {
    ctx.require_project()?;
    let mut packets = distinct_history_packets(local_store::list_handoffs_local(&ctx.cwd)?);
    if packets.is_empty() {
        println!("no handoffs recorded yet (local)");
        return Ok(());
    }
    // The current packet stays pinned to the top even when a repair restored
    // an older history entry — it is the handoff `show` renders by default.
    let current_id = match local_store::read_handoff_local(&ctx.cwd) {
        Ok(packet) => packet.as_ref().map(handoff_body_sha256),
        Err(error) => {
            note!("warning: current handoff is unreadable: {error}; no current marker shown");
            None
        }
    };
    if let Some(id) = &current_id {
        if let Some(position) = packets.iter().position(|(packet_id, _)| packet_id == id) {
            let packet = packets.remove(position);
            packets.insert(0, packet);
        } else {
            note!("warning: current handoff has no matching history packet; use `stateroot handoff show` to inspect it");
        }
    }
    println!(
        "{:<6} {:<22} {:<12} {:<12} {:<64} PHASE",
        "SEQ", "CREATED", "FROM", "TO", "PACKET ID"
    );
    for (id, packet) in packets {
        let seq = packet.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
        let created = packet
            .get("created_at")
            .and_then(|v| v.as_str())
            .map(|s| truncate(s, 19))
            .unwrap_or_default();
        let from = packet
            .get("created_by_harness")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let to = packet
            .get("recommended_next_harness")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let phase = packet
            .get("current_phase")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let marker = if current_id.as_ref() == Some(&id) {
            " ← current"
        } else {
            ""
        };
        println!("{seq:<6} {created:<22} {from:<12} {to:<12} {id} {phase}{marker}");
    }
    Ok(())
}

/// `stateroot handoff show [seq]` / `show --id <body SHA256>`.
pub async fn show(ctx: &Ctx, seq: Option<i64>, id: Option<&str>) -> anyhow::Result<()> {
    ctx.require_project()?;
    if let Some(id) = id {
        if seq.is_some() || id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            anyhow::bail!("use either a sequence number or --id with an exact 64-character packet SHA256 from `stateroot handoff list`");
        }
        let packets = distinct_history_packets(local_store::list_handoffs_local(&ctx.cwd)?);
        let (_, packet) = packets
            .iter()
            .find(|(packet_id, _)| packet_id.eq_ignore_ascii_case(id))
            .with_context(|| format!("handoff packet {id} not found in local history"))?;
        print!(
            "{}",
            render_handoff_digest_full(packet, false, &[], None, Some(&ctx.cwd))
        );
        return Ok(());
    }
    match seq {
        None => {
            let (packet, source) = fetch_handoff(&ctx.cwd);
            match packet {
                Some(packet) => {
                    note!("(source: {source})");
                    print!(
                        "{}",
                        render_handoff_digest_full(&packet, false, &[], None, Some(&ctx.cwd))
                    );
                    Ok(())
                }
                None => anyhow::bail!("no current handoff found"),
            }
        }
        Some(seq) => {
            // P1 REST exposes only the *current* handoff packet; older packets
            // are available from the local history directory.
            let history = distinct_history_packets(local_store::list_handoffs_local(&ctx.cwd)?);
            let matches: Vec<_> = history
                .iter()
                .filter(|(_, packet)| packet.get("seq").and_then(Value::as_i64) == Some(seq))
                .collect();
            if matches.len() > 1 {
                let candidates = matches
                    .iter()
                    .map(|(id, packet)| {
                        format!(
                            "  stateroot handoff show --id {id} (created={}, from={}, to={}, fork={})",
                            packet["created_at"].as_str().unwrap_or("unknown"),
                            packet["created_by_harness"].as_str().unwrap_or("unknown"),
                            packet["recommended_next_harness"].as_str().unwrap_or("-"),
                            packet["fork_id"].as_str().unwrap_or("trunk")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                anyhow::bail!("handoff #{seq} is ambiguous ({} distinct packets); select an exact packet:\n{candidates}", matches.len());
            }
            if let Some((_, packet)) = matches.first() {
                print!(
                    "{}",
                    render_handoff_digest_full(packet, false, &[], None, Some(&ctx.cwd))
                );
                return Ok(());
            }
            anyhow::bail!(
                "handoff #{seq} not found (server REST exposes only the current handoff; checked local history too)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_identity_ignores_bookkeeping_but_not_fork_or_authored_content() {
        let packet = json!({"seq":110,"task":"same task","fork_id":"left"});
        let mut accepted = packet.clone();
        accepted["accepted_by"] = json!(["kimi"]);
        accepted["acceptances"] = json!([{"by":"kimi","body_sha256":"recorded"}]);
        accepted["last_activity"] = json!({"at":"later"});
        assert_eq!(handoff_body_sha256(&packet), handoff_body_sha256(&accepted));
        let mut other = packet.clone();
        other["fork_id"] = json!("right");
        assert_ne!(handoff_body_sha256(&packet), handoff_body_sha256(&other));
        assert_eq!(
            distinct_history_packets(vec![packet.clone(), accepted, other.clone()]),
            vec![
                (handoff_body_sha256(&packet), packet),
                (handoff_body_sha256(&other), other)
            ]
        );
    }
    use stateroot_core::transcripts::{PlanStep, TailEntry};

    #[test]
    fn explicit_plan_and_no_plan_preserve_automatic_behavior_without_unrelated_assignment() {
        let fixture = tempfile::tempdir().unwrap();
        let plan = stateroot_core::plans::record(
            fixture.path(),
            "unrelated approved",
            "codex",
            None,
            "# Other\n- [ ] work",
        )
        .unwrap();
        stateroot_core::plans::transition(
            fixture.path(),
            &plan.id,
            stateroot_core::plans::PlanStatus::Approved,
        )
        .unwrap();
        let assemble = |input| {
            assemble_packet(
                input,
                None,
                PacketContext {
                    project_dir: fixture.path(),
                    project_id: "fixture",
                    seq: 1,
                    source: "codex",
                    routing_dest: None,
                    note_text: None,
                    objective_override: None,
                    state_objective: String::new(),
                    state_phase: String::new(),
                    handing_to_another: false,
                },
            )
            .unwrap()
        };
        assert_eq!(assemble(HandoffInput::default())["plan_ref"]["id"], plan.id);
        assert!(assemble(HandoffInput {
            no_plan: true,
            ..Default::default()
        })
        .get("plan_ref")
        .is_none());
        stateroot_core::plans::complete(
            fixture.path(),
            &plan.id,
            "explicit fixture completion",
            "codex",
            None,
        )
        .unwrap();
        let completed = assemble(HandoffInput {
            plan: Some(plan.id.clone()),
            ..Default::default()
        });
        assert_eq!(completed["plan_ref"]["status"], "done");
        let digest = render_handoff_digest_full(&completed, true, &[], None, Some(fixture.path()));
        assert!(digest.contains("## Referenced Plan"));
        assert!(digest.contains("Reference only"));
        assert!(!digest.contains("Execute it as written"));
    }

    #[test]
    fn bounds_preserve_full_content() {
        let mut packet = json!({
            "schema_version": "stateroot.handoff.v1",
            "project_id": "p",
            "seq": 1,
            "task": "x".repeat(4000),
            "context_summary": "y".repeat(7000),
            "next_actions": (0..25).map(|i| format!("action {i}")).collect::<Vec<_>>(),
            "bugs_found": ["z".repeat(2000)],
            "changed_files": (0..600).map(|i| format!("src/f{i}.rs")).collect::<Vec<_>>(),
            "created_at": "2026-07-18T00:00:00Z",
            "created_by_harness": "codex",
        });
        let original = packet.clone();
        packet = bound_packet(packet);
        assert_eq!(packet, original, "bound_packet must not truncate");
    }

    #[test]
    fn bounds_leave_small_packets_alone() {
        let packet = json!({
            "task": "small",
            "context_summary": "fine",
            "next_actions": ["one", "two"],
        });
        let bounded = bound_packet(packet.clone());
        assert_eq!(bounded, packet);
    }

    #[test]
    fn task_falls_back_to_first_noncompleted_plan_item_and_full_tail() {
        let session = TranscriptSession {
            harness: "codex",
            session_id: "s".into(),
            plan_state: vec![
                PlanStep {
                    step: "finished".into(),
                    status: "completed".into(),
                },
                PlanStep {
                    step: "immediate pending step".into(),
                    status: "pending".into(),
                },
            ],
            conversation_tail: vec![
                TailEntry {
                    role: "user",
                    text: "u1".into(),
                },
                TailEntry {
                    role: "assistant",
                    text: "a1".into(),
                },
                TailEntry {
                    role: "user",
                    text: "u2".into(),
                },
                TailEntry {
                    role: "assistant",
                    text: "a2".into(),
                },
                TailEntry {
                    role: "user",
                    text: "u3".into(),
                },
                TailEntry {
                    role: "assistant",
                    text: "a3".into(),
                },
            ],
            ..Default::default()
        };
        let packet = assemble_packet(
            HandoffInput {
                objective: Some("durable goal".into()),
                context_summary: Some("Evidence summary distinct from the plan step.".into()),
                ..Default::default()
            },
            Some(&session),
            PacketContext {
                project_dir: Path::new("."),
                project_id: "project",
                seq: 1,
                source: "codex",
                routing_dest: None,
                note_text: None,
                objective_override: None,
                state_objective: String::new(),
                state_phase: String::new(),
                handing_to_another: false,
            },
        )
        .expect("packet");
        assert_eq!(packet["task"], "immediate pending step");
        assert_eq!(
            packet["conversation_tail"],
            json!([
                {"role":"user","text":"u1"},
                {"role":"assistant","text":"a1"},
                {"role":"user","text":"u2"},
                {"role":"assistant","text":"a2"},
                {"role":"user","text":"u3"},
                {"role":"assistant","text":"a3"}
            ])
        );
    }

    #[test]
    fn unsafe_legacy_numbering_is_preserved_as_one_item() {
        assert_eq!(
            conservative_numbered_items("1. safe first\ncontinuation without a number"),
            vec!["1. safe first\ncontinuation without a number"]
        );
    }

    #[test]
    fn input_accepts_immediate_task_alias() {
        let input = parse_handoff_input_text(
            r#"{"immediate_task":"boundary","objective":"goal","context_summary":"summary"}"#,
            "test.json",
        )
        .expect("parse");
        assert_eq!(input.task.as_deref(), Some("boundary"));
    }

    #[test]
    fn input_coerces_decision_objects() {
        let input = parse_handoff_input_text(
            r#"{"objective":"goal","task":"task","context_summary":"summary","decisions":[{"decision":"Use async","rationale":"Lower latency"}]}"#,
            "test.json",
        )
        .expect("parse");
        assert_eq!(
            input.decisions,
            Some(vec!["Use async — Lower latency".to_string()])
        );
    }

    #[test]
    fn input_unknown_key_lists_allowed_fields() {
        let err = parse_handoff_input_text(
            r#"{"surprise":true,"objective":"goal","task":"task","context_summary":"summary"}"#,
            "test.json",
        )
        .expect_err("unknown");
        let message = format!("{err:#}");
        assert!(message.contains("unknown handoff input key(s): surprise"));
        assert!(message.contains("Allowed content keys"));
        assert!(message.contains("task"));
    }

    #[test]
    fn write_flags_override_input_fields() {
        let input = apply_write_flags(
            HandoffInput {
                task: Some("from file".into()),
                next_actions: Some(vec!["old".into()]),
                ..Default::default()
            },
            &HandoffWriteFlags {
                task: Some("from flag"),
                next: &["new".into()],
                ..Default::default()
            },
        )
        .expect("flags");
        assert_eq!(input.task.as_deref(), Some("from flag"));
        assert_eq!(input.next_actions, Some(vec!["new".to_string()]));
    }

    #[test]
    fn failed_approach_flag_parsing_and_outcome_vocabulary() {
        let parsed = parse_failed_approach_flag("Naive parser → failed: blew the stack")
            .expect("valid flag");
        assert_eq!(parsed.approach, "Naive parser");
        assert_eq!(parsed.outcome, "failed");
        assert_eq!(parsed.reason, "blew the stack");

        // Outcome parsing is case-insensitive but stores the canonical label.
        let parsed = parse_failed_approach_flag("Cache → Partial: warmed").expect("case");
        assert_eq!(parsed.outcome, "partial");

        let err = parse_failed_approach_flag("No arrow here: failed").expect_err("no arrow");
        assert!(format!("{err:#}").contains("expected \"<approach> → <outcome>: <reason>\""));
        let err = parse_failed_approach_flag("X → exploded: boom").expect_err("bad outcome");
        assert!(
            format!("{err:#}").contains("invalid --failed-approach outcome 'exploded'"),
            "{err:#}"
        );

        // The JSON input channel applies the same vocabulary.
        let err = parse_handoff_input_text(
            r#"{"task":"t","failed_approaches":[{"approach":"a","outcome":"mystery","reason":"r"}]}"#,
            "test.json",
        )
        .expect_err("bad input outcome");
        assert!(
            format!("{err:#}").contains("is not success|partial|failed"),
            "{err:#}"
        );
    }
}
