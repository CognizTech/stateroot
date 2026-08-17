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

### 0) Shared rules (constitution)

StateRoot keeps a **shared rules pool** — same idea as skills and learnings. Product-intent ships by default. Rules from other harnesses (Cursor `.mdc`, Codex/Claude `AGENTS.md`/`CLAUDE.md`, …) are pulled in on `stateroot rules sync` (also run by `init` / `install`).

Before any architectural or behavioral change, read `stateroot rules show product-intent`. Preserve product intent. Do not replace agent judgment with classifiers, approval gates, or generic architecture. Do not add friction "because it seems safer." Follow imported harness rules in the pool as well (`stateroot rules list`).

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
1. prefer a **flag-first** one-liner: `scripts/handoff.sh write --from <resolved-current-harness> [--to <harness>] --objective "…" --task "…" --context-summary "…" [--next "…"]` — one command, no temp JSON
2. omit `--to` for continuity-only; use it only when orchestrating a cross-harness switch; when continuity suffices and hooks may not run, `stateroot handoff finalize` is acceptable — or rely on session_end/stop hook finalize when it ran
3. resolve `--from` to the actual current harness id; never copy a placeholder or infer it from an environment variable
4. include the durable objective, immediate task (`--task`, not `immediate_task`), detailed continuity narrative, decisions, next actions, and truthful failures; the CLI auto-captures recent verified conversation when author content is absent
5. use `--input <handoff.json>` only when the payload is too large for flags; never write under `.stateroot/handoffs/` by hand
6. thin fields warn; they do not refuse the write — continuity beats form-filling
7. do not paste giant state or transcript dumps into `--note`; `--note` is only a legacy short-summary fallback
8. never invent a second approval story — learnings, soul, skills, memory, and distill activate immediately

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
| `stateroot handoff write --from CURRENT_HARNESS [--to H] [--task …] [--context-summary …] [--next …]` | session end / harness switch | prefer flags near limits; `--to` optional (routing only); `--input` for large payloads |
| `stateroot handoff finalize [--from H]` | hook missed / quota exit | observed continuity from verified transcript; no routing |
| `stateroot handoff list` / `stateroot handoff show` | inspect prior handoffs | read-only |
| `stateroot search <query> [--kinds ...] [--top-k N]` | find decisions, failures, memories | hybrid search over project state and memory |
| `stateroot rules list` / `show` / `sync` | shared rules pool | product-intent always on; harness rules imported |
| `stateroot pack [--harness H] [--budget N]` | need a fresh context pack | prints the pack to stdout |
| `stateroot learn record "…"` | durable project taste | judgment rule, not a fact — see Learnings below |
| `stateroot learn record --user "…"` | durable global taste | follows the user across projects |
| `stateroot learnings list` / `--user` | read before writing | update rather than duplicate |
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
- `references/learnings.md` — quality bar, examples, anti-examples for `learn record`
- `references/harnesses.md` — per-harness install layout
- `assets/` — harness stub templates used by `stateroot skill install`
- `scripts/` — thin wrappers: `resume.sh`, `checkpoint.sh`, `handoff.sh`, `search.sh`

## Learnings (taste — not memory)

Learnings are durable **preferences**: how the next harness should judge a choice when two valid options exist. They are the StateRoot equivalent of CommandCode taste. They are not a wiki, not a layout dump, and not a fact log.

`learn_record` always writes a learning. Facts go to `memory` / `memory_save` (curated MEMORY.md). Procedures go to `skill_propose`. Pull long-term knowledge with `memory_recall` or `wiki_show` — page bodies are not dumped into the digest.

### Layers

- **Global (user):** `stateroot learn record --user "…"` or MCP `learn_record` with `scope: "user"` — communication, recurring methods, design/engineering judgment, boundaries that follow this human across repos.
- **Project:** `stateroot learn record "…"` or MCP `learn_record` with `scope: "project"` — this repo's quality bars, preferred patterns, anti-patterns.

Read first: `stateroot learnings list` and `stateroot learnings list --user`. Update rather than duplicate.

### When to write

1. the user corrects you
2. the user states a durable preference
3. first session after `stateroot init`, if a layer is empty — seed **2–7 evidenced judgments**, then stop
4. you just followed a rule the next harness would otherwise miss

### When not to write

- inventory ("this is a TypeScript/Python monorepo", "uses uv") → `memory_save` / `memory` if it must persist at all
- facts (deadline, version, port, "the dashboard is graphite") → `memory_save` (or `memory` add/replace)
- slogans ("write good code", "be careful", "prefer evidence")
- session recap, directory listings, one-off notes
- anything already in `learnings list`

### Format (required)

One `learn record` call per learning. The note must stand alone so another harness can apply it without this session:

`<judgment>. <when it applies / what to do / what never to do>.`

Good:

- `Prefer small, reviewable diffs over rewrites. Touch only files that implement the asked change; do not restyle or restructure adjacent code.`
- `Server-side tools must never include arbitrary command execution. exec belongs on the client.`
- `Always read .gitignore before exploring a codebase so ignored trees do not pollute context.`
- `When two designs are valid, prefer the restrained, practical one. Do not add architecture from another project without fitting this one.`

Bad:

- `Laiq is a TypeScript/Python monorepo` — fact
- `prefer evidence over assertion` — too thin; no when, no never
- `the deploy uses systemd` — inventory

First session after init: seed whichever layer is empty **with this quality bar** before other work. Later harnesses inherit and must keep both current.
