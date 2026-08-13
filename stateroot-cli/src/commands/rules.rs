//! `stateroot rules` — shared rules pool (product-intent + federated harness rules).

use anyhow::Result;
use stateroot_core::rules as core;

use super::Ctx;

fn home() -> Result<std::path::PathBuf> {
    stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))
}

/// `stateroot rules list`
pub fn list(ctx: &Ctx) -> Result<()> {
    let home = home()?;
    let _ = core::ensure_product_intent(&home);
    for scope in ["user", "project"] {
        let rules = core::list_scope(&ctx.cwd, &home, scope);
        if rules.is_empty() {
            println!("no rules ({scope} scope)");
            continue;
        }
        println!("Rules ({scope} scope):");
        for rule in rules {
            let kind = if rule.product { "always" } else { "imported" };
            println!(
                "  {} [{kind}; {}; {}] {}",
                rule.slug, rule.origin, rule.scope, rule.title
            );
        }
    }
    Ok(())
}

/// `stateroot rules show <slug>`
pub fn show(ctx: &Ctx, slug: &str) -> Result<()> {
    let home = home()?;
    let _ = core::ensure_product_intent(&home);
    match core::show(&ctx.cwd, &home, slug) {
        Some((rule, body)) => {
            println!(
                "# {} [{} / {}]\n# origin: {}\n\n{body}",
                rule.slug, rule.origin, rule.scope, rule.origin_path
            );
            Ok(())
        }
        None => anyhow::bail!("rule '{slug}' not found — try `stateroot rules list`"),
    }
}

/// `stateroot rules sync`
pub fn sync(ctx: &Ctx) -> Result<()> {
    let home = home()?;
    let report = core::sync(&ctx.cwd, &home).map_err(|e| anyhow::anyhow!(e))?;
    println!(
        "rules sync: product-intent {} · imported {} · updated {} · unchanged {} · pruned {}",
        if report.seeded { "refreshed" } else { "current" },
        report.imported,
        report.updated,
        report.unchanged,
        report.pruned
    );
    Ok(())
}
