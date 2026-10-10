//! `stateroot service` — the per-user background continuity service.
//!
//! One resident deterministic process reconciles every registered project:
//! it scans the canonical registry on the poll interval, writes each
//! project's machine-local continuity projection atomically, maintains a
//! heartbeat, and keeps a capped log. It is active but NON-agentic — no
//! model calls, no inferred intent, ever.
//!
//! OS registration: per-user systemd service (Linux),
//! logon Scheduled Task (native Windows, hidden launcher). Under WSL a
//! functional user-systemd wins; otherwise a Windows-host task launches the
//! service through the current WSL distribution. Scheduled-task names are
//! scoped to the owning user + config home so a second WSL distribution or
//! an isolated fixture never collides with the owner's real task. When OS
//! registration is unavailable the service runs detached instead, hooks/CLI
//! keep reconciling on activity, and `doctor` reports degraded coverage.
//! Native macOS ownership is currently unverified: LaunchAgent templates
//! remain inspectable, but this build never mutates an unproven manager or
//! signals a numeric PID without a retained native process identity.

use std::path::{Path, PathBuf};

use anyhow::anyhow;
use stateroot_core::continuity::{self, ServiceHeartbeat, ServiceRegistration};
use stateroot_core::local_store::now_rfc3339;
use stateroot_core::safe_io::{self, LockError, ResourceLock};

use super::{bounded_run, bounded_status, detached, note, Ctx};

const SYSTEMD_UNIT: &str = "stateroot-continuity.service";
/// Pre-WS3 global task name. Install/remove repair it into the scoped name —
/// but only after the descriptor verifiably references a stateroot launcher;
/// an unrecognized task by that name is left untouched (unknown stays
/// degraded, never a blind removal).
const LEGACY_SCHTASKS_NAME: &str = "StateRoot Continuity";
const LAUNCHD_LABEL: &str = "dev.stateroot.continuity";
const LOG_FILE: &str = "continuity-service.log";
const LOCK_FILE: &str = "continuity-service.lock";
const LAUNCHER_FILE: &str = "continuity-service.launcher.vbs";
const HEARTBEAT_SCHEMA: &str = "stateroot.continuity-heartbeat.v1";
const REGISTRATION_SCHEMA: &str = "stateroot.continuity-registration.v1";
/// How long install waits for the first heartbeat of a service it started.
const STARTUP_HEARTBEAT_WAIT_MS: u64 = 4_000;
/// How long stop waits for a signalled service to exit before reporting.
const SHUTDOWN_WAIT_MS: u64 = 5_000;
/// Wall-clock budget for one OS manager operation (systemctl/launchctl/
/// schtasks/taskkill/powershell). A hung manager must never wedge
/// install/status/doctor — an overrun reports unknown, never success.
const MANAGER_OP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn lock_path(config_dir: &Path) -> PathBuf {
    config_dir.join(LOCK_FILE)
}

fn log_path(config_dir: &Path) -> PathBuf {
    config_dir.join(LOG_FILE)
}

fn launcher_path(config_dir: &Path) -> PathBuf {
    config_dir.join(LAUNCHER_FILE)
}

/// OS probes and scheduler mutations are disabled in tests.
fn probes_disabled() -> bool {
    std::env::var_os("STATEROOT_TEST_CMD_PROBES").is_some()
}

fn run_quiet(program: &str, args: &[&str]) -> bool {
    bounded_status(program, args, MANAGER_OP_TIMEOUT)
}

fn run_output(program: &str, args: &[&str]) -> Option<String> {
    let run = bounded_run(program, args, MANAGER_OP_TIMEOUT, true)?;
    run.success.then_some(run.stdout)
}

// ---------------------------------------------------------------------
// registration descriptors (pure — unit-tested on every OS)
// ---------------------------------------------------------------------

/// systemd quoted argument: double-quote wrapped, `\` and `"` escaped, `%`
/// doubled (unit specifier expansion would otherwise eat it).
fn systemd_quote(arg: &str) -> String {
    let s = arg
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{s}\"")
}

/// XML text/attribute escaping for the launchd plist.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Quote one argument for the Windows command line a task action stores.
/// `"` is invalid in Windows file names; a trailing backslash would escape
/// the closing quote, so it is doubled.
fn cmd_quote(arg: &str) -> String {
    let mut s = arg.replace('"', "");
    if s.ends_with('\\') {
        s.push('\\');
    }
    format!("\"{s}\"")
}

/// The Task Scheduler name for THIS installation: scoped to the owning user,
/// the config home, AND the WSL distribution — the same path and user in two
/// distributions (or two distros sharing one Windows host) must never share
/// one task. Characters invalid in task names are stripped.
fn schtasks_name_for(config_dir: &Path, distro: Option<&str>) -> String {
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "user".into());
    let clean: String = user
        .chars()
        .filter(|c| !r#"\/:*?"<>|"#.contains(*c))
        .collect();
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(config_dir.display().to_string().as_bytes());
    // The distro is part of the scoped identity even when absent (native
    // Windows) so a native task and a WSL task never collide either.
    hasher.update(b"\0distro:\0".as_slice());
    hasher.update(distro.unwrap_or("").as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    format!("StateRoot Continuity ({}-{})", clean, &hash[..8])
}

/// The scoped task name for this runtime (WSL distro from the environment).
fn schtasks_name(config_dir: &Path) -> String {
    let distro = std::env::var("WSL_DISTRO_NAME")
        .ok()
        .filter(|d| !d.trim().is_empty());
    schtasks_name_for(config_dir, distro.as_deref())
}

/// The schtasks program name for this runtime (`schtasks.exe` under WSL
/// interop, bare `schtasks` on native Windows).
fn schtasks_program() -> &'static str {
    if cfg!(windows) {
        "schtasks"
    } else {
        "schtasks.exe"
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn systemd_unit_text(exe: &Path, config_home: &Path) -> String {
    // The descriptor pins the config home it was registered for — the
    // service starts with THIS STATEROOT_HOME, not whatever the logon
    // environment happens to carry.
    format!(
        "[Unit]\n\
         Description=StateRoot continuity service (deterministic local reconciliation)\n\
         \n\
         [Service]\n\
         Type=simple\n\
         Environment={}\n\
         ExecStart={} service run\n\
         Restart=on-failure\n\
         RestartSec=10\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        systemd_quote(&format!("STATEROOT_HOME={}", config_home.display())),
        systemd_quote(&exe.display().to_string())
    )
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launchd_plist_text(exe: &Path, log: &Path, config_home: &Path) -> String {
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
         \t<key>EnvironmentVariables</key>\n\
         \t<dict>\n\
         \t\t<key>STATEROOT_HOME</key>\n\
         \t\t<string>{}</string>\n\
         \t</dict>\n\
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
        xml_escape(&exe.display().to_string()),
        xml_escape(&config_home.display().to_string()),
        xml_escape(&log.display().to_string()),
        xml_escape(&log.display().to_string())
    )
}

/// Native Windows hidden launcher: wscript runs the service through cmd
/// redirection with no window (0 = hidden, False = do not wait), appending
/// stdout/stderr to the persistent service log. The descriptor pins the
/// config home it was registered for (Wscript PROCESS environment), so the
/// launched service selects THIS config home regardless of the logon
/// environment. VBScript literals double their quotes; `"` is invalid in
/// Windows paths so the defensive doubling never triggers for real paths.
fn windows_launcher_vbs(exe: &Path, log: &Path, config_home: &Path) -> String {
    let exe_v = exe.display().to_string().replace('"', "\"\"");
    let log_v = log.display().to_string().replace('"', "\"\"");
    let cfg_v = config_home.display().to_string().replace('"', "\"\"");
    format!(
        "' StateRoot continuity service — generated hidden launcher; do not edit\r\n\
         Set shell = CreateObject(\"Wscript.Shell\")\r\n\
         Set processEnv = shell.Environment(\"PROCESS\")\r\n\
         processEnv(\"STATEROOT_HOME\") = \"{cfg_v}\"\r\n\
         processEnv(\"STATEROOT_SERVICE_EXE\") = \"{exe_v}\"\r\n\
         processEnv(\"STATEROOT_SERVICE_LOG\") = \"{log_v}\"\r\n\
         shell.Run \"cmd.exe /d /s /c \"\"\"\"%STATEROOT_SERVICE_EXE%\"\" service run >> \"\"%STATEROOT_SERVICE_LOG%\"\" 2>&1\"\"\", 0, False\r\n"
    )
}

/// WSL hidden launcher: wscript runs wsl.exe directly with an explicit Linux
/// env argument. Existing WSLENV entries/flags are never rewritten; the
/// service's own Linux-side log captures output.
fn wsl_launcher_vbs(distro: &str, exe: &Path, config_home: &Path) -> String {
    let distro_v = distro.replace('"', "\"\"");
    let exe_v = exe.display().to_string().replace('"', "\"\"");
    let cfg_v = config_home.display().to_string().replace('"', "\"\"");
    format!(
        "' StateRoot continuity service — generated hidden launcher (WSL); do not edit\r\n\
         Set shell = CreateObject(\"Wscript.Shell\")\r\n\
         shell.Run \"wsl.exe -d \"\"{distro_v}\"\" -e env \"\"STATEROOT_HOME={cfg_v}\"\" \"\"{exe_v}\"\" service run\", 0, False\r\n"
    )
}

/// schtasks /create argv for the hidden-launcher task.
#[cfg_attr(not(any(target_os = "linux", target_os = "windows")), allow(dead_code))]
fn schtasks_create_args(name: &str, launcher: &Path) -> Vec<String> {
    vec![
        "/create".into(),
        "/f".into(),
        "/tn".into(),
        name.into(),
        "/sc".into(),
        "onlogon".into(),
        "/tr".into(),
        format!(
            "wscript.exe //B //nologo {}",
            cmd_quote(&launcher.display().to_string())
        ),
    ]
}

fn schtasks_delete_args(name: &str) -> Vec<String> {
    vec!["/delete".into(), "/f".into(), "/tn".into(), name.into()]
}

/// Write the hidden launcher for this install kind. Native Windows logs to
/// the service log via cmd redirection; WSL relies on the service's own
/// Linux-side log. Both pin STATEROOT_HOME to THIS config home.
#[cfg_attr(not(any(target_os = "linux", target_os = "windows")), allow(dead_code))]
fn write_launcher(config_dir: &Path, kind: &str, exe: &Path) -> std::io::Result<PathBuf> {
    let launcher = launcher_path(config_dir);
    let body = if kind == "wsl-schtasks" {
        let distro = std::env::var("WSL_DISTRO_NAME").unwrap_or_else(|_| "Ubuntu".into());
        wsl_launcher_vbs(&distro, exe, config_dir)
    } else {
        windows_launcher_vbs(exe, &log_path(config_dir), config_dir)
    };
    std::fs::write(&launcher, launcher_bytes(&body))?;
    Ok(launcher)
}

fn launcher_bytes(body: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend(body.encode_utf16().flat_map(u16::to_le_bytes));
    bytes
}

// ---------------------------------------------------------------------
// registration + heartbeat files
// ---------------------------------------------------------------------

fn write_registration(
    config_dir: &Path,
    kind: &str,
    detail: &str,
    exe: &Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let registration = ServiceRegistration {
        schema_version: REGISTRATION_SCHEMA.into(),
        kind: kind.into(),
        installed_at: now_rfc3339(),
        detail: detail.into(),
        exe: exe.display().to_string(),
        config_home: config_dir.display().to_string(),
        exe_identity: file_identity(exe).unwrap_or_default(),
        build_version: crate::cli::BUILD_VERSION.to_string(),
    };
    let value = serde_json::to_value(&registration)?;
    safe_io::atomic_replace_json(&continuity::service_registration_path(config_dir), &value)?;
    Ok(())
}

fn clear_service_files(config_dir: &Path) {
    let _ = std::fs::remove_file(continuity::service_registration_path(config_dir));
    let _ = std::fs::remove_file(continuity::service_heartbeat_path(config_dir));
    let _ = std::fs::remove_file(launcher_path(config_dir));
}

/// Write the heartbeat. Failures are real (C5): the caller logs them — a
/// service that cannot heartbeat looks dead to every status surface.
fn write_heartbeat(config_dir: &Path, projects_scanned: usize) -> anyhow::Result<()> {
    let heartbeat = ServiceHeartbeat {
        schema_version: HEARTBEAT_SCHEMA.into(),
        pid: std::process::id(),
        beat_at: now_rfc3339(),
        version: crate::cli::BUILD_VERSION.to_string(),
        exe: std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        namespace: safe_io::host_namespace().unwrap_or("").to_string(),
        pid_start: pid_start_token(std::process::id()).unwrap_or(0),
        config_home: config_dir.display().to_string(),
        exe_identity: running_image_identity(),
        projects_scanned,
    };
    let value = serde_json::to_value(&heartbeat)?;
    safe_io::atomic_replace_json(&continuity::service_heartbeat_path(config_dir), &value)?;
    Ok(())
}

/// `(running, last_beat)`: the heartbeat is fresh AND its pid verifies
/// against the full recorded identity (binary, args, namespace,
/// process-start token, config home). A beat we cannot verify is NOT a
/// running service — unknown stays unknown (degraded), never claimed live.
pub(crate) fn service_live_for(
    config_dir: &Path,
    poll_interval_seconds: u64,
) -> (bool, Option<String>) {
    let Some(beat) = continuity::read_service_heartbeat(config_dir) else {
        return (false, None);
    };
    let fresh =
        !continuity::service_beat_stale(&beat.beat_at, &now_rfc3339(), poll_interval_seconds);
    if !fresh {
        return (false, Some(beat.beat_at));
    }
    let recorded_exe = if !beat.exe.is_empty() {
        beat.exe.clone()
    } else {
        continuity::read_service_registration(config_dir)
            .map(|reg| reg.exe)
            .unwrap_or_default()
    };
    let live = verified_service_pid(beat.pid, &recorded_exe, &beat, config_dir);
    (live, Some(beat.beat_at))
}

/// `(running, last_beat)` for this context.
fn service_live(ctx: &Ctx) -> (bool, Option<String>) {
    service_live_for(&ctx.config_dir, ctx.config.continuity.poll_interval_seconds)
}

// ---------------------------------------------------------------------
// process identity (C6): never signal an unverified pid
// ---------------------------------------------------------------------

/// The argv of `pid`, when this runtime can read it. Unix reads
/// /proc/<pid>/cmdline (NUL-separated — no quoting ambiguity); Windows asks
/// CIM for the CommandLine and splits it quote-aware. Cross-namespace pids
/// (WSL reading a Windows heartbeat pid or vice versa) resolve to None —
/// unverifiable, never signalled.
#[cfg(unix)]
fn pid_argv(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let argv: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    (!argv.is_empty()).then_some(argv)
}

/// Quote-aware command-line split (Windows CIM returns one string; `"` wraps
/// a segment that may contain spaces).
#[cfg(windows)]
fn split_command_line(cmdline: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut has_segment = false;
    for ch in cmdline.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                has_segment = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has_segment || !current.is_empty() {
                    argv.push(std::mem::take(&mut current));
                    has_segment = false;
                }
            }
            c => {
                current.push(c);
                has_segment = true;
            }
        }
    }
    if has_segment || !current.is_empty() {
        argv.push(current);
    }
    argv
}

