//! `stateroot memory …` — curated hot-apex + FTS recall.

use anyhow::Result;
use stateroot_core::{hot_apex, memory_index};

use super::Ctx;

fn home() -> Result<std::path::PathBuf> {
    stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))
}

fn print_result(r: &hot_apex::MutationResult) {
    if r.success {
        if r.noop {
            println!("noop — already present ({})", r.usage);
        } else {
            println!(
                "ok — {}{}",
                r.usage,
                r.path
                    .as_ref()
                    .map(|p| format!(" → {}", p.display()))
                    .unwrap_or_default()
            );
        }
    } else {
        println!("error: {}", r.error.as_deref().unwrap_or("unknown"));
        println!("usage: {}", r.usage);
        if let Some(entries) = &r.current_entries {
            println!("current entries ({}):", entries.len());
            for (i, e) in entries.iter().enumerate() {
                println!("  {}. {e}", i + 1);
            }
        }
    }
}

/// `stateroot memory add --target memory|user [--private] <content>`
pub fn add(ctx: &Ctx, target: &str, content: &str, private: bool) -> Result<()> {
    ctx.require_project()?;
    let home = home()?;
    hot_apex::ensure_migrated(&ctx.cwd, &home);
    let r =
        hot_apex::add(&ctx.cwd, &home, target, content, private).map_err(|e| anyhow::anyhow!(e))?;
    let _ = memory_index::rebuild_if_needed(&ctx.cwd, &home);
    print_result(&r);
    if !r.success {
        anyhow::bail!("memory add failed");
    }
    Ok(())
}

/// `stateroot memory replace --target … --old <needle> <content>`
pub fn replace(ctx: &Ctx, target: &str, old: &str, content: &str, private: bool) -> Result<()> {
    ctx.require_project()?;
    let home = home()?;
    let r = hot_apex::replace(&ctx.cwd, &home, target, old, content, private)
        .map_err(|e| anyhow::anyhow!(e))?;
    let _ = memory_index::rebuild_if_needed(&ctx.cwd, &home);
    print_result(&r);
    if !r.success {
        anyhow::bail!("memory replace failed");
    }
    Ok(())
}

/// `stateroot memory remove --target … <old>`
pub fn remove(ctx: &Ctx, target: &str, old: &str) -> Result<()> {
    ctx.require_project()?;
    let home = home()?;
    let r = hot_apex::remove(&ctx.cwd, &home, target, old).map_err(|e| anyhow::anyhow!(e))?;
    let _ = memory_index::rebuild_if_needed(&ctx.cwd, &home);
    print_result(&r);
    if !r.success {
        anyhow::bail!("memory remove failed");
    }
    Ok(())
}

/// `stateroot memory show [--target memory|user]`
pub fn show(ctx: &Ctx, target: &str) -> Result<()> {
    ctx.require_project()?;
    let home = home()?;
    hot_apex::ensure_migrated(&ctx.cwd, &home);
    print!(
        "{}",
        hot_apex::show(&ctx.cwd, &home, target).map_err(|e| anyhow::anyhow!(e))?
    );
    Ok(())
}

/// `stateroot memory recall <query> [--limit N]`
pub fn recall(ctx: &Ctx, query: &str, limit: usize) -> Result<()> {
    ctx.require_project()?;
    let home = home()?;
    hot_apex::ensure_migrated(&ctx.cwd, &home);
    let hits = memory_index::search(&ctx.cwd, &home, query, limit, true)
        .map_err(|e| anyhow::anyhow!(e))?;
    if hits.is_empty() {
        println!("no hits for {query:?}");
        return Ok(());
    }
    for hit in hits {
        let priv_mark = if hit.private { " [private]" } else { "" };
        println!(
            "[{} | {} | score={:.3}]{priv_mark}",
            hit.kind, hit.path, hit.score
        );
        let snippet = if hit.text.len() > 400 {
            format!("{}…", &hit.text[..400])
        } else {
            hit.text.clone()
        };
        println!("  {snippet}\n");
    }
    Ok(())
}
