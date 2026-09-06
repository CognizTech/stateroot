# StateRoot

**Switch harnesses. Keep the agent.**

StateRoot is the cross-harness continuity layer for AI coding agents: one
continuous agent across Claude Code, Codex, Cursor, Kimi Code, Pi, DeepSeek
Harness and friends — same persona, memory, plans, skills, sessions, and
project history — while each model keeps its own native runtime. One local
CLI, everything on your machine.

Close Claude Code. Open Codex. Keep working. The next agent starts already
knowing the goal, the plan, the decisions, and how you work — and every
lesson one agent learns becomes a rule for all of them.

This extension is the Conductor UI for that CLI: the sidebar glance and the
workbench for the human directing several harnesses at once.

## What the StateRoot CLI does

One `stateroot` binary next to Git, sharing everything above each harness's
own runtime:

- **Continue anywhere** — hooks inject a bounded digest (goal, plan,
  decisions, memories, next actions) at session start. No pasting
  transcripts between tools.
- **Plan in one harness, implement in another** — a strong model authors the
  plan in its plan mode; a cheaper model executes it. `stateroot plan`
  carries the artifact and its approval state, and the executor's digest
  says *"execute this plan; do not re-plan."*
- **Spawn subagents across harnesses** — `stateroot delegate --to codex
  --task "…"` runs a bounded task inside another harness with full project
  context. The parent gets the conclusion, not the transcript.
- **Move the session itself** — sessions canonicalize from every supported
  harness into one store, and transfer into Pi / DeepSeek Harness as real,
  resumable native sessions.
- **Branch and restore the work** — snapshots live in Git plumbing under
  `refs/stateroot`; your branches are never rewritten. Restore exactly, fork
  safely, compare honestly, and read the receipt of what changed.
- **One personality everywhere** — soul + USER.md injected in full at every
  session start, never trimmed. The agent you brief in Codex is the same
  person in Claude Code.
- **Memory in three layers** — a curated hot-apex every session sees, a
  compiled wiki distilled from evidence, and an episodic log with full-text
  recall (`stateroot memory recall`). Every fact carries provenance:
  verified, observed, or synthesized.
- **Learnings that compound** — record one judgment (`prefer X over Y`,
  `never Z`, with *when it applies*) and it activates immediately for every
  harness. A correction made in one tool becomes a rule for all of them.

## What the extension adds

- **Activity bar glance** — the lamp in your sidebar: what's Now, what Needs
  You, the current root, and an Initialize action on empty workspaces.
- **Workbench** (`StateRoot: Open workbench`) — the full board:
  - **Control** — inbox and delegation state, handoffs waiting on you.
  - **Plans** — review, approve, assign a harness, or delegate a plan into
    another agent.
  - **Todos** — federated todo lists from every harness, plan-bound and
    standalone.
  - **Crew** — reassign failed or timed-out delegations, inspect logs.
  - **Learnings** — the shared taste pool: accept, edit, reject.
  - **Memory** — the curated hot-apex of facts.
  - **Lineage** — compare roots, native diff, restore, or fork the work
    itself.

Writes go through the `stateroot` CLI only — the extension never edits
project state directly.

## Requirements

- The **StateRoot CLI** on the machine:
  - Linux: `curl -sSfL https://github.com/CognizTech/stateroot/releases/latest/download/install.sh | sh`
  - Windows: `irm https://github.com/CognizTech/stateroot/releases/latest/download/install.ps1 | iex` (or the `StateRootSetup-x64.msi` GUI installer)
- Run `stateroot init` in a project — the extension lights up on any
  workspace that has one.

## Links

- [Website](https://stateroot.dev) · [Docs](https://stateroot.dev/docs/intro) ·
  [GitHub](https://github.com/CognizTech/stateroot) ·
  [Releases](https://github.com/CognizTech/stateroot/releases)

Cross-harness continuity for AI coding agents — local-first, Apache-2.0.
