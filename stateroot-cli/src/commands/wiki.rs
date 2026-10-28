//! `stateroot wiki …` — compiled catalog (show / lint / compile).

use anyhow::Result;
use stateroot_core::wiki;

use super::Ctx;

/// `stateroot wiki show <path>`
pub fn show(ctx: &Ctx, path: &str) -> Result<()> {
    ctx.require_project()?;
    let _ = wiki::ensure_layout(&ctx.cwd);
    let body = wiki::show(&ctx.cwd, path).map_err(|e| anyhow::anyhow!(e))?;
    print!("{body}");
    if !body.ends_with('\n') {
        println!();
    }
    Ok(())
}

/// `stateroot wiki lint`
pub fn lint(ctx: &Ctx) -> Result<()> {
    ctx.require_project()?;
    let findings = wiki::lint(&ctx.cwd).map_err(|e| anyhow::anyhow!(e))?;
    if findings.is_empty() {
        println!("wiki lint: clean");
        return Ok(());
    }
    for f in &findings {
        match &f.path {
            Some(p) => println!("[{}] {} ({p})", f.code, f.message),
            None => println!("[{}] {}", f.code, f.message),
        }
    }
    println!("wiki lint: {} finding(s)", findings.len());
    Ok(())
}

/// `stateroot wiki compile` — deterministic (and agentic when available) ingest.
pub async fn compile(ctx: &Ctx, force: bool, dependencies: Option<&str>) -> Result<()> {
    ctx.require_project()?;
    if let Some(raw) = dependencies {
        wiki::set_compile_dependencies(&ctx.cwd, &serde_json::from_str(raw)?)?;
    }
    let outcome = super::compiler::try_ingest(ctx, force).await?;
    println!("{outcome}");
    Ok(())
}
