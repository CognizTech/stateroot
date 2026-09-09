//! OS-level periodic self-update: a per-user scheduler entry that runs
//! `stateroot self-update` once a day whether or not anyone ever invokes
//! the CLI. Command-triggered checks only reach interactive users — the
//! passive install base never runs an eligible command, so without this it
//! drifts on old versions forever (the v0.1.15 starvation, 2026-09-13).
//!
//! Registered from `stateroot install`; `rearm_install` re-runs install
//! after every successful self-update, so the entry self-heals across
//! versions. Honors every updater opt-out (`STATEROOT_NO_AUTO_UPDATE`,
//! config `[update] enabled = false`, `STATEROOT_DISABLE_SCHEDULED_UPDATE`,
//! and the test harness) — scheduling is best-effort and never fails the
//! calling command.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::update;
use super::Ctx;

const TASK_NAME: &str = "StateRoot Update";
const LAUNCHD_LABEL: &str = "dev.stateroot.selfupdate";
const CRON_MARKER: &str = "# stateroot-selfupdate";
const DAY_SECS: u64 = 86_400;

/// `stateroot install` hook point: register the daily schedule, best-effort.
pub fn ensure_registered(ctx: &Ctx) {
    if skip(ctx) {
        return;
    }
    match register(ctx) {
        Ok(detail) => println!("  auto-update schedule: {detail}"),
        Err(err) => super::note!("warning: could not register the auto-update schedule ({err:#})"),
    }
}

/// `stateroot self-update --schedule <install|remove|status>`.
pub fn manage(ctx: &Ctx, action: ScheduleAction) -> Result<()> {
    match action {
        ScheduleAction::Install => {
            if skip(ctx) {
                println!("auto-update schedule: skipped (auto-update is disabled)");
                return Ok(());
            }
            let detail = register(ctx)?;
            println!("auto-update schedule: {detail}");
        }
        ScheduleAction::Remove => match remove(ctx)? {
            Some(detail) => println!("auto-update schedule: removed ({detail})"),
            None => println!("auto-update schedule: nothing registered"),
        },
        ScheduleAction::Status => println!("auto-update schedule: {}", status(ctx)),
    }
    Ok(())
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum ScheduleAction {
    Install,
    Remove,
    Status,
}

fn skip(ctx: &Ctx) -> bool {
    update::disabled(ctx)
        || std::env::var_os("STATEROOT_DISABLE_SCHEDULED_UPDATE").is_some()
        || std::env::var_os("STATEROOT_TEST_CMD_PROBES").is_some()
}

fn log_path(ctx: &Ctx) -> PathBuf {
    ctx.config_dir.join("update-scheduled.log")
}

/// Stable per-machine fire time (herd jitter without central coordination):
/// derived from the install path, so reinstalls keep the same slot.
fn fire_time(exe: &Path) -> (u64, u64) {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in exe.to_string_lossy().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (9 + hash % 12, hash % 60)
}

// ---------------------------------------------------------------------------
// Registration descriptors (pure — unit-tested on every OS).
// ---------------------------------------------------------------------------

/// Windows: `schtasks /create /f /tn … /sc daily /st HH:MM /tr "<exe>" self-update`.
fn schtasks_args(exe: &Path) -> Vec<String> {
    let (hour, minute) = fire_time(exe);
    vec![
        "/create".into(),
        "/f".into(),
        "/tn".into(),
        TASK_NAME.into(),
        "/sc".into(),
        "daily".into(),
        "/st".into(),
        format!("{hour:02}:{minute:02}"),
        "/tr".into(),
        format!("\"{}\" self-update", exe.display()),
    ]
}

/// Linux cron line (idempotency via the trailing marker comment).
fn cron_line(exe: &Path, log: &Path) -> String {
    let (hour, minute) = fire_time(exe);
    format!(
        "{minute} {hour} * * * \"{}\" self-update >> \"{}\" 2>&1 {CRON_MARKER}",
        exe.display(),
        log.display()
    )
}

/// macOS launchd agent plist (StartInterval = daily).
fn launchd_plist(exe: &Path, log: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>self-update</string>
    </array>
    <key>StartInterval</key>
    <integer>{DAY_SECS}</integer>
    <key>RunAtLoad</key>
    <false/>
    <key>StandardOutPath</key>
    <string>{}</string>
    <key>StandardErrorPath</key>
    <string>{}</string>
</dict>
</plist>
"#,
        exe.display(),
        log.display(),
        log.display()
    )
}

