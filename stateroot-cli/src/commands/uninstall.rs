//! `stateroot uninstall [--purge]` — complete machine removal.
//!
//! Order matters: harness registrations first (this variant's AND legacy
//! monorepo-CLI ones — same paths), then the config dir, and the binary
//! self-delete absolutely LAST. Project `.stateroot/` dirs are NEVER
//! touched by uninstall — `stateroot remove` per project does that.

use std::path::Path;

use anyhow::Result;
use stateroot_core::harness_install::{self as core, registry};

use super::{note, stdin_is_tty, Ctx};

/// Home-level leftovers from product seeding/projections (machine-global).
const HOME_LEFTOVERS: &[&str] = &[
    ".agents/skills/stateroot",
    ".agents/skills/stateroot-skill-router",
    ".stateroot/skills/stateroot",
    // Legacy monorepo-CLI debris (pre-extensions openclaw plugin path).
    ".openclaw/plugins/stateroot",
];

fn remove_path(path: &Path) -> bool {
    if path.is_dir() {
        std::fs::remove_dir_all(path).is_ok()
    } else if path.is_file() {
        std::fs::remove_file(path).is_ok()
    } else {
        false
    }
}

fn remove_harness_registrations(ctx: &Ctx, home: &Path) -> Result<()> {
    let _ = ctx;
    // Every registered adapter (not just detected ones — leftovers from
    // older installs must go too). These paths are shared with the legacy
    // monorepo CLI, so its registrations are removed as well.
    for quirk in registry::adapters() {
        let mut actions: Vec<String> = Vec::new();
        match core::hooks::remove_hooks(home, quirk) {
            Ok(removed) => actions.extend(removed),
            Err(err) => note!("  ! {} hook removal failed: {err:#}", quirk.id),
        }
        if let Some(rel) = quirk.instruction_file {
            match core::remove_marked_block(&home.join(rel)) {
                Ok(true) => actions.push(format!("block removed ({rel})")),
                Ok(false) => {}
                Err(err) => note!("  ! {} block removal failed: {err:#}", quirk.id),
            }
        }
        if let Some(target) = quirk.mcp {
            match core::uninstall_quirk_mcp(home, quirk) {
                Ok(true) => actions.push(format!("MCP registration removed ({})", target.path)),
                Ok(false) => {}
                Err(err) => note!("  ! {} MCP removal failed: {err:#}", quirk.id),
            }
        }
        for action in &actions {
            println!("  {}: {action}", quirk.id);
        }
    }
    // Claude extras (skill copy + slash stub) — legacy spec surface.
    for rel in [".claude/skills/stateroot", ".claude/commands/stateroot.md"] {
        let path = home.join(rel);
        if remove_path(&path) {
            println!("  claude-code: removed {rel}");
        }
    }
    // Home-level product leftovers.
    for rel in HOME_LEFTOVERS {
        let path = home.join(rel);
        if remove_path(&path) {
            println!("  removed {rel}");
        }
    }
    Ok(())
}

fn list_registered_projects(ctx: &Ctx) {
    let Ok(Some(registry)) = core_registry(ctx) else {
        return;
    };
    if registry.is_empty() {
        return;
    }
    println!("\nRegistered projects (left untouched):");
    for entry in &registry {
        println!("  {entry}");
    }
    println!("  (project .stateroot/ dirs keep their state — run `stateroot remove` per project first if you want them gone)");
}

fn core_registry(ctx: &Ctx) -> Result<Option<Vec<String>>> {
    let registry =
        stateroot_core::config::load_registry(&ctx.config_dir).map_err(|e| anyhow::anyhow!(e))?;
    let mut names: Vec<String> = registry
        .projects
        .values()
        .map(|entry| format!("{} ({})", entry.name, entry.project_id))
        .collect();
    names.sort();
    Ok(Some(names))
}

/// Where the exe is allowed to self-delete from. A cargo `target/` path
/// (or anything else non-standard) refuses with the path printed.
fn is_standard_install_location(exe: &Path) -> bool {
    let text = exe.to_string_lossy().replace('\\', "/");
    if text.contains("/target/") {
        return false;
    }
    text.contains("/.local/bin/")
        || text.contains("/.cargo/bin/")
        || text.contains("/Programs/")
        || text.contains("/usr/local/bin/")
        || text.contains("/usr/bin/")
}

