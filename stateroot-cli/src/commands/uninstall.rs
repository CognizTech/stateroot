//! `stateroot uninstall [--purge]` — complete machine removal.
//!
//! Order matters: harness registrations first (this variant's AND legacy
//! monorepo-CLI ones — same paths), then the config dir, and the binary
//! self-delete absolutely LAST. Project `.stateroot/` dirs are NEVER
//! touched by uninstall — `stateroot remove` per project does that.

use std::path::{Path, PathBuf};

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

#[cfg(windows)]
const WINDOWS_INSTALLER_REGISTRY_KEY: &str = r"HKCU\Software\CognizTech\StateRoot";

#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowsMsiInstall {
    product_code: String,
    install_dir: PathBuf,
}

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
        for file in core::paths::instruction_file_candidates(home, quirk) {
            match core::remove_marked_block(&file) {
                Ok(true) => actions.push(format!("block removed ({})", file.display())),
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
        match core::plugins::uninstall_ts_plugin(home, quirk) {
            Ok(lines) => actions.extend(lines),
            Err(err) => note!("  ! {} plugin removal failed: {err:#}", quirk.id),
        }
        for action in &actions {
            println!("  {}: {action}", quirk.id);
        }
    }
    // Claude extras (skill copy + slash stub) — legacy spec surface.
    for path in core::paths::claude_extras_candidates(home) {
        if remove_path(&path) {
            println!("  claude-code: removed {}", path.display());
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

fn encode_powershell_command(script: &str) -> Vec<String> {
    let encoded_bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = BASE64_STANDARD.encode(encoded_bytes);
    vec![
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-WindowStyle".into(),
        "Hidden".into(),
        "-EncodedCommand".into(),
        encoded,
    ]
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
    ("powershell.exe".into(), encode_powershell_command(&script))
}

#[cfg(any(windows, test))]
fn parse_registry_string(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (_, value) = line.split_once("REG_SZ")?;
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

#[cfg(any(windows, test))]
fn normalize_windows_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

#[cfg(windows)]
fn windows_registry_string(name: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;

    let mut command = std::process::Command::new("reg.exe");
    command
        .args([
            "query",
            WINDOWS_INSTALLER_REGISTRY_KEY,
            "/v",
            name,
            "/reg:64",
        ])
        .creation_flags(0x0800_0000);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_registry_string(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(windows)]
fn windows_msi_install(exe: &Path) -> Option<WindowsMsiInstall> {
    let raw_product_code = windows_registry_string("ProductCode")?;
    let parsed = uuid::Uuid::parse_str(
        raw_product_code.trim_matches(|character| character == '{' || character == '}'),
    )
    .ok()?;
    let product_code = format!("{{{}}}", parsed.hyphenated().to_string().to_uppercase());
    let install_dir = PathBuf::from(windows_registry_string("InstallDir")?);
    let exe_dir = exe.parent()?;
    if normalize_windows_path(exe_dir) != normalize_windows_path(&install_dir) {
        return None;
    }
    Some(WindowsMsiInstall {
        product_code,
        install_dir,
    })
}

#[cfg(not(windows))]
fn windows_msi_install(_exe: &Path) -> Option<WindowsMsiInstall> {
    None
}

fn windows_msi_uninstall_command(product_code: &str, parent_pid: u32) -> (String, Vec<String>) {
    let script = format!(
        "$parentPid = {parent_pid}; \
         Wait-Process -Id $parentPid -ErrorAction SilentlyContinue; \
         $process = Start-Process -FilePath 'msiexec.exe' \
           -ArgumentList @('/x', '{product_code}', '/qn', '/norestart', 'STATEROOT_CLEANUP_DONE=1') \
           -Wait -PassThru; \
         exit $process.ExitCode"
    );
    ("powershell.exe".into(), encode_powershell_command(&script))
}

fn spawn_hidden(program: String, args: Vec<String>) -> std::io::Result<()> {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        command.creation_flags(0x0800_0000);
    }
    command.spawn().map(|_| ())
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
pub fn run(ctx: &Ctx, purge: bool, yes: bool, msi_cleanup: bool) -> Result<()> {
    if msi_cleanup && !yes {
        anyhow::bail!("--msi-cleanup requires --yes");
    }
    let home = core::home_dir()?;
    let exe = std::env::current_exe()?;
    let msi_install = windows_msi_install(&exe);

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
        if msi_install.is_some() {
            println!("  remove : Windows Installer registration, PATH entry, and binary (last)");
        } else {
            println!("  delete : this binary (last)");
        }
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

    // An MSI uninstall transaction owns the binary, PATH entry, remembered
    // install directory, and Installed Apps registration. Its custom action
    // uses this cleanup-only mode before Windows Installer removes files.
    if msi_cleanup {
        println!("MSI cleanup complete; Windows Installer will remove the application files");
        return Ok(());
    }

    // For a CLI-initiated MSI uninstall, wait until this process exits and
    // then let Windows Installer remove all installer-owned state. Direct
    // self-deletion would strand PATH and Installed Apps registration.
    if let Some(msi) = msi_install {
        let (program, args) = windows_msi_uninstall_command(&msi.product_code, std::process::id());
        spawn_hidden(program, args)?;
        println!(
            "goodbye — Windows Installer will remove StateRoot from {} after this process exits",
            msi.install_dir.display()
        );
        return Ok(());
    }

    // 4. Self-delete LAST — and only from a standard non-MSI location.
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
    if let Err(spawn_err) = spawn_hidden(program, args) {
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
    fn windows_registry_values_preserve_paths_with_spaces() {
        let output = concat!(
            "HKEY_CURRENT_USER\\Software\\CognizTech\\StateRoot\r\n",
            "    InstallDir    REG_SZ    D:\\AI Tools\\StateRoot\r\n"
        );
        assert_eq!(
            parse_registry_string(output).as_deref(),
            Some("D:\\AI Tools\\StateRoot")
        );
        assert_eq!(
            normalize_windows_path(Path::new("D:/AI Tools/StateRoot/")),
            normalize_windows_path(Path::new("d:\\AI Tools\\StateRoot"))
        );
    }

    #[test]
    fn windows_msi_helper_waits_and_delegates_to_installer() {
        let product_code = "{835594F4-F7DA-42D9-9806-96D037B354B7}";
        let (program, args) = windows_msi_uninstall_command(product_code, 4242);
        assert_eq!(program, "powershell.exe");
        let script = decode_powershell_command(&args);
        assert!(script.contains("Wait-Process -Id $parentPid"), "{script}");
        assert!(script.contains("$parentPid = 4242"), "{script}");
        assert!(script.contains("msiexec.exe"), "{script}");
        assert!(script.contains(product_code), "{script}");
        assert!(script.contains("STATEROOT_CLEANUP_DONE=1"), "{script}");
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
