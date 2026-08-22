# Changelog

All notable changes to StateRoot. Format loosely follows Keep a Changelog;
StateRoot is pre-1.0 and milestones land as minor versions.

## v0.1.5 — 2026-08-23

- The resume/hook digest is now BOUNDED (it reached ~67KB on real projects):
  Shared Rules inlines small rules whole but renders oversized ones as
  title + heading outline + a `rules show` pointer, with an 8000-char
  section budget collapsing later rules to title + pointer; the Federated
  Skills list dedupes packages discovered from multiple scopes (count and
  40-line cap apply to the deduped list); the work-since-handoff
  conversation tail is the last 8 entries at ≤ 400 chars each; and repo
  docs in the observed context pack share a 16000-char total budget with
  past-budget docs title-listed (`capped — N more docs on disk`). The work
  body (objective, active plan, next actions, handoff fields) stays fully
  inline, and every cut leaves a pointer or marker — never silent loss.
- Canonical sessions now cover EVERY harness: `stateroot session sync`
  extracts full-fidelity canonical timelines from all eight transcript
  stores — claude (`~/.claude/projects/**`), codex (rollout + archived
  stores, deduped active-first), kimi (wire files + session index —
  stateroot's own harness, dogfooded), openclaw, cursor and hermes (sqlite
  stores, opened immutable) — alongside pi and dsh. Verbatim content, no
  caps; native ids/parents kept where the format has them (claude
  uuid/parentUuid, tool-call correlations); injected envelopes, thinking
  blocks, harness control records, and unknown types land as `meta` with
  `native_type` (cursor's unverified `toolResults` are preserved raw).
  Transfer targets are unchanged (pi/dsh only).
- Persona injection: removed the wall-clock staleness trigger
  (`SESSION_STALE_MINUTES`) from the injection scheduler. Long agent turns
  routinely idle past any fixed threshold, so the time rule re-injected the
  FULL persona block on nearly every user message. New sessions are now
  recognized by session keys only (harnesses with session ids); the remaining
  FULL triggers are unchanged (first contact, session_start, content change,
  compaction boundaries, first prompt of a session). The digest-delivery
  ledger keeps its own staleness window for resume-dedupe — untouched.
  Follow-up: the COMPRESSED pointer now carries a one-line voice anchor
  extracted from the persona text (`name — tagline (unchanged since last
  full injection: <path>)`) so sparse injections re-anchor *behavior*, not
  just a file path, and its cadence tightened from every 15th to every 8th
  prompt (~30 tokens a pointer).
- Central plan artifacts + lifecycle: `stateroot plan record --file|--stdin
  [--title] [--from]` / `list` / `show` / `approve` / `activate` / `done` /
  `abandon`. Plans live at `.stateroot/plans/<id>.md` (verbatim markdown)
  plus a `stateroot.plan.v1` sidecar with provenance (`root_ref` from
  `refs/stateroot/latest`, author harness, source path). Lifecycle
  `draft → approved → active → done` (`abandoned` from any open state), at
  most one active — activating demotes the current active plan to approved
  with a recorded note; wrong-state transitions error clearly. The resume
  digest gains `## Active Plan` before `## Plan State` — pointer +
  directive only (executor: "execute it as written; do not re-plan or
  re-explore"; draft-only: "refine the plan file; do not implement yet"),
  never the body; the transcript Plan State stays as fallback and is
  suppressed while a central plan exists. `handoff write` auto-attaches
  `plan_ref {id, title, status}` for active/approved plans, and every
  lifecycle event writes an episodic lineage note. No hook tool-gating in
  v1 — strings above the runtime.
- Session canon & transfer: `stateroot session sync|list|show`
  canonicalizes Pi and DSH sessions into `.stateroot/local/sessions/` as
  `stateroot.session.v1` JSONL — full-fidelity entries with no content caps,
  unmapped native types kept as `meta` with `native_type`, local-only (never
  pinned into roots, same rule as `local/memory.sqlite`). New Pi and DSH
  transcript readers (tree-linearizing Pi v3, torn-tail/seq-gap-aware DSH
  v0 with chunk-row accounting) feed the import pipeline too.
  `stateroot session transfer <id> --to pi|dsh [--dry-run]` writes a real,
  resumable session into the target harness's native store — Pi v3 with a
  fresh linear spine (branches flatten with provenance), DSH v0 with
  contiguous seq and a clean tail — never mutating the source, refusing to
  clobber, and always printing the fidelity report (native / adapted /
  dropped). DSH `.jsonl.zstd` artifacts are counted and skipped (no zstd in
  the dependency tree).
- Git-style extension subcommands: any executable named `stateroot-<name>`
  on PATH runs as `stateroot <name> [args…]` — an agent can write a small
  script and the CLI immediately grows a command. Discovery scans PATH (unix
  exec bit, Windows `PATHEXT`, first hit wins, the bare `stateroot` binary
  excluded); extensions run with inherited stdio plus an env contract
  (`STATEROOT_HOME`, `STATEROOT_VERSION`, and `STATEROOT_PROJECT_DIR` /
  `STATEROOT_PROJECT_ID` inside a project; `STATEROOT_DELEGATION_DEPTH`
  passes through untouched) and their exit code becomes the CLI's. Unknown
  subcommands now get a clap-styled did-you-mean over builtins and
  extensions with exit code 2; builtins always win over same-named
  extensions. `stateroot ext list` shows what is discovered and marks
  `shadowed builtin (ignored)` entries.
- `stateroot delegate --to <harness> --task "<bounded task>"` spawns another
  harness CLI as a subagent: piped stdout with a timeout (`--timeout-secs`,
  default 600), a bounded stdout tail back to the caller
  (`--max-output-chars`, default 8000), `--skill`/`--ambient-skills`
  passthrough per the registry policy, and `--json` for agent callers. Every
  run persists a full log plus a `stateroot.delegation.v1` record under
  `.stateroot/delegations/` and appends an episodic lineage note. Unknown,
  handoff-only, or unprobed harnesses are loud errors listing the cli-mode
  harnesses; a failed child exits with its own code and stderr tail; a
  timeout kills the child and records `timed_out`.
  `STATEROOT_DELEGATION_DEPTH` caps recursion — at depth ≥ 2 a subagent may
  not spawn further subagents. The piped spawn-and-capture helper is now
  shared between `delegate` and init synthesis (`init` seeding behavior is
  unchanged), and the skill-router's delegation route points at `delegate`.
- `stateroot init` now **seeds** `.stateroot/` from what the repo already
  declares instead of leaving placeholders: objective from the README title +
  first paragraph (into `project/state.json` and `project/objectives.md`),
  next actions from `TODO.md` checkboxes / roadmap bullets, observed memory
  facts (layout, docs, git origin, recent commits) under `## Seed (observed
  at init)` in `memories/MEMORY.md`, and a seq-1 `handoffs/current.json`
  labeled `"provenance": "observed"`. Writes are placeholder-only — user
  content is never overwritten — and empty repos stay empty.
- Opt-in LLM enrichment: `stateroot init --synthesize [--synthesize-with
  <backend>]`. Auto order probes local harness CLIs first (claude, codex,
  kimi, gemini, … via the registry delegation specs, piped stdout, no
  skills), then the DeepSeek/OpenAI API keys. Synthesized seeds replace only
  same-origin init-seed fields and are labeled `synthesized — unverified
  (<backend>)`; synthesis problems never fail `init`.

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
