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
//!   append one observation verbatim to `.stateroot/spool/observations.jsonl`
//!   (256KB rotation), fire-and-forget.
//! - checkpoint (`tool_failure`, `pre_compact`, `post_compaction`, `stop`,
//!   `session_end`): checkpoint from the spool tail into the local episodic
//!   log; `stop`/`session_end` preserve any explicit structured handoff
//!   (automatic paths never rewrite `handoffs/current.json`), then rotate
//!   the spool.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use stateroot_core::digest_delivery::{self, DeliveryChannel, DeliveryIntent};
use stateroot_core::harness_install::registry::{
    self, event_kind, normalize_event, quirk_any, EventKind, Injection,
};
use stateroot_core::local_store;
use stateroot_core::local_store::now_rfc3339;

use super::{note, truncate, Ctx};

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
    local_store::find_project_root(start)
}

/// The agent's working directory from a hook payload, when the harness sends
/// one: `cwd` / `project_dir` / `workspace_dir` string fields, or the first
/// `workspace_roots` entry (cursor's shape). Translates Windows ↔ WSL forms
/// so a payload from the other OS still attaches. Returns None on anything odd.
fn payload_project_dir(payload: &Value) -> Option<PathBuf> {
    for key in ["cwd", "project_dir", "projectDir", "workspace_dir"] {
        if let Some(dir) = payload.get(key).and_then(|v| v.as_str()) {
            if let Some(path) = stateroot_core::path_identity::resolve_existing_dir(Path::new(dir))
            {
                return Some(path);
            }
        }
    }
    payload
        .get("workspace_roots")
        .and_then(|v| v.as_array())
        .and_then(|roots| roots.first())
        .and_then(|v| v.as_str())
        .map(Path::new)
        .and_then(stateroot_core::path_identity::resolve_existing_dir)
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
    // registry.
    let payload_root = payload_project_dir(&payload).and_then(|d| find_project_root(&d));
    let kimi_root = || kimi_session_project(quirk, &payload);
    let project_dir = payload_root.or_else(|| {
        kimi_root().or_else(|| {
            find_project_root(&ctx.cwd).or_else(|| {
                ctx.current_project()
                    .ok()
                    .flatten()
                    .and_then(|_| ctx.cwd.canonicalize().ok())
                    .filter(|cwd| local_store::is_stateroot_dir(cwd))
            })
        })
    });
    let Some(kind) = event_kind(canonical) else {
        return Ok(0);
    };

    match kind {
        EventKind::Resume => match project_dir.as_ref() {
            Some(project_dir) => {
                if let Err(err) = super::active_harness::record(project_dir, quirk.id) {
                    note!("warning: could not record active harness: {err}");
                }
                resume_output(ctx, quirk, canonical, project_dir, &payload).await
            }
            None => resume_identity_only(ctx, quirk, canonical, &payload),
        },
        EventKind::Capture => match project_dir.as_ref() {
            Some(project_dir) => {
                if let Err(err) = super::active_harness::record(project_dir, quirk.id) {
                    note!("warning: could not record active harness: {err}");
                }
                let code = capture_observation(quirk, canonical, project_dir, &payload)?;
                let prompt_injects =
                    canonical == "user_prompt_submit" && quirk.delivery().prompt_submit_injects;
                let cursor_reanchors = canonical == "post_tool_use"
                    && quirk.id == "cursor"
                    && cursor_post_tool_needs_identity(ctx, quirk, project_dir, &payload);
                if prompt_injects || cursor_reanchors {
                    resume_output(ctx, quirk, canonical, project_dir, &payload).await?;
                }
                Ok(code)
            }
            None if canonical == "user_prompt_submit" && quirk.delivery().prompt_submit_injects => {
                resume_identity_only(ctx, quirk, canonical, &payload)
            }
            None => Ok(0),
        },
        EventKind::Checkpoint => match project_dir.as_ref() {
            Some(project_dir) => {
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
                            note!("plan sync: ingested {line}");
                        }
                        for line in &report.updated {
                            note!("plan sync: updated {line}");
                        }
                        for line in &report.completed {
                            note!("plan sync: completed {line}");
                        }
                        for line in &report.notes {
                            note!("plan sync: {line}");
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
                    if let Some(digest) = scheduled_identity_output(
                        &ctx.config_dir,
                        quirk,
                        canonical,
                        project_dir,
                        &payload,
                        &identity,
                        &|identity| {
                            hook_digest_with_identity(
                                &ctx.config_dir,
                                project_dir,
                                quirk.id,
                                identity,
                            )
                        },
                    ) {
                        print_hook_injection(quirk, canonical, &digest);
                    }
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
    let digest = scheduled_identity_output(
        &ctx.config_dir,
        quirk,
        canonical,
        &ctx.cwd,
        payload,
        &digest,
        &|identity| Some(identity.to_string()),
    );
    let Some(digest) = digest else {
        return Ok(0);
    };
    match quirk.injection {
        Injection::None => Ok(0),
        Injection::StdoutJson
        | Injection::CursorJson
        | Injection::StdoutText
        | Injection::McpPull
        | Injection::UserPromptSubmit => {
            print_hook_injection(quirk, canonical, &digest);
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

/// The scheduler's answer for one resume/identity event: what (if anything)
/// the hook prints. FULL → the digest as-is; COMPRESSED → the pointer;
/// NOTHING → None. State lives in the user-global local dir.
fn scheduled_identity_output(
    config_dir: &Path,
    quirk: &registry::HarnessQuirk,
    canonical: &str,
    project_dir: &Path,
    payload: &Value,
    identity: &str,
    digest_builder: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let home =
        stateroot_core::harness_install::home_dir().unwrap_or_else(|_| config_dir.to_path_buf());
    let hash = stateroot_core::persona_injection::content_hash(identity);
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
    let decision = stateroot_core::persona_injection::decide_and_record(
        &home,
        &key,
        canonical,
        &hash,
        hook_now(),
        mark,
        deliverable,
    );
    match decision {
        stateroot_core::persona_injection::Decision::Full => digest_builder(identity),
        stateroot_core::persona_injection::Decision::Compressed => {
            let name = stateroot_core::persona_injection::persona_name(identity);
            let tagline = stateroot_core::persona_injection::persona_tagline(identity);
            let pointer = stateroot_core::persona_injection::compressed_pointer(
                &name,
                &tagline,
                &home
                    .join(stateroot_core::soul::SOUL_DIR)
                    .join(stateroot_core::soul::CANONICAL_FILE),
            );
            digest_builder(&pointer)
        }
        stateroot_core::persona_injection::Decision::Nothing => None,
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

/// [`hook_digest`] with an explicit identity block (the scheduler decides
/// what the identity section carries: full prefix, compressed pointer, or
/// nothing).
pub fn hook_digest_with_identity(
    config_dir: &Path,
    project_dir: &Path,
    harness_id: &str,
    identity: &str,
) -> Option<String> {
    let _ = (config_dir, harness_id);
    let identity = identity.to_string();
    let handoff = local_store::read_handoff_local(project_dir).ok().flatten();
    let mut work = String::new();
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
    }
    // Empty check only — the scheduler decides the printable content below.
    if hook_digest(&ctx.config_dir, project_dir, quirk.id).is_none() {
        return Ok(0); // nothing worth injecting — silent
    }
    if !identity_event_prints(quirk, canonical) {
        return Ok(0);
    }
    if quirk.injection == Injection::None {
        return Ok(0);
    }
    // Persona injection scheduler (authoritative for what prints): FULL on
    // boundaries/change/first-call, COMPRESSED on the 15-prompt cadence,
    // NOTHING inside the dedupe window or off-cycle.
    let home =
        stateroot_core::harness_install::home_dir().unwrap_or_else(|_| ctx.config_dir.clone());
    let identity = hook_identity_prefix(&ctx.config_dir, &home, Some(project_dir), quirk.id);
    let digest = scheduled_identity_output(
        &ctx.config_dir,
        quirk,
        canonical,
        project_dir,
        payload,
        &identity,
        &|identity| hook_digest_with_identity(&ctx.config_dir, project_dir, quirk.id, identity),
    );
    let Some(digest) = digest else {
        return Ok(0);
    };
    print_hook_injection(quirk, canonical, &digest);
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
    }
    // Heavy session-boundary work runs AFTER the digest is printed: harnesses
    // kill slow hooks (cursor's default timeout), and a killed process must
    // not take the injection with it. All of it is best-effort and fresh
    // enough one step behind (discovery reads sources live anyway).
    if canonical == "session_start" {
        // Skill federation sync (best-effort, never fails the hook).
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
        // MCP federation (pull + project; never fails the hook).
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
    Ok(0)
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
    Ok(0)
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
        // Ingest is local but slow on Windows/WSL mounts (wiki + inbox rewrite).
        // `stop` can pay that cost between turns. `session_end` runs while
        // Cursor is closing the window — skip it so shutdown is not blocked
        // for the full hook timeout. Next `stop` or `wiki compile` catches up.
        if canonical == "stop" {
            match super::compiler::try_ingest(&hook_ctx, false).await {
                Ok(summary) => note!("{summary}"),
                Err(err) => note!("ingest skipped: {err}"),
            }
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
    fn identity_only_digest_needs_no_project() {
        let _guard = TEST_HOME_ENV.lock().expect("env lock");
        let home = tempfile::tempdir().expect("home");
        std::fs::write(
            home.path().join("persona.md"),
            "## Working relationship\n\nYou are Yinyue.\n",
        )
        .expect("persona");
        let prior = std::env::var("STATEROOT_TEST_HOME").ok();
        // SAFETY: serialized by TEST_HOME_ENV.
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
}
