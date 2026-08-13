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
    if let Err(err) = super::active_harness::record(&project_dir, quirk.id) {
        note!("warning: could not record active harness: {err}");
    }
    let Some(canonical) = normalize_event(quirk, event) else {
        return Ok(0);
    };
    let Some(kind) = event_kind(canonical) else {
        return Ok(0);
    };
    let payload = read_payload();

    match kind {
        EventKind::Resume => resume_output(ctx, quirk, canonical, &project_dir, &payload).await,
        EventKind::Capture => {
            let code = capture_observation(quirk, canonical, &project_dir, &payload)?;
            // kimi-code: SessionStart stdout is discarded — prompt-submit is the
            // only resume injection channel for that harness.
            if canonical == "user_prompt_submit" && quirk.injection == Injection::UserPromptSubmit {
                resume_output(ctx, quirk, canonical, &project_dir, &payload).await?;
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

/// Build the identity prefix (full persona + full USER.md). Never budgeted.
fn hook_identity_prefix(
    config_dir: &Path,
    home: &Path,
    project_dir: &Path,
    harness_id: &str,
) -> String {
    let mut out = String::new();
    out.push_str(super::persona::IDENTITY_ACTIVATION);
    out.push_str("\n\n");
    if let Some(persona) =
        super::persona::resolve_in_project(config_dir, Some(project_dir), Some(harness_id))
    {
        out.push_str(persona.trim());
        out.push_str("\n\n");
    }
    if let Some(text) = stateroot_core::user_profile::read(home) {
        out.push_str("(apex user/USER.md)\n");
        out.push_str(text.trim());
        out.push('\n');
    }
    out
}

fn append_handoff_work(work: &mut String, handoff: &Value, project_dir: &Path) {
    let get_str = |key: &str| handoff.get(key).and_then(|v| v.as_str()).unwrap_or("");
    let objective = get_str("objective");
    let phase = get_str("current_phase");
    if !objective.is_empty() {
        work.push_str(&format!("Objective: {objective}\n"));
    }
    if !phase.is_empty() {
        work.push_str(&format!("Phase: {phase}\n"));
    }
    let mut failures = Vec::new();
    for key in ["failures", "bugs_found"] {
        if let Some(items) = handoff.get(key).and_then(|v| v.as_array()) {
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
        work.push_str("Failed approaches / bugs:\n");
        for text in &failures {
            work.push_str(&format!("- {text}\n"));
        }
    }
    for (key, title) in [
        ("next_actions", "Next actions"),
        ("open_questions", "Open questions"),
        ("blockers", "Blockers"),
        ("warnings", "Warnings"),
    ] {
        if let Some(items) = handoff.get(key).and_then(|v| v.as_array()) {
            if !items.is_empty() {
                work.push_str(&format!("{title}:\n"));
                for item in items {
                    let text = match item {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    work.push_str(&format!("- {text}\n"));
                }
            }
        }
    }
    let summary = get_str("context_summary");
    if !summary.is_empty() {
        work.push_str(&format!("Summary: {summary}\n"));
    }

    for rel in [local_store::MEMORY_CORE_PATH] {
        if let Ok(home) = stateroot_core::harness_install::home_dir() {
            if let Some(block) =
                stateroot_core::hot_apex::render_for_digest(project_dir, &home, "memory")
            {
                work.push_str(&format!("\n(apex {rel})\n{block}\n"));
                continue;
            }
        }
        if let Ok(text) = std::fs::read_to_string(local_store::root(project_dir).join(rel)) {
            let text = text.trim();
            if !text.is_empty() {
                work.push_str(&format!("\n(apex {rel})\n{text}\n"));
            }
        }
    }
    if let Ok(home) = stateroot_core::harness_install::home_dir() {
        if let Some(gap) =
            stateroot_core::handoff_continuity::overlay_for_handoff(&home, project_dir, handoff)
        {
            work.push_str(
                &stateroot_core::handoff_continuity::compose_since_handoff_overlay(
                    project_dir,
                    handoff,
                    &gap,
                ),
            );
            work.push('\n');
        }
    }
}

/// Build the full hook digest. Identity (persona + USER) and work body are
/// both uncapped — product-intent forbids char truncation on the compiler path.
pub fn hook_digest(config_dir: &Path, project_dir: &Path, harness_id: &str) -> Option<String> {
    let handoff = local_store::read_handoff_local(project_dir).ok().flatten();
    let mut work = String::new();
    if let Some(ref handoff) = handoff {
        append_handoff_work(&mut work, handoff, project_dir);
    }

    if !work.trim().is_empty() {
        work.push_str(&format!("\n{}", super::resume::NO_REFETCH_FOOTER));
    }
    let home = stateroot_core::harness_install::home_dir().ok();
    let identity = home
        .as_ref()
        .map(|home| hook_identity_prefix(config_dir, home, project_dir, harness_id))
        .unwrap_or_else(|| hook_identity_prefix(config_dir, config_dir, project_dir, harness_id));
    let learnings = home.as_ref().map(|home| {
        let status = stateroot_core::learnings::bootstrap_status(project_dir, home);
        stateroot_core::learnings::compose_instruction(&status)
    });
    let rules = home
        .as_ref()
        .map(|home| stateroot_core::rules::compose_section(project_dir, home));
    let work = work.trim().to_string();
    if identity.trim().is_empty() && work.is_empty() && learnings.is_none() && rules.is_none() {
        return None;
    }
    let mut digest = identity;
    if let Some(learnings) = learnings.filter(|text| !text.trim().is_empty()) {
        if !digest.is_empty() && !digest.ends_with('\n') {
            digest.push('\n');
        }
        digest.push_str(learnings.trim());
        digest.push('\n');
    }
    if let Some(rules) = rules.filter(|text| !text.trim().is_empty()) {
        if !digest.is_empty() && !digest.ends_with('\n') {
            digest.push('\n');
        }
        digest.push_str(rules.trim());
        digest.push('\n');
    }
    // Wiki catalog (index + recent log) — never page bodies.
    let wiki = stateroot_core::wiki::compose_digest_section(project_dir);
    if !wiki.trim().is_empty() {
        if !digest.is_empty() && !digest.ends_with('\n') {
            digest.push('\n');
        }
        digest.push_str(wiki.trim());
        digest.push('\n');
    }
    if !work.is_empty() {
        if !digest.is_empty() && !digest.ends_with('\n') {
            digest.push('\n');
        }
        digest.push_str(&work);
    }
    Some(digest.trim().to_string())
}

fn hook_event_name(canonical: &str) -> &'static str {
    match canonical {
        "session_start" => "SessionStart",
        "user_prompt_submit" => "UserPromptSubmit",
        "pre_compact" => "PreCompact",
        "post_compaction" => "PostCompact",
        _ => "SessionStart",
    }
}

fn print_hook_injection(quirk: &registry::HarnessQuirk, canonical: &str, digest: &str) {
    match quirk.injection {
        Injection::StdoutJson => {
            let envelope = json!({
                "hookSpecificOutput": {
                    "hookEventName": hook_event_name(canonical),
                    "additionalContext": digest,
                }
            });
            println!("{envelope}");
        }
        Injection::CursorJson => {
            let envelope = json!({ "additional_context": digest });
            println!("{envelope}");
        }
        Injection::StdoutText | Injection::UserPromptSubmit | Injection::McpPull => {
            println!("{digest}");
        }
        Injection::None => {}
    }
}

fn session_id_from_payload(payload: &Value) -> Option<String> {
    for key in ["session_id", "conversation_id", "generation_id"] {
        if let Some(id) = payload.get(key).and_then(|v| v.as_str()) {
            let id = id.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
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

fn resume_already_delivered(
    project_dir: &Path,
    harness: &str,
    seq: i64,
    session_id: Option<&str>,
) -> bool {
    let path = resume_marker_path(project_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(marker) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    if marker.get("harness").and_then(|v| v.as_str()) != Some(harness) {
        return false;
    }
    if let Some(session_id) = session_id {
        return marker.get("session_id").and_then(|v| v.as_str()) == Some(session_id);
    }
    marker.get("handoff_seq").and_then(|v| v.as_i64()) == Some(seq)
}

fn mark_resume_delivered(project_dir: &Path, harness: &str, seq: i64, session_id: Option<&str>) {
    let path = resume_marker_path(project_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut marker = json!({
        "harness": harness,
        "handoff_seq": seq,
        "delivered_at": now_rfc3339(),
    });
    if let Some(session_id) = session_id {
        marker["session_id"] = json!(session_id);
    }
    let _ = std::fs::write(path, serde_json::to_string(&marker).unwrap_or_default());
}

async fn resume_output(
    ctx: &Ctx,
    quirk: &registry::HarnessQuirk,
    canonical: &str,
    project_dir: &Path,
    payload: &Value,
) -> anyhow::Result<u8> {
    // W5: session_start queues a draft heartbeat for the server root model.
    // Fire-and-forget — replayed (best-effort) by the next online command.
    if canonical == "session_start" {
        if let Err(err) = stateroot_core::learnings::record_first_session(project_dir, quirk.id) {
            note!("warning: could not record first-run harness: {err}");
        }
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
        // Session-boundary rules federation (product-intent + harness imports).
        if let Ok(home) = stateroot_core::harness_install::home_dir() {
            if let Err(err) = stateroot_core::rules::sync(project_dir, &home) {
                note!("warning: rules sync skipped: {err}");
            }
        }
        // Dual-mode compiler: agentic when keyed/logged-in; never fails the hook.
        let hook_ctx = Ctx {
            cwd: project_dir.to_path_buf(),
            config_dir: ctx.config_dir.clone(),
            config: ctx.config.clone(),
        };
        match super::compiler::try_agentic(&hook_ctx, false).await {
            Ok(_) => {}
            Err(err) => note!("warning: compiler skipped: {err}"),
        }
    }
    let digest = hook_digest(&ctx.config_dir, project_dir, quirk.id);
    let Some(digest) = digest else {
        return Ok(0); // nothing worth injecting — silent
    };
    let session_id = session_id_from_payload(payload);
    if let Some(seq) = handoff_seq(project_dir) {
        if resume_already_delivered(project_dir, quirk.id, seq, session_id.as_deref()) {
            return Ok(0);
        }
    }
    match quirk.injection {
        Injection::None => Ok(0),
        Injection::McpPull => {
            // Plugins (OpenClaw, etc.) and MCP-pull adapters capture stdout.
            print_hook_injection(quirk, canonical, &digest);
            if let Some(seq) = handoff_seq(project_dir) {
                mark_resume_delivered(project_dir, quirk.id, seq, session_id.as_deref());
            }
            Ok(0)
        }
        Injection::UserPromptSubmit => {
            if canonical == "user_prompt_submit" {
                print_hook_injection(quirk, canonical, &digest);
                if let Some(seq) = handoff_seq(project_dir) {
                    mark_resume_delivered(project_dir, quirk.id, seq, session_id.as_deref());
                }
            }
            Ok(0)
        }
        Injection::StdoutJson | Injection::CursorJson | Injection::StdoutText => {
            print_hook_injection(quirk, canonical, &digest);
            if let Some(seq) = handoff_seq(project_dir) {
                mark_resume_delivered(project_dir, quirk.id, seq, session_id.as_deref());
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
    // Pre-compact: extract into wiki/memory BEFORE re-injecting the digest.
    if quirk.compact_injection && matches!(canonical, "pre_compact" | "post_compaction") {
        if canonical == "pre_compact" {
            let _ = super::compiler::try_ingest(&hook_ctx, false).await;
        }
        if let Some(digest) = hook_digest(&hook_ctx.config_dir, project_dir, quirk.id) {
            print_hook_injection(quirk, canonical, &digest);
        }
    }

    // stop/session_end: checkpoint already recorded above. Try transcript
    // finalize when gates pass; never clobber an explicit handoff at the
    // current seq.
    if matches!(canonical, "stop" | "session_end") {
        if super::handoff::try_auto_finalize(&hook_ctx, quirk.id).unwrap_or(false) {
            note!("finalized observed session into handoff continuity");
        } else if !tail.is_empty() {
            note!("checkpoint recorded; existing structured handoff preserved");
        }
        // Compile mined notes into wiki inbox / pages (not into learnings).
        match super::compiler::try_ingest(&hook_ctx, false).await {
            Ok(summary) => note!("{summary}"),
            Err(err) => note!("ingest skipped: {err}"),
        }
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
    use std::sync::Mutex;

    static TEST_HOME_ENV: Mutex<()> = Mutex::new(());

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

        let digest = hook_digest(home.path(), project.path(), "codex").expect("digest");
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
    fn digest_seeds_global_and_project_learnings_on_first_run() {
        let _guard = TEST_HOME_ENV.lock().expect("env lock");
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        local_store::init_skeleton(project.path(), "p1", "demo", "default").expect("init");
        std::fs::write(home.path().join("persona.md"), "You are YinYue.\n").expect("persona");
        let prior = std::env::var("STATEROOT_TEST_HOME").ok();
        unsafe { std::env::set_var("STATEROOT_TEST_HOME", home.path()) };
        let digest = hook_digest(home.path(), project.path(), "cursor").expect("digest");
        match prior {
            Some(value) => unsafe { std::env::set_var("STATEROOT_TEST_HOME", value) },
            None => unsafe { std::env::remove_var("STATEROOT_TEST_HOME") },
        }
        assert!(digest.contains("first harness"), "{digest}");
        assert!(
            digest.contains("Global (user) learnings are empty"),
            "{digest}"
        );
        assert!(digest.contains("Project learnings are empty"), "{digest}");
        assert!(digest.contains("learn record --user"), "{digest}");
    }

    #[test]
    fn digest_emits_identity_without_handoff() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        std::fs::write(
            home.path().join("persona.md"),
            "## Working relationship\n\nYou are Yinyue.\n",
        )
        .expect("persona");
        let digest = hook_digest(home.path(), project.path(), "cursor").expect("digest");
        assert!(digest.contains("Active identity"));
        assert!(digest.contains("You are Yinyue."));
        assert!(!digest.contains("Objective:"));
    }

    #[test]
    fn cursor_registry_uses_native_json_injection() {
        let quirk = registry::quirk("cursor").expect("cursor");
        assert_eq!(quirk.injection, Injection::CursorJson);
        assert_eq!(quirk.instruction_file, Some(".cursor/AGENTS.md"));
    }

    #[test]
    fn digest_includes_full_persona_and_user_without_truncation() {
        let _guard = TEST_HOME_ENV.lock().expect("env lock");
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let root = local_store::root(project.path());
        std::fs::create_dir_all(root.join("handoffs")).expect("mkdir");
        let packet = json!({
            "schema_version": "stateroot.handoff.v1",
            "objective": "ship",
            "context_summary": "continuity",
            "next_actions": ["continue"],
        });
        std::fs::write(
            root.join("handoffs/current.json"),
            serde_json::to_string_pretty(&packet).expect("json"),
        )
        .expect("write");

        let persona_lines: String = (0..20)
            .map(|i| format!("Persona voice line {i}: stay in character always"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(home.path().join("persona.md"), persona_lines).expect("persona");

        let long_user = format!("Fellow Daoist Han — {}", "x".repeat(500));
        std::fs::create_dir_all(home.path().join(".stateroot/user")).expect("user dir");
        std::fs::write(home.path().join(".stateroot/user/USER.md"), &long_user).expect("user");

        let prior = std::env::var("STATEROOT_TEST_HOME").ok();
        // SAFETY: serialized by TEST_HOME_ENV.
        unsafe { std::env::set_var("STATEROOT_TEST_HOME", home.path()) };
        let digest = hook_digest(home.path(), project.path(), "codex").expect("digest");
        match prior {
            Some(value) => unsafe { std::env::set_var("STATEROOT_TEST_HOME", value) },
            None => unsafe { std::env::remove_var("STATEROOT_TEST_HOME") },
        }

        assert!(digest.contains("Persona voice line 19: stay in character always"));
        assert!(digest.contains(&long_user));
        assert!(
            hook_identity_prefix(home.path(), home.path(), project.path(), "cursor")
                .contains(&long_user)
        );
    }

    #[test]
    fn digest_keeps_full_work_body_uncapped() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let root = local_store::root(project.path());
        std::fs::create_dir_all(root.join("handoffs")).expect("mkdir");
        let huge_summary = "W".repeat(5000);
        let packet = json!({
            "schema_version": "stateroot.handoff.v1",
            "objective": "ship",
            "context_summary": huge_summary,
            "next_actions": ["continue"],
        });
        std::fs::write(
            root.join("handoffs/current.json"),
            serde_json::to_string_pretty(&packet).expect("json"),
        )
        .expect("write");
        std::fs::write(
            home.path().join("persona.md"),
            "Identity marker line must survive\n",
        )
        .expect("persona");

        let digest = hook_digest(home.path(), project.path(), "codex").expect("digest");
        assert!(digest.contains("Identity marker line must survive"));
        assert!(
            digest.contains(&"W".repeat(5000)),
            "work body must not be char-truncated: {}",
            digest.len()
        );
    }
}
