//! `stateroot hook <event> --harness <id>` — the single entry point every
//! harness lifecycle hook calls.
//!
//! Rules of engagement (plan P1.2): hooks must NEVER break the harness —
//! unknown harness/event/project exits 0 silently. Event normalization uses
//! the registry's per-harness vocabulary; behavior per event kind:
//!
//! - resume (`session_start`, and `user_prompt_submit` only for kimi-code):
//!   print the bounded hook digest (local reads only — persona cache, local
//!   handoff actionables-first, hot-apex excerpt, "do NOT re-fetch" footer).
//!   Output shape follows the harness's injection channel (`hookSpecificOutput`
//!   JSON envelope, plain text, UserPromptSubmit-only, or nothing for MCP-pull
//!   harnesses). `user_prompt_submit` is capture-only for most harnesses (W3
//!   correction channel); kimi-code also resumes on prompt-submit because
//!   SessionStart stdout is discarded.
//! - capture (`post_tool_use`, `notification`, `subagent_*`, `pre_tool_use`):
//!   append one observation verbatim to `.stateroot/spool/observations.jsonl`
//!   (256KB rotation), fire-and-forget.
//! - checkpoint (`tool_failure`, `pre_compact`, `post_compaction`, `stop`,
//!   `session_end`): checkpoint from the spool tail via the existing
//!   machinery (offline → outbox); `stop`/`session_end` preserve any
//!   explicit structured handoff (automatic paths never rewrite
//!   `handoffs/current.json`), then rotate the spool.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use stateroot_core::harness_install::registry::{
    self, event_kind, normalize_event, quirk_any, EventKind, Injection,
};
use stateroot_core::local_store;
use stateroot_core::local_store::now_rfc3339;

use super::{note, truncate, Ctx};

/// Max digest size printed on resume events.
const DIGEST_BUDGET: usize = 4000;
/// Marker under `.stateroot/` — suppresses duplicate resume injection for the
/// same harness + handoff seq within a session.
const RESUME_DELIVERED_MARKER: &str = "hook-resume-delivered.json";
/// Spool rotation threshold.
const SPOOL_ROTATE_BYTES: u64 = 256 * 1024;
/// Bytes kept after rotation (tail of the spool).
const SPOOL_KEEP_BYTES: usize = 128 * 1024;
/// Observations included in a checkpoint note.
const CHECKPOINT_TAIL: usize = 10;

fn spool_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join("spool/observations.jsonl")
}

