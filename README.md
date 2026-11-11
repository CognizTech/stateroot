# StateRoot

[English](README.md) | [简体中文](README.zh-CN.md)

**The harness is disposable. The work is not.**

StateRoot connects the AI agents you already use so they share knowledge, tools and what they learn across sessions. Pick up where another agent stopped, ask one to plan and others to work in parallel, then combine their results or restore an earlier saved version of your project.

After setup, you direct the work through your agents. They operate StateRoot through its installed Skill and CLI; the editor workbench lets you inspect progress and results.

Works with Claude Code, Codex, Cursor, Kimi, CodeBuddy, OpenClaw, Hermes and other supported agents.

*Persistent, federated meta-harness for AI agents.*

**Shared intelligence. Independent execution.**

<p align="center">
  <img alt="Agents carrying shared knowledge and project work across tools" src="demo.gif">
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

Changing tools, hitting a usage limit or coming back to a project can mean repeating decisions and explaining the work again. StateRoot keeps useful project knowledge and saved versions available so you can choose the agent for each task and carry the work forward.

You can stay with one agent, continue in another, or ask several to work at once. Your preferences, plans and lessons remain available to the connected agents that need them, while independent work happens on separate copies of the project.

## You direct the work. Your agents handle StateRoot.

Talk to your agents normally. For example:

> Make a shared StateRoot plan for this feature. After I review it, give Kimi and Cursor independent parts on separate forks, then review and combine their results.

Or:

> Remember this project preference in StateRoot so I do not have to explain it to the next agent.

These are ordinary requests, not special prompt syntax. The installed StateRoot Skill teaches agents how to save progress, share knowledge and operate on project history. You review the work through the conversation or the editor workbench.

Start with [Working with your agents](https://stateroot.dev/docs/guides/daily-workflow). Setup and updates remain available as direct CLI controls; routine checkpoints and handoffs belong to the agent's workflow.

## Continue. Delegate. Fork. Merge.

**Continue.** Open the same project in another supported agent and ask it to pick up the work. StateRoot supplies the retained goal, plan, decisions and next steps, so you do not have to reconstruct the previous conversation. [Handoffs](https://stateroot.dev/docs/features/handoffs).

**Delegate.** Ask your current agent to send a specific task to another supported agent with the project context it needs. It brings back a result for your current agent to review and use. [Delegation](https://stateroot.dev/docs/features/delegation).

**Fork.** Give agents separate working copies of a project, called StateRoot forks, for independent tasks or alternative approaches. They share project knowledge while making changes independently. Completed forks can be parked and reopened without losing their retained history. [Forks and saved versions](https://stateroot.dev/docs/features/roots).

**Merge.** Ask your coordinating agent to review contributions, resolve conflicts, run the relevant checks and combine the accepted work. The results join the continuing project with their history intact. [Working across agents](https://stateroot.dev/docs/guides/versioning-across-agents).

```text
                   one project, shared knowledge
                               |
                +--------------+--------------+
                |              |              |
              Codex          Kimi          Cursor
                |              |              |
             fork A         fork B         fork C
                |              |              |
                +--------------+--------------+
                               |
                    review and combine results
                               |
                       continue the project
```

### Continuity that stays active

StateRoot surfaces due work, unfinished handoffs and plans needing completion evidence in agent sessions and the editor workbench. Ask your agent what needs attention or what remains to finish. Its local background service carries these follow-ups between sessions without running an AI agent of its own.

## Keep the agents you already use

Each agent keeps its own model, interface and strengths. Have one agent plan, others implement, and your coordinating agent review the result. General-purpose agents such as OpenClaw and Hermes can participate alongside Claude Code, Codex, Cursor, Kimi, CodeBuddy and Pi.

### What crosses the boundary

Shared project knowledge, useful preferences and saved work carry between connected agents. Each agent can continue the task or work independently on a fork while retaining the relevant shared context.

## What it shares

| What carries forward | What it lets you do |
| --- | --- |
| **Personality and preferences** | Keep how you want agents to address you, communicate and approach your work. |
| **Project knowledge** | Retain goals, decisions, facts, rules and previous approaches for the next task. |
| **Plans and dependencies** | Plan in one agent, implement in another, and track prerequisites, ownership, progress and completion evidence. |
| **Skills and tools** | Reuse complete Skill packages and compatible MCP configuration. Use a native capability through delegation to the agent that provides it. |
| **Learnings** | Carry useful corrections into later agent sessions as shared guidance. |
| **Native sessions** | Transfer supported sessions into Pi or DeepSeek Harness; recover qualified retained Codex sessions with their documented limits. |
| **Portable project state** | Export and inspect a retained root with shared intelligence and evidence, then import it elsewhere or apply a compatible project state. |
| **Commands agents can extend** | Add executable CLI extensions when a project needs another operation. |

Details: [Skills](https://stateroot.dev/docs/features/skills), [MCP tools](https://stateroot.dev/docs/features/mcp), [rules](https://stateroot.dev/docs/features/rules), [sessions](https://stateroot.dev/docs/features/sessions), [portable exchange](https://stateroot.dev/docs/guides/sharing-a-project#portable-local-exchange) and [CLI extensions](https://stateroot.dev/docs/features/extensions).

### One personality across your connected agents

Your shared persona and user profile carry how you want to work with your agents: name, voice, preferences and working relationship. StateRoot delivers the full persona and USER.md through supported session integrations. Each agent keeps its own model and strengths while inheriting that shared identity. [Personality](https://stateroot.dev/docs/features/personality).

### Memory, in three layers

- **Current facts:** a compact, curated brief for the next session.
- **Longer project knowledge:** a wiki built from retained evidence, with optional model-assisted synthesis.
- **Work history:** searchable checkpoints and observations that let an agent recover earlier decisions and context.

Records distinguish verified changes, observed activity and AI-generated summaries. [Memory](https://stateroot.dev/docs/features/memory) and [project wiki](https://stateroot.dev/docs/features/wiki).

### Teach one agent. Carry the lesson forward.

Tell Cursor that business logic belongs outside route handlers, for example. StateRoot records that project preference and brings it into later sessions with Codex, Kimi and other connected agents.

Learnings are guidance about how to work, with a scope such as this project, your work generally, a workspace or a domain. They activate when recorded; updates preserve their history. Facts and personality have their own shared records. [Learnings](https://stateroot.dev/docs/features/learnings).

## Keep a way back as your agents work

StateRoot saves versions of the project as work progresses. Compare changes, try a different approach on a separate copy, or restore an earlier saved version when something goes wrong.

Snapshots use Git while preserving your normal branches. Restoring records a new root, so the intervening work remains in history. Today's file exclusions still apply when restoring an older state.

Ask your agent what changed, what was checked and what remains. The editor's Lineage view shows retained history and comparisons. [Snapshots and restoration](https://stateroot.dev/docs/features/roots) explain coverage and limits; [First continuity proof](https://stateroot.dev/docs/guides/first-continuity-proof) provides a walkthrough you can check yourself.

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
stateroot doctor     # check local setup
```

## Quickstart

Initialize your project and finish the once-per-machine setup. Core continuity needs no additional model API key.

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