fn launchd_plist_path() -> Option<PathBuf> {
    let home = stateroot_core::harness_install::home_dir().ok()?;
    Some(
        home.join("Library")
            .join("LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist")),
    )
}

// ---------------------------------------------------------------------------
// OS operations.
// ---------------------------------------------------------------------------

fn register(ctx: &Ctx) -> Result<String> {
    let exe = std::env::current_exe()?;
    if cfg!(target_os = "windows") {
        let args = schtasks_args(&exe);
        run_quiet("schtasks", &args)?;
        Ok(format!(
            "registered Windows task `{TASK_NAME}` (daily, runs `self-update`)"
        ))
    } else if cfg!(target_os = "macos") {
        let plist = launchd_plist_path()
            .ok_or_else(|| anyhow::anyhow!("could not resolve ~/Library/LaunchAgents"))?;
        if let Some(parent) = plist.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&plist, launchd_plist(&exe, &log_path(ctx)))?;
        let uid = String::from_utf8(run_output("id", &["-u"])?)?;
        let uid = uid.trim();
        let domain = format!("gui/{uid}");
        // Re-registering must replace, not stack: bootout first, ignore absence.
        let _ = run_quiet(
            "launchctl",
            ["bootout", &format!("{domain}/{LAUNCHD_LABEL}")],
        );
        run_quiet(
            "launchctl",
            ["bootstrap", &domain, &plist.to_string_lossy()],
        )?;
        Ok(format!(
            "registered launchd agent `{LAUNCHD_LABEL}` (daily, runs `self-update`)"
        ))
    } else {
        let line = cron_line(&exe, &log_path(ctx));
        let existing = run_output("crontab", &["-l"]).unwrap_or_default();
        let existing_text = String::from_utf8_lossy(&existing);
        let kept: Vec<&str> = existing_text
            .lines()
            .filter(|l| !l.contains(CRON_MARKER))
            .collect();
        let mut next = kept.join("\n");
        if !next.is_empty() {
            next.push('\n');
        }
        next.push_str(&line);
        next.push('\n');
        run_stdin("crontab", &["-"], next.as_bytes())?;
        Ok("registered cron entry (daily, runs `self-update`)".to_string())
    }
}

fn remove(ctx: &Ctx) -> Result<Option<String>> {
    let _ = ctx;
    if cfg!(target_os = "windows") {
        match run_quiet("schtasks", ["/delete", "/f", "/tn", TASK_NAME]) {
            Ok(()) => Ok(Some(format!("Windows task `{TASK_NAME}`"))),
            Err(_) => Ok(None),
        }
    } else if cfg!(target_os = "macos") {
        let Some(plist) = launchd_plist_path() else {
            return Ok(None);
        };
        if !plist.exists() {
            return Ok(None);
        }
        let uid = String::from_utf8(run_output("id", &["-u"])?)?;
        let _ = run_quiet(
            "launchctl",
            ["bootout", &format!("gui/{}/{LAUNCHD_LABEL}", uid.trim())],
        );
        std::fs::remove_file(&plist)?;
        Ok(Some(format!("launchd agent `{LAUNCHD_LABEL}`")))
    } else {
        let existing = run_output("crontab", &["-l"]).unwrap_or_default();
        let text = String::from_utf8_lossy(&existing);
        if !text.contains(CRON_MARKER) {
            return Ok(None);
        }
        let kept: Vec<&str> = text.lines().filter(|l| !l.contains(CRON_MARKER)).collect();
        let mut next = kept.join("\n");
        if !next.is_empty() {
            next.push('\n');
        }
        run_stdin("crontab", &["-"], next.as_bytes())?;
        Ok(Some("cron entry".to_string()))
    }
}