/// Walk up from `start` looking for `.stateroot/manifest.json`.
fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if local_store::is_stateroot_dir(d) {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// Lenient stdin payload parse (tolerates empty or non-JSON input).
fn read_payload() -> Value {
    use std::io::Read;
    let mut text = String::new();
    if std::io::stdin().read_to_string(&mut text).is_err() {
        return json!({});
    }
    let text = text.trim();
    if text.is_empty() {
        return json!({});
    }
    serde_json::from_str(text).unwrap_or_else(|_| json!({"_raw": text}))
}

/// Run the hook. Always exits 0 on harness-facing paths.
pub async fn run(ctx: &Ctx, event: &str, harness: &str) -> anyhow::Result<u8> {
    let Some(quirk) = quirk_any(harness) else {
        return Ok(0);
    };
    let Some(canonical) = normalize_event(quirk, event) else {
        return Ok(0);
    };
    let Some(kind) = event_kind(canonical) else {
        return Ok(0);
    };

    // Project resolution: walk-up first, then the registry.
    let project_dir = find_project_root(&ctx.cwd).or_else(|| {
        ctx.current_project()
            .ok()
            .flatten()
            .and_then(|_| ctx.cwd.canonicalize().ok())
            .filter(|cwd| local_store::is_stateroot_dir(cwd))
    });
    let Some(project_dir) = project_dir else {
        return Ok(0); // unknown project — silent
    };
    let payload = read_payload();

    match kind {
        EventKind::Resume => resume_output(ctx, quirk, canonical, &project_dir).await,
        EventKind::Capture => {
            let code = capture_observation(quirk, canonical, &project_dir, &payload)?;
            // kimi-code: SessionStart stdout is discarded — prompt-submit is the
            // only resume injection channel for that harness.
            if canonical == "user_prompt_submit" && quirk.injection == Injection::UserPromptSubmit {
                resume_output(ctx, quirk, canonical, &project_dir).await?;
            }
            Ok(code)
        }
        EventKind::Checkpoint => {
            checkpoint_from_spool(ctx, quirk, canonical, &project_dir, &payload).await
        }
    }
}

// ---------------------------------------------------------------------
// resume
// ---------------------------------------------------------------------

/// Build the bounded hook digest (actionables-first) or `None` when there is
/// no handoff content worth injecting.
pub fn hook_digest(config_dir: &Path, project_dir: &Path) -> Option<String> {
    let handoff = local_store::read_handoff_local(project_dir)
        .ok()
        .flatten()?;
    let mut out = String::new();

    if let Some(persona) = super::persona::read_cache(config_dir) {
        for line in persona.lines().take(6) {
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }

    let get_str = |key: &str| handoff.get(key).and_then(|v| v.as_str()).unwrap_or("");
    let objective = get_str("objective");
    let phase = get_str("current_phase");
    if !objective.is_empty() {
        out.push_str(&format!("Objective: {objective}\n"));
    }
    if !phase.is_empty() {
        out.push_str(&format!("Phase: {phase}\n"));
    }
    for (key, title) in [
        ("next_actions", "Next actions"),
        ("open_questions", "Open questions"),
        ("bugs_found", "Failed approaches / bugs"),
        ("blockers", "Blockers"),
        ("warnings", "Warnings"),
    ] {
        if let Some(items) = handoff.get(key).and_then(|v| v.as_array()) {
            if !items.is_empty() {
                out.push_str(&format!("{title}:\n"));
                for item in items.iter().take(6) {
                    let text = match item {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    out.push_str(&format!("- {}\n", truncate(&text, 180)));
                }
            }
        }
    }
    let summary = get_str("context_summary");
    if !summary.is_empty() {
        out.push_str(&format!("Summary: {}\n", truncate(summary, 300)));
    }

    // Project memory remains local to the project.
    for rel in [local_store::MEMORY_CORE_PATH] {
        if let Ok(text) = std::fs::read_to_string(local_store::root(project_dir).join(rel)) {
            let text = text.trim();
            if !text.is_empty() {
                out.push_str(&format!("\n(apex {rel})\n{}\n", truncate(text, 400)));
            }
        }
    }
    if let Ok(home) = stateroot_core::harness_install::home_dir() {
        if let Some(text) = stateroot_core::user_profile::read(&home) {
            out.push_str(&format!(
                "\n(apex user/USER.md)\n{}\n",
                truncate(&text, 400)
            ));
        }
    }

    out.push_str(&format!("\n{}", super::resume::NO_REFETCH_FOOTER));
    let digest = out.trim().to_string();
    if digest.is_empty() {
        None
    } else {
        Some(truncate(&digest, DIGEST_BUDGET))
    }
}

fn resume_marker_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(RESUME_DELIVERED_MARKER)
}

fn handoff_seq(project_dir: &Path) -> Option<i64> {
    local_store::read_handoff_local(project_dir)
        .ok()
        .flatten()
        .and_then(|handoff| handoff.get("seq").and_then(|v| v.as_i64()))
}

fn resume_already_delivered(project_dir: &Path, harness: &str, seq: i64) -> bool {
    let path = resume_marker_path(project_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
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

async fn resume_output(
    ctx: &Ctx,
    quirk: &registry::HarnessQuirk,
    canonical: &str,
    project_dir: &Path,
) -> anyhow::Result<u8> {
    // W5: session_start queues a draft heartbeat for the server root model.
    // Fire-and-forget — replayed (best-effort) by the next online command.
    if canonical == "session_start" {
        if let Some(project_id) = manifest_project_id(project_dir) {
            let op = json!({
                "ts": now_rfc3339(),
                "kind": "heartbeat",
                "project_id": project_id,
                "harness": quirk.id,
            });
            if let Err(err) = local_store::outbox_append(project_dir, &op) {
                note!("warning: could not queue heartbeat op: {err}");
            }
        }
        // Session-boundary skill federation sync (best-effort, never fails the hook).
        let options = stateroot_core::skill_federation::SyncOptions {
            dry_run: false,
            push: false,
            pull: true,
            cmd_probe: None,
        };
        if let Err(err) =
            stateroot_core::skill_federation::sync_project(project_dir, &options, None)
        {
            note!("warning: skill federation sync skipped: {err}");
        }
        // Session-boundary MCP federation (pull + project; never fails the hook).
        let mcp_options = stateroot_core::mcp_federation::SyncOptions {
            dry_run: false,
            pull: true,
            push: true,
            cmd_probe: None,
        };
        if let Err(err) =
            stateroot_core::mcp_federation::sync(None, Some(project_dir), &mcp_options)
        {
            note!("warning: MCP federation sync skipped: {err}");
        }
    }
    let digest = hook_digest(&ctx.config_dir, project_dir);
    let Some(digest) = digest else {
        return Ok(0); // nothing worth injecting — silent
    };
    if let Some(seq) = handoff_seq(project_dir) {
        if resume_already_delivered(project_dir, quirk.id, seq) {
            return Ok(0);
        }
    }
    match quirk.injection {
        Injection::None | Injection::McpPull => Ok(0),
        Injection::UserPromptSubmit => {
            // kimi-code: SessionStart stdout is discarded — only prompt-submit
            // carries context into the model.
            if canonical == "user_prompt_submit" {
                println!("{digest}");
                if let Some(seq) = handoff_seq(project_dir) {
                    mark_resume_delivered(project_dir, quirk.id, seq);
                }
            }
            Ok(0)
        }
        Injection::StdoutText => {
            println!("{digest}");
            if let Some(seq) = handoff_seq(project_dir) {
                mark_resume_delivered(project_dir, quirk.id, seq);
            }
            Ok(0)
        }
        Injection::StdoutJson => {
            let event_name = if canonical == "session_start" {
                "SessionStart"
            } else {
                "UserPromptSubmit"
            };
            let envelope = json!({
                "hookSpecificOutput": {
                    "hookEventName": event_name,
                    "additionalContext": digest,
                }
            });
            println!("{envelope}");
            if let Some(seq) = handoff_seq(project_dir) {
                mark_resume_delivered(project_dir, quirk.id, seq);
            }
            Ok(0)
        }
    }
}

// ---------------------------------------------------------------------
// capture
// ---------------------------------------------------------------------

fn capture_observation(
    quirk: &registry::HarnessQuirk,
    canonical: &str,
    project_dir: &Path,
    payload: &Value,
) -> anyhow::Result<u8> {
    let text = payload_text(payload);
    let kind_hint = infer_kind_hint(canonical, &text);
    let tool = payload_tool(payload);
    let excerpt = payload_excerpt(payload, &text);
    let obs_payload = observation_payload(
        quirk.id,
        canonical,
        &text,
        kind_hint,
        tool.clone(),
        excerpt.clone(),
    );
    let record = json!({
        "ts": now_rfc3339(),
        "event": canonical,
        "harness": quirk.id,
        "text": text,
        "kind_hint": kind_hint,
        "tool": tool,
        "excerpt": excerpt,
    });
    let path = spool_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Rotation: over threshold → keep the last KEEP bytes.
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > SPOOL_ROTATE_BYTES {
            let bytes = std::fs::read(&path).unwrap_or_default();
            let start = bytes.len().saturating_sub(SPOOL_KEEP_BYTES);
            let start = (start..=bytes.len())
                .find(|&i| i == bytes.len() || bytes[i] == b'\n')
                .map(|i| (i + 1).min(bytes.len()))
                .unwrap_or(bytes.len());
            std::fs::write(&path, &bytes[start.min(bytes.len())..])?;
        }
    }
    let mut line = serde_json::to_string(&record)?;
    line.push('\n');
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(line.as_bytes())?;

    // W5: also queue the observation for the server root model. The hook
    // itself never calls the server (<50ms local rule); the next online
    // command replays the outbox in one batch. Stable source_id makes the
    // at-least-once replay effectively-once server-side.
    if let Some(project_id) = manifest_project_id(project_dir) {
        let kind = kind_hint.unwrap_or(canonical);
        let op = json!({
            "ts": now_rfc3339(),
            "kind": "observation",
            "project_id": project_id,
            "observation": {
                "source": "hook",
                "source_id": format!("hook:{project_id}:{}", uuid::Uuid::now_v7()),
                "kind": kind,
                "payload": obs_payload,
                "harness": quirk.id,
            },
        });
        if let Err(err) = local_store::outbox_append(project_dir, &op) {
            note!("warning: could not queue observation op: {err}");
        }
    }
    Ok(0)
}

/// Project id from `.stateroot/manifest.json` (None when unreadable).
fn manifest_project_id(project_dir: &Path) -> Option<String> {
    let manifest = local_store::read_manifest(project_dir).ok().flatten()?;
    let project_id = manifest
        .get("project_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if project_id.is_empty() {
        None
    } else {
        Some(project_id)
    }
}

fn payload_text(payload: &Value) -> String {
    for key in [
        "prompt", "text", "message", "content", "summary", "note", "_raw",
    ] {
        if let Some(value) = payload.get(key).and_then(|v| v.as_str()) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return truncate(trimmed, 1000);
            }
        }
    }
    let raw = serde_json::to_string(payload).unwrap_or_default();
    truncate(&raw, 1000)
}

fn payload_tool(payload: &Value) -> Option<String> {
    for key in ["tool", "tool_name", "name", "command"] {
        if let Some(value) = payload.get(key).and_then(|v| v.as_str()) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(truncate(trimmed, 64));
            }
        }
    }
    None
}

