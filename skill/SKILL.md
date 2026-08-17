---
name: stateroot
description: Install the StateRoot CLI and initialize a project so coding agents share memory, personality, skills, and session handoffs. Use when the user asks to install or set up StateRoot, when `stateroot` is missing from PATH, or when a project has no `.stateroot/` yet and the user wants persistent cross-harness state. Do not use this skill for the session protocol after the CLI is already installed in an initialized project.
---

# StateRoot

StateRoot is a local CLI. This skill gets it onto the machine and into a project. After that, follow the session skill the CLI writes — do not invent a protocol here.

Site: https://stateroot.dev
Install docs: https://stateroot.dev/docs/getting-started/installation
Releases: https://github.com/CognizTech/stateroot/releases

## When To Use

1. the user asks to install, set up, or start using StateRoot
2. `stateroot` is not on PATH (`command not found`)
3. this project has no `.stateroot/` directory and the user wants shared agent state

## When Not To Use

1. `stateroot` is on PATH and `.stateroot/` already exists — resume / checkpoint / handoff via the installed CLI skill
2. the user only asked an unrelated question

## Check first

```bash
stateroot --version
```

If that works, skip install. Run `stateroot doctor`, then go to **Project** or **Machine** below.

## Install

Official assets only — latest GitHub release. Do not invent other download URLs. Current releases ship **Linux x64** and **Windows x64**. macOS: build from source until a release asset is published.

Ask before piping a remote script to a shell. Prefer that the user run the installer themselves if they hesitate.

### Linux

```bash
curl -sSfL https://github.com/CognizTech/stateroot/releases/latest/download/install.sh | sh
```

Installs to `~/.local/bin`. If `stateroot` is still not found, add that directory to `PATH` and retry in a new shell.

### Windows

Prefer the MSI: [StateRootSetup-x64.msi](https://github.com/CognizTech/stateroot/releases/latest/download/StateRootSetup-x64.msi).

Or PowerShell:

```powershell
irm https://github.com/CognizTech/stateroot/releases/latest/download/install.ps1 | iex
```

`stateroot-windows-x64.exe` is the portable CLI, not an installer. Windows assets are unsigned for now. SmartScreen may warn.

### macOS / from source

Follow https://stateroot.dev/docs/getting-started/installation — do not guess a macOS binary URL.

### Verify

```bash
stateroot --version
stateroot doctor
```

`doctor` should pass with zero config and zero keys. If it fails, quote the CLI output; do not work around a broken install by writing `.stateroot/` by hand.

## Machine (once)

Once per machine, after the binary works:

```bash
stateroot setup
```

Interactive is preferred. In a non-TTY agent shell, `stateroot setup --yes` accepts defaults (same as a non-interactive run). `--dry-run` prints planned writes only.

## Project (once per repo)

From the project root:

```bash
stateroot init
```

Creates `.stateroot/`, registers the workspace, and installs harness integrations. Never create `.stateroot/` with file tools.

Then tell the user to keep using their usual agent. Session hooks inject a digest. If no digest appears:

```bash
stateroot resume --harness <id>
```

Use the harness you are actually in: `claude`, `codex`, `cursor`, `kimi`, `openclaw`, `hermes`.

## After install

The CLI embeds the session contract. Mechanically:

1. session start → prefer the injected digest; otherwise `stateroot resume --harness <id>` once
2. after any state-changing step → `stateroot checkpoint --note "…"`
3. before ending or switching harness → `stateroot handoff write --from <current-harness> …`
4. never edit `.stateroot/` directly

Do not duplicate the full protocol in this skill. Point at `stateroot --help` and https://stateroot.dev/docs/getting-started/quickstart.

## Failure modes

| Symptom | Action |
|---|---|
| `command not found: stateroot` | install, then ensure `~/.local/bin` (Linux) is on PATH |
| SmartScreen / unsigned warning | expected on Windows for now; use the MSI from the GitHub release |
| "not a stateroot project" | `stateroot init` from the repo root |
| doctor fails | quote the output; do not hand-write state files |
