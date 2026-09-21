# StateRoot

**Persistent, federated meta-harness for AI agents.**

**The harness is disposable. The work is not.**

StateRoot federates Claude Code, Codex, Cursor, Kimi, OpenClaw, Hermes, Pi, DeepSeek Harness and other agent harnesses through shared working intelligence and versioned project state. Continue work in another harness. Hand a plan to a different agent. Delegate bounded tasks across tools. Run agents in parallel on independent StateRoot forks. Merge the results when the work converges.

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
</p>

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

**Delegate.** `stateroot delegate --to codex --task "…"` runs a bounded task inside another harness CLI with full project context — depth-capped, lineage recorded. The parent gets the conclusion, not the transcript.

**Fork.** `stateroot fork` branch-materializes any project state into its own worktree under `refs/stateroot/forks/`. Agents work in parallel on independent states of the same project — shared intelligence, independent execution. Your Git branches are never touched.

**Merge.** `stateroot merge` folds fork lineages back into the trunk: each fork is 3-way-merged into the accumulated union and committed as one root with the trunk and every fork tip as parents. Conflicts are reported per path; contained forks report nothing-to-merge. The work converges with its lineage intact.

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

## Native harnesses stay native

StateRoot does not replace Claude Code, Codex, Cursor, Kimi or any other tool with a proprietary runtime. Each harness keeps its own model, interface, capabilities and strengths; StateRoot is the persistent layer through which they share intelligence and work state. Choose the agent for the job: Codex authors the plan, Kimi implements one task, Cursor implements another on its own fork, Codex reviews and integrates. The agents remain distinct; the project does not reset when the worker changes.

OpenClaw, Hermes and other general agent harnesses can join the same persistent layer alongside Claude Code, Codex, Cursor, Kimi and Pi.

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

Restores and digests say where information came from: **verified** (Git), **observed** (transcripts), or **synthesized** (LLM). Empty stays empty.

Full map: **[stateroot.dev/docs](https://stateroot.dev/docs/intro)**.

## Install

Install the StateRoot extension for [**Cursor**](https://open-vsx.org/extension/CognizTech/stateroot) or [**VS Code**](https://marketplace.visualstudio.com/items?itemName=CognizTech.stateroot) for guided setup and automatic CLI installation on supported platforms.

The CLI ships for **Linux x64**, **Windows x64**, and **macOS Apple Silicon**. One binary, no extra runtime.

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

Installs to `~/.local/bin`. Intel Macs can [build from source](https://stateroot.dev/docs/getting-started/installation#from-source).

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

Work in your usual agent. Session hooks inject a digest. You do not need to paste anything.

**Then the aha:** close that agent and open any other supported harness in the same project. It starts the session already knowing the goal, the plan, the decisions, and the next actions — no re-explaining, no re-reading the codebase.

```bash
stateroot status
stateroot log
stateroot resume --harness cursor
stateroot checkpoint --note "wired auth middleware — unblocks handlers" --files src/auth.rs
stateroot handoff write --from cursor \
  --objective "Ship local handoffs" \
  --task "Finish adversarial CLI tests" \
  --context-summary "Structured write path is in; remaining work is workspace checks."
```

If a digest did not appear, run `stateroot resume --harness <id>` with the harness you are actually in (`claude`, `codex`, `cursor`, `kimi`, `openclaw`, `hermes`). Run it unpiped — never `| head` / `| tail`. The full digest is the state of record.

Walkthrough: [Quickstart](https://stateroot.dev/docs/getting-started/quickstart).

## Supported harnesses

**Hooks + transcripts:** Claude Code · Codex · Cursor · Kimi Code · OpenClaw · Hermes · Pi

**Transcripts / delegation:** DeepSeek Harness · OpenCode · others detected at install

Per-harness notes: [Harnesses](https://stateroot.dev/docs/harnesses/overview).

## Documentation

User docs live at **[stateroot.dev](https://stateroot.dev)**. This repository is the CLI.

| Section | Contents |
| --- | --- |
| [Install](https://stateroot.dev/docs/getting-started/installation) | Cursor, VS Code, Linux, Windows, macOS, from source |
| [Quickstart](https://stateroot.dev/docs/getting-started/quickstart) | Init → setup → first resume |
| [Concepts](https://stateroot.dev/docs/concepts/overview) | Roots, continuity, identity, memory, provenance |
| [Capabilities](https://stateroot.dev/docs/features/roots) | Lineage, handoffs, memory, wiki, skills, MCP, rules |
| [Harnesses](https://stateroot.dev/docs/harnesses/overview) | Per-tool install and protocol |
| [CLI reference](https://stateroot.dev/docs/reference/cli) | Every command |
| [MCP tools](https://stateroot.dev/docs/reference/mcp-tools) | Local stdio server |

Machine-readable index: [stateroot.dev/llms.txt](https://stateroot.dev/llms.txt).

## Privacy

Project data stays in the repo (`.stateroot/`). Persona and USER.md live in `~/.stateroot/`. Search stays in `.stateroot/local/` and is never included in snapshots.

Snapshots honor root `.gitignore` and `.staterootignore`, plus `.git/` and `local/`. Optional LLM synthesis runs only with `DEEPSEEK_API_KEY` (preferred, `deepseek-v4-flash`) or `OPENAI_API_KEY` (`gpt-5.6-luna`).

Details: [Privacy](https://stateroot.dev/docs/guides/privacy).

## Sharing a project

Committing `.stateroot/` makes the project's state of record a team asset — and the boundary is deliberate so it merges cleanly:

- **Travels with the repo:** goal and project state, plans, learnings, rules, the skill pool, wiki and memory pages, handoff history, the project soul overlay, transitions. Your persona and USER.md never travel — they live in `~/.stateroot/` by design, so every teammate keeps their own agent's personality.
- **Stays local** (written to `.stateroot/.gitignore` at init): the search index, spool, delegations, sync cursors, the hot-apex `memories/MEMORY.md` and `memories/episodic.jsonl` (per-person lens and private journal — shared truth lives in the wiki and learnings), the current handoff (per-session continuity; history is shared), and `roots/` — lineage travels through Git itself: `git push origin 'refs/stateroot/*'`.
- Append-only journals merge by union via `.stateroot/.gitattributes`. `stateroot doctor` warns when a local-set path is tracked in git.

A teammate's flow: clone, `stateroot install` once, open any harness — the digest already knows the goal, the plans, and every lesson the project has learned.

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
