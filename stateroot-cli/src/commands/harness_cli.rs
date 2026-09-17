//! Shared harness-CLI spawn-and-capture helper.
//!
//! Unlike `harness run` (which inherits stdio for an interactive session),
//! this launches a registry CLI with piped stdout/stderr and a `try_wait`
//! poll timeout, and returns everything the child produced. Init seeding
//! synthesis and `stateroot delegate` both build on it; pty-marked rows may
//! misbehave when piped — callers note and fall through honestly.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
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
    run_capture_inner(dir, id, spec, prompt, policy, timeout, false)
}

/// As [`run_capture`], but tee each harness stream to this process while it
/// runs. Delegation workers have stdout/stderr redirected to their durable
/// log, so this makes live agent work observable without giving up the final
/// captured result used for outcome records.
pub fn run_capture_streaming(
    dir: &Path,
    id: &str,
    spec: &DelegationSpec,
    prompt: &str,
    policy: &LaunchPolicy,
    timeout: Option<Duration>,
) -> Result<HarnessOutput> {
    run_capture_inner(dir, id, spec, prompt, policy, timeout, true)
}

fn run_capture_inner(
    dir: &Path,
    id: &str,
    spec: &DelegationSpec,
    prompt: &str,
    policy: &LaunchPolicy,
    timeout: Option<Duration>,
    stream_live: bool,
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
    if stream_live {
        // Start draining immediately. Waiting for `try_wait` first would
        // recreate the invisible-buffering failure this mode exists to fix.
        let out_reader = tee_reader(child.stdout.take().expect("stdout piped"), false);
        let err_reader = tee_reader(child.stderr.take().expect("stderr piped"), true);
        let timed_out = wait_for_child(&mut child, deadline)?;
        let status = child.wait()?;
        let stdout = out_reader.join().unwrap_or_default();
        let stderr = err_reader.join().unwrap_or_default();
        return Ok(HarnessOutput {
            stdout: String::from_utf8_lossy(&stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
            status,
            timed_out,
        });
    }
    let timed_out = wait_for_child(&mut child, deadline)?;
    let output = child.wait_with_output()?;
    Ok(HarnessOutput {
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        status: output.status,
        timed_out,
    })
}

fn wait_for_child(child: &mut std::process::Child, deadline: Option<Instant>) -> Result<bool> {
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
    Ok(timed_out)
}

/// Drain one child stream so a chatty harness cannot block on a full pipe,
/// while also forwarding bytes to the delegated worker's redirected stream.
fn tee_reader<R: Read + Send + 'static>(
    mut reader: R,
    is_stderr: bool,
) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut captured = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            captured.extend_from_slice(&buf[..n]);
            if is_stderr {
                let mut sink = std::io::stderr();
                let _ = sink.write_all(&buf[..n]);
                let _ = sink.flush();
            } else {
                let mut sink = std::io::stdout();
                let _ = sink.write_all(&buf[..n]);
                let _ = sink.flush();
            }
        }
        captured
    })
}
