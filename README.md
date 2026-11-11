# StateRoot

[English](README.md) | [简体中文](README.zh-CN.md)

**Persistent, federated meta-harness for AI agents.**

**The harness is disposable. The work is not.**

StateRoot federates Claude Code, Codex, Cursor, Kimi, CodeBuddy, OpenClaw, Hermes, Pi, DeepSeek Harness and other agent harnesses through shared working intelligence and versioned project state. Continue work in another harness. Hand a plan to a different agent. Delegate bounded tasks across tools. Run agents in parallel on independent StateRoot forks. Merge the results when the work converges.

Memory, personality, rules, skills, tools, plans and hard-won learnings persist across the harnesses using them. Each harness keeps its own runtime, model and strengths.

**Shared intelligence. Independent execution.**

<p align="center">
  <img alt="Close one agent, open another — it just knows" src="demo.gif">
</p>

<p align="center">
  <a href="https://open-vsx.org/extension/CognizTech/stateroot"><img src="https://img.shields.io/open-vsx/v/CognizTech/stateroot?color=7ee0c8&labelColor=0c1016&logo=cursor&style=for-the-badge&label=Cursor" alt="Install for Cursor"></a>
  <a href="https://marketplace.visualstudio.com/items?itemName=CognizTech.stateroot"><img src="https://img.shields.io/visual-studio-marketplace/v/CognizTech.stateroot?color=7ee0c8&labelColor=0c1016&logo=visualstudiocode&style=for-the-badge" alt="VS Code Marketplace"></a>
  <a href="https://github.com/CognizTech/stateroot/releases"><img src="https://img.shields.io/github/v/release/CognizTech/stateroot?color=7ee0c8&labelColor=0c1016&logo=github&style=for-the-badge" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-7ee0c8?labelColor=0c1016&style=for-the-badge" alt="License"></a>
</p>
<p align="center">
  <a href="https://stateroot.dev"><img src="https://img.shields.io/badge/Website-stateroot.dev-7ee0c8?style=for-the-badge&labelColor=0c1016" alt="Website"></a>
  <a href="https://stateroot.dev/docs/intro"><img src="https://img.shields.io/badge/Docs-7ee0c8?style=for-the-badge&labelColor=0c1016" alt="Docs"></a>
  <a href="https://stateroot.dev/docs/getting-started/quickstart"><img src="https://img.shields.io/badge/Quickstart-7ee0c8?style=for-the-badge&labelColor=0c1016" alt="Quickstart"></a>
  <a href="https://stateroot.dev/docs/reference/cli"><img src="https://img.shields.io/badge/CLI-7ee0c8?style=for-the-badge&labelColor=0c1016" alt="CLI reference"></a>
  <a href="https://discord.gg/wrjgpnuky"><img src="https://img.shields.io/badge/Discord-Join%20community-5865F2?style=for-the-badge&logo=discord&logoColor=white&labelColor=0c1016" alt="Join the StateRoot Discord community"></a>
  <a href="https://qm.qq.com/q/ZFoqWSP9yo"><img src="https://img.shields.io/badge/QQ-Join%20group-12B7F5?style=for-the-badge&logo=qq&logoColor=white&labelColor=0c1016" alt="Join the StateRoot QQ group"></a>
</p>

