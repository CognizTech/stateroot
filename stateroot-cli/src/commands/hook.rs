//! `stateroot hook <event> --harness <id>` — the single entry point every
//! harness lifecycle hook calls.
//!
//! Rules of engagement (plan P1.2): hooks must NEVER break the harness —
//! unknown harness/event exits 0 silently. Capture/checkpoint still no-op
//! outside a `.stateroot/` project. Resume still injects **identity**
//! (persona + USER.md) with no project — working identity is global.
//! Event normalization uses the registry's per-harness vocabulary; behavior
//! per event kind:
//!
//! - resume (`session_start`, plus `user_prompt_submit` when the harness
//!   policy says so): print the hook digest (persona + USER.md + work body).
//!   Outside a project, print identity only. First-prompt fallback injects
//!   when session-start stdout was discarded or never marked delivered.
//!   Output shape follows the harness's injection channel.
//! - capture (`post_tool_use`, `notification`, `subagent_*`, `pre_tool_use`):
//!   durably append one observation to the session's immutable segment under
//!   `.stateroot/spool/segments/` (locked append + sync before ack; dedup by
//!   verified native event identity; full body + raw payload retained unless
//!   an explicitly declared file-tool capture exclusion keeps them local).
//! - checkpoint (`tool_failure`, `pre_compact`, `post_compaction`, `stop`,
//!   `session_end`): checkpoint from the observation tail into the local
//!   episodic log; `stop`/`session_end` enqueue transcript finalize + snap +
//!   ingest onto the outbox and return (a detached `_drain-finalize` worker
//!   finishes the heavy work, including post-ingest segment sealing). Stop
//!   never scans or seals segments synchronously — stop latency stays
//!   bounded on long segments. Evidence is never rotated, cleared, or
//!   truncated.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use stateroot_core::digest_delivery::{self, DeliveryChannel, DeliveryIntent};
use stateroot_core::harness_install::registry::{
    self, event_kind, normalize_event, quirk_any, EventKind, Injection,
};
use stateroot_core::local_store;
use stateroot_core::local_store::now_rfc3339;

use super::{note, truncate, Ctx};

/// Observations included in a checkpoint note.
const CHECKPOINT_TAIL: usize = 10;

/// Walk up from `start` looking for `.stateroot/manifest.json`.
fn find_project_root(start: &Path) -> Option<PathBuf> {
    local_store::find_project_root(start)
}

/// The agent's working directory from a hook payload, when the harness sends
/// one: `cwd` / `project_dir` / `workspace_dir` string fields, or the first
/// `workspace_roots` entry (cursor's shape), or Antigravity's `workspacePaths`
/// array. Translates Windows ↔ WSL forms so a payload from the other OS still
/// attaches. Returns None on anything odd.
fn payload_project_dir(payload: &Value) -> Option<PathBuf> {
    for key in ["cwd", "project_dir", "projectDir", "workspace_dir"] {
        if let Some(dir) = payload.get(key).and_then(|v| v.as_str()) {
            if let Some(path) = stateroot_core::path_identity::resolve_existing_dir(Path::new(dir))
            {
                return Some(path);
            }
        }
    }
    if let Some(dir) = payload
        .get("workspace_roots")
        .and_then(|v| v.as_array())
        .and_then(|roots| roots.first())
        .and_then(|v| v.as_str())
        .map(Path::new)
        .and_then(stateroot_core::path_identity::resolve_existing_dir)
    {
        return Some(dir);
    }
    payload_workspace_paths(payload)
}

/// Antigravity payloads carry `workspacePaths`: every mounted workspace, any
/// of which may hold the project. Prefer the entry that resolves inside an
/// initialized project; fall back to the first existing directory so capture
/// still attaches to a real cwd outside any project.
fn payload_workspace_paths(payload: &Value) -> Option<PathBuf> {
    let dirs: Vec<PathBuf> = payload
        .get("workspacePaths")
        .and_then(|v| v.as_array())
        .map(|roots| {
            roots
                .iter()
                .filter_map(|v| v.as_str())
                .filter_map(|dir| {
                    stateroot_core::path_identity::resolve_existing_dir(Path::new(dir))
                })
                .collect()
        })
        .unwrap_or_default();
    dirs.iter()
        .find(|dir| find_project_root(dir).is_some())
        .or_else(|| dirs.first())
        .cloned()
}

/// Antigravity's PreInvocation fires before every model call and carries
/// `invocationNum` (0-indexed). Past the first invocation the event is
/// classified mid-turn — the host's model counter, not a session boundary.
/// SessionStart payloads carry no `invocationNum`, and legacy
/// SessionStart-less hosts open with `invocationNum: 0`: both are treated as
/// session starts. A non-numeric or negative value degrades to "treat as a
/// start" rather than dropping a genuine boundary. The classification only
/// feeds the state-based delivery gate in `run` — the counter itself never
/// suppresses delivery (StateRoot may first observe a conversation at any
/// invocation number).
fn preinvocation_mid_turn(payload: &Value) -> bool {
    payload
        .get("invocationNum")
        .and_then(Value::as_i64)
        .is_some_and(|n| n > 0)
}

/// Read-only preflight for a mid-turn PreInvocation (`invocationNum` > 0).
/// True only when this conversation is already settled: the persona-injection
/// scheduler recorded a completed first FULL with unchanged identity and no
/// armed compaction, so it could only decide Nothing. Anything else — a
/// conversation StateRoot never observed (late attach, resumed conversation),
/// changed identity, a pending compaction — returns false and falls through
/// to normal session-start handling, which delivers once and re-settles.
fn antigravity_mid_turn_settled(
    ctx: &Ctx,
    quirk: &registry::HarnessQuirk,
    project_dir: Option<&Path>,
    payload: &Value,
) -> bool {
    let home =
        stateroot_core::harness_install::home_dir().unwrap_or_else(|_| ctx.config_dir.clone());
    let identity = hook_identity_prefix(&ctx.config_dir, &home, project_dir, quirk.id);
    let hash = stateroot_core::persona_injection::content_hash(&identity);
    let key = format!(
        "{}:{}",
        quirk.id,
        stateroot_core::persona_injection::session_key(project_dir.unwrap_or(&ctx.cwd), payload)
    );
    let Some(state) = stateroot_core::persona_injection::load_state(&home, &key) else {
        return false;
    };
    !state.last_full_ts.is_empty()
        && !state.pending_compaction
        && (hash.is_empty() || hash == state.content_hash)
}

/// kimi-code IDE sessions report the extension host's cwd in hook payloads
/// (kimi_code_vscode), so project resolution from the payload lands nowhere.
/// The harness's session index maps sessionId → workDir — resolve the true
/// project from it. Terminal kimi sessions already send the project cwd.
fn kimi_session_project(quirk: &registry::HarnessQuirk, payload: &Value) -> Option<PathBuf> {
    if quirk.id != "kimi" && quirk.id != "kimi-code" {
        return None;
    }
    let home = stateroot_core::harness_install::home_dir().ok()?;
    let dir = stateroot_core::session_identity::kimi_session_workdir(&home, payload)?;
    find_project_root(&dir)
}

/// Hermes hook payloads carry the hermes PROCESS cwd (`Path.cwd()` of the
/// CLI or gateway — for a gateway that is its launch dir, not the agent's
/// project; `agent/shell_hooks.py` `_serialize_payload`). The configured
/// working directory rides the inherited `TERMINAL_CWD` env instead (the
/// gateway sets it; CLI/cron fall back to launch cwd — `agent/runtime_cwd.py`),
/// so for hermes it resolves BEFORE the payload cwd.
fn hermes_terminal_project(quirk: &registry::HarnessQuirk) -> Option<PathBuf> {
    if quirk.id != "hermes" {
        return None;
    }
    let raw = std::env::var_os("TERMINAL_CWD")?;
    if raw.to_str().map(str::trim).unwrap_or_default().is_empty() {
        return None;
    }
    let dir = stateroot_core::path_identity::resolve_existing_dir(Path::new(&raw))?;
    find_project_root(&dir)
}

/// A hermes `delegate_task` child is its own agent with its own session
/// (`tools/delegate_tool.py`: `platform="subagent"`, `parent_session_id`
/// stamped). The current serializer consumes `parent_session_id` into the
/// `session_id` fallback (so `platform` is the reliable marker); the docs
/// also list it as a payload field, so both placements count. A child must
/// never consume its parent's identity digest or finalize its parent's
/// session — its own evidence stays captured under its own native session id.
fn hermes_delegated_child(quirk: &registry::HarnessQuirk, payload: &Value) -> bool {
    if quirk.id != "hermes" {
        return false;
    }
    let non_empty_str = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let extra = payload.get("extra");
    if non_empty_str(extra.and_then(|e| e.get("platform"))) == "subagent" {
        return true;
    }
    !non_empty_str(payload.get("parent_session_id")).is_empty()
        || !non_empty_str(extra.and_then(|e| e.get("parent_session_id"))).is_empty()
}

/// Part types hermes itself drops when it flattens message content
/// (`agent/message_content.py`); text is read from these keys, in order.
const HERMES_NON_TEXT_PART_TYPES: [&str; 5] =
    ["image", "image_url", "input_image", "audio", "input_audio"];
const HERMES_TEXT_PART_KEYS: [&str; 5] = [
    "text",
    "content",
    "input_text",
    "output_text",
    "summary_text",
];

