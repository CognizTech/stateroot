//! `stateroot uninstall [--purge]` — complete machine removal.
//!
//! Order matters: harness registrations first (this variant's AND legacy
//! monorepo-CLI ones — same paths), then the config dir, and the binary
//! self-delete absolutely LAST. Project `.stateroot/` dirs are NEVER
//! touched by uninstall — `stateroot remove` per project does that.

use std::path::Path;

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
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

fn windows_self_delete_command(exe: &Path) -> (String, Vec<String>) {
    // PowerShell's encoded-command form avoids cmd.exe quoting hazards for
    // paths containing spaces or shell metacharacters. Windows keeps a
    // running executable locked, so retry until this process exits and the
    // removal succeeds. The helper gives up honestly after 30 seconds.
    let target = exe.display().to_string().replace('\'', "''");
    let script = format!(
        "$target = '{target}'; \
         for ($attempt = 0; $attempt -lt 120; $attempt++) {{ \
           Start-Sleep -Milliseconds 250; \
           Remove-Item -LiteralPath $target -Force -ErrorAction SilentlyContinue; \
           if (-not (Test-Path -LiteralPath $target)) {{ exit 0 }} \
         }}; \
         exit 1"
    );
    let encoded_bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = BASE64_STANDARD.encode(encoded_bytes);
    (
        "powershell.exe".into(),
        vec![
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-WindowStyle".into(),
            "Hidden".into(),
            "-EncodedCommand".into(),
            encoded,
        ],
    )
}

fn park_for_windows_uninstall(exe: &Path) -> Result<std::path::PathBuf> {
    let parent = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", exe.display()))?;
    let name = exe
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{} has no file name", exe.display()))?;
    let parked = parent.join(format!(
        "{}.uninstalling-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    if parked.exists() {
        std::fs::remove_file(&parked)?;
    }
    std::fs::rename(exe, &parked).map_err(|err| {
        anyhow::anyhow!(
            "could not remove {} from PATH by renaming it to {}: {err}",
            exe.display(),
            parked.display()
        )
    })?;
    Ok(parked)
}

/// The detached self-delete helper (spawned, never waited on). Windows: a
/// hidden PowerShell process retries until the running exe can be deleted.
/// Unix: unlink after the current process exits. Returned for testing.
pub fn self_delete_command(exe: &Path) -> (String, Vec<String>) {
    if cfg!(windows) {
        windows_self_delete_command(exe)
    } else {
        let exe = exe.display().to_string();
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
    // Rename first on Windows. This removes `stateroot.exe` from PATH before
    // the process exits, while the detached helper cleans up the still-locked
    // parked file afterward. The updater uses the same rename-park property.
    let delete_target = if cfg!(windows) {
        park_for_windows_uninstall(&exe)?
    } else {
        exe.clone()
    };
    let (program, args) = self_delete_command(&delete_target);
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        // Do not flash another console window while the helper waits for this
        // process to exit. CREATE_NO_WINDOW from WinBase.h.
        command.creation_flags(0x0800_0000);
    }
    if let Err(spawn_err) = command.spawn() {
        if delete_target != exe {
            if let Err(rollback_err) = std::fs::rename(&delete_target, &exe) {
                anyhow::bail!(
                    "could not start uninstall cleanup ({spawn_err}) and could not restore the binary ({rollback_err}); it remains at {}",
                    delete_target.display()
                );
            }
        }
        return Err(spawn_err.into());
    }
    println!(
        "goodbye — removed {}; final cleanup runs on exit",
        exe.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_powershell_command(args: &[String]) -> String {
        let encoded = args.last().expect("encoded command");
        let bytes = BASE64_STANDARD.decode(encoded).expect("base64");
        assert_eq!(bytes.len() % 2, 0, "UTF-16LE byte count");
        let words: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&words).expect("UTF-16LE command")
    }

    #[test]
    fn self_delete_helper_waits_then_deletes() {
        let exe = Path::new(if cfg!(windows) {
            "C:\\Users\\u\\AppData\\Local\\Programs\\stateroot\\stateroot.exe"
        } else {
            "/home/u/.local/bin/stateroot"
        });
        let (program, args) = self_delete_command(exe);
        if cfg!(windows) {
            assert_eq!(program, "powershell.exe");
            let script = decode_powershell_command(&args);
            assert!(script.contains("Remove-Item"), "{script}");
            assert!(script.contains("Test-Path"), "{script}");
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
    fn windows_self_delete_helper_encodes_special_paths_and_retries() {
        let exe = Path::new("C:\\Users\\O'Brien & Sons\\stateroot.exe");
        let (program, args) = windows_self_delete_command(exe);
        assert_eq!(program, "powershell.exe");
        assert!(args.iter().any(|arg| arg == "-EncodedCommand"));
        let script = decode_powershell_command(&args);
        assert!(script.contains("O''Brien & Sons"), "{script}");
        assert!(script.contains("$attempt -lt 120"), "{script}");
        assert!(script.contains("Start-Sleep -Milliseconds 250"), "{script}");
    }

    #[test]
    fn windows_uninstall_parks_the_command_name_before_cleanup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = dir.path().join("stateroot.exe");
        std::fs::write(&exe, b"fixture").expect("fixture");
        let parked = park_for_windows_uninstall(&exe).expect("park");
        assert!(!exe.exists(), "command name must disappear immediately");
        assert!(parked.is_file(), "parked binary must remain for cleanup");
        assert!(
            parked
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("stateroot.exe.uninstalling-")),
            "{}",
            parked.display()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_self_delete_helper_retries_a_locked_file() {
        use std::os::windows::fs::OpenOptionsExt;
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir");
        let exe = dir.path().join("StateRoot O'Brien & Sons.exe");
        std::fs::write(&exe, b"fixture").expect("fixture");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&exe)
            .expect("exclusive lock");

        let (program, args) = windows_self_delete_command(&exe);
        let mut helper = std::process::Command::new(program)
            .args(args)
            .spawn()
            .expect("spawn helper");
        std::thread::sleep(Duration::from_millis(750));
        assert!(
            exe.exists(),
            "locked fixture must survive the first retries"
        );
        drop(lock);

        let status = helper.wait().expect("wait for helper");
        assert!(status.success(), "helper exited with {status}");
        assert!(!exe.exists(), "helper must delete the fixture after unlock");
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