For everyday use, start with the [Quickstart](https://stateroot.dev/docs/getting-started/quickstart) and [Working with your agents](https://stateroot.dev/docs/guides/daily-workflow). The [built-in Skill](stateroot-cli/assets/stateroot-skill/SKILL.md) carries the agent's operating instructions.

## Why StateRoot

Every model wants its own harness — Claude works best in Claude Code, GPT in Codex, DeepSeek in its own. And every harness keeps its own context: its own transcripts, rules, skills, and plans. So the everyday moments of modern AI work — a usage limit hit mid-task, a better model launching in a rival tool, an expensive model you'd rather only plan with — all carry the same hidden tax: re-explaining the project, re-reading the codebase, re-teaching how you work.

StateRoot is the persistent, federated meta-harness above those runtimes: a shared layer through which agents inherit common working intelligence while operating on versioned project states that can continue, diverge, and later converge — while each model keeps its own native runtime. One local CLI, everything on your machine.

It supports two ways of working:

- **Sequential continuity** — close Claude Code, open Codex, keep working. The next agent starts already knowing the goal, the plan, the decisions, and how you work — and every lesson one agent learns becomes a rule for all of them.
- **Parallel collaboration** — several harnesses work at the same time on independent StateRoot forks, sharing what they know while their execution stays separate. When the work converges, `stateroot merge` folds it back into one continuing project.

Memory tools preserve what your agents *know*. StateRoot also preserves what the work *is* — every meaningful state of the project, immutable and restorable, with a provable lineage of how it changed.

## Continue. Delegate. Fork. Merge.

**Continue.** One harness stops; another continues from the accumulated state. Hooks inject a bounded digest (goal, plan, decisions, memories, next actions) at session start — no pasting transcripts. Plans cross too: a strong model authors the plan in its plan mode, a cheaper model executes it, and the executor's digest says *"execute this plan; do not re-plan."* Sessions themselves canonicalize into one store and transfer into Pi / DeepSeek Harness as real, resumable native sessions.

```text
Claude Code ──→ State A ──→ State B
                             │
                             ├── continue with Codex
                             ├── branch with Cursor
                             └── restore an earlier state
```

**Delegate.** Ask your agent to give a bounded task to another supported harness CLI with full project context. Delegation is depth-capped and recorded in lineage. The parent gets the conclusion, not the transcript.

**Fork.** Ask for an independent StateRoot fork when work should run in parallel. Each materialized fork has an isolated checkout in the managed local area and an internal lineage ref. Agents work in parallel on independent states of the same project — shared intelligence, independent execution. Park retained work and reopen the same fork when needed. Your Git branches are never touched.

**Merge.** Ask your coordinating agent to review and merge the completed forks. StateRoot folds their lineages back into the trunk: each fork is 3-way-merged into the accumulated union and committed as one root with the trunk and every fork tip as parents. Conflicts are reported per path; contained forks report nothing-to-merge. The work converges with its lineage intact.

```text
                    one project, three forks
                            │
             ┌──────────────┼──────────────┐
             │              │              │
           Codex          Kimi          Cursor
             │              │              │
          fork A         fork B         fork C
             │              │              │
             └──────────────┼──────────────┘
                            │
                     stateroot merge
                            │
                  one continuing project
```

Switch harnesses, change models, retire tools — the project, the persona, and every lesson it ever learned carry on.

### Continuity that stays active

StateRoot surfaces due work and unfinished handoffs in `## Needs Attention` when an agent starts, in `stateroot status`, and in the editor extension. Durable `stateroot obligation` items carry follow-ups across harnesses; the per-user `stateroot service` reconciles them in the background. Local continuity is deterministic and runs without an AI agent of its own.

Agents close plans with an explicit completion receipt and the checks actually performed. Finished todos alone do not close a plan; StateRoot keeps a completion obligation visible until the result is recorded. Ask your agent about unresolved items, or inspect them in the workbench.

## Native harnesses stay native

StateRoot does not replace Claude Code, Codex, Cursor, Kimi, CodeBuddy or any other tool with a proprietary runtime. Each harness keeps its own model, interface, capabilities and strengths; StateRoot is the persistent layer through which they share intelligence and work state. Choose the agent for the job: Codex authors the plan, Kimi implements one task, Cursor implements another on its own fork, Codex reviews and integrates. The agents remain distinct; the project does not reset when the worker changes.

OpenClaw, Hermes and other general agent harnesses can join the same persistent layer alongside Claude Code, Codex, Cursor, Kimi, CodeBuddy and Pi.

### What crosses the boundary

One project, every harness — no new runtime, no required cloud, no lock-in:

| Travels with the work | How |
| --- | --- |
| Goal, state & next actions | one state of record every harness reads |
| Plans with an approval lifecycle | `stateroot plan` |
| Memory & facts | curated, provenance-labeled, searchable |
| Rules & preferences | shared pool, recorded once, seen everywhere |
| Real, resumable sessions | canon across harnesses + transfer |
| Subagents in *other* harnesses | `stateroot delegate` |
| State lineage (fork / merge / restore) | Git plumbing, your branches untouched |
| Personality | full persona + USER.md, never trimmed |

## What it shares

| | |
| --- | --- |
| **Project state** | Objective, phase, handoffs, next actions — one place every harness reads. |
| **Plans** | A plan store with a lifecycle (draft → approved → active → done) and provenance. |
| **Skills and tools** | SKILL.md packages and MCP servers sync across agent configs. Conflicts are left alone. |
| **Sessions** | Full-fidelity canonical session store across harnesses; transfer into Pi / DeepSeek Harness. |
| **Subagents** | Delegate bounded tasks into other harness CLIs — depth-capped, bounded result, lineage recorded. |
| **Portable state** | Export a retained root and its shared intelligence/evidence, inspect a bundle, import without moving the head, or materialize a compatible checkout. Local v2 exchange works offline. |
| **Extensions** | Any `stateroot-<name>` executable on PATH becomes a subcommand — agents can extend the CLI itself. |

### One personality across every agent

Soul + USER.md — your agent's name, character, voice, and boundaries, plus who you are and how you work — are injected in full at every session start, never truncated to fit a token budget. StateRoot projects the same persona, USER.md, communication preferences and working identity into every harness. The agents remain distinct — Codex stays Codex, Claude Code stays Claude Code — but the working relationship does not reset when the harness changes. You never re-introduce yourself.

### Memory, in three layers

- **Hot apex (`MEMORY.md`)** — the curated few hundred lines every session sees: the project's current facts, decisions, and hard-won context, scoped to project or user.
- **Compiled wiki** — long-form knowledge distilled from evidence over time: pages, an index, and a log, compiled deterministically with optional LLM synthesis behind your own keys.
- **Episodic log + full-text recall** — every checkpoint and observation, append-only and locally searchable (`stateroot memory recall`), so anything the project ever learned is one query away.

Every fact carries provenance — **verified** (Git), **observed** (transcripts), or **synthesized** (LLM) — and empty stays empty.

### Learnings: taste that compounds

Learnings are judgment, not facts: `prefer X over Y`, `never Z`, each with *when it applies*. Record one — yourself, or any agent on your behalf — and it activates immediately for every harness: no approval queue, no classifier. Scoped to project, user, workspace, or domain; superseded over time, never silently lost. A correction made in one harness becomes a rule for all of them — this is how the team of agents gets smarter together instead of repeating the same mistake in six different tools.

## What it snapshots

Git versions the commits you make. StateRoot snapshots the working tree during agent work, stored with Git *plumbing* under `refs/stateroot`. Your branches are never rewritten.

Every snapshot is a complete, immutable, content-addressed state: restore it exactly, fork it safely, compare any two states honestly, and read the receipt of what changed between them.

Current `.gitignore` and `.staterootignore` exclusions still apply when materializing an older state. `stateroot doctor` reports the current root, metadata compatibility, capture evidence, and restoration limits separately from general setup health. Ask your agent to show the root comparison and the checks supporting its completion claim. Optional command observations bind actual results to the plan and eligible project files; a recorded command result does not establish that every acceptance criterion passed. See the [CLI guide](docs/cli.md#optional-plan-command-observations-and-state-compatibility) for the exact scope.

Restores and digests say where information came from: **verified** (Git), **observed** (transcripts), or **synthesized** (LLM). Empty stays empty.

Full map: **[stateroot.dev/docs](https://stateroot.dev/docs/intro)**.

## Install

Install the StateRoot extension for [**Cursor**](https://open-vsx.org/extension/CognizTech/stateroot) or [**VS Code**](https://marketplace.visualstudio.com/items?itemName=CognizTech.stateroot) for guided setup and automatic CLI installation on supported platforms.

The CLI ships for **Linux x64 and ARM64**, **Windows x64**, and **macOS Apple Silicon**. One binary, no extra runtime.

### Linux

```bash
curl -sSfL https://github.com/CognizTech/stateroot/releases/latest/download/install.sh | sh
```

Installs to `~/.local/bin`. Put that directory on your `PATH`. The binary needs glibc 2.17 or newer (Ubuntu 16.04, Debian 9, RHEL 7, and later).

### Windows

Download [**StateRootSetup-x64.msi**](https://github.com/CognizTech/stateroot/releases/latest/download/StateRootSetup-x64.msi) from [Releases](https://github.com/CognizTech/stateroot/releases), or:

```powershell
irm https://github.com/CognizTech/stateroot/releases/latest/download/install.ps1 | iex
```

`stateroot-windows-x64.exe` is the portable CLI, not an installer.

### macOS (Apple Silicon)

```bash
curl -sSfL https://github.com/CognizTech/stateroot/releases/latest/download/install.sh | sh
```

Installs to `~/.local/bin`. Intel Macs can [build from source](https://stateroot.dev/docs/getting-started/installation#from-source), or straight from this repo: clone it, then `cargo build --release -p stateroot-cli` (Rust 1.85+; libgit2 is vendored) — the binary lands at `target/release/stateroot`.

```bash
stateroot --version
stateroot doctor     # passes with zero config and zero keys
```

## Quickstart

Zero config, zero keys, one binary. `stateroot doctor` passes out of the box.

```bash
cd my-project
stateroot init
stateroot setup      # once per machine: identity, harnesses, skills
```

Open your usual agent in this project and describe the work you want done. StateRoot's installed Skill tells the agent how to retain progress and use shared project state; supported session hooks supply the continuation brief automatically.

For your first task, try:

> Use StateRoot for this project. Tell me the current goal and next step, then help me finish the task.

Before switching tools, tell that agent:

> Prepare a StateRoot handoff with what changed, what was checked and what remains. I will continue in Codex.

Then open the same project in the next supported agent and ask:

> Continue from the latest StateRoot handoff. Explain where the work stands before proceeding.

You direct the work; the agents handle checkpoints, shared plans and handoffs. If the new agent lacks context, ask it to load the StateRoot Skill and diagnose the integration. Native hook support varies by harness and interface; the Skill supplies the manual resume procedure where needed.

### Work through your agents

| What you want | What to ask your agent |
| --- | --- |
| Plan now, implement elsewhere | “Record a shared StateRoot plan for this task. I will review it before another agent implements it.” |
| Delegate a bounded task | “Delegate this review to Codex through StateRoot and bring back its findings.” |
| Work in parallel | “Give these independent tasks separate StateRoot forks. Track each agent's progress.” |
| Bring the results together | “Review the completed forks, resolve conflicts, run the relevant checks and merge the accepted work.” |
| Recover an earlier state | “Show what changed since the last good root and restore that state.” |
| Carry a correction forward | “Remember this preference in StateRoot so the other agents follow it too.” |

These are example requests, not special prompt syntax. Cross-harness execution requires the destination's supported CLI and account access. Your agent should report the actual handoff, plan, fork or verification result, including anything blocked or unfinished.

The [human guide](https://stateroot.dev/docs/guides/daily-workflow) explains these workflows and how to recognize success. Agents follow the [built-in Skill](stateroot-cli/assets/stateroot-skill/SKILL.md).

### Inspect the work yourself

The editor workbench shows plans, agents, intelligence and lineage. If you prefer a terminal, these manual checks are useful:

```bash
stateroot status     # current work and items needing attention
stateroot log        # retained project history
stateroot doctor     # setup health and restoration diagnostics
```

Start with the [Quickstart](https://stateroot.dev/docs/getting-started/quickstart). The complete [CLI reference](https://stateroot.dev/docs/reference/cli) remains available for agents, integrations and direct use.

### Add or refresh coding-agent integrations

When StateRoot adds support for another coding agent, or you install a supported agent later, install that agent's CLI first, update StateRoot, then rerun the harness section of setup:

```bash
stateroot self-update
stateroot setup --only harnesses
stateroot doctor
```

If setup says the harnesses are already configured, choose **Yes** to reconfigure. Keep the agents you use selected, select the newly detected agent, and confirm. This refreshes the selected integrations without rerunning identity setup or reinitializing your projects. Restart the coding agent to load its updated configuration.

For an integration marked **nightly only** below, use this update command instead of the first command above, then run the same setup and doctor commands:

```bash
stateroot self-update --tag nightly
```

Nightly builds are available from the [rolling preview release](https://github.com/CognizTech/stateroot/releases/tag/nightly); a nightly-only integration is not yet included in the latest stable release. To refresh detected integrations without the setup wizard, run `stateroot install`.

## Supported harnesses

**Hooks + transcripts:** Claude Code · Codex · Cursor · Kimi Code · CodeBuddy · OpenClaw · Hermes · Pi

**Transcripts / delegation:** DeepSeek Harness · OpenCode · others detected at install

Per-harness notes: [Harnesses](https://stateroot.dev/docs/harnesses/overview).

## Documentation

User docs live at **[stateroot.dev](https://stateroot.dev)**. This repository is the CLI.

| Section | Contents |
| --- | --- |
| [Install](https://stateroot.dev/docs/getting-started/installation) | Cursor, VS Code, Linux, Windows, macOS, from source |
| [Quickstart](https://stateroot.dev/docs/getting-started/quickstart) | Setup → first task → continue in another agent |
| [Human guide](https://stateroot.dev/docs/guides/daily-workflow) | Direct agents, review results, recover and continue |
| [Concepts](https://stateroot.dev/docs/concepts/overview) | Roots, continuity, identity, memory, provenance |
| [Capability details](https://stateroot.dev/docs/features/roots) | Technical behavior, limits and command examples |
| [Harnesses](https://stateroot.dev/docs/harnesses/overview) | Per-tool install and protocol |
| [Agent Skill](stateroot-cli/assets/stateroot-skill/SKILL.md) | Installed operating instructions for agents |
| [CLI reference](https://stateroot.dev/docs/reference/cli) | Complete command surface for agents and direct use |
| [MCP tools](https://stateroot.dev/docs/reference/mcp-tools) | Local stdio server |

Machine-readable index: [stateroot.dev/llms.txt](https://stateroot.dev/llms.txt).

## Privacy

Project data stays in the repo (`.stateroot/`). Persona and USER.md live in `~/.stateroot/`. Search stays in `.stateroot/local/` and is never included in snapshots.

Snapshots honor root `.gitignore` and `.staterootignore`, plus `.git/` and `local/`. Optional LLM synthesis runs only with `DEEPSEEK_API_KEY` (preferred, `deepseek-v4-flash`) or `OPENAI_API_KEY` (`gpt-5.6-luna`).

Details: [Privacy](https://stateroot.dev/docs/guides/privacy).

## Sharing a project

Committing `.stateroot/` makes the project's state of record a team asset — and the boundary is deliberate so it merges cleanly:

- **Travels with the repo:** goal and project state, plans, learnings, rules, the skill pool, wiki and memory pages, handoff history, the project soul overlay, transitions. Your persona and USER.md never travel — they live in `~/.stateroot/` by design, so every teammate keeps their own agent's personality.
- **Stays local** (written to `.stateroot/.gitignore` at init): the search index, spool, delegations, sync cursors, the hot-apex `memories/MEMORY.md` and `memories/episodic.jsonl` (per-person lens and private journal — shared truth lives in the wiki and learnings), the current handoff (per-session continuity; history is shared), and `roots/`. Ask your agent to transfer retained lineage separately when teammates need it.
- Append-only journals merge by union via `.stateroot/.gitattributes`. `stateroot doctor` warns when a local-set path is tracked in git.

A teammate's flow: clone the shared project, finish machine setup, then open a supported agent in the checkout. Ask it to inspect the shared goal, plans and learnings before continuing. See [Sharing a project](https://stateroot.dev/docs/guides/sharing-a-project) for repository and portable-exchange options.

## Contributing

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Rust 1.85+. libgit2 is vendored. The CLI-embedded session skill is in [`stateroot-cli/assets/stateroot-skill/`](stateroot-cli/assets/stateroot-skill/). The marketplace install skill is in [`skills/stateroot/`](skills/stateroot/). Docs: [Contributing](https://stateroot.dev/docs/developer-guide/contributing).

Please preserve [product intent](https://stateroot.dev/docs/features/rules): inject full persona/USER.md, warn on thin handoffs instead of refusing, do not auto-categorize learnings, and do not trim identity to save tokens.

## License

Apache-2.0 — see [LICENSE](LICENSE).
