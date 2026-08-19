# Harness Install Layout

`stateroot skill install --harness H` (also run by `stateroot init`) writes the per-harness stubs from `assets/` into the project. Harness identifiers are lowercase; the server-side canonical set includes `planner`, `cursor`, `codex`, `kimi`, `opencode`, `hermes`, `openclaw`, `statesmith`, and others in the harness registry contract.

After upgrading the CLI, run `stateroot install` (machine hooks/plugins) **and** `stateroot skill install` (project stubs) so identity injection stays current.

## Identity delivery

Automatic harnesses inject persona + USER.md + work context onto the first usable prompt (session-start and/or first-prompt fallback). Manual `stateroot resume` is the last fallback, not the primary reliability mechanism.

| Harness | Delivery | Notes |
| --- | --- | --- |
| Claude Code, Codex, Kimi, Devin | Automatic | Session-start inject; first prompt retries if that missed |
| Cursor | Automatic | Session-start stdout may be ignored; first prompt always injects |
| Kimi Code | Automatic | Identity rides UserPromptSubmit |
| OpenClaw | Automatic | `before_prompt_build` pulls digest stdout |
| OpenCode, OMP, pi | Automatic | Generated plugin consumes hook stdout on the first prompt |
| Gemini CLI, Antigravity | Automatic | Session-start only (no prompt-submit fallback) |
| Hermes, VS Code Copilot, Crush, Zero | Degraded | No verified injection — run `stateroot resume` |

`stateroot doctor` prints each detected harness's honest tier.

## Claude Code

- Skill: `.claude/skills/stateroot/SKILL.md` (copy of this skill).
- Slash command: `.claude/commands/stateroot.md` — the `/stateroot` stub from `assets/claude-command.md`.
- Effect: running `/stateroot` (or starting a session that loads the skill) triggers `stateroot resume` and binds the hard rules for the session.

## Cursor

- Rule: `.cursor/rules/stateroot.mdc` — from `assets/cursor-rule.mdc`.
- The rule is `alwaysApply: true`, so the resume/checkpoint/handoff protocol is injected into every agent session in the project.

## Codex / OpenCode / Kimi Code (AGENTS.md harnesses)

- Marked block appended to the project `AGENTS.md` — from `assets/agents-block.md`.
- The block is delimited by `<!-- stateroot:begin -->` / `<!-- stateroot:end -->` so `stateroot skill install` can update it idempotently without touching hand-written content.
- One block per `AGENTS.md`; re-installing replaces the block in place.
- Kimi (and every other harness) must run `stateroot resume` **unpiped**. Never `2>&1 | head -N`. The CLI already sized the digest.

## StateSmith (native)

- No project files needed: this skill ships with the platform and is seeded into the skill catalog by the server bootstrap (`DEFAULT_SKILL_SPECS` in `app/core/services/default_skill_bootstrap_service.py`, install strategy `first_party_bootstrap`).
- Its execution locality is `client_local` — the scripts execute the `stateroot` binary on the user's machine — so the server seeds it into the catalog but never attaches it to server-side runs.

## Re-install / Update

1. Re-run `stateroot skill install --harness H` after upgrading the CLI to refresh stubs.
2. Never edit installed stubs by hand; fix the templates in `assets/` (or the CLI) and re-install.