/// The detached self-delete helper (spawned, never waited on). Windows: a
/// running exe can't delete itself — cmd waits on the pid, then deletes.
/// Unix: unlink after a short delay. Returned for structure testing.
pub fn self_delete_command(exe: &Path) -> (String, Vec<String>) {
    let exe = exe.display().to_string();
    if cfg!(windows) {
        let pid = std::process::id();
        (
            "cmd".into(),
            vec![
                "/C".into(),
                format!(
                    "ping 127.0.0.1 -n 3 > nul & tasklist /FI \"PID eq {pid}\" | find \"{pid}\" > nul && goto wait & del /F /Q \"{exe}\" & exit & :wait & ping 127.0.0.1 -n 2 > nul & goto check & :check & tasklist /FI \"PID eq {pid}\" | find \"{pid}\" > nul && (ping 127.0.0.1 -n 2 > nul & goto check) || del /F /Q \"{exe}\""
                ),
            ],
        )
    } else {
        (
            "sh".into(),
            vec![
                "-c".into(),
                format!(
                    "while kill -0 {} 2>/dev/null; do sleep 0.2; done; rm -f \"{}\"",
                    std::process::id(),
                    exe
                ),
            ],
        )
    }
}

/// Run `stateroot uninstall`.
pub fn run(ctx: &Ctx, purge: bool, yes: bool) -> Result<()> {
    let home = core::home_dir()?;

    // Interactive confirm (default NO) unless --yes.
    if !yes {
        println!("stateroot uninstall — plan");
        println!(
            "  remove : all machine-level harness registrations (hooks, MCP, blocks, extensions)"
        );
        println!("  remove : config dir {}", ctx.config_dir.display());
        if purge {
            println!(
                "  purge  : user-global data at {} (soul, learnings, memories)",
                home.join(".stateroot").display()
            );
        } else {
            println!(
                "  keep   : user-global data at {} (pass --purge to remove)",
                home.join(".stateroot").display()
            );
        }
        println!("  delete : this binary (last)");
        println!("  keep   : project .stateroot/ dirs (never touched by uninstall)");
        if !stdin_is_tty() {
            anyhow::bail!(
                "refusing to uninstall without confirmation (non-interactive) — re-run with --yes"
            );
        }
        let proceed = dialoguer::Confirm::new()
            .with_prompt("Proceed with full uninstall?")
            .default(false)
            .interact()?;
        if !proceed {
            println!("aborted — nothing changed");
            return Ok(());
        }
        if purge {
            let sure = dialoguer::Confirm::new()
                .with_prompt("Also DELETE user-global data (soul, learnings, memories)?")
                .default(false)
                .interact()?;
            if !sure {
                println!("aborted — nothing changed");
                return Ok(());
            }
        }
    }

    // 1. Harness registrations (both variants' paths).
    println!("Removing harness integrations (home: {}):", home.display());
    remove_harness_registrations(ctx, &home)?;

    // 2. Projects stay; say so.
    list_registered_projects(ctx);

    // 3. Config dir; user-global data only under --purge.
    if ctx.config_dir.exists() {
        std::fs::remove_dir_all(&ctx.config_dir)?;
        println!("removed config dir {}", ctx.config_dir.display());
    }
    let global_dir = home.join(".stateroot");
    if purge {
        if global_dir.exists() {
            std::fs::remove_dir_all(&global_dir)?;
            println!("purged {}", global_dir.display());
        }
    } else {
        println!(
            "kept user-global data at {} (soul, learnings, memories)",
            global_dir.display()
        );
    }

    // 4. Self-delete LAST — and only from a standard install location.
    let exe = std::env::current_exe()?;
    if !is_standard_install_location(&exe) {
        println!(
            "not self-deleting: {} is not a standard install location — delete it manually if desired",
            exe.display()
        );
        return Ok(());
    }
    let (program, args) = self_delete_command(&exe);
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    println!(
        "goodbye — the binary at {} will delete itself on exit",
        exe.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_delete_helper_waits_then_deletes() {
        let exe = Path::new(if cfg!(windows) {
            "C:\\Users\\u\\AppData\\Local\\Programs\\stateroot\\stateroot.exe"
        } else {
            "/home/u/.local/bin/stateroot"
        });
        let (program, args) = self_delete_command(exe);
        if cfg!(windows) {
            assert_eq!(program, "cmd");
            let script = args.join(" ");
            assert!(script.contains("del"), "{script}");
            assert!(script.contains("stateroot.exe"), "{script}");
        } else {
            assert_eq!(program, "sh");
            let script = args.join(" ");
            // waits for THIS process to exit, then unlinks the exe
            assert!(script.contains("kill -0"), "{script}");
            assert!(script.contains("rm -f"), "{script}");
            assert!(script.contains("/home/u/.local/bin/stateroot"), "{script}");
        }
    }

    #[test]
    fn nonstandard_locations_are_refused() {
        assert!(!is_standard_install_location(Path::new(
            "/home/u/code/stateroot/target/release/stateroot"
        )));
        assert!(!is_standard_install_location(Path::new(
            "C:\\code\\stateroot\\target\\release\\stateroot.exe"
        )));
        assert!(is_standard_install_location(Path::new(
            "/home/u/.local/bin/stateroot"
        )));
        assert!(is_standard_install_location(Path::new(
            "C:\\Users\\u\\AppData\\Local\\Programs\\stateroot\\stateroot.exe"
        )));
    }
}
