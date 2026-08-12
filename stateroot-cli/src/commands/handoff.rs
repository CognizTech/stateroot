//! `stateroot handoff write|list|show`.

use std::io::Read as _;
use std::io::Write as _;
use std::path::Path;

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{json, Value};
use stateroot_core::local_store::now_rfc3339;
use stateroot_core::local_store::{self, SCHEMA_HANDOFF_V1};
use stateroot_core::transcripts::{self, TranscriptSession};

use super::resume::{fetch_handoff, render_handoff_digest};
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

/// Handoff quality bounds at write (plan P4.1).
const SUMMARY_MAX: usize = stateroot_core::handoff_bounds::CONTEXT_SUMMARY_MAX;
const TEXT_MAX: usize = 3000;
const ITEM_MAX: usize = 1500;
const LIST_ITEMS_MAX: usize = 20;
const LIST_TOTAL_MAX: usize = 6000;
const FILES_ITEMS_MAX: usize = 512;
const FILES_TOTAL_MAX: usize = 4000;

/// Apply the quality bounds to a packet in place, warning on stderr about
/// everything truncated. Returns the packet for chaining.
fn bound_packet(mut packet: Value) -> Value {
    for (key, max) in [
        ("task", TEXT_MAX),
        ("objective", TEXT_MAX),
        ("current_phase", ITEM_MAX),
        ("implementation_status", ITEM_MAX),
        ("context_summary", SUMMARY_MAX),
    ] {
        if let Some(text) = packet.get(key).and_then(|v| v.as_str()) {
            if text.chars().count() > max {
                note!("warning: {key} exceeded {max} chars — truncated with an ellipsis marker");
                packet[key] = Value::String(truncate(text, max));
                if key == "context_summary" {
                    let marker = format!("context_summary truncated to {max} characters");
                    if let Some(warnings) = packet.get_mut("warnings").and_then(Value::as_array_mut)
                    {
                        if !warnings.iter().any(|item| item.as_str() == Some(&marker)) {
                            warnings.push(Value::String(marker));
                        }
                    }
                }
            }
        }
    }
    let list_keys = [
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
        "progress_summaries",
        "milestones",
    ];
    for key in list_keys {
        let item_cap = if key == "changed_files" {
            FILES_ITEMS_MAX
        } else {
            LIST_ITEMS_MAX
        };
        let total_cap = if key == "changed_files" {
            FILES_TOTAL_MAX
        } else {
            LIST_TOTAL_MAX
        };
        if let Some(arr) = packet.get_mut(key).and_then(|v| v.as_array_mut()) {
            let original_len = arr.len();
            if original_len > item_cap {
                note!(
                    "warning: {key} had {original_len} items (>{item_cap}) — truncated to {item_cap}"
                );
            }
            let mut total = 0usize;
            let mut kept: Vec<Value> = Vec::new();
            let mut dropped_for_total = 0usize;
            for (i, item) in std::mem::take(arr).into_iter().enumerate() {
                if kept.len() >= item_cap {
                    dropped_for_total += 1;
                    continue;
                }
                let mut text = match item {
                    Value::String(s) => s,
                    other => serde_json::to_string(&other).unwrap_or_default(),
                };
                if text.chars().count() > ITEM_MAX {
                    note!("warning: {key}[{i}] exceeded {ITEM_MAX} chars — truncated");
                    text = truncate(&text, ITEM_MAX);
                }
                if text.trim().is_empty()
                    || kept.iter().any(|existing| existing.as_str() == Some(&text))
                {
                    continue;
                }
                total += text.chars().count();
                if total > total_cap {
                    dropped_for_total += 1;
                    continue;
                }
                kept.push(Value::String(text));
            }
            if dropped_for_total > 0 {
                note!("warning: {key} dropped {dropped_for_total} item(s) over quality bounds");
            }
            *arr = kept;
        }
    }
    packet
}

