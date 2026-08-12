//! Command-line surface for the local-first `stateroot` binary.
//!
//! Only offline commands exist here — anything that fundamentally needed the
//! StateSmith server (packs, proposals, soul service, goals, reviews, roots)
//! is left out of M1 entirely, not stubbed.

use clap::{Args, Parser, Subcommand};

/// Exact version embedded in this binary. Rolling CI previews append an
/// automatically increasing `-dev.<run>` suffix without mutating Cargo.toml.
pub const BUILD_VERSION: &str = match option_env!("STATEROOT_BUILD_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// StateRoot — local-first continuity for every harness.
#[derive(Debug, Parser)]
#[command(name = "stateroot", version = BUILD_VERSION, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize a project (creates `.stateroot/`, product skill, projections).
    Init(InitArgs),
    /// Remove a project (`.stateroot/`, registry entry, convenience layer,
    /// our git refs) — plan preview + confirmation.
    Remove(RemoveArgs),
    /// Import sessions from native harness transcripts (all six readers).
    Import(ImportArgs),
    /// Print the compact resume digest for the current project.
    Resume(ResumeArgs),
    /// Append an episodic checkpoint to the local log.
    Checkpoint(CheckpointArgs),
    /// Handoff packets (write/read/list/accept) — local store.
    Handoff(HandoffArgs),
    /// Create a root snapshot of the working state (git plumbing).
    Snap(SnapArgs),
    /// Root lineage with coverage lines and fork markers.
    Log,
    /// Show one root by hash (prefix allowed).
    Show {
        /// Root hash or prefix.
        hash: String,
    },
    /// Diff two roots (names+status; --content for unified diffs).
    Diff(DiffArgs),
    /// Append-only revert to a root's tree (NEW root; confirm required).
    Revert(RevertArgs),
    /// Branch-materialize a root under refs/stateroot/forks/<name>.
    Fork(ForkArgs),
    /// Render a transition receipt (verified tier = git delta).
    Receipt {
        /// Transition id or prefix.
        id: String,
    },
    /// Project status (manifest, handoff, counts) — local only.
    Status,
    /// Diagnose the local setup (config, store, registry, hooks, federation).
    Doctor,
    /// Harness session hook (SessionStart/UserPromptSubmit/PreCompact/Stop).
    Hook(HookArgs),
    /// Install stateroot integration for detected harnesses (global).
    Install,
    /// Full machine removal: harness registrations, config dir, and the
    /// binary itself (project .stateroot/ dirs are never touched).
    Uninstall {
        /// Also delete user-global data (~/.stateroot: soul, learnings, memories).
        #[arg(long)]
        purge: bool,
        /// Skip the interactive confirmation.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Remove integrations/data without deleting the binary. Reserved for
        /// the Windows Installer uninstall transaction.
        #[arg(long, hide = true)]
        msi_cleanup: bool,
    },
    /// Guided local setup (identity, harnesses, skills).
    Setup(SetupArgs),
    /// Log in (OAuth device flow). Currently: --via github.
    Login {
        /// Provider (only `github` exists).
        #[arg(long, default_value = "github")]
        via: String,
    },
    /// Clear the stored credential.
    Logout,
    /// GitHub repo binding for refs sync.
    Repo(RepoArgs),
    /// Push/pull refs/stateroot/* against the linked remote (never force).
    Sync(SyncArgs),
    /// Cloud run: objective executed in StateSmith cloud (requires login).
    Run(RunArgs),
    /// List or inspect cloud runs.
    Runs(RunsArgs),
    /// Check for / install a newer stateroot binary (from GitHub releases).
    SelfUpdate {
        /// Only report current vs latest; do not install.
        #[arg(long)]
        check: bool,
    },
    /// Local stdio MCP server (line-delimited JSON-RPC; W8 tools, local stores).
    McpStdio,
    /// Canonical soul, overlay, projections (all local).
    Soul(SoulArgs),
    /// Local proposals (the shared approval gate).
    Proposals(ProposalsArgs),
    /// Scoped learnings: list/accept/reject/edit/distill.
    Learnings(LearningsArgs),
    /// Record a note into the review loop (classify → proposal).
    Learn(LearnArgs),
    /// Local LLM synthesis over transcript bundles (own provider key).
    Synthesize {
        /// Re-run even when the bundle hash is unchanged.
        #[arg(long)]
        force: bool,
    },
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
        /// Harness creating the handoff (falls back to the active local marker).
        #[arg(long)]
        from: Option<String>,
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
    #[arg(long)]
    pub harness: String,
}

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Only run these sections (comma-separated): identity, harnesses, skills.
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
    /// Open a proposal to activate a quarantined skill (approve projects it
    /// into harness roots).
    Promote {
        /// Skill slug.
        slug: String,
        /// Optional rationale.
        #[arg(long)]
        rationale: Option<String>,
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
    /// List the local stdio MCP server's tool surface (W8 tools).
    Tools,
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

#[derive(Debug, Args)]
pub struct SnapArgs {
    /// Free-text reason recorded in the root manifest + transition.
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// Base root hash.
    pub from: String,
    /// Target root hash.
    pub to: String,
    /// Print unified line diffs (capped at 20 files / 200 lines each).
    #[arg(long)]
    pub content: bool,
}

#[derive(Debug, Args)]
pub struct RevertArgs {
    /// Root hash to revert to (a NEW root is created; history is append-only).
    pub root: String,
    /// Do not ask for confirmation.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct ForkArgs {
    /// Root hash to branch from.
    pub root: String,
    /// Branch name (default: fork-<hash8>).
    #[arg(long)]
    pub branch: Option<String>,
}

#[derive(Debug, Args)]
pub struct SoulArgs {
    #[command(subcommand)]
    pub action: SoulAction,
}

#[derive(Debug, Subcommand)]
pub enum SoulAction {
    /// Show canonical soul, project overlay, and projection.
    Show {
        /// Render the harness-appropriate projection (per registry framing).
        #[arg(long)]
        harness: Option<String>,
    },
    /// Edit the canonical soul in $EDITOR (user-authoring, with snapshot).
    Edit,
    /// Import a soul from openclaw, hermes, or a file path.
    Import {
        /// `openclaw` | `hermes` | a file path.
        #[arg(long)]
        from: String,
    },
    /// Deterministic Q&A draft (zero model calls by default).
    Generate {
        /// Write via direct canonical update (user-authoring).
        #[arg(long)]
        apply: bool,
        /// Accept Q&A defaults without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// Propose a soul change through the gated proposals flow.
    Propose {
        /// Markdown file to propose.
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        /// Read content from stdin.
        #[arg(long)]
        stdin: bool,
        /// Optional rationale recorded on the proposal.
        #[arg(long)]
        rationale: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct ProposalsArgs {
    #[command(subcommand)]
    pub action: ProposalsAction,
}

#[derive(Debug, Subcommand)]
pub enum ProposalsAction {
    /// List proposals (optionally by status).
    List {
        #[arg(long)]
        status: Option<String>,
    },
    /// Show one proposal by id (prefix allowed).
    Show { id: String },
    /// Approve a proposal and activate its change.
    Approve {
        id: String,
        /// Replace the payload with edited JSON before activating.
        #[arg(long)]
        edit: Option<String>,
    },
    /// Reject a proposal (kept for audit).
    Reject { id: String },
}

#[derive(Debug, Args)]
pub struct LearningsArgs {
    #[command(subcommand)]
    pub action: LearningsAction,
}

#[derive(Debug, Subcommand)]
pub enum LearningsAction {
    /// List learnings (project scope by default).
    List {
        /// User-global scope (~/.stateroot/learnings).
        #[arg(long)]
        user: bool,
        #[arg(long)]
        status: Option<String>,
    },
    /// Promote a candidate to active (user approval).
    Accept {
        id: String,
        #[arg(long)]
        user: bool,
    },
    /// Reject a candidate (archived for audit).
    Reject {
        id: String,
        #[arg(long)]
        user: bool,
    },
    /// Edit a learning's statement in place.
    Edit {
        id: String,
        #[arg(long)]
        statement: String,
        #[arg(long)]
        user: bool,
    },
    /// Mine episodic + spool for new candidates (→ proposals).
    Distill,
}

#[derive(Debug, Args)]
pub struct LearnArgs {
    #[command(subcommand)]
    pub action: LearnAction,
}

#[derive(Debug, Subcommand)]
pub enum LearnAction {
    /// Classify a note and file it as a proposal (never a direct write).
    Record {
        /// The note to record.
        note: String,
    },
}

#[derive(Debug, Args)]
pub struct RepoArgs {
    #[command(subcommand)]
    pub action: RepoAction,
}

#[derive(Debug, Subcommand)]
pub enum RepoAction {
    /// Bind the project to a GitHub repo (verifies access with the token).
    Link {
        /// owner/repo (a github.com URL works too).
        repo: String,
        /// `same-repo` (default; refs/stateroot/* inside the repo) or
        /// `companion` (a dedicated <project>-stateroot repo).
        #[arg(long)]
        layout: Option<String>,
    },
    /// Show the current binding + last sync.
    Status,
}

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Push local refs/stateroot/* (default if neither flag set).
    #[arg(long)]
    pub push: bool,
    /// Fetch remote refs/stateroot/* (default if neither flag set).
    #[arg(long)]
    pub pull: bool,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// The objective for the cloud run.
    #[arg(long)]
    pub cloud: String,
    /// Start from this root (default: latest synced).
    #[arg(long)]
    pub from: Option<String>,
    /// Harness to run as (default: server decides).
    #[arg(long)]
    pub harness: Option<String>,
    /// Verification surface to execute in the cloud.
    #[arg(long)]
    pub verification: Option<String>,
    /// Poll until the run reaches a terminal state (with an event tail).
    #[arg(long)]
    pub watch: bool,
}

#[derive(Debug, Args)]
pub struct RunsArgs {
    #[command(subcommand)]
    pub action: RunsAction,
}

#[derive(Debug, Subcommand)]
pub enum RunsAction {
    /// List cloud runs for this project.
    List,
    /// Show one run (status + last events).
    Status {
        /// Run id.
        id: String,
    },
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// Skip the interactive confirmation.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Print the plan without touching anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Skip the server-side deletion (when the cloud path applies).
    #[arg(long)]
    pub keep_server_state: bool,
}
