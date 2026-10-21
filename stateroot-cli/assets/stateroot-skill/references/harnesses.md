# Harness Install Layout

`stateroot skill install --harness H` (also run by `stateroot init`) writes the per-harness stubs from `assets/` into the project. Harness identifiers are lowercase; the server-side canonical set includes `planner`, `cursor`, `codex`, `kimi`, `opencode`, `hermes`, `openclaw`, `statesmith`, and others in the harness registry contract.

After upgrading the CLI, run `stateroot install` (machine hooks/plugins) **and** `stateroot skill install` (project stubs) so identity injection stays current.

## Identity delivery

Automatic harnesses inject persona + USER.md + work context onto the first usable prompt (session-start and/or first-prompt fallback). Manual `stateroot resume` is the last fallback, not the primary reliability mechanism.

| Harness | Delivery | Notes |
| --- | --- | --- |
| Claude Code, Codex, Kimi, Devin | Automatic | Session-start inject; first prompt retries if that missed |
| Cursor | Automatic | `sessionStart` injects initially; the first successful `postToolUse` restores an armed identity after compaction (`beforeSubmitPrompt` and `preCompact` cannot inject context) |
| Kimi Code | Automatic | Identity rides UserPromptSubmit |
| OpenClaw | Automatic | `before_prompt_build` pulls digest stdout |
| OpenCode, OMP, pi | Automatic | Generated plugin consumes hook stdout on the first prompt |
| Gemini CLI | Automatic | Session-start only (no prompt-submit fallback) |
| Antigravity | Automatic | `SessionStart` injects via the `injectSteps`/`systemMessage` envelope; on hosts that never fire `SessionStart`, the first-seen `PreInvocation` delivers once — at any invocation number |
| Hermes | Automatic | Native YAML shell hooks in `~/.hermes/config.yaml`; `pre_llm_call` injects `{context: digest}` into the turn's user message |
| VS Code Copilot | Automatic | Session-start inject via `additionalContext`; one `~/.copilot/hooks` file serves VS Code + Copilot CLI |
| Crush, Zero, ChatGPT Dot | Degraded | No verified injection — run `stateroot resume` |

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

## Hermes

- Hooks: a `hooks:` block merged into the harness's own `~/.hermes/config.yaml` (`HERMES_HOME/config.yaml`) — the file also carries `mcp_servers` and operator settings, so the installer merges and never replaces; the pre-edit original stays at `.bak`.
- `pre_llm_call` is the injection channel (canonical `user_prompt_submit`): its `{context: …}` stdout is injected into the turn's user message. `on_session_start` fires only for brand-new sessions and its return is dropped by the harness, so it prints (federation syncs run) but never marks delivery.
- `on_session_end` is per-turn (canonical `stop`); `on_session_finalize` is the session teardown (canonical `session_end`).
- Hermes gates each (event, command) hook pair behind its own first-use consent prompt — the operator answers it in the harness on first run. The installer never sets `hooks_auto_accept`; that choice belongs to the operator, and StateRoot adds no consent step of its own.
- A `delegate_task` child is its own agent with its own session: its evidence is captured under its own native session id, and it never consumes its parent's identity digest or finalizes its parent's session.
- Proof level: fixtures and parser/wire tests. Native Hermes app execution remains unrun.

## Antigravity

- Hooks: `config/hooks.json` named groups under the harness home. `SessionStart` is the real conversation boundary (once per conversation, before the first model invocation); `PreInvocation` fires before EVERY model call and stays mapped for legacy hosts that never fire `SessionStart`.
- Injection envelope on stdout: `{"injectSteps": [{"systemMessage": {"systemMessage": "…"}}]}` — persistent system-message steps, not discarded text.
- Delivery is gated by per project+conversation state, not the host's invocation counter: a conversation StateRoot first observes at any invocation number (late attach, resumed) still receives its one first delivery; ordinary mid-turn repeats for an already-delivered, unchanged identity stay silent; an identity change re-delivers.
- Project routing reads the payload's `workspacePaths` array (preferring the entry inside an initialized project); `transcriptPath` locates the native transcript at session boundaries.
- `PostToolUse` with a non-empty `error` durably captures the full authorized tool-result/error payload before the failure checkpoint. A `Stop` that is explicitly not fully idle (`fullyIdle: false` — background commands still running) retains evidence without queuing a completion boundary; a later idle Stop can queue finalization. Safe entrypoints retry already-retained jobs; they do not declare still-running work complete.
- Proof level: fixtures plus real StateRoot CLI wire tests passed; native app/CLI/IDE host execution remains unrun.

## StateSmith (native)

- No project files needed: this skill ships with the platform and is seeded into the skill catalog by the server bootstrap (`DEFAULT_SKILL_SPECS` in `app/core/services/default_skill_bootstrap_service.py`, install strategy `first_party_bootstrap`).
- Its execution locality is `client_local` — the scripts execute the `stateroot` binary on the user's machine — so the server seeds it into the catalog but never attaches it to server-side runs.

## Re-install / Update

1. Re-run `stateroot skill install --harness H` after upgrading the CLI to refresh stubs.
2. Never edit installed stubs by hand; fix the templates in `assets/` (or the CLI) and re-install.