/// Author-controlled handoff content. Envelope, provenance, transcript-rich
/// fields, and timestamps intentionally do not appear here: serde rejects
/// them instead of allowing the input file to impersonate the CLI.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct HandoffInput {
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
}

fn read_input(path: Option<&str>) -> anyhow::Result<HandoffInput> {
    let Some(path) = path else {
        return Ok(HandoffInput::default());
    };
    let mut text = String::new();
    if path == "-" {
        std::io::stdin()
            .read_to_string(&mut text)
            .context("could not read handoff JSON from stdin")?;
    } else {
        text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "could not read handoff input '{}'",
                Path::new(path).display()
            )
        })?;
    }
    serde_json::from_str(&text).with_context(|| {
        format!(
            "invalid strict handoff input '{}': expected only content fields",
            path
        )
    })
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

fn session_order(left: &TranscriptSession, right: &TranscriptSession) -> std::cmp::Ordering {
    let left_latest = if left.ended_at.is_empty() {
        &left.started_at
    } else {
        &left.ended_at
    };
    let right_latest = if right.ended_at.is_empty() {
        &right.started_at
    } else {
        &right.ended_at
    };
    left_latest
        .cmp(right_latest)
        .then_with(|| left.started_at.cmp(&right.started_at))
        .then_with(|| left.session_id.cmp(&right.session_id))
}

fn latest_verified_session(
    home: &Path,
    project: &Path,
    harness: &str,
) -> Option<TranscriptSession> {
    transcripts::readers()
        .into_iter()
        .find(|reader| reader.id() == harness)
        .and_then(|reader| reader.scan(home, project).into_iter().max_by(session_order))
}

pub(crate) fn compact_tail(session: &TranscriptSession) -> Vec<Value> {
    let mut user_remaining = 2usize;
    let mut assistant_remaining = 2usize;
    let mut keep = vec![false; session.conversation_tail.len()];
    for (index, entry) in session.conversation_tail.iter().enumerate().rev() {
        let remaining = match entry.role {
            "user" => &mut user_remaining,
            "assistant" => &mut assistant_remaining,
            _ => continue,
        };
        if *remaining > 0 {
            keep[index] = true;
            *remaining -= 1;
        }
    }
    session
        .conversation_tail
        .iter()
        .zip(keep)
        .filter(|(_, keep)| *keep)
        .map(|(entry, _)| json!({"role": entry.role, "text": entry.text}))
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
    project_id: &'a str,
    seq: i64,
    source: &'a str,
    to: &'a str,
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
        "recommended_next_harness": context.to,
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

    packet = bound_packet(packet);
    validate_packet(&packet, context.handing_to_another)?;
    Ok(packet)
}

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
            anyhow::bail!("handoff quality check failed: {key} must not be empty");
        }
    }
    if required("task").eq_ignore_ascii_case(required("context_summary")) {
        anyhow::bail!(
            "handoff quality check failed: task and context_summary must describe different things"
        );
    }
    if handing_to_another
        && packet
            .get("next_actions")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
    {
        anyhow::bail!(
            "handoff quality check failed: next_actions must not be empty when handing off to another harness"
        );
    }
    Ok(())
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
    history_result?;

    #[cfg(windows)]
    {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&current)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }

    #[cfg(not(windows))]
    {
        let temporary = parent.join(format!(".current-{}.tmp", uuid::Uuid::now_v7()));
        let current_result = (|| -> anyhow::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temporary, &current)?;
            Ok(())
        })();
        if current_result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        current_result
    }
}

/// `stateroot handoff write [--from H] --to H [--input PATH] [--note …]`.
///
/// Explicit origin replaces the current structured handoff. Automatic origin
/// (lifecycle hooks) records a checkpoint only and preserves any existing
/// structured handoff.
pub async fn write(
    ctx: &Ctx,
    from: Option<&str>,
    to: &str,
    note_text: Option<&str>,
    input_path: Option<&str>,
    objective_override: Option<&str>,
) -> anyhow::Result<()> {
    write_with_origin(
        ctx,
        from,
        to,
        note_text,
        input_path,
        objective_override,
        HandoffOrigin::Explicit,
    )
    .await
}