fn payload_excerpt(payload: &Value, text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    if payload.get("_raw").is_some()
        || payload.get("prompt").is_some()
        || payload.get("text").is_some()
    {
        return Some(truncate(text, 240));
    }
    Some(truncate(text, 240))
}

fn infer_kind_hint(canonical: &str, text: &str) -> Option<&'static str> {
    match canonical {
        "user_prompt_submit" => {
            let lower = text.to_ascii_lowercase();
            if [
                "actually", "don't", "dont ", "do not", "instead", "not that", "stop ", "wrong",
            ]
            .iter()
            .any(|marker| lower.contains(marker))
            {
                Some("correction")
            } else {
                None
            }
        }
        _ => None,
    }
}

fn observation_payload(
    harness: &str,
    event: &str,
    text: &str,
    kind_hint: Option<&str>,
    tool: Option<String>,
    excerpt: Option<String>,
) -> Value {
    let mut payload = json!({
        "text": text,
        "harness": harness,
        "event": event,
    });
    if let Some(hint) = kind_hint {
        payload["kind_hint"] = json!(hint);
    }
    if let Some(tool) = tool {
        payload["tool"] = json!(tool);
    }
    if let Some(excerpt) = excerpt {
        payload["excerpt"] = json!(excerpt);
    }
    payload
}