#[cfg(windows)]
fn pid_argv(pid: u32) -> Option<Vec<String>> {
    let out = run_output(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); (Get-CimInstance Win32_Process -Filter \"ProcessId={pid}\").CommandLine"),
        ],
    )?;
    let line = out.trim();
    if line.is_empty() {
        return None;
    }
    let argv = split_command_line(line);
    (!argv.is_empty()).then_some(argv)
}

#[cfg(not(any(unix, windows)))]
fn pid_argv(_pid: u32) -> Option<Vec<String>> {
    None
}

/// Process-start token: binds a pid to THIS process instance — a reused pid
/// fails the comparison. unix: /proc/<pid>/stat starttime (jiffies, field
/// 22). Windows: process creation FILETIME via GetProcessTimes.
#[cfg(unix)]
fn pid_start_token(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The comm field may contain spaces/parens — everything after the LAST
    // ')' is fields 3..; starttime (field 22) is index 19 there.
    let rest = stat.rsplit(')').next()?;
    rest.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(windows)]
fn pid_start_token(pid: u32) -> Option<u64> {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetProcessTimes(
            handle: *mut c_void,
            creation: *mut u64,
            exit: *mut u64,
            kernel: *mut u64,
            user: *mut u64,
        ) -> i32;
    }
    // PROCESS_QUERY_LIMITED_INFORMATION; SAFETY: query-only handle, closed.
    let handle = unsafe { OpenProcess(0x1000, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let (mut creation, mut exit_t, mut kernel, mut user) = (0u64, 0u64, 0u64, 0u64);
    // SAFETY: live handle; out pointers are valid, writable u64 slots.
    let ok = unsafe { GetProcessTimes(handle, &mut creation, &mut exit_t, &mut kernel, &mut user) };
    unsafe {
        CloseHandle(handle);
    }
    (ok != 0 && creation != 0).then_some(creation)
}

#[cfg(not(any(unix, windows)))]
fn pid_start_token(_pid: u32) -> Option<u64> {
    None
}

/// Two executable paths are the same binary: exact string equality after
/// quote/separator normalization, or canonicalized equality when both
/// resolve. Windows volume paths compare case-insensitively.
fn exe_path_eq(a: &str, b: &str) -> bool {
    let clean = |s: &str| s.trim().trim_matches('"').to_string();
    let (a, b) = (clean(a), clean(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    // Do not fold Windows paths: directories can be case-sensitive. An
    // unprovable alias stays unknown rather than authorizing the wrong image.
    let eq = |x: &str, y: &str| x == y;
    if eq(&a, &b) {
        return true;
    }
    match (
        std::fs::canonicalize(Path::new(&a)),
        std::fs::canonicalize(Path::new(&b)),
    ) {
        (Ok(ca), Ok(cb)) => eq(&ca.display().to_string(), &cb.display().to_string()),
        _ => false,
    }
}

/// Relevant bounded metadata, not a binary hash on every poll. Identity
/// and build stamp detect replacement at the SAME installed path.
fn file_identity(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let m = std::fs::metadata(path).ok()?;
        Some(format!(
            "{}:{}:{}:{}:{}",
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec()
        ))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let handle = unsafe {
            win_process::CreateFileW(
                wide.as_ptr(),
                0,
                7,
                std::ptr::null_mut(),
                3,
                0x02000000,
                std::ptr::null_mut(),
            )
        };
        if handle as isize == -1 {
            return None;
        }
        let mut info = win_process::FileInfo::default();
        let ok = unsafe { win_process::GetFileInformationByHandle(handle, &mut info) };
        unsafe {
            win_process::CloseHandle(handle);
        }
        (ok != 0).then(|| {
            format!(
                "{}:{}:{}:{}:{}:{}:{}",
                info.volume,
                info.index_high,
                info.index_low,
                info.size_high,
                info.size_low,
                info.write[0],
                info.write[1]
            )
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

fn running_image_identity() -> String {
    #[cfg(target_os = "linux")]
    {
        file_identity(Path::new("/proc/self/exe")).unwrap_or_default()
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::current_exe()
            .ok()
            .and_then(|p| file_identity(&p))
            .unwrap_or_default()
    }
}

/// The service at `pid` runs against OUR config home: unix reads the pinned
/// STATEROOT_HOME from /proc/<pid>/environ (a pinned service must pin OUR
/// home; an unpinned service matches only an unpinned — default-home —
/// caller). Other platforms cannot read a foreign process's environment —
/// the config binding there rides the registration + heartbeat location.
#[cfg(unix)]
fn pid_config_matches(pid: u32, config_dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/environ")) else {
        return false; // unreadable environment — unverifiable, never assumed
    };
    let pin = raw
        .split(|b| *b == 0)
        .filter_map(|kv| kv.strip_prefix(b"STATEROOT_HOME="))
        .next();
    match pin {
        Some(value) => {
            let pinned = Path::new(std::ffi::OsStr::from_bytes(value));
            if pinned == config_dir {
                return true;
            }
            match (
                std::fs::canonicalize(pinned),
                std::fs::canonicalize(config_dir),
            ) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            }
        }
        None => std::env::var_os(stateroot_core::config::ENV_HOME).is_none(),
    }
}

#[cfg(not(unix))]
fn pid_config_matches(_pid: u32, _config_dir: &Path) -> bool {
    true
}

/// True only when `pid` VERIFIABLY belongs to our stateroot continuity
/// service — the exact recorded binary running exactly `service run` in our
/// host namespace, with the recorded process-start token, against our
/// config home. Every check is fail-closed: a legacy heartbeat (no exe /
/// namespace / start token), a spoofed argv, a wrong executable or config
/// home, a reused pid, or anything we cannot read NEVER authorizes a signal.
fn verified_service_pid(
    pid: u32,
    recorded_exe: &str,
    beat: &ServiceHeartbeat,
    config_dir: &Path,
) -> bool {
    VerifiedProcess::open(pid, recorded_exe, beat, config_dir).is_some()
}

/// Holds the process identity through signalling. Linux requires pidfd;
/// unsupported kernels/platforms remain unknown rather than falling back
/// to a verify-then-kill numeric PID race.
struct VerifiedProcess {
    #[cfg(target_os = "linux")]
    fd: std::os::fd::OwnedFd,
    #[cfg(windows)]
    handle: *mut std::ffi::c_void,
}

impl VerifiedProcess {
    fn open(
        pid: u32,
        recorded_exe: &str,
        beat: &ServiceHeartbeat,
        config_dir: &Path,
    ) -> Option<Self> {
        if pid == 0 || pid == std::process::id() || recorded_exe.is_empty() {
            return None;
        }
        // Namespace: a pid is meaningful only inside the runtime that recorded
        // it. Legacy beats carry no namespace — unverifiable, never signalled.
        if beat.namespace.is_empty() || Some(beat.namespace.as_str()) != safe_io::host_namespace() {
            return None;
        }
        if beat.pid != pid
            || beat.config_home.is_empty()
            || !exe_path_eq(&beat.config_home, &config_dir.display().to_string())
        {
            return None;
        }
        #[cfg(target_os = "linux")]
        let process = {
            use std::os::fd::FromRawFd as _;
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            if fd < 0 {
                return None;
            }
            Self {
                fd: unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) },
            }
        };
        #[cfg(windows)]
        let process = {
            let handle = unsafe { win_process::OpenProcess(0x1000 | 0x00100000 | 0x0001, 0, pid) };
            if handle.is_null() {
                return None;
            }
            Self { handle }
        };
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = config_dir;
            return None;
        }
        #[cfg(any(target_os = "linux", windows))]
        {
            if process.gone() {
                return None;
            }
            let image = process.image(pid)?;
            let image_path = image.strip_suffix(" (deleted)").unwrap_or(&image);
            let recorded_path = recorded_exe
                .strip_suffix(" (deleted)")
                .unwrap_or(recorded_exe);
            if !exe_path_eq(image_path, recorded_path) {
                return None;
            }
            #[cfg(target_os = "linux")]
            if beat.exe_identity.is_empty()
                || file_identity(Path::new(&format!("/proc/{pid}/exe")))
                    != Some(beat.exe_identity.clone())
            {
                return None;
            }
            let argv = pid_argv(pid)?;
            // argv must be EXACTLY `<recorded exe> service run` — no substring
            // shapes, no filename fallback.
            if argv.len() != 3 || argv[1] != "service" || argv[2] != "run" {
                return None;
            }
            if !exe_path_eq(&argv[0], recorded_path) {
                return None;
            }
            // Process-start identity: the recorded pid must still be the process
            // that wrote the beat, not a reuser of the number.
            if beat.pid_start == 0 || process.birth(pid) != Some(beat.pid_start) {
                return None;
            }
            if !pid_config_matches(pid, config_dir) || process.gone() {
                return None;
            }
            Some(process)
        }
    }

    fn image(&self, pid: u32) -> Option<String> {
        #[cfg(target_os = "linux")]
        {
            std::fs::read_link(format!("/proc/{pid}/exe"))
                .ok()
                .map(|p| p.display().to_string())
        }
        #[cfg(windows)]
        {
            let _ = pid;
            let mut buf = vec![0u16; 32768];
            let mut len = buf.len() as u32;
            let ok = unsafe {
                win_process::QueryFullProcessImageNameW(self.handle, 0, buf.as_mut_ptr(), &mut len)
            };
            if ok == 0 {
                None
            } else {
                String::from_utf16(&buf[..len as usize]).ok()
            }
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = pid;
            None
        }
    }
    fn birth(&self, pid: u32) -> Option<u64> {
        #[cfg(target_os = "linux")]
        {
            pid_start_token(pid)
        }
        #[cfg(windows)]
        {
            let _ = pid;
            let (mut creation, mut exit, mut kernel, mut user) = (0u64, 0u64, 0u64, 0u64);
            let ok = unsafe {
                win_process::GetProcessTimes(
                    self.handle,
                    &mut creation,
                    &mut exit,
                    &mut kernel,
                    &mut user,
                )
            };
            (ok != 0 && creation != 0).then_some(creation)
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = pid;
            None
        }
    }
    fn gone(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd as _;
            let mut p = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut p, 1, 0) };
            result != 0
        }
        #[cfg(windows)]
        {
            unsafe { win_process::WaitForSingleObject(self.handle, 0) != 0x102 }
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            true
        }
    }
    fn signal(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd as _;
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.fd.as_raw_fd(),
                    libc::SIGTERM,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                ) == 0
            }
        }
        #[cfg(windows)]
        {
            unsafe { win_process::TerminateProcess(self.handle, 1) != 0 }
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            false
        }
    }
}

