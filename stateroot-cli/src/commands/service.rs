//! `stateroot service` — the per-user background continuity service.
//!
//! One resident deterministic process reconciles every registered project:
//! it scans the canonical registry on the poll interval, writes each
//! project's machine-local continuity projection atomically, maintains a
//! heartbeat, and keeps a capped log. It is active but NON-agentic — no
//! model calls, no inferred intent, ever.
//!
//! OS registration: per-user systemd service (Linux), LaunchAgent (macOS),
//! logon Scheduled Task (native Windows). Under WSL a functional
//! user-systemd wins; otherwise a Windows-host task launches the service
//! through the current WSL distribution. When OS registration is
//! unavailable the service runs detached instead, hooks/CLI keep
//! reconciling on activity, and `doctor` reports degraded coverage.

use std::path::{Path, PathBuf};

use anyhow::anyhow;
use stateroot_core::continuity::{self, ServiceHeartbeat, ServiceRegistration};
use stateroot_core::local_store::now_rfc3339;
use stateroot_core::safe_io::{self, ResourceLock};

use super::{detached, note, Ctx};

const SYSTEMD_UNIT: &str = "stateroot-continuity.service";
const SCHTASKS_NAME: &str = "StateRoot Continuity";
const LAUNCHD_LABEL: &str = "dev.stateroot.continuity";
const LOG_FILE: &str = "continuity-service.log";
const LOCK_FILE: &str = "continuity-service.lock";
const HEARTBEAT_SCHEMA: &str = "stateroot.continuity-heartbeat.v1";
const REGISTRATION_SCHEMA: &str = "stateroot.continuity-registration.v1";

fn lock_path(config_dir: &Path) -> PathBuf {
    config_dir.join(LOCK_FILE)
}

fn log_path(config_dir: &Path) -> PathBuf {
    config_dir.join(LOG_FILE)
}

/// OS probes and scheduler mutations are disabled in tests.
fn probes_disabled() -> bool {
    std::env::var_os("STATEROOT_TEST_CMD_PROBES").is_some()
}

