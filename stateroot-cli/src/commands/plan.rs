//! `stateroot plan` — central plan artifacts + lifecycle.
//!
//! A strong model in one harness authors a plan; the store keeps it with
//! provenance; approval/activation moves it through the lifecycle; another
//! harness's digest points at the file with an executor directive. No
//! runtime enforcement in v1 — strings above the runtime.

use serde_json::json;
use stateroot_core::local_store::{self, now_rfc3339};
use stateroot_core::plans::{self, PlanStatus};

use super::{note, Ctx};

/// Resolve the harness id for lineage: `--from`, else the active local
/// marker, else `cli`.
fn harness(ctx: &Ctx, from: Option<&str>) -> String {
    from.map(str::to_string)
        .or_else(|| super::active_harness::read(&ctx.cwd).ok().flatten())
        .unwrap_or_else(|| "cli".to_string())
}

/// Episodic lineage note for one plan lifecycle event.
fn episodic(ctx: &Ctx, note_text: &str) -> anyhow::Result<()> {
    let record = json!({
        "ts": now_rfc3339(),
        "harness": "cli",
        "note": note_text,
        "files": [],
    });
    local_store::append_episodic(&ctx.cwd, &record)?;
    Ok(())
}

/// Title fallback: the body's first markdown heading, then the file stem.
fn derive_title(body: &str, file: Option<&str>) -> Option<String> {
    for line in body.lines() {
        let heading = line.trim().strip_prefix("# ").map(str::trim);
        if let Some(heading) = heading {
            if !heading.is_empty() {
                return Some(heading.to_string());
            }
        }
    }
    file.and_then(|f| {
        std::path::Path::new(f)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
    })
}

/// Run `stateroot plan record`.
pub fn record(
    ctx: &Ctx,
    file: Option<&str>,
    stdin: bool,
    title: Option<&str>,
    from: Option<&str>,
) -> anyhow::Result<()> {
    ctx.require_project()?;
    let (body, source_path) = if stdin {
        let mut body = String::new();
        use std::io::Read as _;
        std::io::stdin().read_to_string(&mut body)?;
        (body, None)
    } else {
        let Some(file) = file else {
            anyhow::bail!("pass --file <path> or --stdin");
        };
        let body = std::fs::read_to_string(file)
            .map_err(|e| anyhow::anyhow!("read plan file {file}: {e}"))?;
        (body, Some(file.to_string()))
    };
    let title = title
        .map(str::to_string)
        .or_else(|| derive_title(&body, source_path.as_deref()))
        .unwrap_or_default();
    let author = harness(ctx, from);
    let meta = plans::record(&ctx.cwd, &title, &author, source_path.as_deref(), &body)
        .map_err(|e| anyhow::anyhow!(e))?;
    println!(
        "recorded plan {} (draft) — .stateroot/plans/{}.md",
        meta.id, meta.id
    );
    episodic(
        ctx,
        &format!("plan {} recorded: {} ({author})", meta.id, meta.title),
    )?;
    Ok(())
}

/// Run `stateroot plan list`.
pub fn list(ctx: &Ctx) -> anyhow::Result<()> {
    ctx.require_project()?;
    let all = plans::list(&ctx.cwd);
    if all.is_empty() {
        println!("no plans recorded — `stateroot plan record --file <path>`");
        return Ok(());
    }
    for meta in &all {
        println!(
            "{} · {} · {} · {} · {}",
            meta.id,
            meta.title,
            meta.status,
            meta.created_by_harness,
            meta.updated_at.get(..10).unwrap_or(&meta.updated_at)
        );
    }
    Ok(())
}

/// Run `stateroot plan show <id>` — the verbatim markdown to stdout.
pub fn show(ctx: &Ctx, id: &str) -> anyhow::Result<()> {
    ctx.require_project()?;
    let Some((meta, path)) = plans::load(&ctx.cwd, id) else {
        anyhow::bail!("unknown plan `{id}` — run `stateroot plan list`");
    };
    let body = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    print!("{body}");
    if !body.ends_with('\n') {
        println!();
    }
    note!(
        "({} · {} · by {})",
        meta.id,
        meta.status,
        meta.created_by_harness
    );
    Ok(())
}

/// One lifecycle transition (approve/activate/done/abandon) + lineage.
fn transition(ctx: &Ctx, id: &str, to: PlanStatus) -> anyhow::Result<()> {
    ctx.require_project()?;
    let (meta, demoted) = plans::transition(&ctx.cwd, id, to).map_err(|e| anyhow::anyhow!(e))?;
    let status = to.as_str();
    if let Some(demoted) = demoted {
        println!(
            "plan {} → {status} ({demoted} demoted to approved)",
            meta.id
        );
    } else {
        println!("plan {} → {status}", meta.id);
    }
    let actor = harness(ctx, None);
    episodic(ctx, &format!("plan {} {status} by {actor}", meta.id))?;
    Ok(())
}

/// Run `stateroot plan approve <id>`.
pub fn approve(ctx: &Ctx, id: &str) -> anyhow::Result<()> {
    transition(ctx, id, PlanStatus::Approved)
}

/// Run `stateroot plan activate <id>`.
pub fn activate(ctx: &Ctx, id: &str) -> anyhow::Result<()> {
    transition(ctx, id, PlanStatus::Active)
}

/// Run `stateroot plan done <id>`.
pub fn done(ctx: &Ctx, id: &str) -> anyhow::Result<()> {
    transition(ctx, id, PlanStatus::Done)
}

/// Run `stateroot plan abandon <id>`.
pub fn abandon(ctx: &Ctx, id: &str) -> anyhow::Result<()> {
    transition(ctx, id, PlanStatus::Abandoned)
}

/// Run `stateroot plan sync` — pull harness-native plans (Cursor, Claude,
/// Kimi) into the store as drafts. The explicit pass; session boundaries
/// also run it per harness on an interval.
pub fn sync(ctx: &Ctx) -> anyhow::Result<()> {
    ctx.require_project()?;
    let home = stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))?;
    let mut any = false;
    for harness in ["cursor", "claude-code", "kimi-code"] {
        let report = stateroot_core::plan_federation::sync_from(&home, &ctx.cwd, harness);
        for line in &report.ingested {
            any = true;
            println!("ingested {line}");
        }
        for line in &report.updated {
            any = true;
            println!("updated {line}");
        }
        for line in &report.notes {
            super::note!("plan sync: {line}");
        }
    }
    if !any {
        println!("no new harness-native plans");
    }
    Ok(())
}
