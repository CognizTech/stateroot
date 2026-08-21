//! `stateroot ext` + external-subcommand dispatch — git-style extensions:
//! any `stateroot-<name>` executable on PATH runs as `stateroot <name>`.
//!
//! Extensions are user-managed files (no registry, no install command); they
//! inherit stdio because they may be interactive, and they inherit the
//! ambient env plus the contract below.

use std::process::Command;

use anyhow::{Context as _, Result};
use stateroot_core::extensions;
use stateroot_core::local_store;

use super::Ctx;

/// Run `stateroot ext list`.
pub fn list() -> Result<()> {
    let builtins: std::collections::BTreeSet<String> =
        crate::cli::subcommand_names().into_iter().collect();
    let mut found = extensions::discover();
    for ext in &mut found {
        ext.shadowed_builtin = builtins.contains(&ext.name);
    }
    if found.is_empty() {
        println!("no extensions found on PATH (stateroot-*)");
        return Ok(());
    }
    for ext in &found {
        let shadow = if ext.shadowed_builtin {
            " — shadowed builtin (ignored)"
        } else {
            ""
        };
        println!("{} — {}{}", ext.name, ext.path.display(), shadow);
    }
    Ok(())
}

/// Dispatch an unknown subcommand to a `stateroot-<name>` executable. The
/// child's exit code becomes ours; unknown names get a clap-styled
/// did-you-mean error and exit code 2 (the usage-error convention).
pub fn run_external(ctx: &Ctx, argv: &[String]) -> Result<i32> {
    let Some((name, args)) = argv.split_first() else {
        // clap only ever yields a non-empty capture; stay defensive.
        eprintln!("Usage: stateroot <COMMAND>");
        return Ok(2);
    };
    let Some(path) = extensions::resolve(name) else {
        eprintln!("error: unrecognized subcommand '{name}'");
        let mut candidates = crate::cli::subcommand_names();
        candidates.extend(extensions::discover().into_iter().map(|e| e.name));
        match did_you_mean(name, &candidates).as_slice() {
            [] => {}
            [one] => eprintln!("\n  tip: a similar subcommand exists: '{one}'"),
            many => eprintln!(
                "\n  tip: some similar subcommands exist: {}",
                many.iter()
                    .map(|s| format!("'{s}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
        eprintln!("\nUsage: stateroot <COMMAND>\n\nFor more information, try '--help'.");
        return Ok(2);
    };
    let status = Command::new(&path)
        .args(args)
        .current_dir(&ctx.cwd)
        .envs(extension_env(ctx))
        .status()
        .with_context(|| {
            format!(
                "launching extension `stateroot-{name}` ({})",
                path.display()
            )
        })?;
    Ok(status.code().unwrap_or(1))
}

/// Env contract injected into extension processes — additive over the
/// inherited environment, so `STATEROOT_DELEGATION_DEPTH` passes through
/// untouched and extensions inside delegate flows keep the recursion cap.
fn extension_env(ctx: &Ctx) -> Vec<(String, String)> {
    let mut env = vec![
        (
            "STATEROOT_HOME".to_string(),
            ctx.config_dir.display().to_string(),
        ),
        (
            "STATEROOT_VERSION".to_string(),
            crate::cli::BUILD_VERSION.to_string(),
        ),
    ];
    if local_store::is_stateroot_dir(&ctx.cwd) {
        env.push((
            "STATEROOT_PROJECT_DIR".to_string(),
            ctx.cwd.display().to_string(),
        ));
        if let Ok(Some(manifest)) = local_store::read_manifest(&ctx.cwd) {
            if let Some(id) = manifest.get("project_id").and_then(|v| v.as_str()) {
                if !id.is_empty() {
                    env.push(("STATEROOT_PROJECT_ID".to_string(), id.to_string()));
                }
            }
        }
    }
    env
}

/// Near matches for an unknown subcommand: edit distance ≤ 2 or the typed
/// text is a prefix of the candidate — closest first, capped at 3.
fn did_you_mean(typed: &str, candidates: &[String]) -> Vec<String> {
    let typed = typed.to_ascii_lowercase();
    if typed.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, String)> = candidates
        .iter()
        .map(|c| (levenshtein(&typed, &c.to_ascii_lowercase()), c.clone()))
        .filter(|(d, c)| *d <= 2 || c.starts_with(&typed))
        .collect();
    scored.sort();
    scored.dedup_by(|a, b| a.1 == b.1);
    scored.truncate(3);
    scored.into_iter().map(|(_, c)| c).collect()
}

/// Classic Levenshtein edit distance (small strings; no crate needed).
fn levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr = vec![0; b_chars.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b_chars.iter().enumerate() {
            curr[j + 1] = (prev[j] + usize::from(ca != *cb))
                .min(prev[j + 1] + 1)
                .min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b_chars.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_counts_edits() {
        assert_eq!(levenshtein("status", "status"), 0);
        assert_eq!(levenshtein("helo", "hello"), 1);
        assert_eq!(levenshtein("statsu", "status"), 2);
        assert_eq!(levenshtein("", "abc"), 3);
    }

    #[test]
    fn did_you_mean_ranks_the_closest_first() {
        let candidates = [
            "status".to_string(),
            "setup".to_string(),
            "snap".to_string(),
        ];
        let suggestions = did_you_mean("statsu", &candidates);
        assert_eq!(suggestions.first().map(String::as_str), Some("status"));
        // Extension names are candidates too.
        let suggestions = did_you_mean("hello", &["helo".to_string()]);
        assert_eq!(suggestions, ["helo".to_string()]);
        // Nothing near → no tip.
        assert!(did_you_mean("zzzz", &candidates).is_empty());
        assert!(did_you_mean("", &candidates).is_empty());
    }
}