#[cfg(windows)]
impl Drop for VerifiedProcess {
    fn drop(&mut self) {
        unsafe {
            win_process::CloseHandle(self.handle);
        }
    }
}
#[cfg(windows)]
mod win_process {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Default)]
    pub struct FileInfo {
        pub attributes: u32,
        pub creation: [u32; 2],
        pub access: [u32; 2],
        pub write: [u32; 2],
        pub volume: u32,
        pub size_high: u32,
        pub size_low: u32,
        pub links: u32,
        pub index_high: u32,
        pub index_low: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        pub fn CloseHandle(handle: *mut c_void) -> i32;
        pub fn QueryFullProcessImageNameW(
            handle: *mut c_void,
            flags: u32,
            buf: *mut u16,
            len: *mut u32,
        ) -> i32;
        pub fn GetProcessTimes(
            handle: *mut c_void,
            creation: *mut u64,
            exit: *mut u64,
            kernel: *mut u64,
            user: *mut u64,
        ) -> i32;
        pub fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
        pub fn TerminateProcess(handle: *mut c_void, exit: u32) -> i32;
        pub fn CreateFileW(
            path: *const u16,
            access: u32,
            share: u32,
            security: *mut c_void,
            creation: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        pub fn GetFileInformationByHandle(handle: *mut c_void, info: *mut FileInfo) -> i32;
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
    // C5 taxonomy: only a verifiably live same-namespace owner is "already
    // running". A foreign-namespace or unreadable owner is still held
    // (fail-closed — never double-start) but is NOT called already-running,
    // and an I/O failure is an outright error, never contention.
    let _guard = match ResourceLock::acquire_with_budget(lock_path(&ctx.config_dir), 2, 10) {
        Ok(guard) => guard,
        Err(LockError::Timeout { owner, .. }) => {
            let record = std::fs::read_to_string(lock_path(&ctx.config_dir))
                .ok()
                .and_then(|t| serde_json::from_str::<safe_io::LockOwner>(&t).ok());
            match record {
                Some(o)
                    if o.pid != 0
                        && Some(o.namespace.as_str()) == safe_io::host_namespace()
                        && safe_io::pid_alive(o.pid) =>
                {
                    println!(
                        "continuity service already running (pid {}) — exiting",
                        o.pid
                    );
                }
                Some(o)
                    if !o.namespace.is_empty()
                        && Some(o.namespace.as_str()) != safe_io::host_namespace() =>
                {
                    println!(
                        "continuity service lock held by pid {} in another runtime — not starting a second instance",
                        o.pid
                    );
                }
                _ => {
                    println!(
                        "continuity service lock held but the owner is unverifiable ({owner}) — not starting; if no service is running, remove {}",
                        lock_path(&ctx.config_dir).display()
                    );
                }
            }
            return Ok(());
        }
        Err(LockError::Io(err)) => {
            return Err(anyhow!(
                "continuity service lock I/O failure on {}: {err} — NOT started; fix the config directory permissions/health",
                lock_path(&ctx.config_dir).display()
            ));
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
        if let Err(err) = write_heartbeat(&ctx.config_dir, scanned) {
            log_line(&log, &format!("heartbeat write FAILED: {err}"));
        }
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

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn systemd_user_available() -> bool {
    if probes_disabled() {
        return false;
    }
    run_output("systemctl", &["--user", "is-system-running"])
        .map(|out| out.contains("running") || out.contains("degraded"))
        .unwrap_or(false)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn install_systemd(ctx: &Ctx, exe: &Path) -> anyhow::Result<()> {
    let dir = dirs_systemd_user();
    std::fs::create_dir_all(&dir)?;
    let unit = dir.join(SYSTEMD_UNIT);
    if unit.exists() {
        require_owned_manager(ctx)?;
    } else if !probes_disabled() {
        let state = run_output(
            "systemctl",
            &[
                "--user",
                "show",
                SYSTEMD_UNIT,
                "--property=LoadState",
                "--value",
            ],
        )
        .ok_or_else(|| anyhow!("existing systemd manager state unknown; no overwrite"))?;
        if state.trim() != "not-found" {
            return Err(anyhow!(
                "existing systemd manager has no proven owning descriptor; no overwrite"
            ));
        }
    }
    std::fs::write(&unit, systemd_unit_text(exe, &ctx.config_dir))?;
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
    if !probes_disabled() {
        return Err(anyhow!("native launchd runtime ownership cannot be verified on this build; no manager mutation"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME unset"))?;
    let dir = PathBuf::from(home).join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir)?;
    let plist = dir.join(format!("{LAUNCHD_LABEL}.plist"));
    std::fs::write(
        &plist,
        launchd_plist_text(exe, &log_path(&ctx.config_dir), &ctx.config_dir),
    )?;
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

/// Register or update the scoped logon task. Also repairs the pre-WS3
/// global task name — but only when that task's descriptor verifiably
/// references a stateroot launcher; an unrecognized task by that name is
/// left untouched (unknown remains degraded, never blind removal).
#[cfg_attr(not(any(target_os = "linux", target_os = "windows")), allow(dead_code))]
fn install_schtasks(ctx: &Ctx, kind: &str, exe: &Path) -> anyhow::Result<String> {
    let name = schtasks_name(&ctx.config_dir);
    if !probes_disabled() {
        if let Some(xml) = query_task(&name)? {
            let reg = continuity::read_service_registration(&ctx.config_dir).ok_or_else(|| {
                anyhow!("existing scoped task has no owning registration; retained")
            })?;
            if !task_owned(ctx, &reg, &xml) {
                return Err(anyhow!(
                    "existing scoped task is foreign/unverifiable; retained without mutation"
                ));
            }
        }
        if let Some(xml) = query_task(LEGACY_SCHTASKS_NAME)? {
            let reg = continuity::read_service_registration(&ctx.config_dir)
                .ok_or_else(|| anyhow!("legacy task ownership unknown; retained"))?;
            if !task_owned(ctx, &reg, &xml) {
                return Err(anyhow!(
                    "legacy task is foreign/unverifiable; retained without mutation"
                ));
            }
            let args = schtasks_delete_args(LEGACY_SCHTASKS_NAME);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            if !run_quiet(schtasks_program(), &refs) {
                return Err(anyhow!(
                    "legacy task deletion unconfirmed; controls retained"
                ));
            }
        }
    }
    // The launcher is our own config-dir artifact — written even under test
    // probes (only the scheduler mutation is gated).
    let launcher = write_launcher(&ctx.config_dir, kind, exe)
        .map_err(|err| anyhow!("could not write hidden launcher: {err}"))?;
    if probes_disabled() {
        return Ok(name);
    }
    let program = schtasks_program();
    let host_launcher =
        scheduler_path(&launcher).ok_or_else(|| anyhow!("launcher Windows path unavailable"))?;
    let args = schtasks_create_args(&name, Path::new(&host_launcher));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    if run_quiet(program, &refs) {
        Ok(name)
    } else {
        Err(anyhow!("schtasks /create failed"))
    }
}

#[cfg_attr(not(any(target_os = "linux", target_os = "windows")), allow(dead_code))]
/// True when the task's descriptor verifiably runs a stateroot launcher —
/// the only evidence that a legacy-named task is ours to remove. Anything
/// unreadable/unrecognized is NOT ours.
#[cfg_attr(not(any(target_os = "linux", target_os = "windows")), allow(dead_code))]
fn xml_value(xml: &str, tag: &str) -> Option<String> {
    let start = format!("<{tag}>");
    let end = format!("</{tag}>");
    if xml.matches(&start).count() != 1 {
        return None;
    }
    let value = xml.split_once(&start)?.1.split_once(&end)?.0;
    Some(
        value
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}

fn scheduler_path(path: &Path) -> Option<String> {
    if cfg!(windows) {
        Some(path.display().to_string())
    } else {
        run_output("wslpath", &["-w", &path.display().to_string()])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

fn query_task(name: &str) -> anyhow::Result<Option<String>> {
    let name = name.replace('\'', "''");
    let script = format!("[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); $ErrorActionPreference='Stop'; $t=@(Get-ScheduledTask | Where-Object {{$_.TaskPath -eq '\\' -and $_.TaskName -eq '{name}'}}); if($t.Count -eq 0){{'__STATEROOT_ABSENT__'}} elseif($t.Count -eq 1){{Export-ScheduledTask -TaskName '{name}' -TaskPath '\\'}} else {{throw 'ambiguous task'}}");
    let out = run_output(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
    )
    .ok_or_else(|| anyhow!("scheduled task query unavailable/failed/oversized; no mutation"))?;
    if out.trim() == "__STATEROOT_ABSENT__" {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

fn task_action_owned(xml: &str, launcher: &str, sid: &str, system_root: &str) -> bool {
    if xml.matches("<Exec>").count() != 1 {
        return false;
    }
    let Some(command) = xml_value(xml, "Command") else {
        return false;
    };
    let command_ok = command.eq_ignore_ascii_case("wscript.exe")
        || command.eq_ignore_ascii_case(&format!(
            "{}\\System32\\wscript.exe",
            system_root.trim_end_matches('\\')
        ));
    command_ok
        && xml_value(xml, "Arguments")
            .is_some_and(|a| a == format!("//B //nologo {}", cmd_quote(launcher)))
        && xml_value(xml, "UserId").is_some_and(|u| u.eq_ignore_ascii_case(sid))
}

fn expected_launcher(kind: &str, exe: &Path, home: &Path) -> Option<Vec<u8>> {
    let body = match kind {
        "schtasks" => windows_launcher_vbs(exe, &log_path(home), home),
        "wsl-schtasks" => wsl_launcher_vbs(
            &std::env::var("WSL_DISTRO_NAME")
                .ok()
                .or_else(|| probes_disabled().then(|| "Ubuntu".into()))?,
            exe,
            home,
        ),
        _ => return None,
    };
    Some(launcher_bytes(&body))
}

fn task_owned(ctx: &Ctx, reg: &ServiceRegistration, xml: &str) -> bool {
    if reg.exe.is_empty() || !exe_path_eq(&reg.config_home, &ctx.config_dir.display().to_string()) {
        return false;
    }
    let launcher = launcher_path(&ctx.config_dir);
    let Some(expected) = expected_launcher(&reg.kind, Path::new(&reg.exe), &ctx.config_dir) else {
        return false;
    };
    if std::fs::read(&launcher).ok().as_deref() != Some(expected.as_slice()) {
        return false;
    }
    let Some(host_launcher) = scheduler_path(&launcher) else {
        return false;
    };
    let Some(identity) = run_output(
        if cfg!(windows) {
            "whoami"
        } else {
            "whoami.exe"
        },
        &["/user", "/fo", "csv", "/nh"],
    ) else {
        return false;
    };
    let Some(sid) = identity
        .split(',')
        .nth(1)
        .map(|s| s.trim().trim_matches('"'))
        .filter(|s| s.starts_with("S-1-"))
    else {
        return false;
    };
    let Some(root) = run_output(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); $env:SystemRoot",
        ],
    ) else {
        return false;
    };
    task_action_owned(xml, &host_launcher, sid, root.trim())
}

/// Establish the manager's current action BEFORE starting/stopping/replacing
/// anything. A stale foreign descriptor does not authorize 'repair'.
fn require_owned_manager(ctx: &Ctx) -> anyhow::Result<()> {
    let Some(reg) = continuity::read_service_registration(&ctx.config_dir) else {
        return Err(anyhow!(
            "manager ownership registration missing; no mutation"
        ));
    };
    if reg.exe.is_empty()
        || reg.config_home.is_empty()
        || !exe_path_eq(&reg.config_home, &ctx.config_dir.display().to_string())
    {
        return Err(anyhow!(
            "manager config/binary ownership unknown; controls retained"
        ));
    }
    let owned = match reg.kind.as_str() {
        "detached" => true,
        "systemd-user" => {
            let unit = dirs_systemd_user().join(SYSTEMD_UNIT);
            let local = std::fs::read_to_string(&unit)
                .ok()
                .is_some_and(|s| s == systemd_unit_text(Path::new(&reg.exe), &ctx.config_dir));
            if probes_disabled() {
                local
            } else {
                let fragment = run_output(
                    "systemctl",
                    &[
                        "--user",
                        "show",
                        SYSTEMD_UNIT,
                        "--property=FragmentPath",
                        "--value",
                    ],
                );
                let action = run_output(
                    "systemctl",
                    &[
                        "--user",
                        "show",
                        SYSTEMD_UNIT,
                        "--property=ExecStart",
                        "--value",
                    ],
                );
                let env = run_output(
                    "systemctl",
                    &[
                        "--user",
                        "show",
                        SYSTEMD_UNIT,
                        "--property=Environment",
                        "--value",
                    ],
                );
                local
                    && fragment.is_some_and(|s| exe_path_eq(s.trim(), &unit.display().to_string()))
                    && action.is_some_and(|s| {
                        s.matches("path=").count() == 1
                            && s.contains(&format!(
                                "path={} ; argv[]={} service run ;",
                                reg.exe, reg.exe
                            ))
                    })
                    && env.is_some_and(|s| {
                        s.trim().trim_matches('"')
                            == format!("STATEROOT_HOME={}", ctx.config_dir.display())
                    })
            }
        }
        "schtasks" | "wsl-schtasks" => {
            if probes_disabled() {
                expected_launcher(&reg.kind, Path::new(&reg.exe), &ctx.config_dir).is_some_and(
                    |expected| std::fs::read(launcher_path(&ctx.config_dir)).ok() == Some(expected),
                )
            } else {
                query_task(&schtasks_name(&ctx.config_dir))?
                    .is_some_and(|xml| task_owned(ctx, &reg, &xml))
            }
        }
        // Native loaded LaunchAgent identity is not verified by this build.
        _ => false,
    };
    if owned {
        Ok(())
    } else {
        Err(anyhow!("manager descriptor/action/user/runtime ownership foreign or unknown; no mutation; controls retained"))
    }
}

/// True when the recorded registration already points at this exact binary
/// and config home AND the manager-side descriptor still exists and still
/// carries THIS binary + config home (install/rearm idempotence check — a
/// name/file that exists but describes something else forces
/// re-registration).
fn registration_current(ctx: &Ctx, exe: &Path) -> bool {
    let Some(reg) = continuity::read_service_registration(&ctx.config_dir) else {
        return false;
    };
    if reg.exe.is_empty()
        || reg.exe != exe.display().to_string()
        || reg.build_version != crate::cli::BUILD_VERSION
        || reg.exe_identity.is_empty()
        || file_identity(exe) != Some(reg.exe_identity.clone())
        || (!reg.config_home.is_empty() && reg.config_home != ctx.config_dir.display().to_string())
    {
        return false;
    }
    if require_owned_manager(ctx).is_err() {
        return false;
    }
    match reg.kind.as_str() {
        // The descriptor CONTENT must be what this binary + config home
        // would write — a unit/plist someone edited or repointed is not
        // current, no matter that the file exists.
        "systemd-user" => std::fs::read_to_string(dirs_systemd_user().join(SYSTEMD_UNIT))
            .map(|text| text == systemd_unit_text(exe, &ctx.config_dir))
            .unwrap_or(false),
        "launchd" => std::env::var_os("HOME")
            .map(|home| {
                let plist = PathBuf::from(home)
                    .join("Library/LaunchAgents")
                    .join(format!("{LAUNCHD_LABEL}.plist"));
                std::fs::read_to_string(plist)
                    .map(|text| {
                        text == launchd_plist_text(exe, &log_path(&ctx.config_dir), &ctx.config_dir)
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false),
        "schtasks" | "wsl-schtasks" => {
            let launcher = launcher_path(&ctx.config_dir);
            // The launcher content must match what THIS exe/distro/config
            // would generate — a stale launcher is a stale descriptor.
            let expected = if reg.kind == "wsl-schtasks" {
                let distro = std::env::var("WSL_DISTRO_NAME").unwrap_or_else(|_| "Ubuntu".into());
                wsl_launcher_vbs(&distro, exe, &ctx.config_dir)
            } else {
                windows_launcher_vbs(exe, &log_path(&ctx.config_dir), &ctx.config_dir)
            };
            let launcher_current = std::fs::read(&launcher)
                .map(|bytes| bytes == launcher_bytes(&expected))
                .unwrap_or(false);
            if !launcher_current {
                return false;
            }
            // Manager-side staleness: the task itself must still exist and
            // must invoke this launcher. Under test probes the manager is
            // simulated — the descriptor file is the evidence.
            probes_disabled()
                || query_task(&schtasks_name(&ctx.config_dir))
                    .ok()
                    .flatten()
                    .is_some_and(|xml| task_owned(ctx, &reg, &xml))
        }
        // Detached has no manager descriptor; the registration IS the record.
        _ => true,
    }
}

/// After registration + start, validate against the selected binary, config
/// home and manager (C5): the registration must round-trip with THIS exe,
/// and a service we started ourselves must heartbeat within a bounded wait.
/// Never lies: a missing heartbeat says so.
fn validate_registration(ctx: &Ctx, exe: &Path, started_now: bool) -> Vec<String> {
    let mut findings = Vec::new();
    match continuity::read_service_registration(&ctx.config_dir) {
        Some(reg) => {
            if !reg.exe.is_empty() && reg.exe != exe.display().to_string() {
                findings.push(format!(
                    "registration points at {} but this binary is {} — run `stateroot service install` to re-register",
                    reg.exe,
                    exe.display()
                ));
            }
            if !reg.config_home.is_empty()
                && reg.config_home != ctx.config_dir.display().to_string()
            {
                findings.push(format!(
                    "registration config home {} differs from this config home {}",
                    reg.config_home,
                    ctx.config_dir.display()
                ));
            }
        }
        None => findings
            .push("registration file missing after install — registration failed".to_string()),
    }
    if started_now && !probes_disabled() {
        // Initial heartbeat proof must be FRESH and from THIS exact binary/
        // config/runtime — a merely-alive pid or a predecessor's beat never
        // counts as healthy.
        let waited_from = chrono::Utc::now() - chrono::Duration::seconds(2);
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(STARTUP_HEARTBEAT_WAIT_MS);
        loop {
            if let Some(beat) = continuity::read_service_heartbeat(&ctx.config_dir) {
                let beat_fresh = chrono::DateTime::parse_from_rfc3339(&beat.beat_at)
                    .map(|at| at >= waited_from)
                    .unwrap_or(false);
                let ours = !beat.exe.is_empty()
                    && beat.exe == exe.display().to_string()
                    && beat_fresh
                    && verified_service_pid(beat.pid, &beat.exe, &beat, &ctx.config_dir);
                if beat.pid > 0 && ours {
                    findings.push(format!(
                        "initial heartbeat confirmed (pid {}, this binary, fresh)",
                        beat.pid
                    ));
                    break;
                }
            }
            if std::time::Instant::now() >= deadline {
                findings.push(
                    "no verified fresh heartbeat from this binary yet — coverage is degraded until the first beat; check `stateroot service status`"
                        .to_string(),
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    findings
}

/// Install + start the continuity service. Falls back to a detached process
/// when OS registration is unavailable — degraded, never absent. Re-registers
/// when the recorded exe has drifted (self-update rearm). `quiet` suppresses
/// progress lines (machine-mode callers whose stdout is a JSON document).
pub fn install(ctx: &Ctx) -> anyhow::Result<()> {
    install_quiet(ctx, false)
}

fn install_quiet(ctx: &Ctx, quiet: bool) -> anyhow::Result<()> {
    macro_rules! say {
        ($($arg:tt)*) => {
            if !quiet { println!($($arg)*); }
        };
    }
    if !ctx.config.continuity.enabled {
        say!("continuity is disabled in config ([continuity] enabled = false) — not installing");
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let prior = continuity::read_service_registration(&ctx.config_dir);
    if let Some(reg) = &prior {
        if registration_current(ctx, &exe) {
            say!(
                "continuity service already registered ({}) for this binary",
                reg.kind
            );
            // Registered-but-stopped still gets started by install.
            let (live, _) = service_live(ctx);
            if !live {
                start_quiet(ctx, quiet)?;
            }
            return Ok(());
        }
        require_owned_manager(ctx)?;
        stop_quiet(ctx, quiet)?;
        note!(
            "registration identity/build or descriptor changed ({} -> {}) — re-registering verified ownership",
            if reg.exe.is_empty() {
                "<unrecorded>"
            } else {
                reg.exe.as_str()
            },
            exe.display()
        );
        say!(
            "re-registering continuity service for {} (was {})",
            exe.display(),
            if reg.exe.is_empty() {
                "<unrecorded>"
            } else {
                reg.exe.as_str()
            }
        );
    }
    let rearmed = prior.is_some();
    #[cfg(target_os = "linux")]
    let (kind, mut detail) = {
        if systemd_user_available() {
            install_systemd(ctx, &exe)?;
            ("systemd-user", SYSTEMD_UNIT.to_string())
        } else if super::editor_extensions::is_wsl() {
            let distro = std::env::var("WSL_DISTRO_NAME")
                .ok()
                .or_else(|| probes_disabled().then(|| "Ubuntu".into()))
                .ok_or_else(|| {
                    anyhow!("actual WSL distribution identity unavailable; no task mutation")
                })?;
            let name = install_schtasks(ctx, "wsl-schtasks", &exe)?;
            ("wsl-schtasks", format!("{name} via wsl.exe -d {distro}"))
        } else {
            ("detached", String::new())
        }
    };
    #[cfg(target_os = "macos")]
    let (kind, mut detail) = {
        install_launchd(ctx, &exe)?;
        ("launchd", LAUNCHD_LABEL.to_string())
    };
    #[cfg(target_os = "windows")]
    let (kind, mut detail) = {
        let name = install_schtasks(ctx, "schtasks", &exe)?;
        ("schtasks", name)
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let (kind, mut detail) = ("detached", String::new());

    if kind == "detached" {
        detail = "OS registration unavailable — detached process (session-limited, no restart on logon/boot); doctor reports degraded coverage"
            .into();
    }
    write_registration(&ctx.config_dir, kind, &detail, &exe)?;
    say!(
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
    if rearmed {
        // Update rearm (C6): the descriptor now points at THIS binary — the
        // previously running instance (old exe) is replaced through the
        // verified stop path, then started fresh.
        start_quiet(ctx, quiet)?;
    }
    let started_now = matches!(kind, "schtasks" | "wsl-schtasks" | "detached");
    if started_now && !rearmed {
        start_quiet(ctx, quiet)?;
    }
    for finding in validate_registration(ctx, &exe, started_now || rearmed) {
        say!("  {finding}");
    }
    Ok(())
}

/// Start the service through its registered manager, or detached when the
/// registration says so / is absent.
pub fn start(ctx: &Ctx) -> anyhow::Result<()> {
    start_quiet(ctx, false)
}

fn start_quiet(ctx: &Ctx, quiet: bool) -> anyhow::Result<()> {
    if !ctx.config.continuity.enabled {
        return Err(anyhow!("continuity is disabled; no service start"));
    }
    macro_rules! say {
        ($($arg:tt)*) => {
            if !quiet { println!($($arg)*); }
        };
    }
    let (live, _) = service_live(ctx);
    if live {
        say!("continuity service already running");
        return Ok(());
    }
    let registration = continuity::read_service_registration(&ctx.config_dir);
    if registration.is_some() {
        require_owned_manager(ctx)?;
    }
    match registration.as_ref().map(|r| r.kind.as_str()) {
        Some("systemd-user") if !probes_disabled() => {
            if run_quiet("systemctl", &["--user", "start", SYSTEMD_UNIT]) {
                say!("continuity service started (systemd --user)");
                return Ok(());
            }
            return Err(anyhow!(
                "systemctl start unconfirmed; no detached duplicate spawned; controls retained"
            ));
        }
        Some("launchd") if !probes_disabled() => {
            if let Some(uid) = run_output("id", &["-u"]) {
                let domain = format!("gui/{}/{LAUNCHD_LABEL}", uid.trim());
                if run_quiet("launchctl", &["kickstart", &domain]) {
                    say!("continuity service started (launchd)");
                    return Ok(());
                }
            }
            return Err(anyhow!(
                "launchctl start unconfirmed; no detached duplicate spawned; controls retained"
            ));
        }
        Some("schtasks") | Some("wsl-schtasks") if !probes_disabled() => {
            let name = schtasks_name(&ctx.config_dir);
            if run_quiet(schtasks_program(), &["/run", "/tn", &name]) {
                say!("continuity service started ({name})");
                return Ok(());
            }
            return Err(anyhow!("scheduled task start unconfirmed; no detached duplicate spawned; controls retained"));
        }
        _ => {}
    }
    spawn_detached_service(ctx)?;
    say!(
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

/// Stop the running service (OS manager + the verified heartbeat pid).
/// Unverified ownership or unconfirmed stop retains heartbeat/registration
/// and fails closed; restart/remove cannot silently duplicate or erase it.
pub fn stop(ctx: &Ctx) -> anyhow::Result<()> {
    stop_quiet(ctx, false)
}

fn stop_quiet(ctx: &Ctx, quiet: bool) -> anyhow::Result<()> {
    let registration = continuity::read_service_registration(&ctx.config_dir);
    if registration.is_some() {
        require_owned_manager(ctx)?;
    }
    match registration.as_ref().map(|r| r.kind.as_str()) {
        Some("systemd-user") if !probes_disabled() => {
            if !run_quiet("systemctl", &["--user", "stop", SYSTEMD_UNIT]) {
                return Err(anyhow!("manager stop unconfirmed; controls retained"));
            }
        }
        Some("launchd") if !probes_disabled() => {
            if let Some(uid) = run_output("id", &["-u"]) {
                let domain = format!("gui/{}/{LAUNCHD_LABEL}", uid.trim());
                if !run_quiet("launchctl", &["kill", "SIGTERM", &domain]) {
                    return Err(anyhow!("manager stop unconfirmed; controls retained"));
                }
            }
        }
        Some("schtasks") | Some("wsl-schtasks") if !probes_disabled() => {
            let name = schtasks_name(&ctx.config_dir);
            if !run_quiet(schtasks_program(), &["/end", "/tn", &name]) {
                return Err(anyhow!("manager stop unconfirmed; controls retained"));
            }
        }
        _ => {}
    }
    // The heartbeat pid covers the detached kind and any stale manager run.
    let heartbeat = continuity::read_service_heartbeat(&ctx.config_dir);
    let mut verified_alive: Option<u32> = None;
    if let Some(beat) = &heartbeat {
        let exe_ref = if beat.exe.is_empty() {
            registration.as_ref().map(|r| r.exe.as_str()).unwrap_or("")
        } else {
            beat.exe.as_str()
        };
        if beat.pid > 0 && beat.pid != std::process::id() && safe_io::pid_alive(beat.pid) {
            if let Some(process) = VerifiedProcess::open(beat.pid, exe_ref, beat, &ctx.config_dir) {
                if !process.signal() && !process.gone() {
                    return Err(anyhow!(
                        "verified continuity process could not be signalled; controls retained"
                    ));
                }
                // Bounded shutdown: confirm exit before reporting stopped.
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_millis(SHUTDOWN_WAIT_MS);
                while !process.gone() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                if !process.gone() {
                    verified_alive = Some(beat.pid);
                }
            } else {
                // Keep evidence/control on an unknown stop. The readout
                // already labels it unverified rather than falsely live.
                return Err(anyhow!("heartbeat pid {} ownership unverified; not signalled, controls retained, stop unknown",beat.pid));
            }
        }
    }
    // A failed/stuck shutdown is NOT a success: records stay (ownership and
    // control remain resolved), the heartbeat keeps pointing at the live
    // pid, and callers (restart/remove/rearm) must not proceed to spawn or
    // remove anything.
    if let Some(pid) = verified_alive {
        return Err(anyhow!(
            "continuity service pid {pid} was signalled but did not exit within {SHUTDOWN_WAIT_MS}ms — NOT stopped; registration and heartbeat preserved (inspect with `stateroot service status`)"
        ));
    }
    // The heartbeat file stays only while a verified-live service remains.
    let _ = std::fs::remove_file(continuity::service_heartbeat_path(&ctx.config_dir));
    if !quiet {
        println!("continuity service stopped");
    }
    Ok(())
}

/// Restart the service.
pub fn restart(ctx: &Ctx) -> anyhow::Result<()> {
    stop(ctx)?;
    start(ctx)
}

/// Unregister + stop. The registration, heartbeat and launcher files go
/// with it; the legacy global task name is repaired away too (ours only).
pub fn remove(ctx: &Ctx) -> anyhow::Result<()> {
    if continuity::read_service_registration(&ctx.config_dir).is_some() {
        require_owned_manager(ctx)?;
    }
    if !probes_disabled() && (cfg!(windows) || super::editor_extensions::is_wsl()) {
        if let Some(xml) = query_task(LEGACY_SCHTASKS_NAME)? {
            let reg = continuity::read_service_registration(&ctx.config_dir)
                .ok_or_else(|| anyhow!("legacy task ownership unavailable; no removal"))?;
            if !task_owned(ctx, &reg, &xml) {
                return Err(anyhow!("legacy task foreign/unverifiable; no removal"));
            }
        }
    }
    stop(ctx)?;
    let registration = continuity::read_service_registration(&ctx.config_dir);
    match registration.as_ref().map(|r| r.kind.as_str()) {
        Some("systemd-user") if !probes_disabled() => {
            if !run_quiet("systemctl", &["--user", "disable", "--now", SYSTEMD_UNIT]) {
                return Err(anyhow!("manager removal unconfirmed; controls retained"));
            }
            let _ = std::fs::remove_file(dirs_systemd_user().join(SYSTEMD_UNIT));
        }
        Some("launchd") if !probes_disabled() => {
            if let Some(uid) = run_output("id", &["-u"]) {
                let domain = format!("gui/{}/{LAUNCHD_LABEL}", uid.trim());
                if !run_quiet("launchctl", &["bootout", &domain]) {
                    return Err(anyhow!("manager removal unconfirmed; controls retained"));
                }
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
            let program = schtasks_program();
            let args = schtasks_delete_args(&schtasks_name(&ctx.config_dir));
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            if !run_quiet(program, &refs) {
                return Err(anyhow!("manager removal unconfirmed; controls retained"));
            }
        }
        _ => {}
    }
    if !probes_disabled() && (cfg!(windows) || super::editor_extensions::is_wsl()) {
        // Legacy repair: the pre-WS3 global task is removed only when its
        // descriptor verifiably references a stateroot launcher; an
        // unrecognized task by that name is left untouched.
        if let Some(xml) = query_task(LEGACY_SCHTASKS_NAME)? {
            if registration
                .as_ref()
                .is_some_and(|reg| task_owned(ctx, reg, &xml))
            {
                let program = schtasks_program();
                let args = schtasks_delete_args(LEGACY_SCHTASKS_NAME);
                let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                if !run_quiet(program, &refs) {
                    return Err(anyhow!(
                        "legacy task removal unconfirmed; controls retained"
                    ));
                }
            } else {
                return Err(anyhow!(
                    "legacy task foreign/unverifiable; no mutation; controls retained"
                ));
            }
        }
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
            "exe": registration.as_ref().map(|r| r.exe.clone()).filter(|s| !s.is_empty()),
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
/// install when enabled and (not yet registered OR the registered exe has
/// drifted — update rearm must repoint the descriptor at the new binary);
/// never fails the caller. The owner's disabled setting is untouched.
pub fn ensure_installed(ctx: &Ctx, quiet: bool) {
    if !ctx.config.continuity.enabled {
        return;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(_) => return,
    };
    if registration_current(ctx, &exe) {
        return;
    }
    if continuity::read_service_registration(&ctx.config_dir).is_some() {
        // Registered but pointing at another binary: re-register (rearm).
        if let Err(err) = install_quiet(ctx, quiet) {
            note!("continuity service re-registration: {err}");
        }
        return;
    }
    if let Err(err) = install_quiet(ctx, quiet) {
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
    fn task_ownership_rejects_names_prose_extra_actions_and_other_user() {
        let launcher = r"C:\Users\Owner\工具 & Space\continuity-service.launcher.vbs";
        let sid = "S-1-5-21-123";
        let xml=format!("<Task><UserId>{sid}</UserId><Exec><Command>wscript.exe</Command><Arguments>{}</Arguments></Exec></Task>",xml_escape(&format!("//B //nologo {}",cmd_quote(launcher))));
        assert!(task_action_owned(&xml, launcher, sid, r"C:\Windows"));
        assert!(!task_action_owned(
            &xml.replace("wscript.exe", "foreign.exe"),
            launcher,
            sid,
            r"C:\Windows"
        ));
        assert!(!task_action_owned(
            &xml,
            launcher,
            "S-1-5-21-999",
            r"C:\Windows"
        ));
        assert!(!task_action_owned(
            &format!("{xml}<Exec><Command>foreign.exe</Command></Exec>"),
            launcher,
            sid,
            r"C:\Windows"
        ));
        assert!(!task_action_owned(
            "<Description>stateroot continuity-service.launcher</Description>",
            launcher,
            sid,
            r"C:\Windows"
        ));
    }

    #[test]
    fn launcher_has_utf16_bom_and_exact_unicode_roundtrip() {
        let body = windows_launcher_vbs(
            Path::new("C:\\工具 & Space\\app.exe"),
            Path::new("C:\\工具 & Space\\log"),
            Path::new("C:\\工具 & Space\\config"),
        );
        let bytes = launcher_bytes(&body);
        assert_eq!(&bytes[..2], &[255, 254]);
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), body);
        assert!(body.contains("shell.Environment(\"PROCESS\")"));
        assert!(!body.contains("set \""));
    }

    #[cfg(windows)]
    #[test]
    fn real_cscript_unicode_process_environment_fixture() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("工具 & Space");
        std::fs::create_dir(&home).unwrap();
        let helper = home.join("fixture.cmd");
        std::fs::write(&helper,"@echo off\r\npowershell.exe -NoProfile -NonInteractive -Command \"[IO.File]::WriteAllText([IO.Path]::Combine($env:STATEROOT_HOME,'marker'),'ok')\"\r\n").unwrap();
        let launcher = home.join("fixture.vbs");
        std::fs::write(
            &launcher,
            launcher_bytes(&windows_launcher_vbs(
                &helper,
                &home.join("fixture.log"),
                &home,
            )),
        )
        .unwrap();
        assert!(bounded_status(
            "cscript.exe",
            &["//B", "//nologo", &launcher.display().to_string()],
            std::time::Duration::from_secs(5)
        ));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !home.join("marker").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert_eq!(std::fs::read_to_string(home.join("marker")).unwrap(), "ok");
    }

    #[cfg(target_os = "linux")]
    fn owned_pause_fixture(dir: &Path) -> PathBuf {
        let source = dir.join("fixture.c");
        let binary = dir.join("fixture");
        std::fs::write(&source,"#include <signal.h>\n#include <unistd.h>\n#include <stdio.h>\n#include <stdlib.h>\nint main(void){signal(SIGTERM,SIG_IGN);char*p=getenv(\"FIXTURE_READY\");if(p){FILE*f=fopen(p,\"w\");if(f){fputs(\"ready\",f);fclose(f);}}for(;;) pause();}\n").unwrap();
        assert!(std::process::Command::new("cc")
            .args([
                source.as_os_str(),
                std::ffi::OsStr::new("-o"),
                binary.as_os_str()
            ])
            .status()
            .unwrap()
            .success());
        binary
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn exact_spoofed_argv_is_not_actual_image_and_stuck_stop_retains_controls() {
        use std::os::unix::process::CommandExt as _;
        let dir = tempfile::tempdir().unwrap();
        let binary = owned_pause_fixture(dir.path());
        let ours = std::env::current_exe().unwrap();
        let mut child = std::process::Command::new(&binary)
            .arg0(&ours)
            .args(["service", "run"])
            .env("STATEROOT_HOME", dir.path())
            .spawn()
            .unwrap();
        let mut beat = own_beat();
        beat.pid = child.id();
        beat.pid_start = pid_start_token(child.id()).unwrap();
        beat.config_home = dir.path().display().to_string();
        assert!(
            !verified_service_pid(child.id(), &beat.exe, &beat, dir.path()),
            "exact forged argv must still reject different image"
        );
        child.kill().unwrap();
        child.wait().unwrap();
        let mut child = std::process::Command::new(&binary)
            .args(["service", "run"])
            .env("STATEROOT_HOME", dir.path())
            .env("FIXTURE_READY", dir.path().join("ready"))
            .spawn()
            .unwrap();
        // Exact acknowledgement, not a guessed scheduler delay.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !dir.path().join("ready").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(dir.path().join("ready").exists());
        beat.pid = child.id();
        beat.pid_start = pid_start_token(child.id()).unwrap();
        beat.exe = binary.display().to_string();
        beat.exe_identity = file_identity(&binary).unwrap();
        assert!(verified_service_pid(
            child.id(),
            &beat.exe,
            &beat,
            dir.path()
        ));
        let mut wrong = beat.clone();
        wrong.config_home = dir.path().join("other").display().to_string();
        assert!(!verified_service_pid(
            child.id(),
            &wrong.exe,
            &wrong,
            dir.path()
        ));
        let mut reused = beat.clone();
        reused.pid_start += 1;
        assert!(!verified_service_pid(
            child.id(),
            &reused.exe,
            &reused,
            dir.path()
        ));
        write_registration(dir.path(), "detached", "owned test fixture", &binary).unwrap();
        let beat_path = continuity::service_heartbeat_path(dir.path());
        std::fs::write(&beat_path, serde_json::to_vec(&beat).unwrap()).unwrap();
        let reg_path = continuity::service_registration_path(dir.path());
        let original_reg = std::fs::read(&reg_path).unwrap();
        let original_beat = std::fs::read(&beat_path).unwrap();
        let ctx = Ctx {
            cwd: dir.path().into(),
            config_dir: dir.path().into(),
            config: Default::default(),
        };
        assert!(stop_quiet(&ctx, true).is_err());
        assert_eq!(std::fs::read(&reg_path).unwrap(), original_reg);
        assert_eq!(std::fs::read(&beat_path).unwrap(), original_beat);
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn systemd_unit_runs_service_run() {
        let text = systemd_unit_text(Path::new("/usr/local/bin/stateroot"), Path::new("/cfg"));
        assert!(text.contains("ExecStart=\"/usr/local/bin/stateroot\" service run"));
        assert!(text.contains("WantedBy=default.target"));
        assert!(text.contains("Restart=on-failure"));
        // The descriptor pins the config home it was registered for.
        assert!(
            text.contains("Environment=\"STATEROOT_HOME=/cfg\""),
            "{text}"
        );
    }

    #[test]
    fn systemd_unit_escapes_percent_quotes_and_spaces() {
        // systemd specifier expansion eats %; quotes/backslashes break the
        // ExecStart parse. Unicode passes through unchanged.
        let text = systemd_unit_text(
            Path::new("/opt/Stateroot 100%/bína ries/stateroot"),
            Path::new("/cfg dir"),
        );
        assert!(
            text.contains("ExecStart=\"/opt/Stateroot 100%%/bína ries/stateroot\" service run"),
            "{text}"
        );
        assert!(
            text.contains("Environment=\"STATEROOT_HOME=/cfg dir\""),
            "{text}"
        );
        let text = systemd_unit_text(Path::new("/opt/we\"ird/stateroot"), Path::new("/cfg"));
        assert!(text.contains("\\\""), "{text}");
    }

    #[test]
    fn launchd_plist_keeps_alive_and_logs() {
        let text = launchd_plist_text(
            Path::new("/opt/stateroot"),
            Path::new("/Users/x/.config/stateroot/continuity-service.log"),
            Path::new("/Users/x/.config/stateroot"),
        );
        assert!(text.contains("<string>dev.stateroot.continuity</string>"));
        assert!(text.contains("<string>/opt/stateroot</string>"));
        assert!(text.contains("<key>KeepAlive</key>"));
        assert!(text.contains("<key>RunAtLoad</key>"));
        assert!(text.contains("continuity-service.log"));
        // The descriptor pins the config home via EnvironmentVariables.
        assert!(text.contains("<key>STATEROOT_HOME</key>"), "{text}");
        assert!(
            text.contains("<string>/Users/x/.config/stateroot</string>"),
            "{text}"
        );
    }

    #[test]
    fn launchd_plist_escapes_xml_in_paths() {
        // Ampersands/angle brackets/quotes in user dirs must not corrupt the
        // plist (C4).
        let text = launchd_plist_text(
            Path::new("/Users/A & B <Co>/stateroot"),
            Path::new("/Users/A & B <Co>/.config/stateroot/continuity-service.log"),
            Path::new("/Users/A & B <Co>/.config/stateroot"),
        );
        assert!(
            text.contains("/Users/A &amp; B &lt;Co&gt;/stateroot"),
            "{text}"
        );
        assert!(!text.contains("A & B"), "{text}");
        let parsed = plist_ok(&text);
        assert!(parsed, "plist stays well-formed XML: {text}");
    }

    /// Minimal well-formedness probe: no unescaped bare `&` or `<` in values.
    fn plist_ok(text: &str) -> bool {
        let mut rest = text;
        while let Some(idx) = rest.find('&') {
            let tail = &rest[idx..];
            if !(tail.starts_with("&amp;")
                || tail.starts_with("&lt;")
                || tail.starts_with("&gt;")
                || tail.starts_with("&quot;"))
            {
                return false;
            }
            rest = &tail[1..];
        }
        true
    }

    #[test]
    fn schtasks_name_is_scoped_and_sanitized() {
        let a = schtasks_name_for(Path::new("/home/u/.config/stateroot"), None);
        let b = schtasks_name_for(Path::new("/home/u/.config/stateroot-test"), None);
        assert!(a.starts_with("StateRoot Continuity ("), "{a}");
        assert!(a.ends_with(')'), "{a}");
        assert_ne!(a, b, "config home scopes the task name");
        assert_ne!(a, LEGACY_SCHTASKS_NAME);
        for c in a.chars() {
            assert!(!r#"\/:*?"<>|"#.contains(c), "{c} invalid in {a}");
        }
    }

    #[test]
    fn schtasks_name_scopes_per_wsl_distro() {
        // Same path, same user, two distros sharing one Windows host: the
        // tasks must never collide.
        let ubuntu = schtasks_name_for(Path::new("/home/u/.config/stateroot"), Some("Ubuntu"));
        let debian = schtasks_name_for(Path::new("/home/u/.config/stateroot"), Some("Debian"));
        let native = schtasks_name_for(Path::new("/home/u/.config/stateroot"), None);
        assert_ne!(ubuntu, debian, "distro scopes the task name");
        assert_ne!(ubuntu, native, "native and WSL tasks never collide");
    }

    #[test]
    fn schtasks_args_register_hidden_launcher() {
        let name = schtasks_name_for(Path::new("/cfg"), None);
        let launcher =
            Path::new("C:\\Users\\u\\AppData\\Roaming\\stateroot\\continuity-service.launcher.vbs");
        let args = schtasks_create_args(&name, launcher);
        assert!(args.contains(&"onlogon".to_string()));
        assert!(args.contains(&name));
        let tr = args.last().expect("tr");
        assert!(tr.starts_with("wscript.exe //B //nologo"), "{tr}");
        assert!(tr.contains("continuity-service.launcher.vbs"), "{tr}");
        // Quoted launcher path survives spaces.
        assert!(
            tr.contains(
                "\"C:\\Users\\u\\AppData\\Roaming\\stateroot\\continuity-service.launcher.vbs\""
            ),
            "{tr}"
        );
    }

    #[test]
    fn windows_launcher_is_hidden_and_logs() {
        let vbs = windows_launcher_vbs(
            Path::new("C:\\Program Files\\StateRoot\\stateroot.exe"),
            Path::new("C:\\Users\\u\\AppData\\Roaming\\stateroot\\continuity-service.log"),
            Path::new("C:\\Users\\u\\AppData\\Roaming\\stateroot"),
        );
        assert!(vbs.contains("shell.Run"), "{vbs}");
        assert!(vbs.contains(", 0, False"), "hidden, non-blocking: {vbs}");
        assert!(vbs.contains("service run"), "{vbs}");
        assert!(vbs.contains("continuity-service.log"), "{vbs}");
        assert!(vbs.contains("2>&1"), "stderr rides the log: {vbs}");
        // The generated command line quotes the exe and the log.
        assert!(
            vbs.contains("processEnv(\"STATEROOT_SERVICE_EXE\") = \"C:\\Program Files\\StateRoot\\stateroot.exe\""),
            "{vbs}"
        );
        // …and pins the config home the registration was written for.
        assert!(
            vbs.contains(
                "processEnv(\"STATEROOT_HOME\") = \"C:\\Users\\u\\AppData\\Roaming\\stateroot\""
            ),
            "{vbs}"
        );
    }

    #[test]
    fn wsl_launcher_quotes_distro_and_exe() {
        let vbs = wsl_launcher_vbs(
            "Ubuntu 24.04",
            Path::new("/home/u/bin/stateroot"),
            Path::new("/home/u/.config/stateroot"),
        );
        assert!(
            vbs.contains(
                "wsl.exe -d \"\"Ubuntu 24.04\"\" -e env \"\"STATEROOT_HOME=/home/u/.config/stateroot\"\" \"\"/home/u/bin/stateroot\"\" service run"
            ),
            "{vbs}"
        );
        assert!(vbs.contains(", 0, False"), "hidden, non-blocking: {vbs}");
        // Explicit Linux env argument leaves every inherited WSLENV entry intact.
        assert!(!vbs.contains("WSLENV"), "{vbs}");
    }

    #[test]
    fn schtasks_delete_targets_scoped_and_legacy_names() {
        let scoped = schtasks_delete_args("StateRoot Continuity (u-deadbeef)");
        assert!(scoped.contains(&"StateRoot Continuity (u-deadbeef)".to_string()));
        let legacy = schtasks_delete_args(LEGACY_SCHTASKS_NAME);
        assert!(legacy.contains(&LEGACY_SCHTASKS_NAME.to_string()));
        assert!(legacy.contains(&"/delete".to_string()));
    }

    #[test]
    fn cmd_quote_handles_spaces_ampersands_unicode() {
        assert_eq!(cmd_quote("C:\\plain\\x.exe"), "\"C:\\plain\\x.exe\"");
        assert_eq!(
            cmd_quote("C:\\Program Files\\S & T\\stateroot.exe"),
            "\"C:\\Program Files\\S & T\\stateroot.exe\""
        );
        assert_eq!(
            cmd_quote("D:\\工具\\stateroot.exe"),
            "\"D:\\工具\\stateroot.exe\""
        );
        // Trailing backslash would escape the closing quote.
        assert_eq!(cmd_quote("C:\\dir\\"), "\"C:\\dir\\\\\"");
    }

    /// A heartbeat fixture with the full identity chain of the current
    /// process (used as the baseline the spoof cases degrade one field at a
    /// time).
    fn own_beat() -> ServiceHeartbeat {
        ServiceHeartbeat {
            schema_version: HEARTBEAT_SCHEMA.into(),
            pid: std::process::id(),
            beat_at: now_rfc3339(),
            version: "test".into(),
            exe: std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            namespace: safe_io::host_namespace().unwrap_or("").to_string(),
            pid_start: pid_start_token(std::process::id()).unwrap_or(0),
            config_home: std::env::temp_dir().display().to_string(),
            exe_identity: running_image_identity(),
            projects_scanned: 0,
        }
    }

    #[test]
    fn verified_pid_rejects_wrong_shapes() {
        let beat = own_beat();
        let exe = beat.exe.clone();
        let config = std::env::temp_dir();
        // Pid 0, self, dead pids, and legacy/empty identity never verify.
        assert!(!verified_service_pid(0, &exe, &beat, &config));
        assert!(!verified_service_pid(
            std::process::id(),
            &exe,
            &beat,
            &config
        ));
        assert!(!verified_service_pid(u32::MAX - 1, &exe, &beat, &config));
        assert!(!verified_service_pid(beat.pid, "", &beat, &config));
        let mut legacy = beat.clone();
        legacy.namespace = String::new();
        assert!(!verified_service_pid(beat.pid, &exe, &legacy, &config));
        let mut legacy = beat.clone();
        legacy.pid_start = 0;
        assert!(!verified_service_pid(beat.pid, &exe, &legacy, &config));
        let mut wrong_ns = beat.clone();
        wrong_ns.namespace = if safe_io::host_namespace() == Some("windows") {
            "unix".into()
        } else {
            "windows".into()
        };
        assert!(!verified_service_pid(beat.pid, &exe, &wrong_ns, &config));
    }

    /// A live foreign process is never signalled: not argv shape, not exe
    /// identity, not process-start identity.
    #[cfg(unix)]
    #[test]
    fn verified_pid_rejects_spoofed_and_foreign_processes() {
        use std::process::{Command, Stdio};
        let beat = own_beat();
        let config = std::env::temp_dir();

        // Foreign process: plain `sleep`, heartbeat CLAIMS our exe.
        let mut foreign = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleep");
        let mut claim = beat.clone();
        claim.pid = foreign.id();
        claim.pid_start = pid_start_token(foreign.id()).unwrap_or(0);
        assert!(
            !verified_service_pid(foreign.id(), &beat.exe, &claim, &config),
            "a foreign process is never ours"
        );

        // Spoofed argv[0]: the forged process has no `service run` args.
        let mut spoof = Command::new("bash")
            .arg("-c")
            .arg(format!("exec -a '{}' sleep 30", beat.exe))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spoof");
        claim.pid = spoof.id();
        claim.pid_start = pid_start_token(spoof.id()).unwrap_or(0);
        assert!(
            !verified_service_pid(spoof.id(), &beat.exe, &claim, &config),
            "a forged argv[0] without the exact args is never ours"
        );

        // Reused-pid shape: correct exe/args shape recorded, but the
        // process-start token belongs to a DIFFERENT (earlier) instance.
        claim.pid = foreign.id();
        claim.pid_start = claim.pid_start.wrapping_add(1);
        assert!(
            !verified_service_pid(foreign.id(), &beat.exe, &claim, &config),
            "a stale start token (pid reuse) is never ours"
        );

        spoof.kill().ok();
        foreign.kill().ok();
        let _ = spoof.wait();
        let _ = foreign.wait();
    }

    /// A live process running under a DIFFERENT config home is never ours,
    /// even with the exact binary/args shape — the config pin is part of
    /// the identity.
    #[cfg(target_os = "linux")]
    #[test]
    fn verified_pid_rejects_a_wrong_config_home() {
        use std::process::{Command, Stdio};
        let fixture = tempfile::tempdir().unwrap();
        let binary = owned_pause_fixture(fixture.path());
        let other = fixture.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let ready = fixture.path().join("ready");
        let mut foreign_home = Command::new(&binary)
            .args(["service", "run"])
            .env("STATEROOT_HOME", &other)
            .env("FIXTURE_READY", &ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            ready.exists(),
            "fixture acknowledged actual exec/environment"
        );
        let our_home = fixture.path().join("ours");
        assert!(
            !pid_config_matches(foreign_home.id(), &our_home),
            "a service pinned to another config home is not ours"
        );
        assert!(pid_config_matches(foreign_home.id(), &other));
        foreign_home.kill().ok();
        let _ = foreign_home.wait();
    }

    /// `service_live_for`: a stale beat is not running; a fresh beat whose
    /// identity cannot be verified (legacy fields, foreign pid) is not
    /// running either — degraded, never claimed live.
    #[test]
    fn service_live_requires_fresh_verified_identity() {
        let config = tempfile::tempdir().expect("config");
        let beat = own_beat();
        let path = continuity::service_heartbeat_path(config.path());
        let write_beat = |beat: &ServiceHeartbeat| {
            std::fs::write(&path, serde_json::to_string(beat).expect("json")).expect("write");
        };

        // Stale beat: not running.
        let mut stale = beat.clone();
        stale.beat_at = "2020-01-01T00:00:00Z".into();
        write_beat(&stale);
        let (live, _) = service_live_for(config.path(), 30);
        assert!(!live, "a stale beat is not a live service");

        // Fresh but legacy identity (no namespace/start token): not running.
        let mut legacy = beat.clone();
        legacy.namespace = String::new();
        legacy.pid_start = 0;
        write_beat(&legacy);
        let (live, _) = service_live_for(config.path(), 30);
        assert!(!live, "a legacy/unverifiable beat is not a live service");

        // Fresh beat pointing at a dead pid: not running.
        let mut dead = beat.clone();
        dead.pid = u32::MAX - 1;
        write_beat(&dead);
        let (live, _) = service_live_for(config.path(), 30);
        assert!(!live, "a dead pid is not a live service");
    }
}