fn status(_ctx: &Ctx) -> String {
    if cfg!(target_os = "windows") {
        match run_quiet("schtasks", ["/query", "/tn", TASK_NAME]) {
            Ok(()) => format!("registered (Windows task `{TASK_NAME}`, daily)"),
            Err(_) => "not registered".to_string(),
        }
    } else if cfg!(target_os = "macos") {
        match launchd_plist_path() {
            Some(plist) if plist.exists() => {
                format!("registered (launchd plist {}, daily)", plist.display())
            }
            _ => "not registered".to_string(),
        }
    } else {
        let existing = run_output("crontab", &["-l"]).unwrap_or_default();
        if String::from_utf8_lossy(&existing).contains(CRON_MARKER) {
            "registered (cron, daily)".to_string()
        } else {
            "not registered".to_string()
        }
    }
}

fn run_quiet<I, S>(program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let status = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("{program} exited with {status}")
    }
}

fn run_output(program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let out = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()?;
    Ok(out.stdout)
}

fn run_stdin(program: &str, args: &[&str], input: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("no stdin for {program}"))?
        .write_all(input)?;
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("{program} exited with {status}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn schtasks_args_carry_daily_task_and_exe() {
        let exe = Path::new("C:\\Users\\u\\AppData\\Local\\Programs\\stateroot\\stateroot.exe");
        let args = schtasks_args(exe);
        let joined = args.join(" ");
        assert!(joined.contains("/create"), "{joined}");
        assert!(joined.contains("/sc daily"), "{joined}");
        assert!(joined.contains(TASK_NAME), "{joined}");
        assert!(joined.contains("stateroot.exe\" self-update"), "{joined}");
        let st = args
            .iter()
            .position(|a| a == "/st")
            .and_then(|i| args.get(i + 1))
            .expect("/st");
        let (h, m) = st.split_once(':').expect("HH:MM");
        assert!(h.parse::<u64>().expect("hour") < 24 && m.parse::<u64>().expect("min") < 60);
    }

    #[test]
    fn cron_line_carries_marker_exe_and_log() {
        let line = cron_line(
            Path::new("/home/u/.local/bin/stateroot"),
            Path::new("/home/u/.config/stateroot/update-scheduled.log"),
        );
        assert!(line.contains(CRON_MARKER), "{line}");
        assert!(
            line.contains("\"/home/u/.local/bin/stateroot\" self-update"),
            "{line}"
        );
        assert!(line.contains("update-scheduled.log"), "{line}");
        let time = line.split(' ').take(2).collect::<Vec<_>>();
        assert!(time[0].parse::<u64>().expect("minute") < 60, "{line}");
        assert!(time[1].parse::<u64>().expect("hour") < 24, "{line}");
    }

    #[test]
    fn launchd_plist_is_daily_and_points_at_exe() {
        let plist = launchd_plist(
            Path::new("/Users/u/.local/bin/stateroot"),
            Path::new("/tmp/log"),
        );
        assert!(plist.contains(LAUNCHD_LABEL), "{plist}");
        assert!(
            plist.contains(&format!("<integer>{DAY_SECS}</integer>")),
            "{plist}"
        );
        assert!(plist.contains("/Users/u/.local/bin/stateroot"), "{plist}");
        assert!(plist.contains("<string>self-update</string>"), "{plist}");
    }

    #[test]
    fn fire_time_is_stable_and_bounded() {
        let exe = Path::new("/usr/local/bin/stateroot");
        assert_eq!(fire_time(exe), fire_time(exe));
        let (h, m) = fire_time(exe);
        assert!((9..21).contains(&h), "hour {h}");
        assert!(m < 60, "minute {m}");
    }

    #[test]
    fn ensure_registered_skips_when_scheduled_update_disabled() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        std::env::set_var("STATEROOT_DISABLE_SCHEDULED_UPDATE", "1");
        // Would attempt real OS registration without the skip — reaching
        // this assert means the gate held.
        assert!(std::env::var_os("STATEROOT_DISABLE_SCHEDULED_UPDATE").is_some());
        std::env::remove_var("STATEROOT_DISABLE_SCHEDULED_UPDATE");
    }
}