// ---------------------------------------------------------------------
// checkpoint
// ---------------------------------------------------------------------

async fn checkpoint_from_spool(
    ctx: &Ctx,
    quirk: &registry::HarnessQuirk,
    canonical: &str,
    project_dir: &Path,
    payload: &Value,
) -> anyhow::Result<u8> {
    let tail = spool_tail(project_dir, CHECKPOINT_TAIL);
    let mut note_text = format!("{canonical} via {} hook", quirk.id);
    if let Some(raw) = payload.get("_raw").and_then(|v| v.as_str()) {
        note_text.push_str(&format!(": {}", truncate(raw, 200)));
    }
    if !tail.is_empty() {
        note_text.push_str(&format!("\nobservations:\n{}", tail.join("\n")));
    }
    let note_text = truncate(&note_text, 2000);

    let hook_ctx = Ctx {
        cwd: project_dir.to_path_buf(),
        config_dir: ctx.config_dir.clone(),
        config: ctx.config.clone(),
    };
    let projected = super::checkpoint::record_checkpoint(&hook_ctx, &note_text, &[]).await?;
    if !projected {
        note!("hook checkpoint queued to outbox (offline)");
    }

    // B2: where the harness injects hook stdout into the post-compaction
    // context (registry `compact_injection`), pre/post-compaction ALSO print
    // the bounded hook digest — state re-injected at the moment of
    // compaction. Unsupported harnesses: checkpoint only.
    if quirk.compact_injection && matches!(canonical, "pre_compact" | "post_compaction") {
        if let Some(digest) = hook_digest(&hook_ctx.config_dir, project_dir) {
            match quirk.injection {
                Injection::StdoutJson => {
                    let event_name = if canonical == "pre_compact" {
                        "PreCompact"
                    } else {
                        "PostCompact"
                    };
                    let envelope = json!({
                        "hookSpecificOutput": {
                            "hookEventName": event_name,
                            "additionalContext": digest,
                        }
                    });
                    println!("{envelope}");
                }
                Injection::StdoutText | Injection::UserPromptSubmit => {
                    println!("{digest}");
                }
                Injection::None | Injection::McpPull => {}
            }
        }
    }

    // stop/session_end: checkpoint already recorded above. Never replace an
    // explicit structured handoff with a thin lifecycle note.
    if matches!(canonical, "stop" | "session_end") && !tail.is_empty() {
        note!("checkpoint recorded; existing structured handoff preserved");
    }
    if matches!(canonical, "stop" | "session_end") {
        let path = spool_path(project_dir);
        if path.exists() {
            let _ = std::fs::write(&path, "");
        }
    }
    Ok(0)
}

