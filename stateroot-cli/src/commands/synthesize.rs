//! `stateroot synthesize [--force]` — dual-mode context compiler.
//!
//! Agentic when a local synthesis API key is present; otherwise reports that
//! the deterministic digest is used. Never blocks resume or import. Rate caps
//! default to 0 (disabled).

use anyhow::Result;

use super::compiler::{self, AgenticOutcome, CompilerMode};
use super::Ctx;

/// `stateroot synthesize [--force]`
pub async fn run(ctx: &Ctx, force: bool) -> Result<()> {
    ctx.require_project()?;
    if !ctx.config.synthesis.enabled {
        println!(
            "synthesis disabled (synthesis.enabled=false in config.toml) — deterministic digest intact"
        );
        return Ok(());
    }
    match compiler::mode(ctx) {
        CompilerMode::Deterministic => {
            println!(
                "synthesis unavailable — no DEEPSEEK_API_KEY or OPENAI_API_KEY; deterministic digest intact"
            );
            Ok(())
        }
        CompilerMode::Agentic => match compiler::try_agentic(ctx, force).await? {
            AgenticOutcome::Merged => {
                println!(
                    "synthesis merged into the local handoff (labeled synthesized, never verified)"
                );
                Ok(())
            }
            AgenticOutcome::Unchanged => {
                println!("bundle unchanged — synthesis skipped (pass --force to re-run)");
                Ok(())
            }
            AgenticOutcome::Deterministic => {
                println!(
                    "synthesis skipped or failed — deterministic digest intact (pass --force to retry)"
                );
                Ok(())
            }
        },
    }
}