/// Same as [`write`] with an explicit/automatic origin.
pub async fn write_with_origin(
    ctx: &Ctx,
    from: Option<&str>,
    to: &str,
    note_text: Option<&str>,
    input_path: Option<&str>,
    objective_override: Option<&str>,
    origin: HandoffOrigin,
) -> anyhow::Result<()> {
    if origin == HandoffOrigin::Automatic {
        return automatic_checkpoint_only(ctx, note_text).await;
    }

    let project = ctx.require_project()?;
    let source = match from {
        Some(explicit) => super::active_harness::canonical_id(explicit)
            .map_err(|_| anyhow::anyhow!("unknown handoff source '{explicit}'; pass --from <harness> with a known harness id"))?,
        None => super::active_harness::read(&ctx.cwd)
            .map_err(|err| anyhow::anyhow!("cannot use active harness marker ({err}); pass --from <harness>"))?
            .ok_or_else(|| anyhow::anyhow!("handoff source is unknown; pass --from <harness>"))?,
    };
    let destination = super::active_harness::canonical_id(to).map_err(|_| {
        anyhow::anyhow!(
            "unknown handoff destination '{to}'; pass --to <harness> with a known harness id or alias"
        )
    })?;
    let input = read_input(input_path)?;
    // Read directly so malformed state cannot silently reset the sequence.
    let current = local_store::read_handoff_local(&ctx.cwd)?;
    let current_seq = current
        .as_ref()
        .and_then(|p| p.get("seq"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    // `project/state.json` holds the objective recorded at init; nothing
    // refreshes it as work progresses, so an explicit restatement wins.
    let (state_objective, phase) = local_state_fields(&ctx.cwd)?;
    let home = super::install::home_dir()?;
    let session = latest_verified_session(&home, &ctx.cwd, &source);
    let handing_to_another = destination != source;
    let packet = assemble_packet(
        input,
        session.as_ref(),
        PacketContext {
            project_id: &project.project_id,
            seq: current_seq + 1,
            source: &source,
            to: &destination,
            note_text,
            objective_override,
            state_objective,
            state_phase: phase,
            handing_to_another,
        },
    )?;

    write_packet_durable(&ctx.cwd, &packet)?;
    println!(
        "handoff #{} written",
        packet.get("seq").and_then(|v| v.as_i64()).unwrap_or(0)
    );
    // Compact digest footer (composed locally — no extra server calls).
    if let Some(footer) = super::resume::digest_footer(&ctx.cwd) {
        println!("{footer}");
    }
    Ok(())
}

/// Lifecycle/automatic exit: checkpoint observation only — never replace
/// `handoffs/current.json` or finalize a separate handoff boundary.
async fn automatic_checkpoint_only(ctx: &Ctx, note_text: Option<&str>) -> anyhow::Result<()> {
    let note = note_text.unwrap_or("automatic session checkpoint");
    let projected = super::checkpoint::record_checkpoint(ctx, note, &[]).await?;
    if projected {
        println!("checkpoint recorded; existing structured handoff preserved");
    } else {
        println!("checkpoint queued (offline); existing structured handoff preserved");
    }
    Ok(())
}

/// `stateroot handoff accept` — mark the current handoff accepted by a harness.
pub async fn accept(ctx: &Ctx, by: &str) -> anyhow::Result<()> {
    ctx.require_project()?;
    let count = super::resume::accept_handoff_local(&ctx.cwd, by)?;
    if count == 0 {
        println!("no current handoff to accept");
    } else {
        println!("handoff accepted by {by} ({count} acceptance(s) total)");
        queue_selection_observation(&ctx.cwd, by);
    }
    if let Some(footer) = super::resume::digest_footer(&ctx.cwd) {
        println!("{footer}");
    }
    Ok(())
}

fn queue_selection_observation(project_dir: &std::path::Path, by: &str) {
    let Ok(Some(manifest)) = local_store::read_manifest(project_dir) else {
        return;
    };
    let Some(project_id) = manifest.get("project_id").and_then(|v| v.as_str()) else {
        return;
    };
    if project_id.is_empty() {
        return;
    }
    // A CLI acceptance is not evidence that any harness performed it. Only
    // queue a harness observation when the caller supplied a registered id.
    let Ok(harness) = super::active_harness::canonical_id(by) else {
        return;
    };
    let op = json!({
        "ts": now_rfc3339(),
        "kind": "observation",
        "project_id": project_id,
        "observation": {
            "source": "cli",
            "source_id": format!("handoff-accept:{project_id}:{}", uuid::Uuid::now_v7()),
            "kind": "selection",
            "payload": {
                "text": format!("Handoff accepted by {by}"),
                "harness": &harness,
                "event": "handoff_accept",
                "kind_hint": "selection",
                "explicit": true,
            },
            "harness": &harness,
        },
    });
    if let Err(err) = local_store::outbox_append(project_dir, &op) {
        note!("warning: could not queue selection observation: {err}");
    }
}

/// `stateroot handoff list`.
pub async fn list(ctx: &Ctx) -> anyhow::Result<()> {
    ctx.require_project()?;
    let packets = local_store::list_handoffs_local(&ctx.cwd)?;
    if packets.is_empty() {
        println!("no handoffs recorded yet (local)");
        return Ok(());
    }
    println!(
        "{:<6} {:<22} {:<12} {:<12} PHASE",
        "SEQ", "CREATED", "FROM", "TO"
    );
    for packet in packets {
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
        println!("{seq:<6} {created:<22} {from:<12} {to:<12} {phase}");
    }
    Ok(())
}

/// `stateroot handoff show [seq]`.
pub async fn show(ctx: &Ctx, seq: Option<i64>) -> anyhow::Result<()> {
    ctx.require_project()?;
    match seq {
        None => {
            let (packet, source) = fetch_handoff(&ctx.cwd);
            match packet {
                Some(packet) => {
                    note!("(source: {source})");
                    print!("{}", render_handoff_digest(&packet));
                    Ok(())
                }
                None => anyhow::bail!("no current handoff found"),
            }
        }
        Some(seq) => {
            // P1 REST exposes only the *current* handoff packet; older packets
            // are available from the local history directory.
            let history = local_store::list_handoffs_local(&ctx.cwd)?;
            for packet in history {
                if packet.get("seq").and_then(|v| v.as_i64()) == Some(seq) {
                    print!("{}", render_handoff_digest(&packet));
                    return Ok(());
                }
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
    use stateroot_core::transcripts::{PlanStep, TailEntry};

    #[test]
    fn bounds_truncate_summary_items_lists_and_files() {
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
        packet = bound_packet(packet);

        let task = packet["task"].as_str().expect("task");
        assert!(
            task.chars().count() <= 3001,
            "task len {}",
            task.chars().count()
        );
        let summary = packet["context_summary"].as_str().expect("summary");
        assert!(summary.chars().count() <= SUMMARY_MAX + 1);
        assert_eq!(summary.chars().count(), SUMMARY_MAX);

        let actions = packet["next_actions"].as_array().expect("arr");
        assert_eq!(actions.len(), 20);
        let bugs = packet["bugs_found"].as_array().expect("arr");
        let bug = bugs[0].as_str().expect("bug");
        assert!(
            bug.chars().count() <= 1501,
            "bug len {}",
            bug.chars().count()
        );
        let files = packet["changed_files"].as_array().expect("arr");
        assert!(files.len() <= 512);
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
    fn task_falls_back_to_first_noncompleted_plan_item_and_tail_is_role_capped() {
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
                project_id: "project",
                seq: 1,
                source: "codex",
                to: "codex",
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
}