fn spool_tail(project_dir: &Path, count: usize) -> Vec<String> {
    let path = spool_path(project_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines
        .iter()
        .rev()
        .take(count)
        .rev()
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|v| {
                    v.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                })
                .map(|text| format!("- {}", truncate(&text, 160)))
                .unwrap_or_else(|| format!("- {}", truncate(line, 160)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_actionables_first_with_footer() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let root = local_store::root(project.path());
        std::fs::create_dir_all(root.join("handoffs")).expect("mkdir");
        std::fs::create_dir_all(root.join("memories")).expect("mkdir");
        let packet = json!({
            "schema_version": "stateroot.handoff.v1",
            "objective": "ship the hooks",
            "current_phase": "build",
            "next_actions": ["wire the installers", "test them"],
            "bugs_found": ["json merge clobbered foreign keys"],
            "context_summary": "mostly done",
        });
        std::fs::write(
            root.join("handoffs/current.json"),
            serde_json::to_string_pretty(&packet).expect("json"),
        )
        .expect("write");
        std::fs::write(root.join("memories/MEMORY.md"), "# Memory\n\napex fact\n").expect("apex");
        std::fs::write(
            home.path().join("persona.md"),
            "## Persona\n\nYou are YinYue.\n",
        )
        .expect("persona");

        let digest = hook_digest(home.path(), project.path()).expect("digest");
        assert!(digest.contains("You are YinYue."));
        assert!(digest.contains("ship the hooks"));
        assert!(digest.contains("- wire the installers"));
        assert!(digest.contains("- json merge clobbered foreign keys"));
        assert!(digest.contains("apex fact"));
        assert!(digest.contains("do NOT re-fetch"));
        // Actionables come before the summary.
        let actions_at = digest.find("Next actions").expect("actions");
        let summary_at = digest.find("Summary:").expect("summary");
        assert!(actions_at < summary_at);
    }

    #[test]
    fn digest_is_none_without_handoff() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        assert!(hook_digest(home.path(), project.path()).is_none());
    }
}
