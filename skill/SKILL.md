---
name: stateroot
description: >-
  StateRoot is the cross-harness continuity layer for AI coding agents
  (Claude Code, Codex, Cursor, Kimi Code, Pi, DeepSeek Harness): shared
  persona, memory, plans, skills, sessions, and project history.
  Bootstrap-only: install the StateRoot CLI from official GitHub release
  assets and run once-per-machine `stateroot setup` so harnesses get hooks
  and the built-in session skill. Use when the user asks to install or set
  up StateRoot, when `stateroot` is missing from PATH, or when setup has
  not been run yet. Do not use after the CLI is installed and harnesses are
  interfaced — that built-in skill (`stateroot skill show stateroot`) is
  then the default. Not for resume, checkpoint, handoff, memory, or daily
  workflow.
metadata:
  homepage: https://stateroot.dev
  source: https://github.com/CognizTech/stateroot
  openclaw:
    emoji: "🌱"
    required_env: []
    optional_env:
      - name: STATEROOT_HOME
        description: Override config directory (config.toml, projects.toml). Default is ~/.config/stateroot on Linux.
      - name: DEEPSEEK_API_KEY
        description: Optional. Preferred key for the local context compiler (DeepSeek deepseek-v4-flash). Not required for install or setup.
      - name: OPENAI_API_KEY
        description: Optional. Used when DEEPSEEK_API_KEY is unset (OpenAI gpt-5.6-luna). Not required for install or setup.
    required_binaries: []
    writes_to_disk: true
    network_access:
      - "github.com (official release assets and checksums during install/self-update only)"
    data_handling:
      uploads_to_external: false
      requires_user_confirmation: true
      deletes_files: false
      modifies_files: false
---

# StateRoot (bootstrap)

StateRoot is the cross-harness continuity layer for AI coding agents: one continuous agent — persona, memory, plans, skills, sessions, and project history — across Claude Code, Codex, Cursor, Kimi Code, Pi, DeepSeek Harness and friends, while each model keeps its own native runtime.

This skill installs the CLI and runs **global** `stateroot setup`. That wires harnesses. After that, **this skill expires**. The CLI-embedded session skill becomes the default — do not keep using these bootstrap instructions, and do not copy the session protocol into this file.

Docs: https://stateroot.dev/docs/getting-started/installation · https://stateroot.dev/docs/getting-started/setup  
Releases: https://github.com/CognizTech/stateroot/releases

## Expire first

Run the check next to this `SKILL.md` (never writes):

```bash
sh scripts/check.sh
```

If `scripts/check.sh` is not on disk, equivalent:

```bash
command -v stateroot && stateroot --version
```

**Stop using this skill** when all of the following are true:

1. `stateroot` is on `PATH`
2. `stateroot setup` has already been run on this machine (harness hooks / identity exist; `stateroot doctor` is not a hard fail)
3. harnesses are interfaced (`stateroot install` is the non-interactive half of the same wiring; setup already does it)

Then follow the built-in skill only:

```bash
stateroot skill show stateroot
```

That skill is what setup/init wrote into the harness. Resume, checkpoint, handoff, memory, learnings, wiki, roots — all of it lives there. If this bootstrap skill is still in context, ignore it.

**Keep using this skill** only when:

1. the user asked to install or set up StateRoot
2. `stateroot` is missing (`command not found`)
3. the binary exists but **setup has not been run** yet

Unrelated questions: do not load this skill.

## What this skill does

Three commands, in order. Exact commands — do not substitute package managers, random GitHub clones, or invented download URLs.

| Step | Scope | Command |
| --- | --- | --- |
| 1. Install | machine | official `install.sh` / MSI / `install.ps1` |
| 2. Setup | machine (required after install) | `stateroot setup` |
| 3. Init | **project**, only if this repo should be a StateRoot project | `stateroot init` |

After step 2, StateRoot can be used. Step 3 is not a substitute for setup. Never create `.stateroot/` with file tools.

Platform install details: [references/install.md](references/install.md)  
What gets written / privacy: [references/disclosure.md](references/disclosure.md)  
Failures: [references/failures.md](references/failures.md)

## Step 1 — Install

Skip if `stateroot --version` works.

Ask before piping a remote script to a shell. Prefer that the user run the installer themselves if they hesitate. Official assets only.

**Linux (x86_64):**

```bash
curl -sSfL https://github.com/CognizTech/stateroot/releases/latest/download/install.sh | sh
```

Installs to `~/.local/bin`. Put that directory on `PATH` if `stateroot` is still not found. Needs glibc 2.17 or newer (Ubuntu 16.04, Debian 9, RHEL 7, and later).

**Windows:** prefer the MSI: https://github.com/CognizTech/stateroot/releases/latest/download/StateRootSetup-x64.msi

