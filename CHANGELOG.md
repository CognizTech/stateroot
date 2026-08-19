# Changelog

All notable changes to StateRoot. Format loosely follows Keep a Changelog;
StateRoot is pre-1.0 and milestones land as minor versions.

## Unreleased

## [0.1.4] — 2026-08-19

- Reliable cross-harness identity delivery: session-start and first-prompt
  share a machine-local ledger (`.stateroot/local/digest-delivery.v1.json`).
  Automatic harnesses inject persona + USER.md onto the first usable prompt
  even with no handoff; Cursor recovers on `beforeSubmitPrompt` when
  session-start stdout is ignored; Kimi Code stays on UserPromptSubmit;
  OpenClaw `before_prompt_build` pulls `user_prompt_submit` stdout.
  Hermes / Copilot / Crush stay honestly degraded. Re-run `stateroot install`
  and `stateroot skill install` after upgrading.
- `stateroot harness run pi` launches Pi with ambient cross-harness skill
  discovery disabled by default, while `--skill <slug>` adds only selected
  StateRoot packages. Use `--ambient-skills` only to opt back into Pi's native
  `.agents/skills` discovery.
- Verified Pi harness support: the generated `~/.pi/agent/extensions/stateroot.ts`
  extension injects identity on `before_agent_start` via Pi's session-message
  return (`customType: "stateroot"`, `display: false`). Session-start stdout is
  not treated as delivered. Honors `$PI_CODING_AGENT_DIR`. Re-run
  `stateroot install` after upgrading.
- MCP `memory_save` / `memory` now honor `scope`: `scope=user` +
  `target=memory` writes `~/.stateroot/memories/MEMORY.md`, while
  `scope=project` writes the current project's MEMORY.md. User-global
  memory is indexed for cross-project recall; the response reports the
  actual routed scope. Legacy user-global `memory.md` migrates globally
  instead of leaking into whichever project happened to run first.
- Session skill and harness stubs forbid truncating `stateroot resume` /
  hook digests (`| head`, `| tail`, pagers, invented `--budget`). The
  full digest is the state of record.
- Resume/hooks inject a **local observed context pack**: repo-root
  `README.md` / `PROGRESS.md` / `ARCHITECTURE.md` / `TODO.md`, plus
  overview/use-case markdown, `.stateroot` project docs, and a top-level
  tree. Optional LLM synthesis runs only when `DEEPSEEK_API_KEY`
  (`deepseek-v4-flash`, preferred) or `OPENAI_API_KEY` (`gpt-5.6-luna`)
  is set; it can compile that pack even with no transcripts.
- Snap/root trees honor root `.gitignore` **and** `.staterootignore` again
  (plus hardcoded `.git/` and `.stateroot/local/`). Dropping `.gitignore`
  from that union was an unauthorized regression.
- Dev CI (`fmt` + `clippy` + `test` on Ubuntu and Windows) runs again on
  `main` pushes, pull requests, and manual dispatch. Rolling preview
  publishes the `nightly` prerelease on `main` pushes.
- `stateroot self-update --tag nightly` installs the rolling preview;
  `--tag v0.1.3` (or `0.1.3`) installs this production release. Bare
  `self-update` still follows GitHub latest production only.
