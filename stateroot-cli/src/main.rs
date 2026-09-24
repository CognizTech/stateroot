//! `stateroot` — the local-first StateRoot binary.
//!
//! Every command runs offline against the local `.stateroot/` store, the
//! harnesses on the machine, and the lifted federation engines. There is no
//! server anywhere in this variant.

mod cli;
mod commands;
mod telemetry;
#[cfg(test)]
mod test_env;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use cli::{
    Command, EditorAction, ExtAction, HandoffAction, HarnessAction, LearnAction, LearningsAction,
    McpAction, MemoryAction, ObservationsAction, PlanAction, ProposalsAction, RulesAction,
    SessionAction, SkillAction, SoulAction, TodoAction, WikiAction,
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

    let cli = match cli::Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            // `--version` / `--help` short-circuit here before any command
            // dispatch — and for a fresh manual install `--version` is often
            // the first (and only) command run, so the install acquisition
            // event must be spooled on this path too or those installs never
            // count. The detached drain delivers it (durable, retried).
            if matches!(
                err.kind(),
                clap::error::ErrorKind::DisplayVersion | clap::error::ErrorKind::DisplayHelp
            ) {
                if let Ok(ctx) = Ctx::load() {
                    telemetry::observe_install(&ctx.config_dir, cli::BUILD_VERSION);
                    telemetry::kick_drain(&ctx);
                }
            }
            err.exit();
        }
    };
    let ctx = Ctx::load()?;

    // Anonymous acquisition telemetry: one `install_observed` spooled per
    // version change per machine — local-only append, detached drain kicks
    // after the command completes (never on hooks). Opt out with
    // STATEROOT_NO_PING=1; dev builds never emit.
    telemetry::observe_install(&ctx.config_dir, cli::BUILD_VERSION);

    // The updater runs only on user-facing entrypoints — never on hook or
    // mcp-stdio (harness event flows must stay fast) and never on
    // self-update itself.
    let update_allowed = !matches!(
        &cli.command,
        cli::Command::Hook(_)
            | cli::Command::McpStdio
            | cli::Command::SelfUpdate { .. }
            | cli::Command::Uninstall { .. }
            | cli::Command::Editor(_)
            | cli::Command::External(_)
            | cli::Command::DrainFinalize
            | cli::Command::DrainTelemetry
            | cli::Command::TelemetryIdentity { .. }
    );

    match cli.command {
        Command::Init(args) => commands::init::run(&ctx, args).await?,
        Command::Remove(args) => {
            commands::remove::run(&ctx, args.yes, args.dry_run, args.full).await?
        }
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
        Command::Session(args) => match args.action {
            SessionAction::Sync { harness } => commands::session::sync(&ctx, harness.as_deref())?,
            SessionAction::List { harness } => commands::session::list(&ctx, harness.as_deref())?,
            SessionAction::Show { id } => commands::session::show(&ctx, &id)?,
            SessionAction::Transfer { id, to, dry_run } => {
                commands::session::transfer(&ctx, &id, &to, dry_run)?
            }
            SessionAction::Purge { id, harness, yes } => {
                commands::session::purge(&ctx, &id, harness.as_deref(), yes)?
            }
        },
        Command::Plan(args) => match args.action {
            PlanAction::Record {
                file,
                stdin,
                title,
                from,
            } => commands::plan::record(
                &ctx,
                file.as_deref(),
                stdin,
                title.as_deref(),
                from.as_deref(),
            )?,
            PlanAction::List => commands::plan::list(&ctx)?,
            PlanAction::Show { id } => commands::plan::show(&ctx, &id)?,
            PlanAction::Approve { id } => commands::plan::approve(&ctx, &id)?,
            PlanAction::Activate { id } => commands::plan::activate(&ctx, &id)?,
            PlanAction::Done { id } => commands::plan::done(&ctx, &id)?,
            PlanAction::Abandon { id } => commands::plan::abandon(&ctx, &id)?,
            PlanAction::Sync => commands::plan::sync(&ctx)?,
        },
        Command::Todo(args) => match args.action {
            TodoAction::List { harness } => commands::todo::list(&ctx, harness.as_deref())?,
        },
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
                    worktree: args.worktree.as_deref(),
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
            HandoffAction::Repair => commands::handoff::repair(&ctx).await?,
        },
        Command::Snap(args) => {
            commands::roots::snap(&ctx, args.reason.as_deref(), args.harness.as_deref())?
        }
        Command::Log(args) => commands::roots::log(&ctx, args.json)?,
        Command::Show { hash } => commands::roots::show(&ctx, &hash)?,
        Command::Diff(args) => commands::roots::diff(&ctx, &args.from, &args.to, args.content)?,
        Command::Compare(args) => commands::roots::compare(&ctx, &args.a, &args.b)?,
        Command::Revert(args) => commands::roots::revert(&ctx, &args.root, args.yes)?,
        Command::Fork(args) => commands::roots::fork(
            &ctx,
            &args.root,
            args.name.as_deref().or(args.branch.as_deref()),
            args.worktree.as_deref(),
            args.plan.as_deref(),
        )?,
        Command::Merge(args) => commands::roots::merge(
            &ctx,
            &args.forks,
            args.json,
            &args.resolve_ours,
            args.cleanup,
            args.prepare,
            args.continue_attempt.as_deref(),
            args.status.as_deref(),
            args.abort.as_deref(),
            &args.evidence,
        )?,
        Command::Receipt { id } => commands::roots::receipt(&ctx, &id)?,
        Command::Status => commands::status::run(&ctx)?,
        Command::Projects { json, prune } => commands::projects::run(&ctx, json, prune)?,
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
        Command::Editor(args) => match args.action {
            EditorAction::Status => commands::editor_extensions::status(&ctx).await?,
            EditorAction::Reconcile => {
                let code = commands::editor_extensions::reconcile(&ctx).await?;
                if code != 0 {
                    std::process::exit(code);
                }
            }
        },
        Command::Harness(args) => match args.action {
            HarnessAction::Run {
                harness,
                objective,
                skills,
                ambient_skills,
                dry_run,
            } => commands::harness::run(
                &ctx,
                &harness,
                objective.as_deref(),
                &skills,
                ambient_skills,
                dry_run,
            )?,
        },
        Command::Uninstall {
            purge,
            yes,
            msi_cleanup,
        } => commands::uninstall::run(&ctx, purge, yes, msi_cleanup)?,
        Command::Delegate(args) => {
            let code = commands::delegate::run(&ctx, &args)?;
            if code != 0 {
                std::process::exit(code);
            }
        }
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
            SoulAction::Sync {
                dry_run,
                accept_theirs,
                accept_mine,
            } => commands::soul::sync(
                &ctx,
                dry_run,
                accept_theirs.as_deref(),
                accept_mine.as_deref(),
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
            MemoryAction::Compact {
                target,
                dry_run,
                synthesis,
                to,
            } => commands::memory::compact(&ctx, &target, dry_run, synthesis, to).await?,
            MemoryAction::Sync {
                harness,
                dry_run,
                push,
            } => commands::memory::sync(&ctx, harness.as_deref(), dry_run, push)?,
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
        Command::Ext(args) => match args.action {
            ExtAction::List => commands::ext::list()?,
        },
        Command::DrainFinalize => commands::drain_finalize::run(&ctx).await?,
        Command::DrainTelemetry => commands::telemetry_drain::run(&ctx).await?,
        Command::TelemetryIdentity { json } => {
            commands::telemetry_drain::print_identity(&ctx, json)?;
        }
        Command::External(argv) => {
            let code = commands::ext::run_external(&ctx, &argv)?;
            std::process::exit(code);
        }
        Command::SelfUpdate {
            check,
            tag,
            schedule,
        } => {
            if let Some(action) = schedule {
                commands::update_schedule::manage(&ctx, action)?;
            } else {
                commands::update::self_update(&ctx, check, tag.as_deref()).await?
            }
        }
    }
    if update_allowed {
        commands::update::maybe_auto_update(&ctx).await;
        // Telemetry: hooks append to the spool only; user-facing entrypoints
        // kick the detached single-flight drain (never blocks, never fails
        // the command).
        telemetry::kick_drain(&ctx);
    }
    Ok(())
}