/// The visible text of a hermes `user_message`, flattened the way hermes
/// flattens message content: a string as-is, a list of content parts joined
/// by line with image/audio parts dropped. Empty means a non-text turn.
fn hermes_prompt_text(message: &Value) -> Option<String> {
    let text = match message {
        Value::Array(parts) => parts
            .iter()
            .filter_map(hermes_part_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        other => hermes_part_text(other)?.to_owned(),
    };
    (!text.trim().is_empty()).then_some(text)
}

fn hermes_part_text(part: &Value) -> Option<&str> {
    match part {
        Value::String(text) => Some(text),
        Value::Object(fields) => {
            let kind = fields
                .get("type")
                .and_then(Value::as_str)
                .map(|kind| kind.trim().to_ascii_lowercase());
            if kind.is_some_and(|kind| HERMES_NON_TEXT_PART_TYPES.contains(&kind.as_str())) {
                return None;
            }
            HERMES_TEXT_PART_KEYS
                .iter()
                .find_map(|key| fields.get(*key).and_then(Value::as_str))
        }
        _ => None,
    }
}

/// The turn-outcome suffix for a hermes on_session_end checkpoint note:
/// `failed`/`interrupted`/`turn_exit_reason` ride `extra`
/// (`agent/turn_finalizer.py`). Some only when the turn did not complete
/// cleanly — clean turns add no noise.
fn hermes_turn_outcome(quirk: &registry::HarnessQuirk, payload: &Value) -> Option<String> {
    if quirk.id != "hermes" {
        return None;
    }
    let extra = payload.get("extra").and_then(Value::as_object)?;
    let flag = |key: &str| extra.get(key).and_then(Value::as_bool).unwrap_or(false);
    let failed = flag("failed");
    let interrupted = flag("interrupted");
    let reason = extra
        .get("turn_exit_reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if !failed && !interrupted && reason.is_empty() {
        return None;
    }
    Some(format!(
        "outcome: failed={failed} interrupted={interrupted} reason={}",
        if reason.is_empty() { "n/a" } else { &reason }
    ))
}

/// Shape a hermes native payload before any consumer sees it. `pre_llm_call`
/// carries the turn's prompt in `extra.user_message` (string or content
/// parts) next to `extra.conversation_history` — the WHOLE conversation so
/// far. Keep the flattened prompt plus bounded routing/identity fields and
/// drop the history: capture must never spool an entire conversation per
/// turn. Other hermes payloads are bounded by contract (tool args/result,
/// outcome flags, child summaries) and pass through untouched.
fn shape_hermes_payload(canonical: &str, payload: &mut Value) {
    if canonical != "user_prompt_submit" || !payload.is_object() {
        return;
    }
    let mut shaped = serde_json::Map::new();
    for key in [
        "hook_event_name",
        "cwd",
        "session_id",
        "profile",
        "parent_session_id",
    ] {
        if let Some(value) = payload.get(key) {
            if !value.is_null() {
                shaped.insert(key.to_string(), value.clone());
            }
        }
    }
    if let Some(extra) = payload.get("extra").and_then(Value::as_object) {
        if let Some(prompt) = extra.get("user_message").and_then(hermes_prompt_text) {
            shaped.insert("prompt".to_string(), prompt.into());
        }
        let mut kept = serde_json::Map::new();
        for key in [
            "model",
            "platform",
            "is_first_turn",
            "turn_id",
            "task_id",
            "parent_session_id",
            "sender_id",
        ] {
            if let Some(value) = extra.get(key) {
                if !value.is_null() {
                    kept.insert(key.to_string(), value.clone());
                }
            }
        }
        if !kept.is_empty() {
            shaped.insert("extra".to_string(), Value::Object(kept));
        }
    }
    *payload = Value::Object(shaped);
}

/// Lenient stdin payload parse (tolerates empty or non-JSON input). A failed
/// stdin read is surfaced as `_transport_error` so capture never acknowledges
/// complete evidence it never received.
fn read_payload() -> Value {
    use std::io::Read;
    let mut text = String::new();
    if std::io::stdin().read_to_string(&mut text).is_err() {
        return json!({"_transport_error": "stdin read failed"});
    }
    let text = text.trim();
    if text.is_empty() {
        return json!({});
    }
    serde_json::from_str(text).unwrap_or_else(|_| json!({"_raw": text}))
}

/// Debug capture: STATEROOT_HOOK_DEBUG=1 — or the marker file
/// /tmp/stateroot-hook-debug.on, because IDE/ACP-spawned hook processes
/// inherit no shell env — appends every hook payload to
/// /tmp/stateroot-hook-payloads.jsonl (payload-shape forensics).
fn debug_dump_payload(event: &str, harness: &str, cwd: &Path, payload: &Value) {
    let armed = std::env::var_os("STATEROOT_HOOK_DEBUG").is_some()
        || Path::new("/tmp/stateroot-hook-debug.on").exists();
    if !armed {
        return;
    }
    let line = serde_json::json!({
        "event": event,
        "harness": harness,
        "cwd": cwd.display().to_string(),
        "env": {
            "HOME": std::env::var_os("HOME").is_some(),
            "USERPROFILE": std::env::var_os("USERPROFILE").is_some(),
            "STATEROOT_HOME": std::env::var_os("STATEROOT_HOME").is_some(),
            "STATEROOT_TEST_HOME": std::env::var_os("STATEROOT_TEST_HOME").is_some(),
        },
        "payload": payload,
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/stateroot-hook-payloads.jsonl")
    {
        use std::io::Write as _;
        let _ = writeln!(f, "{line}");
    }
}

/// Run the hook. Always exits 0 on harness-facing paths.
pub async fn run(ctx: &Ctx, event: &str, harness: &str) -> anyhow::Result<u8> {
    let Some(quirk) = quirk_any(harness) else {
        return Ok(0);
    };

    let mut payload = read_payload();
    debug_dump_payload(event, harness, &ctx.cwd, &payload);

    let Some(canonical) = normalize_event(quirk, event) else {
        return Ok(0);
    };

    // hermes: bound the native payload before any consumer sees it —
    // pre_llm_call carries the whole conversation in extra.conversation_history;
    // only the flattened prompt and bounded routing fields survive.
    if quirk.id == "hermes" {
        shape_hermes_payload(canonical, &mut payload);
    }
    let delegated_child = hermes_delegated_child(quirk, &payload);
    // Antigravity lifecycle routing (native contract: PreInvocation fires
    // before EVERY model invocation, SessionStart fires once per
    // conversation; both register as `session_start` for legacy hosts):
    // - A PreInvocation payload carries `invocationNum`; anything past the
    //   first invocation is mid-turn, never a session boundary. The counter
    //   is the host's model counter, not StateRoot's delivery state, so it
    //   never suppresses delivery on its own: the state-based gate after
    //   project resolution stays silent only for a conversation whose
    //   identity was already delivered unchanged, while a first-seen
    //   conversation (late attach, resumed) or a changed/pending identity
    //   still delivers once. Ordinary repeats for a delivered conversation
    //   never re-accept a handoff (the scheduler and the delivery ledger
    //   dedupe per project+session).
    // - PostToolUse carries an `error` string (empty on success): failures
    //   route to the checkpoint lane for an immediate failure checkpoint, and
    //   the checkpoint arm first captures the full payload durably (the
    //   checkpoint alone would only label the failure over the prior tail).
    let canonical = if quirk.id == "antigravity"
        && canonical == "post_tool_use"
        && payload
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| !error.trim().is_empty())
    {
        "tool_failure"
    } else {
        canonical
    };

    // Harnesses whose hook payloads carry no conversation id (IDE/ACP
    // adapters) get a StateRoot-managed session id, rotated per real
    // conversation — every downstream consumer sees a true per-session id.
    if let Ok(home) = stateroot_core::harness_install::home_dir() {
        stateroot_core::session_identity::tag_payload(
            &home,
            quirk.id,
            canonical,
            &ctx.cwd,
            &mut payload,
        );
    }

    // Project resolution: the event payload's cwd/workspace first (gateway
    // daemons and IDEs run hooks with THEIR cwd, not the agent's project —
    // the openclaw gateway bug), then walk-up from our process cwd, then the
    // registry. hermes is the reverse case: its payload cwd IS the process
    // cwd (a gateway's launch dir), so the inherited TERMINAL_CWD env — the
    // harness's configured working directory — resolves first.
    let payload_root = payload_project_dir(&payload).and_then(|d| find_project_root(&d));
    let kimi_root = || kimi_session_project(quirk, &payload);
    let project_dir = hermes_terminal_project(quirk).or_else(|| {
        payload_root.or_else(|| {
            kimi_root().or_else(|| {
                find_project_root(&ctx.cwd).or_else(|| {
                    ctx.current_project()
                        .ok()
                        .flatten()
                        .and_then(|_| ctx.cwd.canonicalize().ok())
                        .filter(|cwd| local_store::is_stateroot_dir(cwd))
                })
            })
        })
    });
    let Some(kind) = event_kind(canonical) else {
        return Ok(0);
    };

    // Antigravity mid-turn PreInvocation: silent only when this conversation
    // already settled (first FULL delivered, identity unchanged, no armed
    // compaction). A conversation StateRoot first observes past invocation 0
    // — install/attach mid-conversation, or a resumed conversation the
    // ledger never saw — falls through and receives its one first delivery.
    if quirk.id == "antigravity"
        && canonical == "session_start"
        && preinvocation_mid_turn(&payload)
        && antigravity_mid_turn_settled(ctx, quirk, project_dir.as_deref(), &payload)
    {
        return Ok(0);
    }

    match kind {
        EventKind::Resume => match project_dir.as_ref() {
            Some(project_dir) => {
                // A hermes delegated child never consumes the parent's
                // identity digest or its session-boundary federation work.
                if delegated_child {
                    return Ok(0);
                }
                if let Err(err) = super::active_harness::record(project_dir, quirk.id) {
                    note!("warning: could not record active harness: {err}");
                }
                resume_output(ctx, quirk, canonical, project_dir, &payload).await
            }
            None if delegated_child => Ok(0),
            None => resume_identity_only(ctx, quirk, canonical, &payload),
        },
        EventKind::Capture => match project_dir.as_ref() {
            Some(project_dir) => {
                if let Err(err) = super::active_harness::record(project_dir, quirk.id) {
                    note!("warning: could not record active harness: {err}");
                }
                let code = capture_observation(quirk, canonical, project_dir, &payload)?;
                let prompt_injects = canonical == "user_prompt_submit"
                    && quirk.delivery().prompt_submit_injects
                    && !delegated_child;
                let cursor_reanchors = canonical == "post_tool_use"
                    && quirk.id == "cursor"
                    && cursor_post_tool_needs_identity(ctx, quirk, project_dir, &payload);
                if prompt_injects || cursor_reanchors {
                    resume_output(ctx, quirk, canonical, project_dir, &payload).await?;
                }
                Ok(code)
            }
            None if canonical == "user_prompt_submit"
                && quirk.delivery().prompt_submit_injects
                && !delegated_child =>
            {
                resume_identity_only(ctx, quirk, canonical, &payload)
            }
            None => Ok(0),
        },
        EventKind::Checkpoint => match project_dir.as_ref() {
            Some(project_dir) => {
                // A hermes delegated child's session_finalize is not the
                // parent's session teardown — retain the child's captured
                // evidence and drop the boundary semantics entirely.
                if delegated_child && canonical == "session_end" {
                    return Ok(0);
                }
                // Antigravity has no native tool_failure event: for that
                // harness the canonical name only arises from the failed-
                // PostToolUse remap above. Capture the full authorized
                // tool-result/error payload durably BEFORE the immediate
                // failure checkpoint — the checkpoint lane alone retained
                // only a label over the prior tail, never the payload.
                if quirk.id == "antigravity" && canonical == "tool_failure" {
                    capture_observation(quirk, canonical, project_dir, &payload)?;
                }
                let code =
                    checkpoint_from_spool(ctx, quirk, canonical, project_dir, &payload).await?;
                // Plan federation: session boundaries pull the firing
                // harness's native plans into the store (interval-gated).
                if let Ok(home) = stateroot_core::harness_install::home_dir() {
                    if let Some(report) = stateroot_core::plan_federation::maybe_auto(
                        &home,
                        project_dir,
                        quirk.id,
                        15,
                    ) {
                        for line in &report.ingested {
                            hook_note(quirk, &format!("plan sync: ingested {line}"));
                        }
                        for line in &report.updated {
                            hook_note(quirk, &format!("plan sync: updated {line}"));
                        }
                        for line in &report.closure_obligations {
                            hook_note(quirk, &format!("plan sync: closure-obligation {line}"));
                        }
                        for line in &report.notes {
                            hook_note(quirk, &format!("plan sync: {line}"));
                        }
                    }
                }
                // Compact boundaries ARM a FULL identity injection for the
                // next deliverable event — but only where the harness has
                // no working compact channel of its own. kimi discards
                // compact-boundary stdout outright (arm + deliver later);
                // `compact_injection` harnesses (claude) already re-inject
                // identity with the bounded digest below, so arming would
                // only buy a redundant FULL on the next prompt.
                if matches!(canonical, "pre_compact" | "post_compaction")
                    && quirk.injection != Injection::None
                    && !quirk.compact_injection
                {
                    let home = stateroot_core::harness_install::home_dir()
                        .unwrap_or_else(|_| ctx.config_dir.clone());
                    let identity =
                        hook_identity_prefix(&ctx.config_dir, &home, Some(project_dir), quirk.id);
                    let key = format!(
                        "{}:{}",
                        quirk.id,
                        stateroot_core::persona_injection::session_key(project_dir, &payload)
                    );
                    // Compact boundaries never print: the core operation
                    // records the armed flag and resolves Nothing here — the
                    // next deliverable event carries the re-anchoring FULL.
                    // (The old orchestration carried a print branch that was
                    // unreachable: compaction events never resolve to a
                    // delivery by contract.)
                    let _ = stateroot_core::persona_injection::resolve_injection(
                        &home,
                        &key,
                        canonical,
                        &identity,
                        hook_now(),
                        true,
                        false,
                    );
                }
                Ok(code)
            }
            None => Ok(0),
        },
    }
}

// ---------------------------------------------------------------------
// resume
// ---------------------------------------------------------------------

/// Build the identity prefix (full persona + full USER.md). Never budgeted.
fn hook_identity_prefix(
    config_dir: &Path,
    home: &Path,
    project_dir: Option<&Path>,
    harness_id: &str,
) -> String {
    let mut out = String::new();
    out.push_str(super::persona::IDENTITY_ACTIVATION);
    out.push_str("\n\n");
    if let Some(persona) =
        super::persona::resolve_in_project(config_dir, project_dir, Some(harness_id))
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

/// Persona + USER.md for harnesses that fired a resume hook outside any
/// initialized project. No handoff, wiki, learnings seed, or protocol.
fn identity_only_digest(config_dir: &Path, harness_id: &str) -> Option<String> {
    let home = stateroot_core::harness_install::home_dir().ok();
    let home_ref = home.as_deref().unwrap_or(config_dir);
    let digest = hook_identity_prefix(config_dir, home_ref, None, harness_id);
    let digest = digest.trim();
    if digest.is_empty() || digest == super::persona::IDENTITY_ACTIVATION {
        return None;
    }
    Some(digest.to_string())
}

fn resume_identity_only(
    ctx: &Ctx,
    quirk: &registry::HarnessQuirk,
    canonical: &str,
    payload: &Value,
) -> anyhow::Result<u8> {
    let Some(digest) = identity_only_digest(&ctx.config_dir, quirk.id) else {
        return Ok(0);
    };
    if !identity_event_prints(quirk, canonical) {
        return Ok(0);
    }
    let home =
        stateroot_core::harness_install::home_dir().unwrap_or_else(|_| ctx.config_dir.clone());
    let key = format!(
        "{}:{}",
        quirk.id,
        stateroot_core::persona_injection::session_key(&ctx.cwd, payload)
    );
    let mark = match canonical {
        "session_start" => identity_event_marks(quirk, canonical),
        _ => true,
    };
    let deliverable = identity_event_marks(quirk, canonical);
    // The identity-only digest IS the whole content outside a project — no
    // project context exists to build; the scheduler decides full block,
    // compressed pointer, or silence.
    let text = match stateroot_core::persona_injection::resolve_injection(
        &home,
        &key,
        canonical,
        &digest,
        hook_now(),
        mark,
        deliverable,
    ) {
        stateroot_core::persona_injection::InjectionResolution::Full => digest,
        stateroot_core::persona_injection::InjectionResolution::Compressed(pointer) => pointer,
        stateroot_core::persona_injection::InjectionResolution::Nothing => return Ok(0),
    };
    match quirk.injection {
        Injection::None => Ok(0),
        Injection::StdoutJson
        | Injection::CursorJson
        | Injection::ContextJson
        | Injection::AntigravityJson
        | Injection::StdoutText
        | Injection::McpPull
        | Injection::UserPromptSubmit => {
            print_hook_injection(quirk, canonical, &text);
            Ok(0)
        }
    }
}

fn identity_event_prints(quirk: &registry::HarnessQuirk, canonical: &str) -> bool {
    let policy = quirk.delivery();
    match canonical {
        "session_start" => policy.session_start_prints,
        "user_prompt_submit" => policy.prompt_submit_injects,
        // Cursor cannot inject on beforeSubmitPrompt or preCompact. Its
        // postToolUse response does support additional_context, making the
        // first successful tool after compaction the only native re-anchor.
        "post_tool_use" => quirk.id == "cursor",
        _ => false,
    }
}

/// Cheap preflight for Cursor's high-frequency postToolUse event. Building a
/// complete digest on every tool would be wasteful; read only the tiny
/// per-session scheduler record unless a FULL can actually be due.
fn cursor_post_tool_needs_identity(
    ctx: &Ctx,
    quirk: &registry::HarnessQuirk,
    project_dir: &Path,
    payload: &Value,
) -> bool {
    let home =
        stateroot_core::harness_install::home_dir().unwrap_or_else(|_| ctx.config_dir.clone());
    let identity = hook_identity_prefix(&ctx.config_dir, &home, Some(project_dir), quirk.id);
    let hash = stateroot_core::persona_injection::content_hash(&identity);
    let key = format!(
        "{}:{}",
        quirk.id,
        stateroot_core::persona_injection::session_key(project_dir, payload)
    );
    match stateroot_core::persona_injection::load_state(&home, &key) {
        None => true,
        Some(state) => {
            state.pending_compaction
                || (!hash.is_empty() && hash != state.content_hash)
                || !state.started
        }
    }
}

/// DECISION (persona scheduling only — no project context is built here):
/// what this event carries, if anything. Silent covers events this harness
/// never prints, `Injection::None` harnesses, and a scheduler Nothing alike.
/// State lives in the user-global local dir and is recorded by the core
/// operation exactly as before (delivery cadence, compaction arming).
enum DeliveryDecision {
    Silent,
    /// Render the full context carrying this identity section (the full
    /// persona prefix for a FULL, the compressed pointer for a cadence
    /// delivery).
    Render(String),
}

fn decide_delivery(
    ctx: &Ctx,
    quirk: &registry::HarnessQuirk,
    canonical: &str,
    project_dir: &Path,
    payload: &Value,
) -> DeliveryDecision {
    if !identity_event_prints(quirk, canonical) || quirk.injection == Injection::None {
        return DeliveryDecision::Silent;
    }
    let home =
        stateroot_core::harness_install::home_dir().unwrap_or_else(|_| ctx.config_dir.clone());
    let identity = hook_identity_prefix(&ctx.config_dir, &home, Some(project_dir), quirk.id);
    // Per project + harness + session: every harness gets its own start
    // injection even inside a shared conversation.
    let key = format!(
        "{}:{}",
        quirk.id,
        stateroot_core::persona_injection::session_key(project_dir, payload)
    );
    // Marked = the injection actually lands for this harness/event (pi's
    // session_start prints but is discarded → unmarked → first prompt still
    // injects). Prompts and compact boundaries always mark.
    let mark = match canonical {
        "session_start" => identity_event_marks(quirk, canonical),
        _ => true,
    };
    // Deliverable = the event's output can carry identity to the model on
    // this harness: session_start where it marks, prompt_submit where the
    // policy injects, and Cursor postToolUse (its only post-compaction
    // additional_context channel). An armed FULL waits for one of these.
    let deliverable = identity_event_marks(quirk, canonical);
    match stateroot_core::persona_injection::resolve_injection(
        &home,
        &key,
        canonical,
        &identity,
        hook_now(),
        mark,
        deliverable,
    ) {
        stateroot_core::persona_injection::InjectionResolution::Full => {
            DeliveryDecision::Render(identity)
        }
        stateroot_core::persona_injection::InjectionResolution::Compressed(pointer) => {
            DeliveryDecision::Render(pointer)
        }
        stateroot_core::persona_injection::InjectionResolution::Nothing => DeliveryDecision::Silent,
    }
}

/// Scheduler clock. `STATEROOT_HOOK_NOW` (epoch seconds) is a test seam —
/// production always uses the real clock.
fn hook_now() -> chrono::DateTime<chrono::Utc> {
    std::env::var("STATEROOT_HOOK_NOW")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
        .unwrap_or_else(stateroot_core::persona_injection::utc_now)
}

fn identity_event_marks(quirk: &registry::HarnessQuirk, canonical: &str) -> bool {
    let policy = quirk.delivery();
    match canonical {
        "session_start" => policy.session_start_marks,
        "user_prompt_submit" => policy.prompt_submit_injects,
        "post_tool_use" => quirk.id == "cursor",
        _ => false,
    }
}

fn append_handoff_work(work: &mut String, handoff: &Value, project_dir: &Path) {
    let lineage = stateroot_core::roots::compose_digest_section(project_dir);
    if !lineage.is_empty() {
        work.push_str(&lineage);
    }
    if let Ok(home) = stateroot_core::harness_install::home_dir() {
        if let Some(highlight) = stateroot_core::learnings::highlight_for_digest(project_dir, &home)
        {
            work.push_str(&format!("{highlight}\n\n"));
        }
    }
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
    let home = stateroot_core::harness_install::home_dir().ok();
    let identity = home
        .as_ref()
        .map(|home| hook_identity_prefix(config_dir, home, Some(project_dir), harness_id))
        .unwrap_or_else(|| {
            hook_identity_prefix(config_dir, config_dir, Some(project_dir), harness_id)
        });
    hook_digest_with_identity(config_dir, project_dir, harness_id, &identity)
}

// Test seam (M3, the stable in-process seam handed to M5): every FULL
// context build increments a thread-local counter. A silent delivery
// decision must never reach this function; a full delivery reaches it at
// most once. Thread-local because tests run multi-threaded.
#[cfg(test)]
thread_local! {
    static FULL_DIGEST_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// Test seam (M3): session-boundary maintenance runs. Maintenance is
// eligible at the boundary whether or not the event printed.
#[cfg(test)]
thread_local! {
    static SESSION_MAINTENANCE_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn observed_digest_builds() -> usize {
    FULL_DIGEST_BUILDS.with(|c| c.get())
}

#[cfg(test)]
pub(crate) fn observed_maintenance_runs() -> usize {
    SESSION_MAINTENANCE_RUNS.with(|c| c.get())
}

#[cfg(test)]
pub(crate) fn reset_delivery_counters() {
    FULL_DIGEST_BUILDS.with(|c| c.set(0));
    SESSION_MAINTENANCE_RUNS.with(|c| c.set(0));
}

/// [`hook_digest`] with an explicit identity block (the scheduler decides
/// what the identity section carries: full prefix, compressed pointer, or
/// nothing).
pub fn hook_digest_with_identity(
    config_dir: &Path,
    project_dir: &Path,
    harness_id: &str,
    identity: &str,
) -> Option<String> {
    #[cfg(test)]
    FULL_DIGEST_BUILDS.with(|c| c.set(c.get() + 1));
    let _ = (config_dir, harness_id);
    let identity = identity.to_string();
    let handoff = local_store::read_handoff_local(project_dir).ok().flatten();
    let mut work = String::new();
    // Corruption banners first — a quiet unreadable handoff silently degraded
    // sessions for days in the field before anyone noticed.
    let handoff_path = local_store::root(project_dir).join(local_store::HANDOFF_CURRENT_PATH);
    if handoff_path.is_file() && local_store::read_handoff_local(project_dir).is_err() {
        work.push_str(
            "**STATE DEGRADED — the current handoff is unreadable.** Run `stateroot handoff repair`. Work sections below predate the corruption.\n\n",
        );
    }
    // The central plan store and recent checkpoint notes ride the hook digest
    // too — they are the freshest actionable state, and a harness whose digest
    // never shows them will go hunting for them (the openclaw probe lesson).
    // The freshest truth first: latest observed activity (a live session that
    // never wrote a handoff stays visible), then the central plan, the
    // handoff work, and recent checkpoints. The update nudge leads (agents
    // act on what they see).
    if let Some(notice) = super::update::update_notice(config_dir) {
        work.push_str(&notice);
    }
    if let Ok(home) = stateroot_core::harness_install::home_dir() {
        if let Some(notice) = super::soul::soul_sync_notice(&home) {
            work.push_str(&notice);
        }
    }
    if let Some(section) = super::resume::latest_activity_section(project_dir) {
        work.push_str(&section);
    }
    // Push before pull: derived obligations/attention outrank the work view.
    if let Some(section) = super::resume::needs_attention_section(project_dir) {
        work.push_str(&section);
    }
    if let Some(section) = super::resume::central_plan_section(Some(project_dir)) {
        work.push_str(&section);
    }
    if let Some(section) = super::resume::shared_capabilities_section(project_dir) {
        work.push_str(&section);
    }
    if let Some(ref handoff) = handoff {
        append_handoff_work(&mut work, handoff, project_dir);
    }
    if let Some(section) = super::resume::recent_checkpoints_section(project_dir) {
        work.push_str(&section);
    }
    if let Some(section) = super::resume::recent_delegations_section(project_dir) {
        work.push_str(&section);
    }

    if !work.trim().is_empty() {
        work.push_str(&format!("\n{}", super::resume::NO_REFETCH_FOOTER));
    }
    let home = stateroot_core::harness_install::home_dir().ok();
    let learnings = home.as_ref().map(|home| {
        let status = stateroot_core::learnings::bootstrap_status(project_dir, home);
        stateroot_core::learnings::compose_instruction(&status)
    });
    let durable = home.as_ref().map(|home| {
        stateroot_core::learnings::compose_durable_preferences_section(
            &stateroot_core::learnings::collect_active_for_digest(project_dir, home),
        )
    });
    let rules = home
        .as_ref()
        .map(|home| stateroot_core::rules::compose_section(project_dir, home));
    let work = work.trim().to_string();
    if identity.trim().is_empty()
        && work.is_empty()
        && learnings.is_none()
        && durable
            .as_ref()
            .map(|t| t.trim().is_empty())
            .unwrap_or(true)
        && rules.is_none()
    {
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
    if let Some(durable) = durable.filter(|text| !text.trim().is_empty()) {
        if !digest.is_empty() && !digest.ends_with('\n') {
            digest.push('\n');
        }
        digest.push_str(durable.trim());
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
    let pack_md = stateroot_core::context_pack::build(project_dir).render_markdown();
    if !pack_md.trim().is_empty() {
        if !digest.is_empty() && !digest.ends_with('\n') {
            digest.push('\n');
        }
        digest.push_str(pack_md.trim());
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

/// hermes `pre_llm_call` injection envelope: `{"context": "…"}` stdout is
/// injected into the turn's user message (observer events ignore the return).
fn context_json_envelope(digest: &str) -> Value {
    json!({ "context": digest })
}

/// Antigravity's documented injection envelope (SessionStart/PreInvocation
/// stdout contract): steps injected into the conversation trajectory. Kept
/// pure so tests assert the exact parseable shape.
fn antigravity_envelope(digest: &str) -> Value {
    json!({
        "injectSteps": [{
            "systemMessage": {
                "systemMessage": digest,
            }
        }]
    })
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
        Injection::ContextJson => {
            println!("{}", context_json_envelope(digest));
        }
        Injection::AntigravityJson => {
            println!("{}", antigravity_envelope(digest));
        }
        Injection::StdoutText | Injection::UserPromptSubmit | Injection::McpPull => {
            println!("{digest}");
        }
        Injection::None => {}
    }
}

/// Housekeeping notes print to stderr — which harnesses like VS Code Copilot
/// surface as user-facing warning banners after every turn. Success-path
/// chatter is gated per harness (`quiet_housekeeping`); failures always print.
fn hook_note(quirk: &registry::HarnessQuirk, msg: &str) {
    if !registry::quiet_housekeeping(quirk.id) {
        eprintln!("{msg}");
    }
}

/// Deterministic continuity reconciliation at a hook boundary — cheap JSON
/// reads + one atomic projection write, gated on `[continuity] enabled`.
fn reconcile_boundary(ctx: &Ctx, project_dir: &Path) {
    if !ctx.config.continuity.enabled {
        return;
    }
    if let Err(err) =
        stateroot_core::continuity::reconcile(project_dir, &ctx.config_dir, &ctx.config.continuity)
    {
        note!("continuity reconcile: {err}");
    } else {
        let mut project_ctx = ctx.clone();
        project_ctx.cwd = project_dir.to_path_buf();
        for outcome in super::plan::advance_authorized(&project_ctx) {
            if outcome["status"] == "unavailable" {
                note!("plan advancement: {}", outcome);
            }
        }
    }
}

/// Record the verified session-to-plan binding when a harness receives a
/// plan-bearing handoff: digest delivery IS the receipt. `record_binding`
/// validates the plan is approved/active, so a stale packet never binds.
pub(crate) fn bind_session_plan(project_dir: &Path, harness: &str, session_id: &str, source: &str) {
    let Ok(Some(packet)) = stateroot_core::local_store::read_handoff_local(project_dir) else {
        return;
    };
    let Some(plan_id) = packet
        .get("plan_ref")
        .and_then(|p| p.get("id"))
        .and_then(|v| v.as_str())
    else {
        return;
    };
    let _ = stateroot_core::todo_federation::record_binding(
        project_dir,
        harness,
        session_id,
        plan_id,
        source,
    );
}

async fn resume_output(
    ctx: &Ctx,
    quirk: &registry::HarnessQuirk,
    canonical: &str,
    project_dir: &Path,
    payload: &Value,
) -> anyhow::Result<u8> {
    if canonical == "session_start" {
        if let Err(err) = stateroot_core::learnings::record_first_session(project_dir, quirk.id) {
            note!("warning: could not record first-run harness: {err}");
        }
        // Reconcile BEFORE the digest assembles so ## Needs Attention reads
        // a fresh projection.
        reconcile_boundary(ctx, project_dir);
    }
    // One coherent delivery operation with explicit effects:
    //   DECIDE  — persona scheduling only; never builds project context.
    //   RENDER  — at most one full context build, only for a full delivery.
    //   OUTPUT  — serialized here at the command boundary; effects follow a
    //             successful print, never precede one.
    if let DeliveryDecision::Render(identity) =
        decide_delivery(ctx, quirk, canonical, project_dir, payload)
    {
        if let Some(digest) =
            hook_digest_with_identity(&ctx.config_dir, project_dir, quirk.id, &identity)
        {
            print_hook_injection(quirk, canonical, &digest);
            record_delivery_effects(ctx, quirk, canonical, project_dir, payload);
        }
    }
    // Maintenance is eligible at the session boundary whether or not this
    // event printed — a silent dedupe repeat must not skip the intended
    // automatic syncs or boundary recovery. It still runs AFTER any output:
    // harnesses kill slow hooks (cursor's default timeout), and a killed
    // process must not take the injection with it.
    if canonical == "session_start" {
        session_start_maintenance(ctx, project_dir).await;
    }
    Ok(0)
}

/// The effects of a successful print, in the established order: telemetry,
/// the delivery ledger, the session→plan binding. Never called without
/// output — a failed render/output must not claim delivery.
fn record_delivery_effects(
    ctx: &Ctx,
    quirk: &registry::HarnessQuirk,
    canonical: &str,
    project_dir: &Path,
    payload: &Value,
) {
    // Telemetry: a non-empty continuity digest was just emitted in a
    // recognized harness attached to an initialized project. Local append
    // only — hooks never wait on the network. Dedup makes compaction
    // re-injection and duplicate hook events idempotent.
    crate::telemetry::continuity_delivered(&ctx.config_dir, project_dir, quirk.id);
    let content_fp = digest_delivery::content_fingerprint(project_dir);
    if identity_event_marks(quirk, canonical) {
        digest_delivery::mark_delivered(
            project_dir,
            quirk.id,
            DeliveryIntent::Session,
            DeliveryChannel::Hook,
            canonical,
            payload,
            &content_fp,
        );
        if let Some(session_id) = digest_delivery::session_id_from_payload(payload) {
            bind_session_plan(project_dir, quirk.id, &session_id, "hook-delivery");
        }
    }
}

/// MAINTENANCE at the session boundary — explicitly eligible independent of
/// whether the event printed. All of it is best-effort and fresh enough one
/// step behind (discovery reads sources live anyway), self-throttles where
/// interval-gated, and never fails the hook. Kept callable separately from
/// decision/rendering so policy tests never invoke it.
async fn session_start_maintenance(ctx: &Ctx, project_dir: &Path) {
    #[cfg(test)]
    SESSION_MAINTENANCE_RUNS.with(|c| c.set(c.get() + 1));
    // Skill federation sync (best-effort, never fails the hook).
    let options = stateroot_core::skill_federation::SyncOptions {
        dry_run: false,
        push: false,
        pull: true,
        cmd_probe: None,
    };
    if let Err(err) = stateroot_core::skill_federation::sync_project(project_dir, &options, None) {
        note!("warning: skill federation sync skipped: {err}");
    }
    // MCP federation (pull + project; never fails the hook).
    let mcp_options = stateroot_core::mcp_federation::SyncOptions {
        dry_run: false,
        pull: true,
        push: true,
        cmd_probe: None,
    };
    if let Err(err) = stateroot_core::mcp_federation::sync(None, Some(project_dir), &mcp_options) {
        note!("warning: MCP federation sync skipped: {err}");
    }
    // Rules federation (product-intent + harness imports).
    if let Ok(home) = stateroot_core::harness_install::home_dir() {
        if let Err(err) = stateroot_core::rules::sync(project_dir, &home) {
            note!("warning: rules sync skipped: {err}");
        }
    }
    // Soul federation: one sync pass per hour of agent activity —
    // harness-native persona edits are adopted into the canonical soul
    // and pushed outward; conflicts surface in the digest.
    super::soul::maybe_auto_sync(ctx, 1);
    // Automatic update path: fire a detached self-update when the release
    // cache is stale — scheduled by activity, never blocking, never
    // asking an agent to act. (The digest notice stays as the visible
    // layer; this is the layer that actually keeps machines current.)
    super::update::maybe_spawn_scheduled_update(
        &ctx.config_dir,
        ctx.config.update.check_interval_hours,
    );
    // Session-start is a safe entrypoint for the boundary journal: kick
    // recovery for any job whose last drainer died (cheap due-scan only).
    super::drain_finalize::kick(project_dir);
    // Dual-mode compiler: agentic when keyed/logged-in; never fails the hook.
    let hook_ctx = Ctx {
        cwd: project_dir.to_path_buf(),
        config_dir: ctx.config_dir.clone(),
        config: ctx.config.clone(),
    };
    if let Err(err) = super::compiler::try_agentic(&hook_ctx, false).await {
        note!("warning: compiler skipped: {err}");
    }
}

// ---------------------------------------------------------------------
// capture
// ---------------------------------------------------------------------

fn capture_file_local_only(project_dir: &Path, payload: &Value) -> Result<bool, String> {
    let policy = project_dir.join(stateroot_core::sync_engine::ignore::CAPTURE_IGNORE);
    match std::fs::metadata(&policy) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("capture declaration unavailable: {error}")),
        Ok(_) => {}
    }
    let input = payload
        .get("tool_input")
        .or_else(|| payload.get("input"))
        .or_else(|| payload.get("arguments"))
        .unwrap_or(payload);
    let path = ["file_path", "filePath", "filepath", "path"]
        .into_iter()
        .find_map(|key| input.get(key).and_then(Value::as_str));
    let Some(path) = path else {
        return Ok(false);
    };
    let reported = Path::new(path);
    let local = stateroot_core::path_identity::resolve_existing_file(reported).or_else(|| {
        let cwd = payload_project_dir(payload).unwrap_or_else(|| project_dir.to_path_buf());
        stateroot_core::path_identity::resolve_existing_file(&cwd.join(reported))
    });
    let local = local.ok_or_else(|| "tool file path cannot be verified".to_string())?;
    stateroot_core::sync_engine::ignore::capture_file_excluded(project_dir, &local)
}

fn capture_observation(
    quirk: &registry::HarnessQuirk,
    canonical: &str,
    project_dir: &Path,
    payload: &Value,
) -> anyhow::Result<u8> {
    let local_only = match capture_file_local_only(project_dir, payload) {
        Ok(value) => value,
        Err(_) => {
            hook_note(
                quirk,
                "capture declaration could not verify a file path; payload retained locally",
            );
            true
        }
    };
    let text = if local_only {
        stateroot_core::observations::LOCAL_EXCLUDED_TEXT.to_string()
    } else {
        payload_text(payload)
    };
    let kind_hint = if local_only {
        None
    } else {
        infer_kind_hint(canonical, &text)
    };
    let tool = payload_tool(payload);
    let excerpt = if local_only {
        Some(text.clone())
    } else {
        payload_excerpt(payload, &text)
    };
    let session_id = digest_delivery::session_id_from_payload(payload);
    // `anon-*` ids are StateRoot-minted (session_identity::tag_payload) — the
    // host harness never carried one; label the provenance, never confuse it
    // with a verified native session.
    let session_identity = match session_id.as_deref() {
        Some(id) if id.starts_with("anon-") => "minted",
        Some(_) => "native",
        None => "unavailable",
    };
    let source_status = if local_only {
        "local_excluded"
    } else if payload.get("_transport_error").is_some() {
        // The stdin read failed: surface the loss instead of acknowledging
        // complete evidence (A3).
        "transport_read_error"
    } else {
        "complete"
    };
    let outcome = stateroot_core::observations::capture(
        project_dir,
        stateroot_core::observations::CaptureRequest {
            ts: now_rfc3339(),
            event: canonical.to_string(),
            harness: quirk.id.to_string(),
            session_id,
            session_identity: session_identity.to_string(),
            event_identity: native_event_identity(payload),
            text,
            kind_hint: kind_hint.map(str::to_string),
            tool,
            excerpt,
            source: if local_only {
                None
            } else {
                Some(payload.clone())
            },
            local_payload: if local_only {
                Some(payload.clone())
            } else {
                None
            },
            source_status: source_status.to_string(),
            meta: if local_only {
                None
            } else {
                payload_meta(payload)
            },
        },
    )?;
    match outcome.status {
        stateroot_core::observations::CaptureStatus::Replay => {}
        stateroot_core::observations::CaptureStatus::Captured => {}
        stateroot_core::observations::CaptureStatus::Conflict => {
            hook_note(
                quirk,
                &format!(
                    "observation conflict on reused event identity — both bodies retained (earlier: {})",
                    outcome.related.as_deref().unwrap_or("unknown")
                ),
            );
        }
    }
    if !outcome.projection_synced {
        note!(
            "observation durable but projection update failed (rebuildable): {}",
            outcome.projection_error.as_deref().unwrap_or("unknown")
        );
    }
    if !outcome.frontier_synced {
        note!(
            "observation durable but session frontier update failed (recover with observations health): {}",
            outcome.frontier_error.as_deref().unwrap_or("unknown")
        );
    }
    Ok(0)
}

/// Payload fields consumed into first-class record slots; everything else is
/// kept as observed metadata (unknown fields are never dropped).
const CONSUMED_PAYLOAD_KEYS: &[&str] = &[
    "prompt",
    "text",
    "message",
    "content",
    "summary",
    "note",
    "_raw",
    "tool",
    "tool_name",
    "name",
    "command",
    "cwd",
    "project_dir",
    "projectDir",
    "workspace_dir",
    "workspace_roots",
    "workspacePaths",
    "session_id",
    "conversation_id",
    "generation_id",
    "sessionId",
    "conversationId",
    "generationId",
    "messageID",
    "composerId",
    "session",
    "hook_event_name",
    "_transport_error",
    // Native event identity fields (see native_event_identity).
    "tool_use_id",
    "tool_call_id",
    "event_id",
    "hook_event_id",
    "request_id",
    "message_id",
];

/// Verified native event identity from the payload, when the harness sends
/// one. Verified per adapter payloads: `tool_use_id` (claude/codex),
/// `tool_call_id` (kimi), then generic id fields. None means the host has no
/// event identity — capture must mint one and never dedup by content.
fn native_event_identity(payload: &Value) -> Option<(String, String)> {
    for key in [
        "tool_use_id",
        "tool_call_id",
        "event_id",
        "hook_event_id",
        "request_id",
        "message_id",
    ] {
        if let Some(value) = payload.get(key).and_then(|v| v.as_str()) {
            let value = value.trim();
            if !value.is_empty() {
                return Some((key.to_string(), value.to_string()));
            }
        }
    }
    None
}

/// Unknown top-level payload fields, kept as observed metadata.
fn payload_meta(payload: &Value) -> Option<Value> {
    let obj = payload.as_object()?;
    let rest: serde_json::Map<String, Value> = obj
        .iter()
        .filter(|(key, _)| !CONSUMED_PAYLOAD_KEYS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if rest.is_empty() {
        None
    } else {
        Some(Value::Object(rest))
    }
}

/// The captured text is the authorized evidence: retained in full, never
/// truncated at capture. Bounded excerpts are produced for digests and list
/// views only (`payload_excerpt`).
fn payload_text(payload: &Value) -> String {
    for key in [
        "prompt", "text", "message", "content", "summary", "note", "_raw",
    ] {
        if let Some(value) = payload.get(key).and_then(|v| v.as_str()) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    serde_json::to_string(payload).unwrap_or_default()
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
    let native_session = stateroot_core::digest_delivery::session_id_from_payload(payload);
    let tail = spool_tail(
        project_dir,
        quirk.id,
        native_session.as_deref(),
        CHECKPOINT_TAIL,
    );
    let mut note_text = format!("{canonical} via {} hook", quirk.id);
    // hermes on_session_end carries the turn outcome in extra — a failed or
    // interrupted turn's evidence rides the checkpoint note, never dropped.
    if let Some(outcome) = hermes_turn_outcome(quirk, payload) {
        note_text.push_str(&format!(" [{outcome}]"));
    }
    if let Some(raw) = payload.get("_raw").and_then(|v| v.as_str()) {
        note_text.push_str(&format!(": {}", truncate(raw, 200)));
    }
    // Antigravity Stop carries the loop's termination state
    // (`terminationReason` / `error` / `fullyIdle`): record it verbatim so an
    // errored or interrupted turn is never read back as a clean session end.
    let antigravity_stop = if quirk.id == "antigravity" && canonical == "stop" {
        let fully_idle = payload.get("fullyIdle").and_then(Value::as_bool);
        let reason = payload
            .get("terminationReason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let error = payload
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        Some((fully_idle, reason, error))
    } else {
        None
    };
    if let Some((fully_idle, reason, error)) = &antigravity_stop {
        let mut detail = Vec::new();
        if !reason.is_empty() {
            detail.push(format!("terminationReason={reason}"));
        }
        if let Some(idle) = fully_idle {
            detail.push(format!("fullyIdle={idle}"));
        }
        if !error.is_empty() {
            detail.push(format!("error={}", truncate(error, 200)));
        }
        if !detail.is_empty() {
            note_text.push_str(&format!(" ({})", detail.join(", ")));
        }
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
    let projected =
        super::checkpoint::record_checkpoint(&hook_ctx, quirk.id, &note_text, &[]).await?;
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
            reconcile_boundary(&hook_ctx, project_dir);
        }
        if let Some(digest) = hook_digest(&hook_ctx.config_dir, project_dir, quirk.id) {
            print_hook_injection(quirk, canonical, &digest);
            // Post-compaction re-injection delivered a non-empty digest.
            crate::telemetry::continuity_delivered(&hook_ctx.config_dir, project_dir, quirk.id);
        }
    }

    // stop/session_end: checkpoint already recorded above. Enqueue ONE
    // composite boundary job (durable journal, repair Phase 3) and return
    // inside the hook's latency budget — the detached drainer and every
    // later safe entrypoint share recovery. A hermes delegated child's
    // turn-end checkpoints stay project evidence but never enqueue boundary
    // work (transcript finalize/snap/ingest belong to real sessions).
    if matches!(canonical, "stop" | "session_end") && !hermes_delegated_child(quirk, payload) {
        let session_id = stateroot_core::digest_delivery::session_id_from_payload(payload)
            .unwrap_or_else(|| "unknown".to_string());
        let lineage_ref = stateroot_core::roots::lineage_refname(project_dir);
        let locator = payload
            .get("transcript_path")
            .or_else(|| payload.get("transcript"))
            .or_else(|| payload.get("transcriptPath"))
            .and_then(Value::as_str)
            .filter(|locator| !locator.is_empty());
        let occurrence = payload
            .get("event_id")
            .or_else(|| payload.get("boundary_id"))
            .or_else(|| payload.get("turn_id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("{canonical}:{}", uuid::Uuid::now_v7()));
        let capture_watermark = match stateroot_core::observations::session_watermark(
            project_dir,
            quirk.id,
            &session_id,
        ) {
            stateroot_core::observations::SessionWatermark::Absent => {
                json!({"status":"absent","harness":quirk.id,"session_id":session_id})
            }
            stateroot_core::observations::SessionWatermark::Present(watermark) => {
                json!({"status":"present","harness":quirk.id,"session_id":session_id,"watermark":watermark})
            }
            stateroot_core::observations::SessionWatermark::Unreadable(reason) => {
                json!({"status":"unreadable","harness":quirk.id,"session_id":session_id,"reason":reason})
            }
        };
        // A Stop that is explicitly not fully idle means background commands
        // or async tasks are still running: the session is NOT complete, so
        // the completion pipeline (snap → handoff finalize → ingest → seal)
        // must not seal still-running work as a finished session. Evidence
        // stays durable in the spool and episodic log; a later idle Stop (or
        // any safe entrypoint) finalizes the boundary. Errored/interrupted
        // stops that ARE idle still enqueue — abnormal stops are retained
        // through the durable journal, never discarded.
        let boundary_complete =
            antigravity_stop.as_ref().and_then(|(idle, _, _)| *idle) != Some(false);
        if boundary_complete {
            match stateroot_core::finalize_journal::enqueue_with_capture(
                project_dir,
                quirk.id,
                &session_id,
                locator,
                &lineage_ref,
                &occurrence,
                Some(capture_watermark),
            ) {
                Ok(_) => {
                    hook_note(quirk, "boundary job queued (durable journal)");
                    super::drain_finalize::kick(project_dir);
                }
                Err(err) => note!("boundary enqueue skipped: {err}"),
            }
        } else {
            hook_note(
                quirk,
                "stop not fully idle — background work still running; boundary finalize deferred (evidence retained)",
            );
        }
        reconcile_boundary(&hook_ctx, project_dir);
        // No synchronous segment sealing here: sealing is the WS2 drainer's
        // non-destructive post-ingest step. Stop stays bounded on long
        // segments and capture history is never deleted or truncated.
    }
    Ok(0)
}

fn spool_tail(
    project_dir: &Path,
    harness: &str,
    session_id: Option<&str>,
    count: usize,
) -> Vec<String> {
    match stateroot_core::observations::recent_brief(project_dir, harness, session_id, count) {
        Ok(rows) => rows
            .into_iter()
            .map(|row| format!("- {}", truncate(&row, 240)))
            .collect(),
        Err(error) => vec![format!(
            "- current capture brief unavailable: {error}; retained evidence is intact"
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_file_capture_exclusion_routes_full_payload_to_local_storage() {
        let project = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p-test", "proj", "local").unwrap();
        std::fs::write(project.path().join(".staterootcaptureignore"), "private/\n").unwrap();
        std::fs::create_dir(project.path().join("private")).unwrap();
        let file = project.path().join("private/ignored.txt");
        std::fs::write(&file, "private-tool-canary").unwrap();
        let payload = json!({
            "cwd": project.path(),
            "session_id": "capture-fixture",
            "tool_name": "Read",
            "tool_input": {"file_path": file},
            "tool_response": {"content": "private-tool-canary"}
        });
        let quirk = quirk_any("cursor").unwrap();
        assert!(capture_file_local_only(project.path(), &payload).unwrap());
        let relative =
            json!({"cwd": project.path(), "tool_input": {"path": "private/ignored.txt"}});
        assert!(capture_file_local_only(project.path(), &relative).unwrap());
        let shell = json!({"cwd": project.path(), "tool_name": "Bash", "tool_input": {"command": "cat private/ignored.txt"}});
        assert!(!capture_file_local_only(project.path(), &shell).unwrap());
        assert_eq!(
            capture_observation(quirk, "post_tool_use", project.path(), &payload).unwrap(),
            0
        );
        let rows = stateroot_core::observations::load_spool(project.path());
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].text.contains("private-tool-canary"));
        let record = stateroot_core::observations::archive_lookup(
            project.path(),
            rows[0].capture_id.as_deref().unwrap(),
        )
        .unwrap();
        assert_eq!(record.capture.source_status, "local_excluded");
        let shared = serde_json::to_string(&record).unwrap();
        assert!(!shared.contains("private-tool-canary"));
        let local = record.source.unwrap().payload["relative_path"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            std::fs::read_to_string(local_store::root(project.path()).join(local))
                .unwrap()
                .contains("private-tool-canary")
        );
        let health = stateroot_core::observations::health(project.path());
        assert_eq!(health.local_only, 1);
        assert_eq!(health.source_unavailable, 0);
    }

    #[test]
    fn unverifiable_capture_file_path_stays_local_when_policy_exists() {
        let project = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p-test", "proj", "local").unwrap();
        std::fs::write(project.path().join(".staterootcaptureignore"), "private/\n").unwrap();
        let payload = json!({
            "cwd": project.path(),
            "session_id": "capture-fixture",
            "tool_name": "Read",
            "tool_input": {"file_path": "private/missing.txt"},
            "tool_response": {"content": "unverified-private-canary"}
        });
        assert!(capture_file_local_only(project.path(), &payload).is_err());
        let quirk = quirk_any("cursor").unwrap();
        assert_eq!(
            capture_observation(quirk, "post_tool_use", project.path(), &payload).unwrap(),
            0
        );
        let rows = stateroot_core::observations::load_spool(project.path());
        assert_eq!(rows.len(), 1);
        let record = stateroot_core::observations::archive_lookup(
            project.path(),
            rows[0].capture_id.as_deref().unwrap(),
        )
        .unwrap();
        assert_eq!(record.capture.source_status, "local_excluded");
        assert!(!serde_json::to_string(&record)
            .unwrap()
            .contains("unverified-private-canary"));
    }

    #[test]
    fn payload_project_dir_prefers_agent_cwd_over_process() {
        let project = tempfile::tempdir().expect("project");
        local_store::init_skeleton(project.path(), "p-test", "proj", "local").expect("skeleton");
        // cwd string field (openclaw/kimi shape).
        let payload = json!({"cwd": project.path().display().to_string()});
        let dir = payload_project_dir(&payload).expect("cwd dir");
        // Canonicalize BOTH sides: Windows canonicalize adds the \\?\ prefix
        // and expands 8.3 short names — comparing raw forms flakes there.
        let found = find_project_root(&dir)
            .expect("root")
            .canonicalize()
            .expect("canonical root");
        assert_eq!(found, project.path().canonicalize().unwrap());
        // workspace_roots array (cursor shape).
        let payload = json!({"workspace_roots": [project.path().display().to_string()]});
        assert!(payload_project_dir(&payload).is_some());
        // Garbage stays None: relative paths, missing dirs, wrong types.
        assert!(payload_project_dir(&json!({"cwd": "relative/path"})).is_none());
        assert!(payload_project_dir(&json!({"cwd": "/definitely/not/here"})).is_none());
        assert!(payload_project_dir(&json!({"cwd": 42})).is_none());
        assert!(payload_project_dir(&json!({})).is_none());
    }

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
        let _guard = crate::test_env::env_lock();
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
    fn identity_only_digest_needs_no_project() {
        let _guard = crate::test_env::env_lock();
        let home = tempfile::tempdir().expect("home");
        std::fs::write(
            home.path().join("persona.md"),
            "## Working relationship\n\nYou are Yinyue.\n",
        )
        .expect("persona");
        let prior = std::env::var("STATEROOT_TEST_HOME").ok();
        // SAFETY: serialized by the shared test env lock.
        unsafe { std::env::set_var("STATEROOT_TEST_HOME", home.path()) };
        let digest = identity_only_digest(home.path(), "cursor").expect("digest");
        match prior {
            Some(value) => unsafe { std::env::set_var("STATEROOT_TEST_HOME", value) },
            None => unsafe { std::env::remove_var("STATEROOT_TEST_HOME") },
        }
        assert!(digest.contains("Active identity"), "{digest}");
        assert!(digest.contains("You are Yinyue."), "{digest}");
        assert!(!digest.contains("Objective:"), "{digest}");
        assert!(!digest.contains("learnings are empty"), "{digest}");
    }

    #[test]
    fn cursor_registry_uses_native_json_injection() {
        let quirk = registry::quirk("cursor").expect("cursor");
        assert_eq!(quirk.injection, Injection::CursorJson);
        assert_eq!(quirk.instruction_file, Some(".cursor/AGENTS.md"));
    }

    #[test]
    fn digest_includes_full_persona_and_user_without_truncation() {
        let _guard = crate::test_env::env_lock();
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
        // SAFETY: serialized by the shared test env lock.
        unsafe { std::env::set_var("STATEROOT_TEST_HOME", home.path()) };
        let digest = hook_digest(home.path(), project.path(), "codex").expect("digest");
        match prior {
            Some(value) => unsafe { std::env::set_var("STATEROOT_TEST_HOME", value) },
            None => unsafe { std::env::remove_var("STATEROOT_TEST_HOME") },
        }

        assert!(digest.contains("Persona voice line 19: stay in character always"));
        assert!(digest.contains(&long_user));
        assert!(
            hook_identity_prefix(home.path(), home.path(), Some(project.path()), "cursor")
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

    #[tokio::test]
    async fn stop_hook_enqueues_under_one_second_even_if_snap_would_fail() {
        let project = tempfile::tempdir().expect("project");
        local_store::init_skeleton(project.path(), "p-test", "proj", "local").expect("skeleton");
        std::fs::write(project.path().join(".git"), "not-a-repo").expect("poison git");
        let home = tempfile::tempdir().expect("home");
        let ctx = Ctx {
            cwd: project.path().to_path_buf(),
            config_dir: home.path().to_path_buf(),
            config: stateroot_core::config::AppConfig::default(),
        };
        let quirk = quirk_any("cursor").expect("cursor");
        let start = std::time::Instant::now();
        let code = checkpoint_from_spool(&ctx, quirk, "stop", project.path(), &json!({}))
            .await
            .expect("stop");
        assert_eq!(code, 0);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "elapsed {:?}",
            start.elapsed()
        );
        // Phase 3 contract: ONE durable boundary job in the journal — no
        // outbox trio — enqueued inside the latency budget even though the
        // snap is poisoned (the drainer carries the failure, not the hook).
        let pending = local_store::outbox_pending(project.path()).expect("pending");
        assert!(pending.is_empty(), "outbox stays empty: {pending:?}");
        let journal_dir = project.path().join(".stateroot/local/finalize-journal");
        let jobs: Vec<_> = std::fs::read_dir(&journal_dir)
            .expect("journal dir")
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .collect();
        assert_eq!(jobs.len(), 1, "one composite boundary job");
        let job: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(jobs[0].path()).expect("job"))
                .expect("json");
        assert_eq!(job["phase"], "queued");
        assert_eq!(job["state"], "active");
    }

    // -----------------------------------------------------------------
    // hermes native lifecycle fixtures (wire contract: agent/shell_hooks.py
    // + hermes-agent.nousresearch.com/docs/user-guide/features/hooks;
    // live hermes host unavailable on this machine — see plan receipt)
    // -----------------------------------------------------------------

    #[test]
    fn hermes_pre_llm_call_payload_keeps_prompt_drops_conversation() {
        let mut payload = json!({
            "hook_event_name": "pre_llm_call",
            "tool_name": null,
            "tool_input": null,
            "session_id": "ses-1",
            "cwd": "/home/user/project",
            "profile": "default",
            "extra": {
                "task_id": "task-1",
                "turn_id": "turn-1",
                "user_message": "fix the flaky test",
                "conversation_history": [
                    {"role": "user", "content": "HISTORY_SENTINEL earlier prompt"},
                    {"role": "assistant", "content": "HISTORY_SENTINEL earlier answer"}
                ],
                "is_first_turn": false,
                "model": "hermes-4",
                "platform": "cli",
                "parent_session_id": "",
                "sender_id": ""
            }
        });
        shape_hermes_payload("user_prompt_submit", &mut payload);
        let text = serde_json::to_string(&payload).expect("json");
        assert!(
            !text.contains("HISTORY_SENTINEL"),
            "history dropped: {text}"
        );
        assert_eq!(payload["prompt"].as_str(), Some("fix the flaky test"));
        assert_eq!(payload["session_id"].as_str(), Some("ses-1"));
        // The docs-newer `profile` key is tolerated (passthrough), as is a
        // checkout that predates it.
        assert_eq!(payload["profile"].as_str(), Some("default"));
        assert_eq!(payload["extra"]["model"].as_str(), Some("hermes-4"));
        assert_eq!(payload["extra"]["turn_id"].as_str(), Some("turn-1"));

        // Parts-form prompt (multimodal turn): text parts joined, image dropped.
        let mut parts_payload = json!({
            "hook_event_name": "pre_llm_call",
            "session_id": "ses-2",
            "extra": {
                "user_message": [
                    {"type": "text", "text": "describe this"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,xx"}},
                    {"type": "input_text", "input_text": "carefully"}
                ],
                "conversation_history": ["HISTORY_SENTINEL"]
            }
        });
        shape_hermes_payload("user_prompt_submit", &mut parts_payload);
        assert_eq!(
            parts_payload["prompt"].as_str(),
            Some("describe this\ncarefully")
        );
        assert!(!serde_json::to_string(&parts_payload)
            .unwrap()
            .contains("HISTORY_SENTINEL"));

        // Image-only turn: no prompt key, still no history.
        let mut image_only = json!({
            "session_id": "ses-3",
            "extra": {
                "user_message": [{"type": "image", "image": "data:image/png;base64,xx"}],
                "conversation_history": ["HISTORY_SENTINEL"]
            }
        });
        shape_hermes_payload("user_prompt_submit", &mut image_only);
        assert!(image_only.get("prompt").is_none());
        assert!(!serde_json::to_string(&image_only)
            .unwrap()
            .contains("HISTORY_SENTINEL"));

        // Non-prompt events are never shaped.
        let mut stop = json!({"session_id": "ses-1", "extra": {"completed": true}});
        let before = stop.clone();
        shape_hermes_payload("stop", &mut stop);
        assert_eq!(stop, before);
    }

    #[test]
    fn hermes_finalize_payload_finalizes_the_ending_session() {
        // Gateway /new: on_session_finalize names the ENDING session in
        // session_id; old/new ids ride extra. The new session that has just
        // begun must never be closed by it.
        let payload = json!({
            "hook_event_name": "on_session_finalize",
            "session_id": "ses-OLD",
            "cwd": "/home/user/project",
            "extra": {
                "reason": "new_session",
                "old_session_id": "ses-OLD",
                "new_session_id": "ses-NEW",
                "platform": "cli"
            }
        });
        assert_eq!(
            digest_delivery::session_id_from_payload(&payload).as_deref(),
            Some("ses-OLD"),
            "the ending session is the payload's session_id"
        );
        let mut shaped = payload.clone();
        shape_hermes_payload("session_end", &mut shaped);
        assert_eq!(shaped, payload, "finalize payloads are bounded already");
    }

    #[test]
    fn hermes_context_json_envelope_is_the_native_injection_shape() {
        let quirk = quirk_any("hermes").expect("hermes");
        assert_eq!(quirk.injection, Injection::ContextJson);
        let envelope = context_json_envelope("DIGEST BODY");
        assert_eq!(envelope, json!({ "context": "DIGEST BODY" }));
        // The envelope hermes parses on pre_llm_call stdout: a dict with a
        // non-empty string `context` key (agent/shell_hooks.py _parse_response).
        let text = serde_json::to_string(&envelope).expect("json");
        let parsed: Value = serde_json::from_str(&text).expect("round-trip");
        assert!(parsed["context"]
            .as_str()
            .is_some_and(|c| !c.trim().is_empty()));
    }

    #[test]
    fn hermes_turn_outcome_flags_failed_and_interrupted_evidence() {
        let hermes = quirk_any("hermes").expect("hermes");
        let cursor = quirk_any("cursor").expect("cursor");
        // Clean turn: no suffix.
        assert!(hermes_turn_outcome(
            hermes,
            &json!({"extra": {"completed": true, "failed": false, "interrupted": false}})
        )
        .is_none());
        // Failed turn.
        let failed = hermes_turn_outcome(
            hermes,
            &json!({"extra": {"completed": false, "failed": true, "interrupted": false, "turn_exit_reason": "error"}}),
        )
        .expect("failed outcome");
        assert!(failed.contains("failed=true"), "{failed}");
        assert!(failed.contains("reason=error"), "{failed}");
        // Interrupted turn.
        let interrupted = hermes_turn_outcome(
            hermes,
            &json!({"extra": {"completed": false, "failed": false, "interrupted": true, "turn_exit_reason": "user_interrupt"}}),
        )
        .expect("interrupted outcome");
        assert!(interrupted.contains("interrupted=true"), "{interrupted}");
        // Iteration-limit exit with no flags still keeps its reason.
        let limited = hermes_turn_outcome(
            hermes,
            &json!({"extra": {"completed": false, "turn_exit_reason": "max_iterations"}}),
        )
        .expect("reason-only outcome");
        assert!(limited.contains("reason=max_iterations"), "{limited}");
        // The suffix is hermes-scoped.
        assert!(hermes_turn_outcome(cursor, &json!({"extra": {"failed": true}})).is_none());
    }

    #[test]
    fn hermes_delegated_child_markers() {
        let hermes = quirk_any("hermes").expect("hermes");
        let cursor = quirk_any("cursor").expect("cursor");
        // tools/delegate_tool.py: children run with platform="subagent" and
        // a stamped parent_session_id; either marker scopes the payload.
        assert!(hermes_delegated_child(
            hermes,
            &json!({"extra": {"platform": "subagent"}})
        ));
        assert!(hermes_delegated_child(
            hermes,
            &json!({"extra": {"platform": "cli", "parent_session_id": "parent-1"}})
        ));
        assert!(!hermes_delegated_child(
            hermes,
            &json!({"extra": {"platform": "cli", "parent_session_id": ""}})
        ));
        assert!(!hermes_delegated_child(
            hermes,
            &json!({"extra": {"platform": "telegram"}})
        ));
        assert!(!hermes_delegated_child(hermes, &json!({})));
        // The markers are hermes-scoped — no other harness is affected.
        assert!(!hermes_delegated_child(
            cursor,
            &json!({"extra": {"platform": "subagent"}})
        ));
    }

    #[test]
    fn hermes_terminal_cwd_resolves_project() {
        let _guard = crate::test_env::env_lock();
        let project = tempfile::tempdir().expect("project");
        local_store::init_skeleton(project.path(), "p-test", "proj", "local").expect("skeleton");
        let quirk = quirk_any("hermes").expect("hermes");
        let prior = std::env::var_os("TERMINAL_CWD");
        // SAFETY: serialized by the shared test env lock.
        unsafe { std::env::set_var("TERMINAL_CWD", project.path()) };
        let found = hermes_terminal_project(quirk).map(|p| p.canonicalize().expect("canonical"));
        // Non-hermes quirks never consult TERMINAL_CWD.
        let cursor = hermes_terminal_project(quirk_any("cursor").expect("cursor"));
        match prior {
            Some(value) => unsafe { std::env::set_var("TERMINAL_CWD", value) },
            None => unsafe { std::env::remove_var("TERMINAL_CWD") },
        }
        assert_eq!(
            found,
            Some(project.path().canonicalize().expect("canonical")),
            "gateway-configured workdir resolves the project"
        );
        assert!(cursor.is_none());
    }

    #[test]
    fn hermes_shaped_prompt_capture_stores_prompt_not_history() {
        let project = tempfile::tempdir().expect("project");
        local_store::init_skeleton(project.path(), "p-test", "proj", "local").expect("skeleton");
        let quirk = quirk_any("hermes").expect("hermes");
        let mut payload = json!({
            "hook_event_name": "pre_llm_call",
            "session_id": "ses-1",
            "cwd": project.path(),
            "extra": {
                "user_message": "actually, use the staging bucket instead",
                "conversation_history": [{"role": "user", "content": "HISTORY_SENTINEL"}],
                "is_first_turn": false,
                "model": "hermes-4",
                "platform": "cli"
            }
        });
        shape_hermes_payload("user_prompt_submit", &mut payload);
        assert_eq!(
            capture_observation(quirk, "user_prompt_submit", project.path(), &payload)
                .expect("capture"),
            0
        );
        let rows = stateroot_core::observations::load_spool(project.path());
        assert_eq!(rows.len(), 1);
        assert!(rows[0].text.contains("staging bucket"));
        assert!(!rows[0].text.contains("HISTORY_SENTINEL"));
        assert_eq!(rows[0].session_id.as_deref(), Some("ses-1"));
        assert_eq!(rows[0].session_identity.as_deref(), Some("native"));
        // Correction hinting works on the flattened prompt.
        assert_eq!(rows[0].kind_hint.as_deref(), Some("correction"));
        // The archived source payload carries no conversation history either.
        let record = stateroot_core::observations::archive_lookup(
            project.path(),
            rows[0].capture_id.as_deref().unwrap(),
        )
        .unwrap();
        let serialized = serde_json::to_string(&record).unwrap();
        assert!(!serialized.contains("HISTORY_SENTINEL"));
        assert!(serialized.contains("staging bucket"));
    }

    #[tokio::test]
    async fn hermes_child_turn_checkpoint_never_enqueues_boundary_work() {
        let project = tempfile::tempdir().expect("project");
        local_store::init_skeleton(project.path(), "p-test", "proj", "local").expect("skeleton");
        let home = tempfile::tempdir().expect("home");
        let ctx = Ctx {
            cwd: project.path().to_path_buf(),
            config_dir: home.path().to_path_buf(),
            config: stateroot_core::config::AppConfig::default(),
        };
        let quirk = quirk_any("hermes").expect("hermes");
        let journal_dir = project.path().join(".stateroot/local/finalize-journal");
        let count_jobs = || -> usize {
            std::fs::read_dir(&journal_dir)
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                        .count()
                })
                .unwrap_or(0)
        };

        // A delegated child's on_session_end (turn finalization with
        // platform="subagent") checkpoints but never enqueues boundary work.
        let child_payload = json!({
            "hook_event_name": "on_session_end",
            "session_id": "child-ses-1",
            "cwd": project.path(),
            "extra": {
                "platform": "subagent",
                "parent_session_id": "parent-ses-1",
                "completed": true,
                "interrupted": false,
                "model": "hermes-4",
                "turn_id": "t-1"
            }
        });
        let code = checkpoint_from_spool(&ctx, quirk, "stop", project.path(), &child_payload)
            .await
            .expect("child stop");
        assert_eq!(code, 0);
        assert_eq!(count_jobs(), 0, "child never enqueues boundary jobs");

        // The parent's own turn-end DOES enqueue — turn checkpoints are not
        // dropped evidence, and real sessions keep their boundary job.
        let parent_payload = json!({
            "hook_event_name": "on_session_end",
            "session_id": "parent-ses-1",
            "cwd": project.path(),
            "extra": {
                "platform": "cli",
                "parent_session_id": "",
                "completed": true,
                "interrupted": false,
                "turn_id": "t-2"
            }
        });
        checkpoint_from_spool(&ctx, quirk, "stop", project.path(), &parent_payload)
            .await
            .expect("parent stop");
        assert_eq!(count_jobs(), 1, "parent turn enqueues one boundary job");
    }

    // ---- Antigravity native contract regressions -------------------------

    fn journal_jobs(project: &Path) -> Vec<Value> {
        let journal_dir = project.join(".stateroot/local/finalize-journal");
        std::fs::read_dir(&journal_dir)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok())
            .collect()
    }

    fn episodic_notes(project: &Path) -> String {
        std::fs::read_to_string(project.join(".stateroot/memories/episodic.jsonl"))
            .unwrap_or_default()
    }

    #[test]
    fn antigravity_envelope_is_the_native_injectsteps_shape() {
        let digest = "persona line\nObjective: ship the hooks";
        let text = serde_json::to_string(&antigravity_envelope(digest)).expect("serialize");
        // Exact parseable output: one systemMessage step carrying the digest.
        let parsed: Value = serde_json::from_str(&text).expect("parseable JSON");
        let steps = parsed["injectSteps"].as_array().expect("injectSteps array");
        assert_eq!(steps.len(), 1, "{text}");
        assert_eq!(
            steps[0]["systemMessage"]["systemMessage"],
            json!(digest),
            "{text}"
        );
        // The documented shape only — no Claude/Cursor envelope leaks in.
        assert!(parsed.get("hookSpecificOutput").is_none(), "{text}");
        assert!(parsed.get("additional_context").is_none(), "{text}");
        // Unrelated harnesses keep their own channels.
        assert_eq!(
            quirk_any("claude-code").unwrap().injection,
            Injection::StdoutJson
        );
        assert_eq!(
            quirk_any("cursor").unwrap().injection,
            Injection::CursorJson
        );
        assert_eq!(
            quirk_any("gemini-cli").unwrap().injection,
            Injection::StdoutText
        );
        assert_eq!(quirk_any("kimi").unwrap().injection, Injection::StdoutText);
    }

    #[test]
    fn preinvocation_mid_turn_gating() {
        // SessionStart payloads carry no invocationNum → a real boundary.
        assert!(!preinvocation_mid_turn(&json!({})));
        // Legacy hosts without SessionStart open with invocationNum 0.
        assert!(!preinvocation_mid_turn(&json!({"invocationNum": 0})));
        // Every later model invocation is mid-turn noise.
        assert!(preinvocation_mid_turn(&json!({"invocationNum": 1})));
        assert!(preinvocation_mid_turn(&json!({"invocationNum": 19})));
        // Malformed values degrade toward treating the event as a boundary
        // (the scheduler dedupes repeats) rather than dropping a real start.
        assert!(!preinvocation_mid_turn(&json!({"invocationNum": "3"})));
        assert!(!preinvocation_mid_turn(&json!({"invocationNum": -1})));
        assert!(!preinvocation_mid_turn(&json!({"invocationNum": 1.5})));
    }

    #[test]
    fn workspace_paths_project_routing() {
        let base = tempfile::tempdir().expect("base");
        // Spaces and Unicode in mounted workspace names.
        let other = base.path().join("plain workspace");
        std::fs::create_dir(&other).expect("other");
        let project = base.path().join("projet étendu");
        std::fs::create_dir(&project).expect("project");
        local_store::init_skeleton(&project, "p-test", "proj", "local").expect("skeleton");
        let missing = base.path().join("not-here");
        // Multiple roots: the project may be any entry (serde_json escapes
        // the paths — never hand-built JSON).
        let payload = json!({
            "workspacePaths": [
                missing.to_string_lossy(),
                other.to_string_lossy(),
                project.to_string_lossy(),
            ]
        });
        let dir = payload_project_dir(&payload).expect("dir");
        assert_eq!(dir.canonicalize().unwrap(), project.canonicalize().unwrap());
        assert!(find_project_root(&dir).is_some());
        // Without any project, the first existing workspace attaches.
        let payload =
            json!({"workspacePaths": [other.to_string_lossy(), missing.to_string_lossy()]});
        let dir = payload_project_dir(&payload).expect("dir");
        assert_eq!(dir.canonicalize().unwrap(), other.canonicalize().unwrap());
        // Garbage degrades to None, never a guess.
        assert!(payload_project_dir(&json!({"workspacePaths": 42})).is_none());
        assert!(payload_project_dir(&json!({"workspacePaths": []})).is_none());
        assert!(payload_project_dir(&json!({"workspacePaths": ["relative/dir"]})).is_none());
        assert!(
            payload_project_dir(&json!({"workspacePaths": ["/definitely/not/here"]})).is_none()
        );
    }

    #[test]
    fn workspace_paths_windows_json_escaping_round_trips() {
        // A Windows-shaped path must survive the JSON round-trip exactly
        // (serde_json does the escaping; hand-built JSON corrupted these).
        let win = r"C:\Users\Usama\work\projet étendu";
        let text = serde_json::to_string(&json!({"workspacePaths": [win]})).expect("serialize");
        let parsed: Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(parsed["workspacePaths"][0], json!(win));
        // A path that does not exist on this host degrades to None cleanly.
        assert!(payload_project_dir(&parsed).is_none());
    }

    #[tokio::test]
    async fn antigravity_stop_not_fully_idle_defers_finalize_but_keeps_evidence() {
        let project = tempfile::tempdir().expect("project");
        local_store::init_skeleton(project.path(), "p-test", "proj", "local").expect("skeleton");
        let home = tempfile::tempdir().expect("home");
        let ctx = Ctx {
            cwd: project.path().to_path_buf(),
            config_dir: home.path().to_path_buf(),
            config: stateroot_core::config::AppConfig::default(),
        };
        let quirk = quirk_any("antigravity").expect("antigravity");
        let payload = json!({
            "conversationId": "conv-nonidle",
            "executionNum": 1,
            "terminationReason": "model_stop",
            "fullyIdle": false,
            "transcriptPath": "/tmp/transcript.jsonl",
            "event_id": "stop-nonidle-1",
        });
        let code = checkpoint_from_spool(&ctx, quirk, "stop", project.path(), &payload)
            .await
            .expect("stop");
        assert_eq!(code, 0);
        // Still-running background work is not sealed as a completed session.
        assert!(
            journal_jobs(project.path()).is_empty(),
            "non-idle stop must not enqueue a boundary job"
        );
        // Evidence stays durable and honest about the termination state.
        let notes = episodic_notes(project.path());
        assert!(notes.contains("stop via antigravity hook"), "{notes}");
        assert!(notes.contains("fullyIdle=false"), "{notes}");
        assert!(notes.contains("terminationReason=model_stop"), "{notes}");
        assert!(!notes.contains("successful"), "{notes}");
    }

    #[tokio::test]
    async fn antigravity_idle_error_stop_enqueues_once_per_occurrence() {
        let project = tempfile::tempdir().expect("project");
        local_store::init_skeleton(project.path(), "p-test", "proj", "local").expect("skeleton");
        let home = tempfile::tempdir().expect("home");
        let ctx = Ctx {
            cwd: project.path().to_path_buf(),
            config_dir: home.path().to_path_buf(),
            config: stateroot_core::config::AppConfig::default(),
        };
        let quirk = quirk_any("antigravity").expect("antigravity");
        let transcript =
            "/home/u/.gemini/antigravity/brain/conv-err/.system_generated/logs/transcript.jsonl";
        let payload = json!({
            "conversationId": "conv-err",
            "terminationReason": "error",
            "error": "model exploded: quota",
            "fullyIdle": true,
            "transcriptPath": transcript,
            "event_id": "stop-err-1",
        });
        for _ in 0..2 {
            // Replay of the same boundary occurrence.
            let code = checkpoint_from_spool(&ctx, quirk, "stop", project.path(), &payload)
                .await
                .expect("stop");
            assert_eq!(code, 0);
        }
        // Abnormal-but-idle stops are retained through the durable journal —
        // exactly once per boundary occurrence.
        let jobs = journal_jobs(project.path());
        assert_eq!(jobs.len(), 1, "replay dedupes by occurrence: {jobs:?}");
        assert_eq!(jobs[0]["transcript"], json!(transcript));
        let notes = episodic_notes(project.path());
        assert!(notes.contains("terminationReason=error"), "{notes}");
        assert!(notes.contains("fullyIdle=true"), "{notes}");
        assert!(notes.contains("error=model exploded: quota"), "{notes}");
    }

    #[tokio::test]
    async fn antigravity_abnormal_stop_without_idle_state_still_enqueues() {
        let project = tempfile::tempdir().expect("project");
        local_store::init_skeleton(project.path(), "p-test", "proj", "local").expect("skeleton");
        let home = tempfile::tempdir().expect("home");
        let ctx = Ctx {
            cwd: project.path().to_path_buf(),
            config_dir: home.path().to_path_buf(),
            config: stateroot_core::config::AppConfig::default(),
        };
        let quirk = quirk_any("antigravity").expect("antigravity");
        // An interrupted stop whose fullyIdle never arrived: absent is not
        // "still running" — the evidence must not be discarded.
        let payload = json!({
            "conversationId": "conv-maxsteps",
            "terminationReason": "max_steps_exceeded",
            "event_id": "stop-max-1",
        });
        let code = checkpoint_from_spool(&ctx, quirk, "stop", project.path(), &payload)
            .await
            .expect("stop");
        assert_eq!(code, 0);
        assert_eq!(journal_jobs(project.path()).len(), 1);
        let notes = episodic_notes(project.path());
        assert!(
            notes.contains("terminationReason=max_steps_exceeded"),
            "{notes}"
        );
    }

    #[tokio::test]
    async fn antigravity_stop_malformed_fields_degrade_to_finalize() {
        let project = tempfile::tempdir().expect("project");
        local_store::init_skeleton(project.path(), "p-test", "proj", "local").expect("skeleton");
        let home = tempfile::tempdir().expect("home");
        let ctx = Ctx {
            cwd: project.path().to_path_buf(),
            config_dir: home.path().to_path_buf(),
            config: stateroot_core::config::AppConfig::default(),
        };
        let quirk = quirk_any("antigravity").expect("antigravity");
        // Wrong-typed fields: not an explicit "still running" signal, so the
        // boundary proceeds and nothing panics.
        let payload = json!({
            "conversationId": "conv-malformed",
            "fullyIdle": "no",
            "terminationReason": 7,
            "event_id": "stop-mal-1",
        });
        let code = checkpoint_from_spool(&ctx, quirk, "stop", project.path(), &payload)
            .await
            .expect("stop");
        assert_eq!(code, 0);
        assert_eq!(journal_jobs(project.path()).len(), 1);
    }

    // -----------------------------------------------------------------
    // M3 delivery-structure contract: silent decisions build NO full
    // context, a full delivery builds it AT MOST once, and session
    // maintenance is eligible at the boundary whether or not output
    // printed. Measured by the thread-local counter seams, never timing.
    // -----------------------------------------------------------------

    /// Hermetic resume_output fixture: temp home/config/project, cursor
    /// quirk (native JSON injection, session_start prints+marks).
    struct DeliveryFixture {
        _home: tempfile::TempDir,
        _config: tempfile::TempDir,
        project: tempfile::TempDir,
        ctx: Ctx,
        prior_home: Option<std::ffi::OsString>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl DeliveryFixture {
        fn new() -> Self {
            let guard = crate::test_env::env_lock();
            let home = tempfile::tempdir().expect("home");
            let config = tempfile::tempdir().expect("config");
            let project = tempfile::tempdir().expect("project");
            local_store::init_skeleton(project.path(), "p-test", "proj", "local")
                .expect("skeleton");
            std::fs::write(
                config
                    .path()
                    .join(super::super::persona::PERSONA_CACHE_FILE),
                "## Persona\n\nYou are YinYue.\n",
            )
            .expect("persona");
            let prior_home = std::env::var_os("STATEROOT_TEST_HOME");
            unsafe {
                std::env::set_var("STATEROOT_TEST_HOME", home.path());
            }
            let ctx = Ctx {
                cwd: project.path().to_path_buf(),
                config_dir: config.path().to_path_buf(),
                config: stateroot_core::config::AppConfig::default(),
            };
            reset_delivery_counters();
            Self {
                _home: home,
                _config: config,
                project,
                ctx,
                prior_home,
                _guard: guard,
            }
        }

        fn payload(&self) -> Value {
            json!({"session_id": "m3-counter-fixture", "cwd": self.project.path()})
        }
    }

    impl Drop for DeliveryFixture {
        fn drop(&mut self) {
            unsafe {
                match &self.prior_home {
                    Some(prior) => std::env::set_var("STATEROOT_TEST_HOME", prior),
                    None => std::env::remove_var("STATEROOT_TEST_HOME"),
                }
            }
        }
    }

    #[tokio::test]
    async fn full_delivery_builds_context_at_most_once() {
        let fixture = DeliveryFixture::new();
        let quirk = quirk_any("cursor").expect("cursor quirk");
        resume_output(
            &fixture.ctx,
            quirk,
            "session_start",
            fixture.project.path(),
            &fixture.payload(),
        )
        .await
        .expect("resume_output");
        assert_eq!(
            observed_digest_builds(),
            1,
            "a full delivery builds the full context AT MOST once"
        );
    }

    #[tokio::test]
    async fn silent_repeat_builds_no_context() {
        let fixture = DeliveryFixture::new();
        let quirk = quirk_any("cursor").expect("cursor quirk");
        let payload = fixture.payload();
        // First delivery settles the session.
        resume_output(
            &fixture.ctx,
            quirk,
            "session_start",
            fixture.project.path(),
            &payload,
        )
        .await
        .expect("first");
        reset_delivery_counters();
        // The unchanged repeat is deduped to silent — no build at all.
        resume_output(
            &fixture.ctx,
            quirk,
            "session_start",
            fixture.project.path(),
            &payload,
        )
        .await
        .expect("repeat");
        assert_eq!(
            observed_digest_builds(),
            0,
            "a silent decision must perform ZERO full digest builds"
        );
    }

    #[tokio::test]
    async fn silent_repeat_still_runs_session_maintenance() {
        let fixture = DeliveryFixture::new();
        let quirk = quirk_any("cursor").expect("cursor quirk");
        let payload = fixture.payload();
        resume_output(
            &fixture.ctx,
            quirk,
            "session_start",
            fixture.project.path(),
            &payload,
        )
        .await
        .expect("first");
        reset_delivery_counters();
        resume_output(
            &fixture.ctx,
            quirk,
            "session_start",
            fixture.project.path(),
            &payload,
        )
        .await
        .expect("repeat");
        assert_eq!(
            observed_maintenance_runs(),
            1,
            "a silent session_start must still run its boundary maintenance"
        );
    }
}
