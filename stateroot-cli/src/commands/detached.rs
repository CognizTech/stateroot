//! Detached process spawn hygiene shared by delegate, auto-update, and
//! `_drain-finalize`.
//!
//! Unix: new session via `setsid` (child after fork, before exec).
//! Windows: new process group + breakaway from job.
//! Stdin is always null. Stdout/stderr go to a capped log (256KB rotation,
//! matching the hook observation spool). Argv is flags-and-paths only —
//! credentials stay in the environment.

use std::path::Path;
use std::process::{Command, Stdio};

/// Rotation threshold matching hook spool (`hook.rs` SPOOL_ROTATE_BYTES).
pub const DETACHED_LOG_ROTATE_BYTES: u64 = 256 * 1024;
/// Bytes kept after rotation.
pub const DETACHED_LOG_KEEP_BYTES: usize = 128 * 1024;

/// What a detached spawn will do — inspectable by tests without forking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachPlan {
    /// Subcommand argv (never env secrets).
    pub args: Vec<String>,
    /// Unix: `setsid` in the child.
    pub setsid: bool,
    /// Windows: `CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB`.
    pub breakaway: bool,
    /// Stdin is `/dev/null`.
    pub stdin_null: bool,
}

/// Plan for the session-end finalize drainer.
pub fn drain_finalize_plan() -> DetachPlan {
    DetachPlan {
        args: vec!["_drain-finalize".into()],
        setsid: cfg!(unix),
        breakaway: cfg!(windows),
        stdin_null: true,
    }
}

/// True when `args` look like they smuggle a credential (env-only is the rule).
pub fn argv_looks_like_secret(args: &[String]) -> bool {
    const NEEDLES: &[&str] = &[
        "TOKEN", "SECRET", "PASSWORD", "API_KEY", "api_key", "Bearer ", "ghp_", "sk-",
    ];
    args.iter().any(|a| NEEDLES.iter().any(|n| a.contains(n)))
}

/// Rotate `path` the way the hook spool does when it exceeds the cap.
pub fn rotate_log_if_needed(path: &Path) -> std::io::Result<()> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    if meta.len() <= DETACHED_LOG_ROTATE_BYTES {
        return Ok(());
    }
    let bytes = std::fs::read(path).unwrap_or_default();
    let start = bytes.len().saturating_sub(DETACHED_LOG_KEEP_BYTES);
    let start = (start..=bytes.len())
        .find(|&i| i == bytes.len() || bytes[i] == b'\n')
        .map(|i| (i + 1).min(bytes.len()))
        .unwrap_or(bytes.len());
    std::fs::write(path, &bytes[start.min(bytes.len())..])?;
    Ok(())
}

/// Open a log for redirected stdout/stderr: create + O_APPEND, after rotation.
pub fn open_log_append(path: &Path) -> std::io::Result<(std::fs::File, std::fs::File)> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    rotate_log_if_needed(path)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let cloned = file.try_clone()?;
    Ok((file, cloned))
}

/// Apply session-detach flags to `cmd`.
pub fn apply_detach_flags(cmd: &mut Command, plan: &DetachPlan) {
    if plan.stdin_null {
        cmd.stdin(Stdio::null());
    }
    #[cfg(unix)]
    if plan.setsid {
        use std::os::unix::process::CommandExt;
        unsafe {
            // SAFETY: runs in the child after fork and before exec; setsid
            // only affects this process.
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    #[cfg(windows)]
    if plan.breakaway {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x01000000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
    }
}

/// Spawn `program` with `plan.args`, cwd, and log redirect. Returns pid.
pub fn spawn_detached(
    program: &Path,
    plan: &DetachPlan,
    cwd: &Path,
    log_path: &Path,
) -> std::io::Result<u32> {
    let (log_out, log_err) = open_log_append(log_path)?;
    let mut cmd = Command::new(program);
    cmd.args(&plan.args)
        .current_dir(cwd)
        .stdout(log_out)
        .stderr(log_err);
    apply_detach_flags(&mut cmd, plan);
    let child = cmd.spawn()?;
    Ok(child.id())
}

/// Convenience: spawn this binary with extra args, using `plan` flags.
pub fn spawn_self(extra_args: &[String], cwd: &Path, log_path: &Path) -> std::io::Result<u32> {
    let mut plan = DetachPlan {
        args: extra_args.to_vec(),
        setsid: cfg!(unix),
        breakaway: cfg!(windows),
        stdin_null: true,
    };
    // Keep drain-finalize argv identical to the inspectable plan.
    if extra_args.first().map(String::as_str) == Some("_drain-finalize") {
        plan = drain_finalize_plan();
        plan.args = extra_args.to_vec();
    }
    let exe = std::env::current_exe()?;
    spawn_detached(&exe, &plan, cwd, log_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_plan_carries_setsid_or_breakaway_and_null_stdin() {
        let plan = drain_finalize_plan();
        assert_eq!(plan.args, ["_drain-finalize"]);
        assert!(plan.stdin_null);
        assert!(!argv_looks_like_secret(&plan.args));
        #[cfg(unix)]
        assert!(plan.setsid);
        #[cfg(windows)]
        assert!(plan.breakaway);
    }

    #[test]
    fn argv_secret_needles_catch_tokens_not_benign_flags() {
        assert!(argv_looks_like_secret(&[
            "--token".into(),
            "ghp_abc".into()
        ]));
        assert!(!argv_looks_like_secret(&[
            "_drain-finalize".into(),
            "self-update".into(),
            "--_worker".into(),
        ]));
    }

    #[test]
    fn log_rotation_keeps_the_tail() {
        let dir = tempfile::tempdir().expect("dir");
        let log = dir.path().join("d.log");
        let mut body = "head\n".to_string();
        body.push_str(&"x".repeat(DETACHED_LOG_ROTATE_BYTES as usize));
        body.push_str("\ntail-marker\n");
        std::fs::write(&log, &body).expect("write");
        rotate_log_if_needed(&log).expect("rotate");
        let kept = std::fs::read_to_string(&log).expect("read");
        assert!(
            kept.len() as u64 <= DETACHED_LOG_ROTATE_BYTES,
            "len {}",
            kept.len()
        );
        assert!(kept.contains("tail-marker"), "{kept}");
        assert!(!kept.contains("head\n"));
    }
}
