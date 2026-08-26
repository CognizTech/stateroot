//! Shared harness-CLI spawn-and-capture helper.
//!
//! Unlike `harness run` (which inherits stdio for an interactive session),
//! this launches a registry CLI with piped stdout/stderr and a `try_wait`
//! poll timeout, and returns everything the child produced. Init seeding
//! synthesis and `stateroot delegate` both build on it; pty-marked rows may
//! misbehave when piped — callers note and fall through honestly.

use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::Result;
use stateroot_core::skill_federation::{build_launch_argv_from_spec, DelegationSpec};

/// Launch policy layered onto a registry delegation spec.
#[derive(Debug, Default)]
pub struct LaunchPolicy {
    /// Explicitly selected skill paths added back per the registry policy.
    pub skill_paths: Vec<String>,
    /// Opt into the harness's own ambient skill discovery.
    pub ambient_skills: bool,
    /// Extra environment for the child (e.g. the delegation depth marker).
    pub env: Vec<(String, String)>,
}

/// Everything a piped harness-CLI run produced.
#[derive(Debug)]
pub struct HarnessOutput {
    /// Captured stdout (lossy UTF-8, trimmed).
    pub stdout: String,
    /// Captured stderr (lossy UTF-8, trimmed).
    pub stderr: String,
    /// Final exit status (`code()` is `None` when killed by a signal).
    pub status: ExitStatus,
    /// The run hit the timeout; the child was killed.
    pub timed_out: bool,
}

/// Launch harness `id` from its registry delegation `spec`, capturing piped
/// stdout/stderr. `Some(timeout)` kills the child past the deadline and
/// returns `timed_out`; `None` means no cap at all — the child runs to its
/// natural end (the async delegate's contract; the harness's own limits
/// belong to the harness). The timeout fact is returned, never an error, so
/// callers can record the outcome honestly.
pub fn run_capture(
    dir: &Path,
    id: &str,
    spec: &DelegationSpec,
    prompt: &str,
    policy: &LaunchPolicy,
    timeout: Option<Duration>,
) -> Result<HarnessOutput> {
    let argv = build_launch_argv_from_spec(
        spec,
        Some(prompt),
        &policy.skill_paths,
        policy.ambient_skills,
    )
    .ok_or_else(|| anyhow::anyhow!("harness `{id}` has no launch command"))?;
    let (command, args) = argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("harness `{id}` produced an empty launch command"))?;
    let started = Instant::now();
    let mut child = Command::new(command)
        .args(args)
        .current_dir(dir)
        .envs(policy.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = timeout.map(|cap| started + cap);
    let timed_out = loop {
        if child.try_wait()?.is_some() {
            break false;
        }
        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                let _ = child.kill();
                break true;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let output = child.wait_with_output()?;
    Ok(HarnessOutput {
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        status: output.status,
        timed_out,
    })
}
