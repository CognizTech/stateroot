//! `stateroot session` — the canonical session store: sync pi/DSH sessions
//! into `.stateroot/local/sessions/`, list and show them. Transfer (M2)
//! writes a canonical session back out as a real native session file.

use serde_json::json;
use stateroot_core::local_store::{self, now_rfc3339};
use stateroot_core::sessions;

use super::{truncate, Ctx};

/// Display caps for `show`/`list` (the store itself is never capped).
const DISPLAY_CAP: usize = 200;
/// How many tail entries `show` prints.
const SHOW_TAIL: usize = 5;

fn per_harness_suffix(report: &sessions::SyncReport) -> String {
    report
        .per_harness
        .iter()
        .map(|(h, n)| format!("{h}: {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Run `stateroot session sync`.
pub fn sync(ctx: &Ctx, harness: Option<&str>) -> anyhow::Result<()> {
    ctx.require_project()?;
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))?;
    let report = sessions::import_from_readers_filtered(&home, &ctx.cwd, harness);
    let summary = format!(
        "session sync: {} sessions canonicalized ({})",
        report.written,
        per_harness_suffix(&report)
    );
    println!("{summary}");
    if report.skipped_zstd > 0 {
        println!(
            "  ({} zstd-compressed dsh log(s) skipped — no zstd support in v1)",
            report.skipped_zstd
        );
    }
    if report.written > 0 {
        let record = json!({
            "ts": now_rfc3339(),
            "harness": "cli",
            "note": summary,
            "files": [],
        });
        local_store::append_episodic(&ctx.cwd, &record)?;
    }
    Ok(())
}

/// Run `stateroot session list`.
pub fn list(ctx: &Ctx, harness: Option<&str>) -> anyhow::Result<()> {
    ctx.require_project()?;
    let mut all = sessions::list(&ctx.cwd);
    if let Some(harness) = harness {
        all.retain(|s| s.harness == harness);
    }
    if all.is_empty() {
        println!("no canonical sessions — run `stateroot session sync` first");
        return Ok(());
    }
    for session in &all {
        let (first, last, outcome) = sessions::summarize_stored(session);
        let span = format!(
            "{} → {}",
            first.get(..10).unwrap_or(&first),
            last.get(..10).unwrap_or(&last)
        );
        println!(
            "{} {:<36} {:<24} {:>3} entries · {:<11} {}",
            session.harness,
            session.session_id,
            span,
            session.entries.len(),
            outcome,
            truncate(&session.cwd, 40),
        );
    }
    Ok(())
}

/// Run `stateroot session show <id>`.
pub fn show(ctx: &Ctx, id: &str) -> anyhow::Result<()> {
    ctx.require_project()?;
    let Some(session) = sessions::load(&ctx.cwd, id) else {
        anyhow::bail!("no canonical session matches `{id}` — run `stateroot session list`");
    };
    let (first, last, outcome) = sessions::summarize_stored(&session);
    println!(
        "session {} ({}) — {}",
        session.session_id, session.harness, session.cwd
    );
    println!(
        "  imported {} from {}",
        session.imported_at, session.source_path
    );
    println!(
        "  entries: {} · span {} → {} · outcome {outcome}",
        session.entries.len(),
        first,
        last
    );
    if let Some(first_user) = session
        .entries
        .iter()
        .find(|e| e.kind == "message" && e.role.as_deref() == Some("user"))
        .and_then(|e| e.content.as_deref())
    {
        println!(
            "\nfirst user message:\n  {}",
            truncate(first_user, DISPLAY_CAP)
        );
    }
    let tail: Vec<String> = session
        .entries
        .iter()
        .rev()
        .take(SHOW_TAIL)
        .map(display_line)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if !tail.is_empty() {
        println!("\nlast entries:");
        for line in tail {
            println!("  {line}");
        }
    }
    Ok(())
}

/// One capped display line for an entry (`#seq type role/name: content…`).
fn display_line(entry: &sessions::CanonicalEntry) -> String {
    let label = match entry.kind.as_str() {
        "message" => entry.role.clone().unwrap_or_default(),
        "tool_call" | "tool_result" => {
            format!("{} {}", entry.kind, entry.name.as_deref().unwrap_or("tool"))
        }
        other => match &entry.native_type {
            Some(native) => format!("{other} {native}"),
            None => other.to_string(),
        },
    };
    let content = entry.content.as_deref().unwrap_or("");
    format!(
        "#{} {}: {}",
        entry.seq,
        label,
        truncate(content, DISPLAY_CAP)
    )
}

/// Run `stateroot session transfer <id> --to pi|dsh [--dry-run]`.
pub fn transfer(ctx: &Ctx, id: &str, to: &str, dry_run: bool) -> anyhow::Result<()> {
    ctx.require_project()?;
    let Some(session) = sessions::load(&ctx.cwd, id) else {
        anyhow::bail!("no canonical session matches `{id}` — run `stateroot session list`");
    };
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))?;
    let new_id = uuid::Uuid::new_v4().to_string();
    let plan = match to {
        "pi" => sessions::transfer::plan_pi(&session, &home, &ctx.cwd, &new_id),
        "dsh" => sessions::transfer::plan_dsh(&session, &home, &ctx.cwd, &new_id),
        other => anyhow::bail!("unknown transfer target '{other}' — supported: pi, dsh"),
    }
    .map_err(|e| anyhow::anyhow!(e))?;

    if dry_run {
        println!("would transfer session {} → {to}", session.session_id);
        println!("  entries: {}", plan.fidelity.line());
        println!("  would write: {}", plan.target_path.display());
        println!("  resume with: {}", plan.resume_hint);
        return Ok(());
    }
    sessions::transfer::write(&plan).map_err(|e| anyhow::anyhow!(e))?;
    println!("transferred session {} → {to}", session.session_id);
    println!("  entries: {}", plan.fidelity.line());
    println!("  wrote: {}", plan.target_path.display());
    println!("  resume with: {}", plan.resume_hint);
    let record = json!({
        "ts": now_rfc3339(),
        "harness": "cli",
        "note": format!(
            "session transfer: {} → {to} ({} native · {} adapted · {} dropped)",
            session.session_id, plan.fidelity.native, plan.fidelity.adapted, plan.fidelity.dropped
        ),
        "files": [plan.target_path.display().to_string()],
    });
    local_store::append_episodic(&ctx.cwd, &record)?;
    Ok(())
}
