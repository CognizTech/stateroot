//! Command-line surface for the local-first `stateroot` binary.
//!
//! Only offline commands exist here — anything that fundamentally needed the
//! StateSmith server (packs, proposals, soul service, goals, reviews, roots)
//! is left out of M1 entirely, not stubbed.

use clap::{Args, Parser, Subcommand};

/// StateRoot — local-first continuity for every harness.
#[derive(Debug, Parser)]
#[command(name = "stateroot", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize a project (creates `.stateroot/`, product skill, projections).
    Init(InitArgs),
    /// Import sessions from native harness transcripts (all six readers).
    Import(ImportArgs),
    /// Print the compact resume digest for the current project.
    Resume(ResumeArgs),
    /// Append an episodic checkpoint to the local log.
    Checkpoint(CheckpointArgs),
    /// Handoff packets (write/read/list/accept) — local store.
    Handoff(HandoffArgs),
    /// Local history: checkpoints and handoff lineage.
    Log,
    /// Project status (manifest, handoff, counts) — local only.
    Status,
    /// Diagnose the local setup (config, store, registry, hooks, federation).
    Doctor,
    /// Harness session hook (SessionStart/UserPromptSubmit/PreCompact/Stop).
    Hook(HookArgs),
    /// Install stateroot integration for detected harnesses (global).
    Install,
    /// Remove stateroot-managed harness integration (global).
    Uninstall,
    /// Guided local setup (harnesses, skills).
    Setup(SetupArgs),
    /// Skill listing, inspection, and federation sync.
    Skill(SkillArgs),
    /// MCP server discovery and federation sync.
    Mcp(McpArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Local project directory (default: current directory; must exist).
    #[arg(value_name = "DIR")]
    pub dir: Option<String>,
    /// Local project directory (alternative to the positional argument).
    #[arg(long, value_name = "DIR", conflicts_with = "dir")]
    pub path: Option<String>,
    /// Project name (defaults to the directory name).
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Restrict to one harness (codex, claude, cursor, kimi, openclaw, hermes).
    #[arg(long)]
    pub harness: Option<String>,
    /// Only import sessions started on/after this date (YYYY-MM-DD).
    #[arg(long)]
    pub since: Option<String>,
    /// Print what would be imported without writing anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ResumeArgs {
    /// Harness to tailor the digest for.
    #[arg(long)]
    pub harness: Option<String>,
    /// Do not mark the handoff as accepted by this harness.
    #[arg(long = "no-accept")]
    pub no_accept: bool,
    /// Print the full digest even if resume already ran this session.
    #[arg(long)]
    pub force: bool,
    /// Deterministic render (local handoff only — the default offline).
    #[arg(long)]
    pub deterministic: bool,
}

#[derive(Debug, Args)]
pub struct CheckpointArgs {
    /// What changed in this step.
    #[arg(long)]
    pub note: String,
    /// Comma-separated list of files touched.
    #[arg(long, value_delimiter = ',')]
    pub files: Vec<String>,
}

#[derive(Debug, Args)]
pub struct HandoffArgs {
    #[command(subcommand)]
    pub action: HandoffAction,
}

#[derive(Debug, Subcommand)]
pub enum HandoffAction {
    /// Write a new handoff packet (local store).
    Write {
        /// Harness the handoff recommends next.
        #[arg(long)]
        to: String,
        /// Short context note.
        #[arg(long)]
        note: Option<String>,
        /// Restate the objective.
        #[arg(long)]
        objective: Option<String>,
    },
    /// List known handoffs.
    List,
    /// Show a handoff packet (defaults to the current one).
    Show {
        /// Sequence number of the handoff to show.
        seq: Option<i64>,
    },
    /// Mark the current handoff as accepted by a harness.
    Accept {
        /// Accepting harness (default: cli).
        #[arg(long, default_value = "cli")]
        by: String,
    },
}

#[derive(Debug, Args)]
pub struct HookArgs {
    /// Harness-native or canonical event name (e.g. SessionStart, stop).
    pub event: String,
    /// Harness id (canonical or legacy), e.g. claude-code, kimi.
    #[arg(long, default_value = "claude-code")]
    pub harness: String,
}

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Only run these sections (comma-separated): harnesses, skills.
    #[arg(long, value_delimiter = ',')]
    pub only: Vec<String>,
    /// Print planned writes without touching disk.
    #[arg(long)]
    pub dry_run: bool,
    /// Accept all defaults.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// YAML file with answers (same keys as the prompts).
    #[arg(long)]
    pub config: Option<std::path::PathBuf>,
}

#[derive(Debug, Args)]
pub struct SkillArgs {
    #[command(subcommand)]
    pub action: SkillAction,
}

#[derive(Debug, Subcommand)]
pub enum SkillAction {
    /// (Re)materialize the project-level convenience layer in the cwd.
    Install,
    /// List skills (local federated inventory).
    List,
    /// Print a skill's SKILL.md.
    Show {
        /// Skill slug.
        slug: String,
    },
    /// Scan all registered harness skill roots (dry discovery).
    Scan {
        /// Also print JSON for scripting.
        #[arg(long)]
        json: bool,
    },
    /// Bidirectional skill sync into `.stateroot/skills` + `.agents/skills`.
    Sync {
        /// Show actions without writing.
        #[arg(long)]
        dry_run: bool,
        /// Pull foreign skills into the portable registry (default if neither flag set).
        #[arg(long)]
        pull: bool,
        /// Push portable skills into harness-specific bridge roots.
        #[arg(long)]
        push: bool,
    },
    /// Print federation status summary.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Diagnose skill federation (registry, roots, counts).
    Doctor,
}

#[derive(Debug, Args)]
pub struct McpArgs {
    #[command(subcommand)]
    pub action: McpAction,
}

#[derive(Debug, Subcommand)]
pub enum McpAction {
    /// Scan MCP servers across registered harness configs.
    Scan {
        #[arg(long)]
        json: bool,
    },
    /// Pull discovered MCP servers into the canonical store and project them.
    Sync {
        #[arg(long)]
        dry_run: bool,
        /// Pull into `.stateroot/tools/mcp.json` (default if neither flag set).
        #[arg(long)]
        pull: bool,
        /// Project canonical servers into harness MCP configs (default if neither flag set).
        #[arg(long)]
        push: bool,
    },
    /// Print MCP federation status.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Diagnose collisions and projection conflicts.
    Doctor,
    /// Remove a server from the canonical store(s) and projection ledger.
    Remove {
        /// Canonical server name to remove.
        name: String,
    },
    /// Adopt a harness-side copy of a server into the canonical store.
    AcceptTheirs {
        /// Canonical server name to resolve.
        name: String,
        /// Pick the copy from this harness when several differ.
        #[arg(long)]
        from: Option<String>,
    },
}
