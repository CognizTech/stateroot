//! `stateroot handoff write|list|show`.

use serde_json::{json, Value};
use stateroot_core::local_store::now_rfc3339;
use stateroot_core::local_store::{self, SCHEMA_HANDOFF_V1};

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

/// Canonical harness ids accepted by the server's `HarnessId` Literal
/// (`app/core/stateroot/schema.py`). Server-validated packet fields are
/// normalized against this set. Keep in sync with
/// `contracts/stateroot_harness_registry.v1.json`.
const CANONICAL_HARNESSES: &[&str] = &[
    "planner",
    "statesmith",
    "cursor",
    "codex",
    "claude",
    "kimi",
    "opencode",
    "hermes",
    "openclaw",
    "gemini",
    "pi",
    "devin",
    "crush",
    "grok",
    "amp",
    "factory",
    "trae",
    "windsurf",
    "vs_code",
    "github_copilot",
    "zero",
    "antigravity",
    "omp",
];

/// Generic actor id for CLI-originated writes ("cli" is not in the server Literal).
const SERVER_ACTOR: &str = "statesmith";

/// Handoff quality bounds at write (plan P4.1).
const SUMMARY_MAX: usize = 3000;
const ITEM_MAX: usize = 1500;
const LIST_ITEMS_MAX: usize = 20;
const LIST_TOTAL_MAX: usize = 6000;
const FILES_ITEMS_MAX: usize = 512;
const FILES_TOTAL_MAX: usize = 4000;

/// Apply the quality bounds to a packet in place, warning on stderr about
/// everything truncated. Returns the packet for chaining.
fn bound_packet(mut packet: Value) -> Value {
    for key in ["task", "context_summary"] {
        if let Some(text) = packet.get(key).and_then(|v| v.as_str()) {
            if text.chars().count() > SUMMARY_MAX {
                note!("warning: {key} exceeded {SUMMARY_MAX} chars — truncated");
                packet[key] = Value::String(truncate(text, SUMMARY_MAX));
            }
        }
    }
    let list_keys = [
        "decisions",
        "changed_files",
        "tests_run",
        "bugs_found",
        "blockers",
        "open_questions",
        "next_actions",
        "warnings",
        "relevant_memories",
        "relevant_skills",
        "artifacts",
        "traces",
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

/// Normalize a user-provided harness name for server-validated packet fields.
/// Returns `None` (with a warning) when the name is not in the canonical set.
fn canonical_harness(input: &str) -> Option<String> {
    let lowered = input.trim().to_lowercase();
    if CANONICAL_HARNESSES.contains(&lowered.as_str()) {
        return Some(lowered);
    }
    note!(
        "warning: harness '{input}' is not in the server canonical set ({}); recording recommended_next_harness as null",
        CANONICAL_HARNESSES.join(", ")
    );
    None
}

/// Build a `stateroot.handoff.v1` packet.
fn build_packet(
    project_id: &str,
    seq: i64,
    to: Option<String>,
    note_text: Option<&str>,
    objective: &str,
    phase: &str,
) -> Value {
    json!({
        "schema_version": SCHEMA_HANDOFF_V1,
        "project_id": project_id,
        "seq": seq,
        "task": note_text.unwrap_or(""),
        "current_phase": phase,
        "last_harness": SERVER_ACTOR,
        "recommended_next_harness": to,
        "objective": objective,
        "implementation_status": "",
        "decisions": [],
        "changed_files": [],
        "tests_run": [],
        "bugs_found": [],
        "blockers": [],
        "open_questions": [],
        "next_actions": [],
        "warnings": [],
        "relevant_memories": [],
        "relevant_skills": [],
        "artifacts": [],
        "traces": [],
        "context_summary": note_text.unwrap_or(""),
        "created_at": now_rfc3339(),
        "written_at": now_rfc3339(),
        "created_by_harness": SERVER_ACTOR,
    })
}

/// Read the project objective/phase from local state (cheap, offline-safe).
fn local_state_fields(cwd: &std::path::Path) -> (String, String) {
    let path = local_store::root(cwd).join(local_store::STATE_PATH);
    let Ok(text) = std::fs::read_to_string(path) else {
        return (String::new(), String::new());
    };
    let Ok(state) = serde_json::from_str::<Value>(&text) else {
        return (String::new(), String::new());
    };
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
    (objective, phase)
}

/// `stateroot handoff write --to H [--note …] [--objective …]`.
///
/// Explicit origin replaces the current structured handoff. Automatic origin
/// (lifecycle hooks) records a checkpoint only and preserves any existing
/// structured handoff.
pub async fn write(
    ctx: &Ctx,
    to: &str,
    note_text: Option<&str>,
    objective_override: Option<&str>,
) -> anyhow::Result<()> {
    write_with_origin(
        ctx,
        to,
        note_text,
        objective_override,
        HandoffOrigin::Explicit,
    )
    .await
}

/// Same as [`write`] with an explicit/automatic origin.
pub async fn write_with_origin(
    ctx: &Ctx,
    to: &str,
    note_text: Option<&str>,
    objective_override: Option<&str>,
    origin: HandoffOrigin,
) -> anyhow::Result<()> {
    if origin == HandoffOrigin::Automatic {
        return automatic_checkpoint_only(ctx, note_text).await;
    }

    let project = ctx.require_project()?;
    // Determine the next seq from the current handoff (local store).
    // Determine the next seq from the current handoff (server first).
    let (current, _) = fetch_handoff(&ctx.cwd);
    let current_seq = current
        .as_ref()
        .and_then(|p| p.get("seq"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    // `project/state.json` holds the objective recorded at init; nothing
    // refreshes it as work progresses, so an explicit restatement wins.
    let (state_objective, phase) = local_state_fields(&ctx.cwd);
    let objective = match objective_override.map(str::trim).filter(|s| !s.is_empty()) {
        Some(text) => text.to_string(),
        None => state_objective,
    };
    let packet = bound_packet(build_packet(
        &project.project_id,
        current_seq + 1,
        canonical_harness(to),
        note_text,
        &objective,
        &phase,
    ));

    local_store::write_handoff_local(&ctx.cwd, &packet)?;
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
    let harness = if by.trim().is_empty() || by == "cli" {
        "statesmith"
    } else {
        by
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
                "harness": harness,
                "event": "handoff_accept",
                "kind_hint": "selection",
                "explicit": true,
            },
            "harness": harness,
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

    #[test]
    fn bounds_truncate_summary_items_lists_and_files() {
        let mut packet = json!({
            "schema_version": "stateroot.handoff.v1",
            "project_id": "p",
            "seq": 1,
            "task": "x".repeat(4000),
            "context_summary": "y".repeat(3200),
            "next_actions": (0..25).map(|i| format!("action {i}")).collect::<Vec<_>>(),
            "bugs_found": ["z".repeat(2000)],
            "changed_files": (0..600).map(|i| format!("src/f{i}.rs")).collect::<Vec<_>>(),
            "created_at": "2026-07-18T00:00:00Z",
            "created_by_harness": "statesmith",
        });
        packet = bound_packet(packet);

        let task = packet["task"].as_str().expect("task");
        assert!(
            task.chars().count() <= 3001,
            "task len {}",
            task.chars().count()
        );
        let summary = packet["context_summary"].as_str().expect("summary");
        assert!(summary.chars().count() <= 3001);

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
}