- Honor documented harness config homes (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`,
  `KIMI_CODE_HOME`, `GROK_HOME`) at install, uninstall, and Codex transcript
  import. Hook commands use an absolute `stateroot` path when the installer
  binary is the real CLI.
- Embedded session skill teaches `snap` / `revert` / `fork` lineage and
  replaces the nonexistent `stateroot search` with `stateroot memory recall`.
  Resume and session-start hooks show current root, last actor, verified
  tree delta, and an active shared learning.
- Cross-harness learnings: hook digests now inject the full durable
  preferences set; workspace/domain scopes are documented and tested.
- Agent Plugins 1.0 wrapper at `agent-plugin/` (MCP stdio + marketplace
  skill). Harness hooks still require `stateroot install`.
- Read-only `stateroot observations list|show|search` and MCP
  `observations_list` over the spool. Append-only `stateroot transplant`
  copies evidence between initialized projects with receipts on both sides.
- Session-start hooks inject persona + USER.md even outside a `stateroot
  init` project. Capture/checkpoint stay project-scoped. Cursor's
  `~/.cursor/AGENTS.md` is not loaded into Agent chats; identity has to
  ride the hook `additional_context` channel.
- Marketplace skill at `skill/` is bootstrap-only: official install, then
  required global `stateroot setup`. It expires once the CLI is installed
  and harnesses are interfaced. Session protocol stays in the built-in
  skill from `stateroot-cli/assets/stateroot-skill/` (`stateroot skill show
  stateroot`). `stateroot init` remains project-scoped, not a setup
  substitute.

## [0.1.2] — 2026-08-17

- Linux release binaries are linked for **glibc 2.17** (Ubuntu 16.04,
  Debian 9, RHEL 7, and newer) instead of GitHub `ubuntu-latest` (glibc
  2.39). Tagged release jobs are on; push/PR tests and the rolling
  preview stay off.

## [0.1.1] — 2026-08-13

- README aligned with https://stateroot.dev product copy (what it shares,
  what it snapshots, install, quickstart). Docs stay on the site.
- Tagged Windows CI builds the MSI without Authenticode unless
  `AZURE_ARTIFACT_SIGNING_ENABLED=true` on the `release` environment.

- README and public docs at https://stateroot.dev (site source is not in this
  repository). Maintainer notes stay under repo `docs/`. Discord:
  https://discord.gg/SfbKEPRD7
- Tagged releases attach Linux + Windows binaries, `StateRootSetup-x64.msi`,
  install scripts, and `checksums.txt`. Dev CI (fmt/clippy/test and the
  rolling `nightly` prerelease) is skipped for commits marked `[skip tests]`
  and does not re-run on `v*` tags.
- Remove unused `login` / `logout` / `repo` / `sync` / `run` / `runs`
  commands, OAuth/keyring auth, and server-side `remove`. Agentic synthesize
  uses a local API key only.
- Scalable memory layers: curated hot-apex `memories/MEMORY.md` (add/replace/remove,
  8000-char write cap) and USER.md (4000-char tool cap); legacy `.stateroot/memory.md`
  migrates once. Distill / session_end compile into wiki inbox + pages instead of
  activating learnings. Digest injects wiki `index.md` + recent `log.md` (not page
  bodies). Local SQLite FTS at `.stateroot/local/memory.sqlite` powers `memory_recall`.
  CLI: `stateroot memory …`, `stateroot wiki …`. MCP: `memory`, `wiki_show`. Soul /
  USER injection and resolve order unchanged.
- CI: drop macOS (`macos-latest` / `aarch64-apple-darwin`) from preview and
  tagged release build matrices — Linux + Windows only until a macOS release.
- Dual-mode context compiler: agentic when a local synthesis API key is
  present (OpenAI-compatible); otherwise a full uncapped deterministic
  digest. Wired into session_start hooks and `resume`. No agent-facing
  character truncation.
- Soul / skill / memory activate immediately — proposals are an optional audit
  log, not a blocking gate. Foreign skills land active and project. Session_end
  runs wiki ingest (deterministic inbox; agentic when keyed), not learnings dump.
- No keyword classifiers for learnings (`learning_category` defaults to
  `general`). Distill mines into the wiki inbox without soul/skill/memory
  routing. Scope comes only from flags (`--user` / `--workspace` /
  `--domain` / project).
- Shared rules digest injects **full** product-intent + imported rule bodies;
  `rules sync` on session_start; one-line Cursor rules import (`MIN_CHARS=1`).
- Workspace and domain learnings scopes; McpPull harnesses print/capture the
  digest (OpenClaw `before_prompt_build` injects); Copilot/Crush get
  instruction files. Sync ignore is `.staterootignore` only (plus hardcoded
  `.git/` and `.stateroot/local/`) — root `.gitignore` is not unioned for
  snap/root trees. Handoff quality is warn-not-refuse.
- `config.installed_harnesses` counts as detected for federation projection
  in PATH-less sandboxes.

## [0.1.0] — 2026-08 (first public release)


The local-first, open-source StateRoot variant: a fully local substrate for
AI-assisted work — no server anywhere.

### Continuity core (M1)

- New self-contained workspace (`stateroot-core` + `stateroot-cli`), seeded by
  copy-and-own lifting of the StateSmith monorepo's proven local modules:
  six harness transcript readers (Claude, Codex, Cursor, Kimi, OpenClaw,
  Hermes) + bundle builder, `.stateroot/` local store, skill & MCP
  federation engines, harness installers, ignore rules, canonical hashing.
- Fully offline command surface: `init`, `import`, `resume`, `checkpoint`,
  `handoff write/list/show/accept`, `log`, `status`, `doctor` (local checks),
  `hook`, `install`/`uninstall`, `setup` (harnesses + skills), `skill` and
  `mcp` federation.
- BSL-1.1 license with automatic conversion to Apache-2.0 on the change date.

### Git-backed roots (M2)

- Roots as `git commit-tree` of the working state (project files honoring
  `.gitignore`/`.staterootignore` + the `.stateroot/` tree), stored under
  `refs/stateroot/roots/` — the user's branch log and index never touched;
  non-git folders auto-`git init`.
- Transitions (`.stateroot/transitions/`) linking roots; receipts rendered
  from transition + the git delta (verified tier for free).
- `snap`, `log` (lineage, coverage, fork markers), `show`, `diff --content`,
  append-only `revert`, `fork` (branch materialization), `receipt`.
- Coverage honesty: `files: N pinned` vs `state-only`.

### Identity, learnings, synthesis (M3)

- Canonical soul at `~/.stateroot/soul/` (history snapshots, provenance),
  project overlay, imports (OpenClaw/Hermes/file), deterministic generate,
  per-harness projections.
- Scoped learnings (category-md, user + project), deterministic distiller,
  lifecycle candidate → proposed → active; memory notes with serve-time gates.
- Local proposals engine (`.stateroot/proposals/`) as the shared approval
  gate; `learn record` review-loop entry (classify → proposal, never direct).
- `stateroot synthesize` — direct OpenAI-compatible synthesis with your own
  key (DeepSeek/OpenAI/Ollama/litellm), hash-idempotent governance, honest
  unavailability without a key.

### Federation + shared MCP tools (M4)

- Foreign skills arrive quarantined as candidates; activation only via
  proposals (`skill promote` → `proposals approve` → projected into harness
  roots).
- `stateroot mcp-stdio`: local stdio MCP server exposing `memory_save`,
  `memory_recall`, `learn_record`, `skill_propose`, `soul_read`,
  `learnings_list` — external-harness writes quarantined
  (session-candidate/private) until approved.
- Harness instruction blocks carry the two-sentence self-improvement
  guidance; install registers the stdio server into harness configs.

[0.1.3]: https://github.com/CognizTech/stateroot/releases/tag/v0.1.3
[0.1.2]: https://github.com/CognizTech/stateroot/releases/tag/v0.1.2
[0.1.1]: https://github.com/CognizTech/stateroot/releases/tag/v0.1.1
[0.1.0]: https://github.com/CognizTech/stateroot/releases/tag/v0.1.0