fn run_quiet(program: &str, args: &[&str]) -> bool {
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_output(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

// ---------------------------------------------------------------------
// registration descriptors (pure — unit-tested on every OS)
// ---------------------------------------------------------------------

fn systemd_unit_text(exe: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=StateRoot continuity service (deterministic local reconciliation)\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart=\"{}\" service run\n\
         Restart=on-failure\n\
         RestartSec=10\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe.display()
    )
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launchd_plist_text(exe: &Path, log: &Path) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{LAUNCHD_LABEL}</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t<string>{}</string>\n\
         \t\t<string>service</string>\n\
         \t\t<string>run</string>\n\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>KeepAlive</key>\n\
         \t<true/>\n\
         \t<key>StandardOutPath</key>\n\
         \t<string>{}</string>\n\
         \t<key>StandardErrorPath</key>\n\
         \t<string>{}</string>\n\
         </dict>\n\
         </plist>\n",
        exe.display(),
        log.display(),
        log.display()
    )
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn schtasks_create_args(exe: &Path) -> Vec<String> {
    vec![
        "/create".into(),
        "/f".into(),
        "/tn".into(),
        SCHTASKS_NAME.into(),
        "/sc".into(),
        "onlogon".into(),
        "/tr".into(),
        format!("\"{}\" service run", exe.display()),
    ]
}

/// WSL fallback: a Windows-host logon task that launches the service inside
/// the current WSL distribution.
fn wsl_schtasks_create_args(distro: &str, exe: &Path) -> Vec<String> {
    vec![
        "/create".into(),
        "/f".into(),
        "/tn".into(),
        SCHTASKS_NAME.into(),
        "/sc".into(),
        "onlogon".into(),
        "/tr".into(),
        format!("wsl.exe -d {distro} -e {} service run", exe.display()),
    ]
}

fn schtasks_delete_args() -> Vec<String> {
    vec![
        "/delete".into(),
        "/f".into(),
        "/tn".into(),
        SCHTASKS_NAME.into(),
    ]
}

// ---------------------------------------------------------------------
// registration + heartbeat files
// ---------------------------------------------------------------------

fn write_registration(config_dir: &Path, kind: &str, detail: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let registration = ServiceRegistration {
        schema_version: REGISTRATION_SCHEMA.into(),
        kind: kind.into(),
        installed_at: now_rfc3339(),
        detail: detail.into(),
    };
    let value = serde_json::to_value(&registration)?;
    safe_io::atomic_replace_json(&continuity::service_registration_path(config_dir), &value)?;
    Ok(())
}

fn clear_service_files(config_dir: &Path) {
    let _ = std::fs::remove_file(continuity::service_registration_path(config_dir));
    let _ = std::fs::remove_file(continuity::service_heartbeat_path(config_dir));
}

fn write_heartbeat(config_dir: &Path, projects_scanned: usize) {
    let heartbeat = ServiceHeartbeat {
        schema_version: HEARTBEAT_SCHEMA.into(),
        pid: std::process::id(),
        beat_at: now_rfc3339(),
        version: crate::cli::BUILD_VERSION.to_string(),
        projects_scanned,
    };
    if let Ok(value) = serde_json::to_value(&heartbeat) {
        let _ =
            safe_io::atomic_replace_json(&continuity::service_heartbeat_path(config_dir), &value);
    }
}

/// `(running, last_beat)`: pid alive AND heartbeat fresh.
fn service_live(ctx: &Ctx) -> (bool, Option<String>) {
    let Some(beat) = continuity::read_service_heartbeat(&ctx.config_dir) else {
        return (false, None);
    };
    let pid_live = beat.pid > 0 && safe_io::pid_alive(beat.pid);
    let fresh = !continuity::service_beat_stale(
        &beat.beat_at,
        &now_rfc3339(),
        ctx.config.continuity.poll_interval_seconds,
    );
    (pid_live && fresh, Some(beat.beat_at))
}

fn kill_pid(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // SAFETY: plain signal send to a recorded service pid.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) == 0 }
    }
    #[cfg(windows)]
    {
        run_quiet("taskkill", &["/PID", &pid.to_string(), "/F"])
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

// ---------------------------------------------------------------------
// the resident loop
// ---------------------------------------------------------------------

/// Run the resident reconciliation loop (what OS descriptors execute).
/// Single-instance per user: a live lock holder means we exit quietly.
pub async fn run_resident(ctx: &Ctx) -> anyhow::Result<()> {
    if !ctx.config.continuity.enabled {
        println!("continuity is disabled in config ([continuity] enabled = false) — exiting");
        return Ok(());
    }
    std::fs::create_dir_all(&ctx.config_dir)?;
    // Two tries: a crashed predecessor's stale lock is reclaimed on the
    // first attempt and acquired on the second (a LIVE holder still loses).
    let _guard = match ResourceLock::acquire_with_budget(lock_path(&ctx.config_dir), 2, 10) {
        Ok(guard) => guard,
        Err(err) => {
            println!("continuity service already running ({err}) — exiting");
            return Ok(());
        }
    };
    let poll = std::time::Duration::from_secs(ctx.config.continuity.poll_interval_seconds.max(5));
    let log = log_path(&ctx.config_dir);
    log_line(
        &log,
        &format!("continuity service started (pid {})", std::process::id()),
    );
    let mut scans: u64 = 0;
    loop {
        let scanned = scan_all_projects(ctx).await;
        write_heartbeat(&ctx.config_dir, scanned);
        scans += 1;
        // Bounded log: per-scan lines only on the first scan and every
        // ~20 minutes thereafter; errors always log immediately.
        if scans == 1 || scans % 40 == 0 {
            log_line(
                &log,
                &format!("scan #{scans}: {scanned} project(s) reconciled"),
            );
        }
        std::thread::sleep(poll);
    }
}

/// Reconcile every registered project once. Returns the count reconciled.
async fn scan_all_projects(ctx: &Ctx) -> usize {
    let registry = match stateroot_core::config::load_registry(&ctx.config_dir) {
        Ok(registry) => registry,
        Err(err) => {
            log_line(
                &log_path(&ctx.config_dir),
                &format!("registry unreadable: {err}"),
            );
            return 0;
        }
    };
    let mut scanned = 0usize;
    for key in registry.projects.keys() {
        let Some(dir) = stateroot_core::path_identity::resolve_existing_dir(Path::new(key)) else {
            continue; // missing projects surface as attention in the others
        };
        if !stateroot_core::local_store::is_stateroot_dir(&dir) {
            continue; // corrupt record: directory without a stateroot store
        }
        // Per-project lock: skip while another reconciler holds this project.
        let lock = stateroot_core::local_store::root(&dir).join("local/locks/continuity.lock");
        let Some(_project_guard) = stateroot_core::fs_lock::FileLock::acquire(&lock) else {
            continue;
        };
        match continuity::reconcile(&dir, &ctx.config_dir, &ctx.config.continuity) {
            Ok(_) => {
                scanned += 1;
                // Optional advisory synthesis: hash-gated, advisory-only,
                // never a blocker for the deterministic projection.
                if ctx.config.continuity.synthesis {
                    let _ = super::continuity_synthesis::maybe_advise(ctx, &dir).await;
                }
            }
            Err(err) => log_line(
                &log_path(&ctx.config_dir),
                &format!("reconcile {}: {err}", dir.display()),
            ),
        }
    }
    scanned
}

fn log_line(path: &Path, line: &str) {
    use std::io::Write;
    let _ = detached::rotate_log_if_needed(path);
    if let Ok((mut out, _)) = detached::open_log_append(path) {
        let _ = writeln!(out, "[{}] {line}", now_rfc3339());
    }
}

// ---------------------------------------------------------------------
// install / remove / lifecycle
// ---------------------------------------------------------------------

fn systemd_user_available() -> bool {
    if probes_disabled() {
        return false;
    }
    run_output("systemctl", &["--user", "is-system-running"])
        .map(|out| out.contains("running") || out.contains("degraded"))
        .unwrap_or(false)
}

fn install_systemd(ctx: &Ctx, exe: &Path) -> anyhow::Result<()> {
    let dir = dirs_systemd_user();
    std::fs::create_dir_all(&dir)?;
    let unit = dir.join(SYSTEMD_UNIT);
    std::fs::write(&unit, systemd_unit_text(exe))?;
    if !probes_disabled() {
        if !run_quiet("systemctl", &["--user", "daemon-reload"]) {
            return Err(anyhow!("systemctl --user daemon-reload failed"));
        }
        if !run_quiet("systemctl", &["--user", "enable", "--now", SYSTEMD_UNIT]) {
            return Err(anyhow!(
                "systemctl --user enable --now {SYSTEMD_UNIT} failed"
            ));
        }
    }
    let _ = ctx;
    Ok(())
}

fn dirs_systemd_user() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("systemd/user");
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config/systemd/user")
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn install_launchd(ctx: &Ctx, exe: &Path) -> anyhow::Result<()> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME unset"))?;
    let dir = PathBuf::from(home).join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir)?;
    let plist = dir.join(format!("{LAUNCHD_LABEL}.plist"));
    std::fs::write(&plist, launchd_plist_text(exe, &log_path(&ctx.config_dir)))?;
    if !probes_disabled() {
        let uid = run_output("id", &["-u"]).ok_or_else(|| anyhow!("id -u failed"))?;
        let uid = uid.trim();
        let domain = format!("gui/{uid}");
        // bootout first is best-effort (the label may not exist yet).
        let _ = run_quiet(
            "launchctl",
            &["bootout", &format!("{domain}/{LAUNCHD_LABEL}")],
        );
        if !run_quiet(
            "launchctl",
            &["bootstrap", &domain, &plist.to_string_lossy()],
        ) {
            return Err(anyhow!("launchctl bootstrap failed"));
        }
    }
    Ok(())
}

