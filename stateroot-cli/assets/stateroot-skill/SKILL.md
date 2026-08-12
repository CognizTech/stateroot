---
name: stateroot
description: Persistent project state and cross-harness handoffs via the `stateroot` CLI. Use when working in a project that has a `.stateroot/` directory — run `stateroot resume --harness <id>` once at session start (harness-specific integration), `stateroot checkpoint` after every state-changing step, check the failed-approaches log before attempting an approach, and run `stateroot handoff write` before ending a session or switching agent harnesses.
---

# StateRoot

StateRoot gives a project one persistent state of record that any agent harness (Claude Code, Codex, Cursor, OpenCode, Kimi Code, OpenClaw, Hermes, StateSmith) can attach to. This skill is the behavioral contract for working in a stateroot project. Follow it mechanically, every session.

## When To Use

1. the project root contains a `.stateroot/` directory, or `stateroot status` reports an attached project
2. starting, continuing, or handing off work in such a project

## When Not To Use

Do not use this skill when:
1. the project is not stateroot-initialized and the user has not asked to initialize it (suggest `stateroot init` instead)
2. the task is unrelated to project work (casual Q&A, one-off questions)

## Hard Rules

### 1) Session start -> resume once

At the start of every session in a stateroot project, before doing any work:
1. prefer the auto-injected StateRoot digest from harness hooks when present
2. only run the harness-specific resume (e.g. `stateroot resume --harness cursor` / `--harness codex` / `--harness claude`) — or `scripts/resume.sh --harness <id>` — if no digest appeared yet
3. treat that output — current handoff, hot-apex memory, context pack — as the project state of record
4. do not run resume again in the same session unless the user explicitly asks (`--force` reprints)
5. do not re-derive project state by scanning the tree when resume already answers the question

### 2) State-changing step -> checkpoint

After completing ANY step that changes project state (files written, decisions made, milestones reached, blockers discovered, approaches abandoned):
1. run `scripts/checkpoint.sh --note "<what changed and why>" [--files a,b]`
2. keep the note specific: what changed, why, and what it unblocks
3. checkpoint the finished step BEFORE starting the next one; never batch several finished steps into one checkpoint

### 3) Before attempting an approach -> check failed approaches

Before attempting any non-trivial approach:
1. run `scripts/search.sh "failed approach <topic>"` or read `failed_approaches` in the current handoff
2. if a matching failure exists, do not repeat it — state explicitly why the new attempt differs

### 4) Session end / usage limit / harness switch -> handoff

Before ending a session, when approaching usage limits, or when the user asks to switch harness:
1. write the handoff content as strict JSON, then run `scripts/handoff.sh write --from <resolved-current-harness> --to <harness> --input <handoff.json>`
2. resolve `--from` to the actual current harness id; never copy a placeholder or infer it from an environment variable
3. include the durable objective, immediate task, detailed continuity narrative in `context_summary` (present state, verified evidence, decisions and rationale, constraints, failed approaches, implications for the next agent), decisions with the why, next actions, and truthful failures; the CLI auto-captures recent verified conversation and other observed transcript evidence
4. do not paste giant state or transcript dumps into `--note`; `--note` is only a legacy short-summary fallback
5. file paths work directly on Windows; `--input -` is an optional stdin convenience, not required shell behavior

### 5) Never edit `.stateroot/` directly

All reads and writes of project state go through the CLI. Do not open, edit, move, or delete files under `.stateroot/` with file tools or shell commands — the CLI maintains revisions, indexes, and outbox consistency.

### 6) Offline behavior

The CLI is offline-safe: when the server is unreachable it queues operations in the local outbox and still exits successfully. Therefore:
1. checkpoint and hand off anyway — never skip a checkpoint because of connectivity
2. do not re-issue the same queued operation in a retry loop
3. never hand-edit outbox files

## Command Reference

| Command | When | Notes |
|---|---|---|
| `stateroot resume [--harness H] [--budget N]` | session start | prints handoff + hot-apex memory + context pack as markdown |
| `stateroot checkpoint --note "..." [--files a,b]` | after any state-changing step | appends an episodic record and updates handoff state |
| `stateroot handoff write --from CURRENT_HARNESS --to H --input PATH` | session end / harness switch | strict structured JSON; writes current handoff plus history entry |
| `stateroot handoff list` / `stateroot handoff show` | inspect prior handoffs | read-only |
| `stateroot search <query> [--kinds ...] [--top-k N]` | find decisions, failures, memories | hybrid search over project state and memory |
| `stateroot pack [--harness H] [--budget N]` | need a fresh context pack | prints the pack to stdout |
| `stateroot skill install [--harness H]` | install this skill into harness dirs | writes stubs from `assets/` |
| `stateroot status` / `stateroot doctor` | diagnose auth, connectivity, project state | doctor checks outbox depth and sync health |
| `stateroot init` | one-time per project | creates `.stateroot/`, registers the workspace, installs harness integrations |

## Failure Modes

| Symptom | Meaning | Action |
|---|---|---|
| `command not found: stateroot` | CLI not installed | tell the user to install the `stateroot` CLI and put it on PATH; do not attempt workarounds |
| "not a stateroot project" | no `.stateroot/` in this tree | suggest `stateroot init`; never create `.stateroot/` manually |
| write reported as queued | offline | normal — the outbox syncs later; continue working |
| conflict / stale revision on write | another harness wrote concurrently | re-run `stateroot resume`, then retry the operation once |
| thin or missing handoff on resume | previous harness wrote an incomplete handoff | note the gap in your next checkpoint; do not fabricate history |

## Output Discipline

1. after resume: tell the user the objective, current state, and next actions in 2-3 sentences
2. after checkpoint or handoff: confirm in one line what was recorded
3. never paste raw `.stateroot/` file contents into the transcript — quote CLI output instead

## References And Assets

Read as needed:
- `references/protocol.md` — checkpoint cadence, handoff packet fields, offline outbox semantics
- `references/harnesses.md` — per-harness install layout
- `assets/` — harness stub templates used by `stateroot skill install`
- `scripts/` — thin wrappers: `resume.sh`, `checkpoint.sh`, `handoff.sh`, `search.sh`

## Self-improvement (shared)

When the user corrects you, call `learn_record`; when a fact is durable, call `memory_save`; when a procedure worked end-to-end, propose it with `skill_propose` (via the `stateroot` MCP tools where registered).
Writes from harnesses stay quarantined (session-candidate/private) until a human approves them — never present your own proposals as already active.
