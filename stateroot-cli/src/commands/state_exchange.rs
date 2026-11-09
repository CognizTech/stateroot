use super::{note, stdin_is_tty, Ctx};
use crate::cli::ExchangeArgs;
use stateroot_core::state_exchange::{self, ExchangeReport};
use std::path::{Path, PathBuf};

fn absolute(ctx: &Ctx, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        ctx.cwd.join(path)
    }
}

fn print(report: &ExchangeReport, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else {
        println!("{} {}", report.operation, report.root_oid);
        println!("project: {}", report.project_id);
        println!(
            "objects: {} · artifacts: {} · auxiliary refs: {} · omissions: {}",
            report.object_count,
            report.artifact_count,
            report.auxiliary_count,
            report.omission_count
        );
        println!(
            "evidence: {}",
            if report.full_evidence {
                "full"
            } else {
                "partial"
            }
        );
        println!("destination: {}", report.destination);
        if let Some(previous) = &report.previous_root {
            println!("previous root: {previous}");
        }
        if let Some(lineage) = &report.lineage_ref {
            println!("lineage: {lineage}");
        }
        println!("native history: {}", report.native_history);
        println!(
            "environment reproduction: {}",
            report.environment_reproduction
        );
    }
    Ok(())
}

pub async fn run(ctx: &Ctx, args: ExchangeArgs) -> anyhow::Result<()> {
    match args.action {
        crate::cli::ExchangeAction::Export { root, output, json } => {
            ctx.require_project()?;
            let output = absolute(ctx, &output);
            print(&state_exchange::export(&ctx.cwd, &root, &output)?, json)?;
        }
        crate::cli::ExchangeAction::Inspect { bundle, json } => {
            let bundle = absolute(ctx, &bundle);
            print(&state_exchange::inspect(&bundle)?, json)?;
        }
        crate::cli::ExchangeAction::Import { bundle, dest, json } => {
            let bundle = absolute(ctx, &bundle);
            let destination = absolute(ctx, dest.as_deref().unwrap_or(&ctx.cwd));
            print(&state_exchange::import(&destination, &bundle)?, json)?;
        }
        crate::cli::ExchangeAction::Materialize {
            bundle,
            dest,
            yes,
            json,
        } => {
            let bundle = absolute(ctx, &bundle);
            let destination = absolute(ctx, dest.as_deref().unwrap_or(&ctx.cwd));
            if !yes {
                let report = state_exchange::inspect(&bundle)?;
                println!(
                    "materialize plan: {} → {}",
                    report.root_oid,
                    destination.display()
                );
                println!(
                    "evidence: {}",
                    if report.full_evidence {
                        "full"
                    } else {
                        "partial"
                    }
                );
                note!("no destination files or refs changed; re-run with --yes to materialize");
                if !stdin_is_tty() {
                    anyhow::bail!("refusing to materialize without --yes");
                }
                if !dialoguer::Confirm::new()
                    .with_prompt("Materialize this project state?")
                    .default(false)
                    .interact()?
                {
                    return Ok(());
                }
            }
            print(&state_exchange::materialize(&destination, &bundle)?, json)?;
        }
    }
    Ok(())
}
