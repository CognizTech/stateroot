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
pub mod delegate;
pub mod doctor;
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
pub mod observations;
pub mod persona;
pub mod plan;
pub mod proposals;
pub mod remove;
pub mod resume;
pub mod roots;
pub mod rules;
pub mod seed;
pub mod session;
pub mod setup;
pub mod skill;
pub mod soul;
pub mod status;
pub mod synthesize;
pub mod transplant;
pub mod uninstall;
pub mod update;
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
        let cwd = std::env::current_dir()?;
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
