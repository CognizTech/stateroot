//! Command modules — the local-first CLI surface.
//!
//! Everything here runs OFFLINE against local services only (`.stateroot/`
//! store, harness configs, transcript readers, federation engines). There is
//! no server to call: commands that fundamentally needed one were left out
//! rather than given a facade.

use std::path::PathBuf;

use anyhow::anyhow;
use stateroot_core::config::{self as core_config, AppConfig, ProjectEntry};
use stateroot_core::local_store;

pub mod active_harness;
pub mod blocks;
pub mod checkpoint;
pub mod compiler;
pub mod continuity_synthesis;
pub mod delegate;
pub mod detached;
pub mod doctor;
pub mod dot;
pub mod dot_integration;
pub mod drain_finalize;
pub mod editor_extensions;
pub mod ext;
pub mod handoff;
pub mod harness;
pub mod harness_cli;
pub mod harness_display;
pub mod hook;
pub mod import;
pub mod init;
pub mod install;
pub mod learn;
pub mod learnings;
pub mod learnings_reader;
pub mod mcp;
pub mod mcp_stdio;
pub mod memory;
pub mod obligation;
pub mod observations;
pub mod persona;
pub mod plan;
pub mod projects;
pub mod proposals;
pub mod remove;
pub mod resume;
pub mod roots;
pub mod rules;
pub mod seed;
pub mod service;
pub mod session;
pub mod setup;
pub mod skill;
pub mod soul;
pub mod status;
pub mod synthesize;
pub mod telemetry_drain;
pub mod todo;
pub mod transplant;
pub mod uninstall;
pub mod update;
pub mod update_schedule;
pub mod wiki;

/// Shared context built once per command invocation.
#[derive(Clone)]
pub struct Ctx {
    /// Directory the command runs in.
    pub cwd: PathBuf,
    /// Resolved config directory (`STATEROOT_HOME` or platform default).
    pub config_dir: PathBuf,
    /// Loaded service configuration.
    pub config: AppConfig,
}

impl Ctx {
    /// Build the context from the process environment.
    pub fn load() -> anyhow::Result<Self> {
        let process_cwd = std::env::current_dir()?;
        // Walk up to the nearest `.stateroot/` so a WSL cwd in a subfolder
        // (or a Windows/WSL translated payload path) still hits the shared store.
        let cwd = local_store::find_project_root(&process_cwd).unwrap_or(process_cwd);
        let config_dir = core_config::config_dir().map_err(|e| anyhow!(e))?;
        let config = core_config::load_config(&config_dir).map_err(|e| anyhow!(e))?;
        Ok(Self {
            cwd,
            config_dir,
            config,
        })
    }

    /// Resolve the project associated with the current directory.
    ///
    /// Looks at `.stateroot/manifest.json` first, then the `projects.toml`
    /// registry.
    pub fn current_project(&self) -> anyhow::Result<Option<ProjectEntry>> {
        if local_store::is_stateroot_dir(&self.cwd) {
            if let Some(manifest) = local_store::read_manifest(&self.cwd)? {
                let project_id = manifest
                    .get("project_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if !project_id.is_empty() {
                    let name = manifest
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    return Ok(Some(ProjectEntry {
                        project_id: project_id.clone(),
                        workspace_id: project_id,
                        name,
                        harnesses_installed: Vec::new(),
                        created_at: manifest
                            .get("created_at")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        ..Default::default()
                    }));
                }
            }
        }
        core_config::lookup_project(&self.config_dir, &self.cwd).map_err(|e| anyhow!(e))
    }

    /// Like [`Ctx::current_project`] but errors with guidance when absent.
    pub fn require_project(&self) -> anyhow::Result<ProjectEntry> {
        self.current_project()?.ok_or_else(|| {
            anyhow!(
                "not a stateroot project (no .stateroot/ here and no registry entry) — run `stateroot init`"
            )
        })
    }
}

/// True when stdin is an interactive terminal (prompts are allowed).
pub fn stdin_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

/// Print to stderr (logs/diagnostics); stdout is reserved for command output.
macro_rules! note {
    ($($arg:tt)*) => {
        eprintln!($($arg)*)
    };
}

pub(crate) use note;

/// Outcome of a bounded child-process probe.
#[derive(Debug)]
pub(crate) struct BoundedRun {
    /// True when the child exited 0 within the budget.
    pub success: bool,
    /// Captured stdout (empty for status-only probes).
    pub stdout: String,
}

