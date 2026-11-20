# Harness Install Layout

`stateroot skill install --harness H` (also run by `stateroot init`) writes the per-harness stubs from `assets/` into the project. Harness identifiers are lowercase; the server-side canonical set includes `planner`, `cursor`, `codex`, `kimi`, `opencode`, `hermes`, `openclaw`, `dsh`, `statesmith`, and others in the harness registry contract.

After upgrading the CLI, run `stateroot install` (machine hooks/plugins) **and** `stateroot skill install` (project stubs) so identity injection stays current.

## Identity delivery

Claude Code and Codex have different hook output limits. A fitting digest
arrives inline. When it exceeds the host limit, StateRoot sends working context
first and includes exact read commands for any required continuation.
See [oversized hook delivery](protocol.md#continue-an-oversized-hook-delivery).

Automatic harnesses inject persona + USER.md + work context onto the first usable prompt (session-start and/or first-prompt fallback). Manual `stateroot resume` is the last fallback, not the primary reliability mechanism.

| Harness | Delivery | Notes |
| --- | --- | --- |
| Claude Code, Codex, Kimi, Devin | Automatic | Session-start inject; first prompt retries if that missed |
| CodeBuddy | Automatic | Session-start and prompt-submit JSON context; PreCompact injects, PostCompact stays observational |
| Cursor | Automatic | `sessionStart` injects initially; the first successful `postToolUse` restores an armed identity after compaction (`beforeSubmitPrompt` and `preCompact` cannot inject context) |
| Kimi Code | Automatic | Identity rides UserPromptSubmit |
| OpenClaw | Automatic | `before_prompt_build` pulls digest stdout |
| OpenCode, OMP, pi | Automatic | Generated plugin consumes hook stdout on the first prompt |
| Gemini CLI | Automatic | Session-start only (no prompt-submit fallback) |
| Antigravity | Automatic | `SessionStart` injects via the `injectSteps`/`systemMessage` envelope; on hosts that never fire `SessionStart`, the first-seen `PreInvocation` delivers once — at any invocation number |
| Hermes | Automatic | Native YAML shell hooks in `~/.hermes/config.yaml`; `pre_llm_call` injects `{context: digest}` into the turn's user message |
| VS Code Copilot | Automatic | Session-start inject via `additionalContext`; one `~/.copilot/hooks` file serves VS Code + Copilot CLI |
| DeepSeek Harness | Automatic | Generated cordis plugin injects via `agent/pre-step`; turn stop checkpoints, compaction re-anchors |
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

## CodeBuddy

- Global instruction block: `~/.codebuddy/CODEBUDDY.md`; the project convenience block in `AGENTS.md` is also native fallback memory when no project `CODEBUDDY.md` exists.
- Hooks: `.codebuddy/settings.json` under the native home, relocated by `CODEBUDDY_CONFIG_DIR`. `SessionStart`, `UserPromptSubmit`, and `PreCompact` inject native `hookSpecificOutput.additionalContext`; `PostCompact` only records evidence.
- MCP: user scope `~/.codebuddy/.mcp.json`; project scope `<project>/.mcp.json`, both under `mcpServers`.
- Skills: native `.codebuddy/skills/` packages participate in discovery and projection alongside `.agents/skills/`.
- Sessions: `session sync --harness codebuddy` imports native `projects/**/*.jsonl` message, tool-call/result, and metadata records into the canonical store.
- Plans and todos: session-attributed `planFilePath` files under `.codebuddy/plans/` enter the central store as drafts; current per-session task files under `.codebuddy/tasks/` federate without inventing plan links.
- Memory: global and native-session-linked project auto-memory plus project team memory are imported. `--push` writes the managed project brief to `.codebuddy/rules/stateroot.md`, including current learnings.
- Delegation: `stateroot delegate --to codebuddy --task "…"` uses `codebuddy --print` with the registry's non-interactive permission policy.

## Command Code

- Instruction: `~/.commandcode/AGENTS.md` (user tier) receives the managed one-agent block; project memory is `<project>/AGENTS.md` or `<project>/.commandcode/AGENTS.md` (first existing wins).
- Hooks: `~/.commandcode/settings.json`, written with **no `matcher` key** — Command Code never matches on `SessionStart`/`Stop`, and an omitted matcher is what matches every tool. Only the four implemented events are registered: `SessionStart`, `PreToolUse`, `PostToolUse`, `Stop`. There is no `UserPromptSubmit`, `PreCompact`, `SessionEnd` or subagent hook, so none is installed.
- Delivery: `SessionStart` injects the digest through `hookSpecificOutput.additionalContext` (first user turn). Compaction recovery rides the same channel on the next `PostToolUse`, armed from the transcript's own `{"type":"compaction"}` entry (tail-window read, compared against the last delivered FULL). `stateroot resume --harness commandcode` remains the manual fallback.
- MCP: user scope `~/.commandcode/mcp.json` under `mcpServers` (stdio entries default when `transport` is absent); project scope `<project>/.mcp.json` (shared with CodeBuddy — same root key, deduped by server name).
- Skills: native `~/.commandcode/skills/` and `.commandcode/skills/` packages participate in discovery and projection alongside the automatically discovered `.agents/skills/`.
- Sessions: `stateroot session sync --harness commandcode` reads `~/.commandcode/projects/<slug>/<session-id>.jsonl` (header `type:"session"` + tree entries), and explicitly skips the `.checkpoints.jsonl` / `.prompts.jsonl` sidecars that share the extension.
- Plans and todos: `~/.commandcode/plans/*.md` is a global pile, so a plan enters the central store as a draft only when one of this project's Command Code sessions was active within the attribution window of its mtime. There is no on-disk todo store; `task_create` / `task_update` / `task_stop` / `todo_write` calls are replayed from the transcript into per-session todos without inventing a plan link.
- Taste: `.commandcode/taste/taste.md` and `.commandcode/taste/<category>/taste.md` (project) plus the user taste store are pulled in as learning **candidates** (`stateroot learnings import-taste`, also run at session start). Pull-only — Command Code owns those files and StateRoot never writes them.
- Delegation: `stateroot delegate --to commandcode --task "…"` resolves the launcher in registry order — `command-code`, then `cmdc`, then `cmd` — and runs the resolved executable with `-p --yolo` (headless blocks mutating tools without `--yolo`). On native Windows the `cmd.exe` system shell is never launched as the harness: if only the shell resolves, Command Code is reported unavailable. On WSL the `cmd` shim *is* Command Code and is accepted as the last candidate.
- Detection: `.commandcode` on disk, or `command-code` / `cmdc` on PATH. Bare `cmd` is deliberately not probed for detection — on Windows `where.exe cmd` resolves the Windows shell.
- Proof level: format verified against live 0.2.x transcripts and the bundled docs on this machine; install/uninstall, session sync, plan/todo/taste federation, and digest injection are covered by tests and live checks.

## DeepSeek Harness

- Hooks: no hook config file exists. `stateroot install` writes a dependency-free cordis plugin at `$DSH_HOME/stateroot/hooks.mjs` and merges a marked block into `$DSH_HOME/cordis.patch.yml` — the home-level patch layer applied to every profile. The block inserts the plugin (`stateroot-hooks`) and an `@deepseek-ai/dsh-mcp-client` instance (`stateroot-mcp`).
- Delivery: the plugin injects the digest through the `agent/pre-step` waterfall, retries failed delivery and consumes prompt fallback output. Compaction recovery is awaited on the next model step, including within a turn. Full tool observations queue with native call IDs; turn stop and session finalization wait for submitted captures. Failures appear in DSH's log. `stateroot resume --harness dsh` remains the fallback.
- Instruction: `$DSH_HOME/AGENTS.md` is DSH's user-global instruction file, read before workspace files.
- MCP: the patch entry runs `stateroot mcp-stdio`; no npm install is needed because `@deepseek-ai/dsh-mcp-client` resolves from DSH's installation scope. `stateroot mcp scan` reads DSH's own mcp-client patch entries; sync projects pooled global servers into separate owned entries, preserving operator configuration and reporting conflicts. Native transports are stdio and Streamable HTTP; unsupported transports/names are reported explicitly.
- Sessions: `stateroot session sync --harness dsh` reads v0 and current v4, including default `.jsonl.zstd` artifacts and concatenated frames. `session transfer --to dsh` writes v0.
- Plans and todos: DSH plan mode stores no plan files; `exit_plan_mode` carries the markdown plan in its tool arguments, and federation drafts it from the session log. The latest `todo/write` whole-list snapshot, including an empty list, enters shared todos. Existing verified session bindings supply plan links; task wording never creates them.
- Memory: DSH has no native memory file. Its sessions reach recall through the canonical session store, and curated memory/learnings arrive in the digest.
- Delegation: `dsh --profile headless` needs a provider credential DSH can resolve (its credential store, profile config, or `DEEPSEEK_API_KEY` in that process). The interactive `web` profile's account session is not inherited. StateRoot's optional `DEEPSEEK_API_KEY` synthesis compiler is a separate consumer and is not required for install/setup.
- Proof level: verified live against DSH 0.2.2-rc.1 (install, dump-config, MCP tools, digest injection, stop checkpoint, v4 zstd sync); source reviewed at 0.2.2-rc.3.

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