fn install_schtasks(exe: &Path, args: Vec<String>) -> anyhow::Result<()> {
    if probes_disabled() {
        return Ok(());
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    if run_quiet("schtasks", &refs) {
        Ok(())
    } else {
        Err(anyhow!("schtasks /create failed"))
    }
    .map(|_| {
        let _ = exe;
    })
}

/// Install + start the continuity service. Falls back to a detached process
/// when OS registration is unavailable — degraded, never absent.
pub fn install(ctx: &Ctx) -> anyhow::Result<()> {
    if !ctx.config.continuity.enabled {
        println!(
            "continuity is disabled in config ([continuity] enabled = false) — not installing"
        );
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let mut kind = "detached";
    let mut detail = String::new();

    #[cfg(target_os = "linux")]
    {
        if systemd_user_available() {
            match install_systemd(ctx, &exe) {
                Ok(()) => {
                    kind = "systemd-user";
                    detail = SYSTEMD_UNIT.into();
                }
                Err(err) => note!("systemd registration failed: {err} — falling back to detached"),
            }
        } else if super::editor_extensions::is_wsl() {
            let distro = std::env::var("WSL_DISTRO_NAME").unwrap_or_else(|_| "Ubuntu".into());
            match install_schtasks(&exe, wsl_schtasks_create_args(&distro, &exe)) {
                Ok(()) => {
                    kind = "wsl-schtasks";
                    detail = format!("{SCHTASKS_NAME} via wsl.exe -d {distro}");
                }
                Err(err) => {
                    note!("Windows-host task registration failed: {err} — falling back to detached")
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        match install_launchd(ctx, &exe) {
            Ok(()) => {
                kind = "launchd";
                detail = LAUNCHD_LABEL.into();
            }
            Err(err) => note!("LaunchAgent registration failed: {err} — falling back to detached"),
        }
    }
    #[cfg(target_os = "windows")]
    {
        match install_schtasks(&exe, schtasks_create_args(&exe)) {
            Ok(()) => {
                kind = "schtasks";
                detail = SCHTASKS_NAME.into();
            }
            Err(err) => {
                note!("Scheduled Task registration failed: {err} — falling back to detached")
            }
        }
    }

    if kind == "detached" {
        detail = "OS registration unavailable — detached process; doctor reports degraded coverage"
            .into();
    }
    write_registration(&ctx.config_dir, kind, &detail)?;
    println!(
        "continuity service registered ({kind}){detail_suffix}",
        detail_suffix = if detail.is_empty() {
            String::new()
        } else {
            format!(" — {detail}")
        }
    );

    // Start now: OS managers were started above (enable --now / bootstrap /
    // onlogon is next-logon) — for schtasks kinds and the detached fallback,
    // start immediately so coverage begins with this session.
    if matches!(kind, "schtasks" | "wsl-schtasks" | "detached") {
        start(ctx)?;
    }
    Ok(())
}

/// Start the service through its registered manager, or detached when the
/// registration says so / is absent.
pub fn start(ctx: &Ctx) -> anyhow::Result<()> {
    let (live, _) = service_live(ctx);
    if live {
        println!("continuity service already running");
        return Ok(());
    }
    let registration = continuity::read_service_registration(&ctx.config_dir);
    match registration.as_ref().map(|r| r.kind.as_str()) {
        Some("systemd-user") if !probes_disabled() => {
            if run_quiet("systemctl", &["--user", "start", SYSTEMD_UNIT]) {
                println!("continuity service started (systemd --user)");
                return Ok(());
            }
            note!("systemctl start failed — spawning detached");
        }
        Some("launchd") if !probes_disabled() => {
            if let Some(uid) = run_output("id", &["-u"]) {
                let domain = format!("gui/{}/{LAUNCHD_LABEL}", uid.trim());
                if run_quiet("launchctl", &["kickstart", &domain]) {
                    println!("continuity service started (launchd)");
                    return Ok(());
                }
            }
            note!("launchctl kickstart failed — spawning detached");
        }
        Some("schtasks") | Some("wsl-schtasks") if !probes_disabled() => {
            let program = if cfg!(windows) {
                "schtasks"
            } else {
                "schtasks.exe"
            };
            if run_quiet(program, &["/run", "/tn", SCHTASKS_NAME]) {
                println!("continuity service started ({SCHTASKS_NAME})");
                return Ok(());
            }
            note!("schtasks /run failed — spawning detached");
        }
        _ => {}
    }
    spawn_detached_service(ctx)?;
    println!(
        "continuity service started (detached, log {})",
        log_path(&ctx.config_dir).display()
    );
    Ok(())
}

fn spawn_detached_service(ctx: &Ctx) -> anyhow::Result<u32> {
    if probes_disabled() {
        // Test seam (mirrors drain_finalize::kick): no real detached
        // children under probes; the registration honestly says detached.
        note!("detached service spawn suppressed under test probes");
        return Ok(0);
    }
    std::fs::create_dir_all(&ctx.config_dir)?;
    let log = log_path(&ctx.config_dir);
    let pid = detached::spawn_self(&["service".into(), "run".into()], &ctx.cwd, &log)?;
    Ok(pid)
}

/// Stop the running service (OS manager + any recorded heartbeat pid).
pub fn stop(ctx: &Ctx) -> anyhow::Result<()> {
    let registration = continuity::read_service_registration(&ctx.config_dir);
    match registration.as_ref().map(|r| r.kind.as_str()) {
        Some("systemd-user") if !probes_disabled() => {
            run_quiet("systemctl", &["--user", "stop", SYSTEMD_UNIT]);
        }
        Some("launchd") if !probes_disabled() => {
            if let Some(uid) = run_output("id", &["-u"]) {
                let domain = format!("gui/{}/{LAUNCHD_LABEL}", uid.trim());
                run_quiet("launchctl", &["kill", "SIGTERM", &domain]);
            }
        }
        Some("schtasks") | Some("wsl-schtasks") if !probes_disabled() => {
            let program = if cfg!(windows) {
                "schtasks"
            } else {
                "schtasks.exe"
            };
            run_quiet(program, &["/end", "/tn", SCHTASKS_NAME]);
        }
        _ => {}
    }
    // The heartbeat pid covers the detached kind and any stale manager run.
    if let Some(beat) = continuity::read_service_heartbeat(&ctx.config_dir) {
        if beat.pid > 0 && safe_io::pid_alive(beat.pid) && beat.pid != std::process::id() {
            kill_pid(beat.pid);
        }
    }
    let _ = std::fs::remove_file(continuity::service_heartbeat_path(&ctx.config_dir));
    println!("continuity service stopped");
    Ok(())
}

/// Restart the service.
pub fn restart(ctx: &Ctx) -> anyhow::Result<()> {
    stop(ctx)?;
    start(ctx)
}

/// Unregister + stop. The registration and heartbeat files go with it.
pub fn remove(ctx: &Ctx) -> anyhow::Result<()> {
    stop(ctx)?;
    let registration = continuity::read_service_registration(&ctx.config_dir);
    match registration.as_ref().map(|r| r.kind.as_str()) {
        Some("systemd-user") if !probes_disabled() => {
            run_quiet("systemctl", &["--user", "disable", "--now", SYSTEMD_UNIT]);
            let _ = std::fs::remove_file(dirs_systemd_user().join(SYSTEMD_UNIT));
        }
        Some("launchd") if !probes_disabled() => {
            if let Some(uid) = run_output("id", &["-u"]) {
                let domain = format!("gui/{}/{LAUNCHD_LABEL}", uid.trim());
                run_quiet("launchctl", &["bootout", &domain]);
            }
            if let Some(home) = std::env::var_os("HOME") {
                let _ = std::fs::remove_file(
                    PathBuf::from(home)
                        .join("Library/LaunchAgents")
                        .join(format!("{LAUNCHD_LABEL}.plist")),
                );
            }
        }
        Some("schtasks") | Some("wsl-schtasks") if !probes_disabled() => {
            let program = if cfg!(windows) {
                "schtasks"
            } else {
                "schtasks.exe"
            };
            let args = schtasks_delete_args();
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            run_quiet(program, &refs);
        }
        _ => {}
    }
    clear_service_files(&ctx.config_dir);
    println!("continuity service removed");
    Ok(())
}

/// Registration + liveness report.
pub fn status(ctx: &Ctx, json: bool) -> anyhow::Result<()> {
    let registration = continuity::read_service_registration(&ctx.config_dir);
    let (live, last_beat) = service_live(ctx);
    if json {
        let payload = serde_json::json!({
            "schema_version": "stateroot.service-status.v1",
            "enabled": ctx.config.continuity.enabled,
            "registered": registration.is_some(),
            "kind": registration.as_ref().map(|r| r.kind.clone()),
            "detail": registration.as_ref().map(|r| r.detail.clone()),
            "running": live,
            "last_beat_at": last_beat,
            "poll_interval_seconds": ctx.config.continuity.poll_interval_seconds,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    println!(
        "continuity: {}",
        if ctx.config.continuity.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    match &registration {
        Some(r) => println!("registered: {} ({})", r.kind, r.detail),
        None => println!(
            "registered: no — hooks/CLI reconcile on activity (degraded background coverage)"
        ),
    }
    println!(
        "running:    {}{}",
        if live { "yes" } else { "no" },
        last_beat
            .map(|b| format!(" · last beat {b}"))
            .unwrap_or_default()
    );
    Ok(())
}

/// Best-effort install used by `stateroot install` and self-update rearm:
/// install only when enabled and not yet registered; never fails the caller.
pub fn ensure_installed(ctx: &Ctx) {
    if !ctx.config.continuity.enabled {
        return;
    }
    if continuity::read_service_registration(&ctx.config_dir).is_some() {
        return;
    }
    if let Err(err) = install(ctx) {
        note!("continuity service install: {err}");
    }
}

/// Best-effort removal used by uninstall.
pub fn ensure_removed(ctx: &Ctx) {
    if continuity::read_service_registration(&ctx.config_dir).is_none()
        && continuity::read_service_heartbeat(&ctx.config_dir).is_none()
    {
        return;
    }
    if let Err(err) = remove(ctx) {
        note!("continuity service remove: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_unit_runs_service_run() {
        let text = systemd_unit_text(Path::new("/usr/local/bin/stateroot"));
        assert!(text.contains("ExecStart=\"/usr/local/bin/stateroot\" service run"));
        assert!(text.contains("WantedBy=default.target"));
        assert!(text.contains("Restart=on-failure"));
    }

    #[test]
    fn launchd_plist_keeps_alive_and_logs() {
        let text = launchd_plist_text(
            Path::new("/opt/stateroot"),
            Path::new("/Users/x/.config/stateroot/continuity-service.log"),
        );
        assert!(text.contains("<string>dev.stateroot.continuity</string>"));
        assert!(text.contains("<string>/opt/stateroot</string>"));
        assert!(text.contains("<key>KeepAlive</key>"));
        assert!(text.contains("<key>RunAtLoad</key>"));
        assert!(text.contains("continuity-service.log"));
    }

    #[test]
    fn schtasks_args_register_logon_task() {
        let args = schtasks_create_args(Path::new("C:\\tools\\stateroot.exe"));
        assert!(args.contains(&"onlogon".to_string()));
        assert!(args.contains(&SCHTASKS_NAME.to_string()));
        let tr = args.last().expect("tr");
        assert!(tr.contains("stateroot.exe"));
        assert!(tr.ends_with("service run"));
    }

    #[test]
    fn wsl_schtasks_args_launch_through_wsl() {
        let args = wsl_schtasks_create_args("Ubuntu", Path::new("/home/u/bin/stateroot"));
        let tr = args.last().expect("tr");
        assert_eq!(tr, "wsl.exe -d Ubuntu -e /home/u/bin/stateroot service run");
        assert!(args.contains(&"onlogon".to_string()));
    }

    #[test]
    fn schtasks_delete_targets_same_task() {
        let args = schtasks_delete_args();
        assert!(args.contains(&"/delete".to_string()));
        assert!(args.contains(&SCHTASKS_NAME.to_string()));
    }
}
