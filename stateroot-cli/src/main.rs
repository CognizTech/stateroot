//! `stateroot` — the local-first StateRoot binary.
//!
//! Every command runs offline against the local `.stateroot/` store, the
//! harnesses on the machine, and the lifted federation engines. There is no
//! server anywhere in this variant.

mod cli;
mod commands;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use cli::{
    Command, HandoffAction, LearnAction, LearningsAction, McpAction, ProposalsAction, SkillAction,
    SoulAction,
};
use commands::Ctx;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let default_level = if cfg!(debug_assertions) {
        "stateroot=info,warn"
    } else {
        "warn"
    };
    let filter =
        EnvFilter::try_from_env("STATEROOT_LOG").unwrap_or_else(|_| EnvFilter::new(default_level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let cli = cli::Cli::parse();
    let ctx = Ctx::load()?;

    match cli.command {
        Command::Init(args) => commands::init::run(&ctx, args).await?,
        Command::Import(args) => {
            commands::import::run(
                &ctx,
                &commands::import::ImportOptions {
                    harness: args.harness,
                    since: args.since,
                    dry_run: args.dry_run,
                    quiet: false,
                },
            )
            .await?;
        }
        Command::Resume(args) => commands::resume::run(
            &ctx,
            args.harness.as_deref(),
            args.no_accept,
            args.force,
            args.deterministic,
        )?,
        Command::Checkpoint(args) => commands::checkpoint::run(&ctx, &args.note, &args.files)?,
        Command::Handoff(args) => match args.action {
            HandoffAction::Write {
                to,
                note,
                objective,
            } => commands::handoff::write(&ctx, &to, note.as_deref(), objective.as_deref()).await?,
            HandoffAction::List => commands::handoff::list(&ctx).await?,
            HandoffAction::Show { seq } => commands::handoff::show(&ctx, seq).await?,
            HandoffAction::Accept { by } => commands::handoff::accept(&ctx, &by).await?,
        },
        Command::Snap(args) => commands::roots::snap(&ctx, args.reason.as_deref())?,
        Command::Log => commands::roots::log(&ctx)?,
        Command::Show { hash } => commands::roots::show(&ctx, &hash)?,
        Command::Diff(args) => commands::roots::diff(&ctx, &args.from, &args.to, args.content)?,
        Command::Revert(args) => commands::roots::revert(&ctx, &args.root, args.yes)?,
        Command::Fork(args) => commands::roots::fork(&ctx, &args.root, args.branch.as_deref())?,
        Command::Receipt { id } => commands::roots::receipt(&ctx, &id)?,
        Command::Status => commands::status::run(&ctx)?,
        Command::Doctor => {
            let code = commands::doctor::run(&ctx).await?;
            if code != 0 {
                std::process::exit(code);
            }
        }
        Command::Hook(args) => {
            let code = commands::hook::run(&ctx, &args.event, &args.harness).await?;
            if code != 0 {
                std::process::exit(code as i32);
            }
        }
        Command::Install => commands::install::install(&ctx).await?,
        Command::Uninstall => commands::install::uninstall(&ctx)?,
        Command::Setup(args) => {
            commands::setup::run(
                ctx.clone(),
                commands::setup::WizardOptions {
                    only: args.only,
                    depth: commands::setup::DepthChoice::Full,
                    dry_run: args.dry_run,
                    yes: args.yes,
                    config_file: args.config,
                },
            )
            .await?
        }
        Command::Soul(args) => match args.action {
            SoulAction::Show { harness } => commands::soul::show(&ctx, harness.as_deref())?,
            SoulAction::Edit => commands::soul::edit(&ctx)?,
            SoulAction::Import { from } => commands::soul::import(&ctx, &from)?,
            SoulAction::Generate { apply, yes } => commands::soul::generate(&ctx, yes, apply)?,
            SoulAction::Propose {
                file,
                stdin,
                rationale,
            } => commands::soul::propose(
                &ctx,
                file.as_deref().map(|p| p.to_str().unwrap_or("")),
                stdin,
                rationale.as_deref(),
            )?,
        },
        Command::Proposals(args) => match args.action {
            ProposalsAction::List { status } => commands::proposals::list(&ctx, status.as_deref())?,
            ProposalsAction::Show { id } => commands::proposals::show(&ctx, &id)?,
            ProposalsAction::Approve { id, edit } => {
                commands::proposals::approve(&ctx, &id, edit.as_deref())?
            }
            ProposalsAction::Reject { id } => commands::proposals::reject(&ctx, &id)?,
        },
        Command::Learnings(args) => match args.action {
            LearningsAction::List { user, status } => {
                commands::learnings::list(&ctx, user, status.as_deref())?
            }
            LearningsAction::Accept { id, user } => commands::learnings::accept(&ctx, &id, user)?,
            LearningsAction::Reject { id, user } => commands::learnings::reject(&ctx, &id, user)?,
            LearningsAction::Edit {
                id,
                statement,
                user,
            } => commands::learnings::edit(&ctx, &id, &statement, user)?,
            LearningsAction::Distill => commands::learnings::distill(&ctx)?,
        },
        Command::Learn(args) => match args.action {
            LearnAction::Record { note } => commands::learn::record(&ctx, &note)?,
        },
        Command::Synthesize { force } => commands::synthesize::run(&ctx, force).await?,
        Command::Skill(args) => match args.action {
            SkillAction::Install => commands::skill::install(&ctx)?,
            SkillAction::List => commands::skill::list(&ctx).await?,
            SkillAction::Show { slug } => commands::skill::show(&ctx, &slug).await?,
            SkillAction::Scan { json } => commands::skill::scan(&ctx, json)?,
            SkillAction::Sync {
                dry_run,
                pull,
                push,
            } => commands::skill::sync(&ctx, dry_run, pull, push).await?,
            SkillAction::Status { json } => commands::skill::status(&ctx, json)?,
            SkillAction::Doctor => commands::skill::doctor(&ctx)?,
        },
        Command::Mcp(args) => match args.action {
            McpAction::Scan { json } => commands::mcp::scan(&ctx, json)?,
            McpAction::Sync {
                dry_run,
                pull,
                push,
            } => commands::mcp::sync(&ctx, dry_run, pull, push)?,
            McpAction::Status { json } => commands::mcp::status(&ctx, json)?,
            McpAction::Doctor => commands::mcp::doctor(&ctx)?,
            McpAction::Remove { name } => commands::mcp::remove(&ctx, &name)?,
            McpAction::AcceptTheirs { name, from } => {
                commands::mcp::accept_theirs(&ctx, &name, from.as_deref())?
            }
        },
    }
    Ok(())
}
