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
    Command, HandoffAction, LearnAction, LearningsAction, McpAction, MemoryAction,
    ObservationsAction, ProposalsAction, RulesAction, SkillAction, SoulAction, WikiAction,
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

    // The updater runs only on user-facing entrypoints — never on hook or
    // mcp-stdio (harness event flows must stay fast) and never on
    // self-update itself.
    let update_allowed = !matches!(
        &cli.command,
        cli::Command::Hook(_)
            | cli::Command::McpStdio
            | cli::Command::SelfUpdate { .. }
            | cli::Command::Uninstall { .. }
    );

    match cli.command {
        Command::Init(args) => commands::init::run(&ctx, args).await?,
        Command::Remove(args) => commands::remove::run(&ctx, args.yes, args.dry_run).await?,
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
        Command::Resume(args) => {
            commands::resume::run(
                &ctx,
                args.harness.as_deref(),
                args.no_accept,
                args.force,
                args.deterministic,
            )
            .await?
        }
        Command::Checkpoint(args) => commands::checkpoint::run(&ctx, &args.note, &args.files)?,
        Command::Handoff(args) => match args.action {
            HandoffAction::Write(args) => {
                let flags = commands::handoff::HandoffWriteFlags {
                    objective: args.objective.as_deref(),
                    task: args.task.as_deref(),
                    context_summary: args.context_summary.as_deref(),
                    next: &args.next,
                    decisions: &args.decision,
                    failures: &args.failure,
                };
                commands::handoff::write(
                    &ctx,
                    args.from.as_deref(),
                    args.to.as_deref(),
                    args.note.as_deref(),
                    args.input.as_deref(),
                    &flags,
                )
                .await?
            }
            HandoffAction::List => commands::handoff::list(&ctx).await?,
            HandoffAction::Show { seq } => commands::handoff::show(&ctx, seq).await?,
            HandoffAction::Accept { by } => commands::handoff::accept(&ctx, &by).await?,
            HandoffAction::Finalize { from } => {
                commands::handoff::finalize(&ctx, from.as_deref()).await?
            }
        },
        Command::Snap(args) => {
            commands::roots::snap(&ctx, args.reason.as_deref(), args.harness.as_deref())?
        }
        Command::Log => commands::roots::log(&ctx)?,
        Command::Show { hash } => commands::roots::show(&ctx, &hash)?,
        Command::Diff(args) => commands::roots::diff(&ctx, &args.from, &args.to, args.content)?,
        Command::Compare(args) => commands::roots::compare(&ctx, &args.a, &args.b)?,
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
        Command::Uninstall {
            purge,
            yes,
            msi_cleanup,
        } => commands::uninstall::run(&ctx, purge, yes, msi_cleanup)?,
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
            LearningsAction::List {
                user,
                workspace,
                domain,
                status,
            } => commands::learnings::list(
                &ctx,
                user,
                workspace,
                domain.as_deref(),
                status.as_deref(),
            )?,
            LearningsAction::Accept {
                id,
                user,
                workspace,
                domain,
            } => commands::learnings::accept(&ctx, &id, user, workspace, domain.as_deref())?,
            LearningsAction::Reject {
                id,
                user,
                workspace,
                domain,
            } => commands::learnings::reject(&ctx, &id, user, workspace, domain.as_deref())?,
            LearningsAction::Edit {
                id,
                statement,
                user,
                workspace,
                domain,
            } => commands::learnings::edit(
                &ctx,
                &id,
                &statement,
                user,
                workspace,
                domain.as_deref(),
            )?,
            LearningsAction::Distill => commands::learnings::distill(&ctx)?,
        },
        Command::Learn(args) => match args.action {
            LearnAction::Record {
                note,
                user,
                workspace,
                domain,
            } => commands::learn::record(&ctx, &note, user, workspace, domain.as_deref())?,
        },
        Command::Synthesize { force } => commands::synthesize::run(&ctx, force).await?,
        Command::Memory(args) => match args.action {
            MemoryAction::Add {
                content,
                target,
                private,
            } => commands::memory::add(&ctx, &target, &content, private)?,
            MemoryAction::Replace {
                content,
                old,
                target,
                private,
            } => commands::memory::replace(&ctx, &target, &old, &content, private)?,
            MemoryAction::Remove { old, target } => commands::memory::remove(&ctx, &target, &old)?,
            MemoryAction::Show { target } => commands::memory::show(&ctx, &target)?,
            MemoryAction::Recall { query, limit } => commands::memory::recall(&ctx, &query, limit)?,
        },
        Command::Observations(args) => match args.action {
            ObservationsAction::List {
                kind,
                harness,
                since,
                until,
                limit,
            } => commands::observations::list(
                &ctx,
                kind.as_deref(),
                harness.as_deref(),
                since.as_deref(),
                until.as_deref(),
                limit,
            )?,
            ObservationsAction::Show { id } => commands::observations::show(&ctx, &id)?,
            ObservationsAction::Search {
                query,
                kind,
                harness,
                limit,
            } => commands::observations::search(
                &ctx,
                &query,
                kind.as_deref(),
                harness.as_deref(),
                limit,
            )?,
        },
        Command::Transplant(args) => commands::transplant::run(
            &ctx,
            &args.from,
            &args.to,
            args.dry_run,
            args.confirm,
            args.harness.as_deref(),
            args.reason.as_deref(),
        )?,
        Command::Wiki(args) => match args.action {
            WikiAction::Show { path } => commands::wiki::show(&ctx, &path)?,
            WikiAction::Lint => commands::wiki::lint(&ctx)?,
            WikiAction::Compile { force } => commands::wiki::compile(&ctx, force).await?,
        },
        Command::McpStdio => commands::mcp_stdio::run(&ctx).await?,
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
            SkillAction::Promote { slug, rationale } => {
                commands::skill::promote(&ctx, &slug, rationale.as_deref()).await?
            }
            SkillAction::Doctor => commands::skill::doctor(&ctx)?,
        },
        Command::Rules(args) => match args.action {
            RulesAction::List => commands::rules::list(&ctx)?,
            RulesAction::Show { slug } => commands::rules::show(&ctx, &slug)?,
            RulesAction::Sync => commands::rules::sync(&ctx)?,
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
            McpAction::Tools => commands::mcp::tools(&ctx)?,
            McpAction::Remove { name } => commands::mcp::remove(&ctx, &name)?,
            McpAction::AcceptTheirs { name, from } => {
                commands::mcp::accept_theirs(&ctx, &name, from.as_deref())?
            }
        },
        Command::SelfUpdate { check } => commands::update::self_update(&ctx, check).await?,
    }
    if update_allowed {
        commands::update::maybe_auto_update(&ctx).await;
    }
    Ok(())
}