```powershell
irm https://github.com/CognizTech/stateroot/releases/latest/download/install.ps1 | iex
```

`stateroot-windows-x64.exe` is the portable CLI, not an installer.

**macOS:** current releases are Linux and Windows. Do not guess a macOS binary URL. Build from source per [references/install.md](references/install.md).

Verify:

```bash
stateroot --version
stateroot doctor
```

`doctor` is designed to pass with zero config and zero keys. If it fails, quote the CLI output. Do not work around a broken install by writing state files.

## Step 2 — Setup (global, required)

Once per machine, immediately after the binary works. Without this, harnesses are not interfaced and the built-in skill is not the default.

Interactive is preferred:

```bash
stateroot setup
```

Sections: **identity**, **harnesses**, **skills**.

- identity — canonical soul / USER.md (import OpenClaw or Hermes if present, or a deterministic draft)
- harnesses — detect agents, write session hooks
- skills — seed the built-in StateRoot skill into detected agent directories

Non-TTY agent shell (same as `--yes`; do not use interactive `read`):

```bash
stateroot setup --yes
```

Optional:

```bash
stateroot setup --dry-run
stateroot setup --only identity,harnesses,skills
stateroot setup --config answers.yaml
```

Do **not** run `--blank-slate` unless the user asked to reconfigure.

`stateroot install` is the non-interactive harness-integration half if identity is already done. Setup is the full onboarding flow. Use setup after a fresh install.

When setup finishes, this bootstrap skill is done for the machine. Tell the user to keep using their usual agent. The built-in skill is now the default.

## Step 3 — Init (project, only if needed)

`stateroot init` is **not** global setup. Use it only from a project root that has no `.stateroot/` and that the user wants as a StateRoot project.

```bash
stateroot init
```

Creates `.stateroot/`, registers the directory in `projects.toml`, seeds project stubs (Cursor rule, Claude command, AGENTS.md block), and seeds the objective / memory / first handoff from what the repo declares (README, TODO.md, git log — labeled observed; `--synthesize` opts into LLM enrichment, labeled unverified). Never `mkdir .stateroot`.

Then expire this skill.

## After expiry

Do not continue from this file. Do not summarize a homemade session protocol. Run `stateroot skill show stateroot` (or follow the copy setup already wrote into the harness) and stop.

## Anti-patterns

- Inventing download URLs or installing via `npm` / `pip` / a random clone
- Guessing a macOS release asset
- Piping `install.sh` without asking
- Creating or editing `.stateroot/` or `~/.stateroot/` with file tools
- Running `setup --blank-slate` unprompted
- Teaching resume / checkpoint / handoff / memory here after setup succeeded
- Staying on this marketplace skill once harnesses are interfaced

---

## About StateRoot (marketplace listing copy)

*This section is for humans browsing skill registries (ClawHub and similar). It plays no role in agent routing or bootstrap behavior.*

StateRoot is an open-source, local-first CLI for **continuity across AI coding agents**. It keeps one shared project state — goal, plans, memory, learnings, skills, rules, personality, and sessions — so Claude Code, Codex, Cursor, Kimi Code, Pi, DeepSeek Harness, OpenClaw, and Hermes each pick up exactly where the last agent left off. No re-explaining the project, no re-reading the codebase, no rebuilding context when you hit a usage limit or a better model lands in a rival tool.

What it does:

- **Session continuity and handoffs** — hooks inject a bounded project digest at session start; the next agent starts knowing the goal, the plan, decisions, and next actions.
- **Cross-agent shared memory** — a three-layer memory (curated hot-apex facts, an evidence-compiled wiki, searchable episodic history) with every fact labeled verified, observed, or synthesized.
- **Shared learnings and self-improvement** — record a correction once and every agent on the machine lives by it immediately; the team of agents gets smarter together.
- **Plans across agents** — a plan store with an approval lifecycle; plan with a strong model in one agent, implement with a cheaper one in another.
- **Cross-agent subagents** — delegate a bounded task into another agent's CLI and get back the conclusion.
- **State versioning** — working-tree snapshots in Git plumbing: restore, fork, compare, with receipts. Your branches are never touched.
- **Skills and MCP sync** — SKILL.md packages and MCP server configs pooled and projected across agent configs without clobbering user content.
- **Personality sharing** — one persona and user profile, injected in full at every session start.
- **Local-first and private** — one static binary, zero config to start, everything on your machine, Apache-2.0.

Keywords for search: AI coding agents, cross-agent memory, shared agent context, project state, session handoff, context engineering, multi-agent workflow, agent skills, MCP, Claude Code, Codex, Cursor, developer tools, local-first, open source.

Homepage: https://stateroot.dev · Source: https://github.com/CognizTech/stateroot
