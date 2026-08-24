# StateRoot

**Switch coding agents without losing the work.**

StateRoot keeps your project's goal, plans, memory, skills, rules, and sessions in one local place — so Claude Code, Codex, Cursor, Kimi Code, Pi, DeepSeek Harness and friends each pick up exactly where the last one left off. One CLI, everything on your machine.

<!-- GIF SLOT: drop the continuity demo here (close one agent, open another — it just knows).
     Suggested: <p align="center"><img alt="Close one agent, open another — it just knows" src="docs/assets/continuity-demo.gif"></p>
     Record ~10s: work in one harness, open a second in the same project, first answer already has the context. -->

<p align="center">
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

StateRoot is the shared layer above the agent runtime:

- **Continue anywhere** — hooks inject a bounded digest (goal, plan, decisions, memories, next actions) at session start. No pasting transcripts.
- **Plan in one harness, implement in another** — a strong model authors the plan in its plan mode; a cheaper model executes it. `stateroot plan` carries the artifact and its approval state, and the executor's digest says *"execute this plan; do not re-plan."*
- **Spawn subagents across harnesses** — `stateroot delegate --to codex --task "…"` runs a bounded task inside another harness with full project context. The parent gets the conclusion, not the transcript.
- **Move the session itself** — sessions canonicalize from every supported harness into one store, and transfer into Pi / DeepSeek Harness as real, resumable native sessions.
- **Branch and restore the work** — snapshots live in Git plumbing under `refs/stateroot`; your branches are never rewritten.

And when a harness is retired or replaced, the work doesn't care. **The harness is disposable; the work is not.**

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
| State lineage (branch / restore) | Git plumbing, your branches untouched |
| Personality | full persona + USER.md, never trimmed |

```text
Claude Code ──→ State A ──→ State B
                             │
                             ├── continue with Codex
                             ├── branch with Cursor
                             └── restore an earlier state
```

## What it shares

| | |
| --- | --- |
| **Personality** | Soul + USER.md, injected in full — not truncated to fit a token budget. |
| **Project state** | Objective, phase, handoffs, next actions — one place every harness reads. |
| **Plans** | A plan store with a lifecycle (draft → approved → active → done) and provenance. |
| **Memory** | Curated facts, a compiled wiki, local search. The next session can look things up. |
| **Preferences** | Record “prefer X over Y” once. Every agent on the machine sees it. |
| **Skills and tools** | SKILL.md packages and MCP servers sync across agent configs. Conflicts are left alone. |
| **Sessions** | Full-fidelity canonical session store across harnesses; transfer into Pi / DeepSeek Harness. |
| **Subagents** | Delegate bounded tasks into other harness CLIs — depth-capped, bounded result, lineage recorded. |
| **Extensions** | Any `stateroot-<name>` executable on PATH becomes a subcommand — agents can extend the CLI itself. |

## What it snapshots

Git versions the commits you make. StateRoot snapshots the working tree during agent work, stored with Git *plumbing* under `refs/stateroot`. Your branches are never rewritten.

Restores and digests say where information came from: **verified** (Git), **observed** (transcripts), or **synthesized** (LLM). Empty stays empty.

Full map: **[stateroot.dev/docs](https://stateroot.dev/docs/intro)**.

## Install

Current releases ship **Linux x64** and **Windows x64**. One binary, no extra runtime. macOS: [build from source](https://stateroot.dev/docs/getting-started/installation) until a release asset is published.

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
| [Install](https://stateroot.dev/docs/getting-started/installation) | Linux, Windows MSI, from source |
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

## Contributing

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Rust 1.85+. libgit2 is vendored. The CLI-embedded session skill is in [`stateroot-cli/assets/stateroot-skill/`](stateroot-cli/assets/stateroot-skill/). The marketplace install skill is in [`skill/`](skill/). Docs: [Contributing](https://stateroot.dev/docs/developer-guide/contributing).

Please preserve [product intent](https://stateroot.dev/docs/features/rules): inject full persona/USER.md, warn on thin handoffs instead of refusing, do not auto-categorize learnings, and do not trim identity to save tokens.

## License

Apache-2.0 — see [LICENSE](LICENSE).