/// Run `program args` with a hard wall-clock budget. The child's stdout is
/// redirected to a private temp file so a chatty child can never deadlock a
/// pipe while the parent polls; on timeout the child is killed and reaped
/// (no orphan fixture children, no hanging doctor/status/service probes).
/// Returns `None` on spawn failure or timeout — an indeterminate probe is
/// unknown, never success.
pub(crate) fn bounded_run(
    program: &str,
    args: &[&str],
    timeout: std::time::Duration,
    capture_stdout: bool,
) -> Option<BoundedRun> {
    use std::io::{Read as _, Seek as _};
    use std::process::{Command, Stdio};
    use std::time::Instant;

    const OUTPUT_CAP: u64 = 1024 * 1024;
    // Anonymous private file: no predictable filename, symlink race or leak
    // on spawn failure. Cap both a still-running producer and the final read.
    let mut capture_file = if capture_stdout {
        Some(tempfile::tempfile().ok()?)
    } else {
        None
    };
    let stdout: Stdio = match &capture_file {
        Some(file) => Stdio::from(file.try_clone().ok()?),
        None => Stdio::null(),
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    let result = loop {
        if capture_file
            .as_ref()
            .is_some_and(|f| f.metadata().map(|m| m.len() > OUTPUT_CAP).unwrap_or(true))
        {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status.success()),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            // Timeout or a wait error: kill + reap, then report unknown.
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let stdout_text = match (&mut capture_file, result) {
        (Some(file), Some(_)) => {
            if Instant::now() >= deadline || file.metadata().ok()?.len() > OUTPUT_CAP {
                return None;
            }
            file.rewind().ok()?;
            let mut bytes = Vec::new();
            file.take(OUTPUT_CAP + 1).read_to_end(&mut bytes).ok()?;
            if bytes.len() as u64 > OUTPUT_CAP || Instant::now() >= deadline {
                return None;
            }
            if bytes.starts_with(&[0xff, 0xfe]) {
                if bytes.len() % 2 != 0 {
                    return None;
                }
                let units: Vec<u16> = bytes[2..]
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect();
                String::from_utf16(&units).ok()?
            } else {
                String::from_utf8(bytes).ok()?
            }
        }
        (Some(_), None) => return None,
        (None, _) => String::new(),
    };
    result.map(|success| BoundedRun {
        success,
        stdout: stdout_text,
    })
}

/// Bounded status-only probe (manager mutations/queries that print nothing
/// we need). `true` only on a confirmed zero exit within the budget.
pub(crate) fn bounded_status(program: &str, args: &[&str], timeout: std::time::Duration) -> bool {
    bounded_run(program, args, timeout, false)
        .map(|run| run.success)
        .unwrap_or(false)
}

/// Truncate a string to a display width (chars, with an ellipsis).
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Run the deterministic continuity reconciliation after a state mutation.
/// Best-effort: reconciliation never breaks the mutation that triggered it,
/// and it only runs inside a project with continuity enabled.
pub fn reconcile_quiet(ctx: &Ctx) {
    if !ctx.config.continuity.enabled || !local_store::is_stateroot_dir(&ctx.cwd) {
        return;
    }
    if let Err(err) =
        stateroot_core::continuity::reconcile(&ctx.cwd, &ctx.config_dir, &ctx.config.continuity)
    {
        note!("continuity reconcile: {err}");
    }
}

#[cfg(test)]
mod bounded_probe_tests {
    use super::*;
    #[test]
    fn oversized_fast_output_is_unknown() {
        #[cfg(windows)]
        let result = bounded_run(
            "powershell.exe",
            &["-NoProfile", "-Command", "[Console]::Write('x' * 1048577)"],
            std::time::Duration::from_secs(5),
            true,
        );
        #[cfg(not(windows))]
        let result = bounded_run(
            "sh",
            &["-c", "exec head -c 1048577 /dev/zero"],
            std::time::Duration::from_secs(5),
            true,
        );
        assert!(
            result.is_none(),
            "oversized output must not be partial successful proof"
        );
    }
    #[cfg(unix)]
    #[test]
    fn hung_probe_is_killed_and_reaped_within_budget() {
        let start = std::time::Instant::now();
        assert!(bounded_run(
            "sh",
            &["-c", "exec sleep 30"],
            std::time::Duration::from_millis(100),
            true
        )
        .is_none());
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }
}
