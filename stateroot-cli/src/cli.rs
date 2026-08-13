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
    /// Compare two roots for experiment semantics (files, state, transitions, activity).
    Compare(CompareArgs),
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
    /// Local proposals (optional audit log — not a blocking gate).
    Proposals(ProposalsArgs),
    /// Scoped learnings: list/accept/reject/edit/distill.
    Learnings(LearningsArgs),
    /// Record a learning (active immediately; scope from flags).
    Learn(LearnArgs),
    /// Local LLM synthesis over transcript bundles (own provider key).
    Synthesize {
        /// Re-run even when the bundle hash is unchanged.
        #[arg(long)]
        force: bool,
    },
    /// Curated hot-apex memory + FTS recall.
    Memory(MemoryArgs),
    /// Compiled wiki catalog (show / lint / compile).
    Wiki(WikiArgs),
    /// Skill listing, inspection, and federation sync.
    Skill(SkillArgs),
    /// Shared rules pool (product-intent plus federated harness rules).
    Rules(RulesArgs),
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

#[derive(Debug, Args)]
pub struct HandoffWriteArgs {
    /// Harness creating the handoff (falls back to the active local marker).
    #[arg(long)]
    pub from: Option<String>,
    /// Optional routing hint for orchestrated harness selection (omit for continuity-only).
    #[arg(long)]
    pub to: Option<String>,
    /// Short context note.
    #[arg(long)]
    pub note: Option<String>,
    /// Strict structured handoff JSON file (`-` reads standard input).
    #[arg(long, value_name = "PATH")]
    pub input: Option<String>,
    /// Restate the objective.
    #[arg(long)]
    pub objective: Option<String>,
    /// Immediate work boundary for the receiving agent.
    #[arg(long)]
    pub task: Option<String>,
    /// Detailed continuity narrative (alias `--summary`).
    #[arg(long, alias = "summary")]
    pub context_summary: Option<String>,
    /// Next action (repeatable; required when `--to` names another harness).
    #[arg(long, action = clap::ArgAction::Append)]
    pub next: Vec<String>,
    /// Decision with rationale (repeatable).
    #[arg(long, action = clap::ArgAction::Append)]
    pub decision: Vec<String>,
    /// Failed approach or bug (repeatable).
    #[arg(long, action = clap::ArgAction::Append)]
    pub failure: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum HandoffAction {
    /// Write a new handoff packet (local store).
    Write(Box<HandoffWriteArgs>),
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
    /// Finalize continuity from the latest verified transcript (no routing).
    Finalize {
        /// Harness whose session to finalize (falls back to the active local marker).
        #[arg(long)]
        from: Option<String>,
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
    /// Activate a skill package and project it to installed harnesses.
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
pub struct RulesArgs {
    #[command(subcommand)]
    pub action: RulesAction,
}

#[derive(Debug, Subcommand)]
pub enum RulesAction {
    /// List the shared rules pool (product-intent first, then imported).
    List,
    /// Print one rule's markdown.
    Show {
        /// Rule slug (`product-intent`, `cursor-no-foo`, …).
        slug: String,
    },
    /// Seed product-intent and pull live harness instruction files into the pool.
    Sync,
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
    /// Harness that drove this snapshot (defaults to active-harness marker or `cli`).
    #[arg(long)]
    pub harness: Option<String>,
}

#[derive(Debug, Args)]
pub struct CompareArgs {
    /// First root hash.
    pub a: String,
    /// Second root hash.
    pub b: String,
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
    /// Propose a soul change (writes canonical immediately; optional audit).
    Propose {
        /// Markdown file to propose.
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        /// Read content from stdin.
        #[arg(long)]
        stdin: bool,
        /// Optional rationale recorded on the optional audit proposal.
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
        /// User-global scope (`~/.stateroot/learnings`).
        #[arg(long, group = "scope")]
        user: bool,
        /// Workspace scope (`~/.stateroot/workspaces/{id}/learnings`).
        #[arg(long, group = "scope")]
        workspace: bool,
        /// Domain scope (`~/.stateroot/domains/{slug}/learnings`).
        #[arg(long, group = "scope")]
        domain: Option<String>,
        #[arg(long)]
        status: Option<String>,
    },
    /// Promote a candidate to active (user approval).
    Accept {
        id: String,
        #[arg(long, group = "scope")]
        user: bool,
        #[arg(long, group = "scope")]
        workspace: bool,
        #[arg(long, group = "scope")]
        domain: Option<String>,
    },
    /// Reject a candidate (archived for audit).
    Reject {
        id: String,
        #[arg(long, group = "scope")]
        user: bool,
        #[arg(long, group = "scope")]
        workspace: bool,
        #[arg(long, group = "scope")]
        domain: Option<String>,
    },
    /// Edit a learning's statement in place.
    Edit {
        id: String,
        #[arg(long)]
        statement: String,
        #[arg(long, group = "scope")]
        user: bool,
        #[arg(long, group = "scope")]
        workspace: bool,
        #[arg(long, group = "scope")]
        domain: Option<String>,
    },
    /// Mine episodic + spool into the wiki inbox (does not activate learnings).
    Distill,
}

#[derive(Debug, Args)]
pub struct LearnArgs {
    #[command(subcommand)]
    pub action: LearnAction,
}

#[derive(Debug, Subcommand)]
pub enum LearnAction {
    /// Record a learning (taste, convention, judgment). Active immediately.
    Record {
        /// The note to record.
        note: String,
        /// User-global scope (`~/.stateroot/learnings`). Default is project.
        #[arg(long, group = "scope")]
        user: bool,
        /// Workspace scope (`~/.stateroot/workspaces/{id}/learnings`).
        #[arg(long, group = "scope")]
        workspace: bool,
        /// Domain scope (`~/.stateroot/domains/{slug}/learnings`).
        #[arg(long, group = "scope")]
        domain: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct MemoryArgs {
    #[command(subcommand)]
    pub action: MemoryAction,
}

#[derive(Debug, Subcommand)]
pub enum MemoryAction {
    /// Add a curated fact (MEMORY.md) or USER.md note.
    Add {
        /// Entry text.
        content: String,
        /// `memory` (default) or `user`.
        #[arg(long, default_value = "memory")]
        target: String,
        /// Mark entry private (foreign harnesses cannot recall it).
        #[arg(long)]
        private: bool,
    },
    /// Replace the first matching entry/substring.
    Replace {
        /// New entry text.
        content: String,
        /// Substring to find.
        #[arg(long)]
        old: String,
        #[arg(long, default_value = "memory")]
        target: String,
        #[arg(long)]
        private: bool,
    },
    /// Remove the first matching entry/substring.
    Remove {
        /// Substring to find.
        old: String,
        #[arg(long, default_value = "memory")]
        target: String,
    },
    /// Show entries + capacity.
    Show {
        #[arg(long, default_value = "memory")]
        target: String,
    },
    /// FTS recall over memory, wiki pages, episodic, transcripts.
    Recall {
        query: String,
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },
}

#[derive(Debug, Args)]
pub struct WikiArgs {
    #[command(subcommand)]
    pub action: WikiAction,
}

#[derive(Debug, Subcommand)]
pub enum WikiAction {
    /// Show one page body.
    Show { path: String },
    /// Lint index/pages consistency.
    Lint,
    /// Compile mined notes into inbox/pages (deterministic; agentic when keyed).
    Compile {
        #[arg(long)]
        force: bool,
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
}
