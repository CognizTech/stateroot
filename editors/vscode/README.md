# StateRoot

**The harness is disposable. The work is not.**

StateRoot connects the AI agents you already use so they share knowledge, tools and what they learn across sessions. Pick up where another agent stopped, ask one to plan and others to work in parallel, then combine their results or restore an earlier saved version of your project.

*Persistent, federated meta-harness for AI agents.*

## You direct the work. Your agents handle StateRoot.

After setup, use your normal agent conversations. Ask an agent to continue work,
record a plan, remember a preference or give independent tasks to other agents.
Your agents follow their installed StateRoot Skill to operate the CLI. Each agent keeps its own model, interface and strengths.

This extension gives you a view of that work in **VS Code and Cursor**. See plans,
agent activity, shared knowledge and project history without memorizing the
agent's commands.

> Give Kimi and Cursor separate StateRoot forks for these independent tasks,
> then review and combine their results.

> Show me what changed, what was checked and what still needs attention.

These are ordinary requests, not special prompt syntax. Supported execution routes
depend on the connected agent and its installed CLI.

<p align="center">
  <img alt="Agents carrying shared knowledge and project work across tools" src="https://raw.githubusercontent.com/CognizTech/stateroot/main/demo.gif">
</p>

## Get started

1. Install the extension in Cursor or VS Code and follow its guided CLI and project setup.
2. Open your project in a supported agent and tell it what you want done.
3. Open **StateRoot: Open workbench** from the command palette when you want to inspect progress.

[Quickstart](https://stateroot.dev/docs/getting-started/quickstart) ·
[Working with your agents](https://stateroot.dev/docs/guides/daily-workflow)

## Continue. Delegate. Fork. Merge.

- **Continue:** pick up where another agent stopped, or return later with project knowledge still available.
- **Delegate:** give a specific task to another supported agent and bring its result back for review.
- **Fork:** use separate project copies for independent tasks while sharing useful knowledge.
- **Merge:** ask your coordinator to review, check and combine the accepted work while preserving its history.

## What carries forward

- **Personality and preferences:** how you want agents to address you, communicate and approach the work.
- **Project knowledge:** goals, plans, decisions, rules, facts and earlier approaches, with searchable history and a project wiki.
- **Skills and tools:** reusable Skill packages and compatible MCP configuration, plus native capabilities available through delegation.
- **Shared self-improvement:** useful corrections recorded as guidance for later agent sessions. Teach one agent and carry the lesson forward.
- **Saved project versions:** compare changes, try alternatives and restore earlier versions while preserving your normal Git branches.

The CLI also supports [native session transfer and qualified recovery](https://stateroot.dev/docs/features/sessions),
[portable project exchange](https://stateroot.dev/docs/guides/sharing-a-project#portable-local-exchange)
and [commands agents can extend](https://stateroot.dev/docs/features/extensions).
Ask your agent to use these capabilities when the task calls for them.

## What the extension adds

- **Activity bar glance** — the lamp in your sidebar: what's Now, what Needs
  You, the current root, and an Initialize action on empty workspaces.
- **Workbench** (`StateRoot: Open workbench`) — the full board:
  - **Control** — inbox and delegation state, handoffs waiting on you.
  - **Plans** — review prerequisites, assigned harness, readiness and blockers, approve, assign a
    harness, or delegate a plan into another agent.
  - **Todos** — federated todo lists from every harness, plan-bound and
    standalone.
  - **Crew** — see parallel-ready and blocked plans from the same CLI graph,
    reassign failed delegations, and inspect logs.
  - **Work** — distinguish prepared merges, reconciliation and staged candidates,
    with the next CLI commands for the coordinating agent. A staged candidate
    does not by itself prove that verification passed.
  - **Learnings** — the shared taste pool: accept, edit, reject.
  - **Memory** — the curated hot-apex of facts.
  - **Intelligence** — inspect discovered Skills and rules, canonical MCP
    servers and local tools, plus named harness health and repair guidance.
    Identity, origin, compatibility and native route come from the CLI;
    unobserved loading and use remain labeled unknown.
  - **Lineage** — browse 50 saved roots per page, compare changes, inspect a native diff, restore or fork the work. Comparison selections stay available across pages.

Writes go through the `stateroot` CLI only — the extension never edits
project state directly.

## Requirements

- The **StateRoot CLI** on the machine. On supported platforms, the extension
  installs it automatically using its bundled installer and the latest stable
  release binary. The installer checks the download's SHA-256 before installation.
  - macOS (Apple Silicon) and Linux x64: `curl -sSfL https://github.com/CognizTech/stateroot/releases/latest/download/install.sh | sh`
  - Windows: `irm https://github.com/CognizTech/stateroot/releases/latest/download/install.ps1 | iex` (or the `StateRootSetup-x64.msi` GUI installer)
  - Intel Macs: build from source; automatic binary installation is not supported.
- Open a new terminal after the first install. The extension adds the resolved
  CLI directory to new terminals. The POSIX installer also configures the shell
  profile, including a fresh Mac with no `.zshrc` yet.
- On macOS, HTTPS downloads use the system's static HTTPS proxy and bypass list
  when no proxy environment variable is configured. Explicit `HTTPS_PROXY`,
  `https_proxy`, or `ALL_PROXY` settings take precedence. PAC-only configurations
  require an explicit proxy environment variable.
- For a CLI-only POSIX installation, set `STATEROOT_SKIP_INTEGRATION=1` when
  running `install.sh`; run `stateroot install` later to configure global hooks
  and persona. This flag does not disable download verification.
- Run `stateroot init` in a project — the extension lights up on any
  workspace that has one.

## Links

- [Website](https://stateroot.dev) · [Human guide](https://stateroot.dev/docs/guides/daily-workflow) · [Docs](https://stateroot.dev/docs/intro) ·
  [GitHub](https://github.com/CognizTech/stateroot) ·
  [Releases](https://github.com/CognizTech/stateroot/releases)

Persistent, federated meta-harness for AI agents — local-first, Apache-2.0.
