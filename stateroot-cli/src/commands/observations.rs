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
    match observations::resolve(&ctx.cwd, id) {
        observations::ObservationLookup::Found(row) => {
            print_row(&row);
            if let Some(conflict_with) = row.conflict_with.as_deref() {
                println!(
                    "  CONFLICT: same event identity as {conflict_with} with different content — both bodies retained"
                );
            }
            if let Some(session) = row.session_id.as_deref() {
                let identity = row.session_identity.as_deref().unwrap_or("unknown");
                println!("  session: {session} ({identity})");
            }
            if let Some(digest) = row.text_digest.as_deref() {
                println!("  text-digest: {digest}");
            }
            // The raw captured source is available for v2 records.
            if let Some(capture_id) = row.capture_id.as_deref() {
                if let Some(record) = observations::archive_lookup(&ctx.cwd, capture_id) {
                    if let Some(source) = record.source.as_ref() {
                        println!(
                            "  source: {} bytes, digest {} ({})",
                            source.bytes, source.digest, record.capture.source_status
                        );
                    } else {
                        println!("  source: unavailable ({})", record.capture.source_status);
                    }
                }
            }
            if !row.text.is_empty() {
                println!("\n---\n{}", row.text);
            }
        }
        observations::ObservationLookup::Unavailable(reason) => {
            anyhow::bail!("observation unavailable: {reason}");
        }
        observations::ObservationLookup::NotFound => {
            anyhow::bail!("observation not found: {id}");
        }
    }
    Ok(())
}

/// `stateroot observations health` — capture/retention/corruption facts.
pub fn health(ctx: &Ctx) -> anyhow::Result<()> {
    ctx.require_project()?;
    let health = observations::health(&ctx.cwd);
    println!(
        "segments: {} ({} sealed, {} pending-seal)",
        health.segments,
        health.sealed,
        health.pending_seal.len()
    );
    for name in &health.pending_seal {
        println!("  pending-seal: {name}");
    }
    println!(
        "records: {} captured/conflict · {} replay sighting(s) · {} conflict(s) · {} source-unavailable",
        health.records, health.replays, health.conflicts, health.source_unavailable
    );
    println!("legacy rows: {}", health.legacy_rows);
    if health.corrupt.is_empty() {
        println!("corrupt/torn lines: none");
    } else {
        println!("corrupt/torn lines: {}", health.corrupt.len());
        for line in &health.corrupt {
            println!(
                "  corrupt: {}:{} — {} ({} bytes preserved)",
                line.file,
                line.line_no,
                line.reason,
                line.raw.len()
            );
        }
    }
    if !health.read_failures.is_empty() {
        println!("unreadable files: {}", health.read_failures.len());
        for failure in &health.read_failures {
            println!("  unreadable: {} — {}", failure.file, failure.error);
        }
    }
    if !health.missing_frontier.is_empty() {
        println!(
            "segments missing frontier (recoverable): {}",
            health.missing_frontier.len()
        );
        for name in &health.missing_frontier {
            println!("  missing-frontier: {name}");
        }
    }
    let watermark = &health.watermark;
    match (&watermark.last_capture_id, &watermark.last_ts) {
        (Some(id), Some(ts)) => println!(
            "watermark: {} record(s), last {id} at {ts} ({})",
            watermark.records,
            watermark.segment.as_deref().unwrap_or("?")
        ),
        _ => println!("watermark: no durable records yet"),
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
    let status = match row.status.as_str() {
        "captured" | "legacy" => String::new(),
        other => format!(" status={other}"),
    };
    println!(
        "{}  {}  {}  {}{}{}",
        row.id, row.ts, row.harness, row.event, scope, status
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
