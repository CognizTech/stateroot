# Disclosure (bootstrap)

StateRoot is a **local** CLI. Install and setup do not upload project files to a StateRoot cloud. There is no `login` in this CLI.

Authoritative: https://stateroot.dev/docs/guides/privacy · https://stateroot.dev/docs/getting-started/installation

## What this skill causes to run

1. Official installer (GitHub release asset + checksums) or MSI / `install.ps1`
2. `stateroot setup` (and possibly `stateroot install` from the Linux installer)
3. `stateroot init` only when the current repo should be a StateRoot project

## Writes

| Path | When | What |
| --- | --- | --- |
| `~/.local/bin/stateroot` (Linux) | install | CLI binary |
| `~/.config/stateroot/` (Linux) or `%APPDATA%\stateroot\` (Windows) | first use / setup | `config.toml`, `projects.toml` |
| `~/.stateroot/` | setup / first use | soul, USER.md, user-global learnings |
| Harness config dirs | setup / `stateroot install` | session hooks so the **built-in** skill can run |
| `<project>/.stateroot/` | `stateroot init` only | project store |
| Project stubs | `stateroot init` | e.g. `.cursor/rules/stateroot.mdc`, Claude command, AGENTS.md block |

Override config home with `STATEROOT_HOME`.

Setup does not delete, move, or rewrite the user's Git branches. `init` may `git init` a non-git folder so snapshots can use plumbing under `refs/stateroot/` — user `HEAD` is not rewritten.

## Network

- **Install / self-update:** GitHub releases (`CognizTech/stateroot`) for the binary and `checksums.txt`. Ask before piping `install.sh`.
- **Setup / init:** local disk. No StateRoot server.
- **Optional later:** if `DEEPSEEK_API_KEY` or `OPENAI_API_KEY` is set, resume/compile may call DeepSeek (`deepseek-v4-flash`, preferred) or OpenAI (`gpt-5.6-luna`). Not required for bootstrap. Failure falls back to the deterministic observed context pack. This skill must not demand a key.

## What is not uploaded

Project files, `.stateroot/`, soul, USER.md, and transcripts stay on the machine. `.stateroot/local/` (FTS) is never part of a snap payload.

## Confirmation

- Ask before `curl … \| sh` or `irm … \| iex`.
- Interactive `stateroot setup` is preferred. `--yes` / non-TTY uses defaults (binary-detected harnesses only).
- `--dry-run` prints planned writes and touches nothing.
- Destructive removal is a different command (`uninstall`, `remove`) and is not part of bootstrap.

## After setup

Harness hooks persist across sessions until `stateroot uninstall`. That is intended: it is how the built-in skill becomes default. This marketplace skill should not keep running.
