//! `stateroot observations list|show|search` — read-only spool audit surface.

use stateroot_core::observations::{self, ObservationFilter};

use super::Ctx;

/// `stateroot observations list`
pub fn list(
    ctx: &Ctx,
    kind: Option<&str>,
    harness: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
    limit: usize,
) -> anyhow::Result<()> {
    ctx.require_project()?;
    let rows = observations::filter_spool(
        &ctx.cwd,
        &ObservationFilter {
            kind: kind.map(str::to_string),
            harness: harness.map(str::to_string),
            since: since.map(str::to_string),
            until: until.map(str::to_string),
            query: None,
            limit,
        },
    );
    if rows.is_empty() {
        println!("no observations matched");
        return Ok(());
    }
    for row in rows {
        print_row(&row);
    }
    Ok(())
}

/// `stateroot observations show <id>`
pub fn show(ctx: &Ctx, id: &str) -> anyhow::Result<()> {
    ctx.require_project()?;
    let Some(row) = observations::get_observation(&ctx.cwd, id) else {
        anyhow::bail!("observation not found: {id}");
    };
    print_row(&row);
    if !row.text.is_empty() {
        println!("\n---\n{}", row.text);
    }
    Ok(())
}

/// `stateroot observations search <query>`
pub fn search(
    ctx: &Ctx,
    query: &str,
    kind: Option<&str>,
    harness: Option<&str>,
    limit: usize,
) -> anyhow::Result<()> {
    ctx.require_project()?;
    let rows = observations::filter_spool(
        &ctx.cwd,
        &ObservationFilter {
            kind: kind.map(str::to_string),
            harness: harness.map(str::to_string),
            since: None,
            until: None,
            query: Some(query.to_string()),
            limit,
        },
    );
    if rows.is_empty() {
        println!("no observations matched");
        return Ok(());
    }
    for row in rows {
        print_row(&row);
    }
    Ok(())
}

fn print_row(row: &stateroot_core::observations::Observation) {
    let scope = row
        .scope_status
        .as_deref()
        .map(|s| format!(" scope={s}"))
        .unwrap_or_default();
    println!(
        "{}  {}  {}  {}{}",
        row.id, row.ts, row.harness, row.event, scope
    );
    if let Some(kind) = row.kind_hint.as_deref() {
        println!("  kind: {kind}");
    }
    if let Some(tool) = row.tool.as_deref() {
        println!("  tool: {tool}");
    }
    let preview = row
        .excerpt
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(row.text.as_str());
    let preview = preview.chars().take(160).collect::<String>();
    if !preview.is_empty() {
        println!("  {preview}");
    }
}
