# Changelog

All notable changes to StateRoot. Format loosely follows Keep a Changelog;
StateRoot is pre-1.0 and milestones land as minor versions.

## Unreleased

- **Hook-config writes are atomic and self-protecting.** Every harness
  config write (TOML hooks, JSON hooks, plugin files, uninstall strips) now
  goes through one atomic write (tempfile + fsync + rename; a crash
  mid-write leaves the old file intact), and the TOML installer warns when
  the existing config doesn't parse — appending still works textually, but
  a broken config breaks the harness's whole session and the user must
  know. `stateroot doctor` reports the store footprint (total, episodic
  journal, search index, spool) so growth is never invisible.
- **Harness-native plans federate into the store.** A plan authored in a
  harness's own plan mode used to strand in the harness home, invisible to
  every other harness (the cursor-plan continuity gap). Each harness now
  pulls its own native plan dir (cursor `~/.cursor/plans/*.plan.md`, claude
  `~/.claude/plans/`) into the project at its session boundaries — as a
  draft with provenance, deduped by content hash, refreshed in place while
  draft, never overwritten once approved or active. `stateroot plan sync`
  runs the explicit pass.
- **One project across the Windows↔WSL seam.** Registry keys, hook payload
  paths, and project-root resolution now fold `D:\foo` and `/mnt/d/foo`
  (plus verbatim prefixes and `\\wsl$` UNC forms) into one identity, and
  the CLI attaches from subdirectories by walking up — Windows Cursor and
  WSL Claude hit the same store instead of splitting one project into two.
- **Self-update follows your channel.** Plain `stateroot self-update` — and
  therefore the scheduled background update — now tracks the running
  binary's channel: a dev/nightly build updates to the latest rolling
  preview, a release build to the latest production release, and `--tag`
  always wins (explicit channel switch in either direction). Channel
  detection reads the binary's true identity (the git-describe `-dev.N`
  suffix), not the crate version. Dev builds compare against the nightly
  release's display name by (base, counter) — a local source build with a
  higher counter is never clobbered — and the production compare is by base
  version, so a dev build is offered a genuinely newer production release
  but never "upgraded" down to its own base. Previously a nightly user got
  no updates at all and a dev binary was never told about new production
  releases.

## v0.1.10 — 2026-08-27

- **Team-ready state: the shared/local boundary is now code.** Teams that
  commit `.stateroot/` asked the right questions — what travels, what
  fights. `stateroot init` (and `install` for existing projects) now writes
  `.stateroot/.gitignore` classifying the store: shareable state of record
  (goal, plans, learnings, rules, wiki, memory pages, handoff history,
  skill pool, soul overlay, transitions) vs machine-local/per-person
  (search, spool, delegations, cursors, hot-apex `MEMORY.md` and
  `episodic.jsonl` — per-person lens and private journal — the current
  handoff, and `roots/`, whose lineage travels via `refs/stateroot`, not
  files). `.stateroot/.gitattributes` merges any committed journal by
  union. `stateroot doctor` warns when a local-set path is tracked in git
  (`collab boundary` check). README gains a "Sharing a project" section
  with the teammate flow: clone, install once, open any harness.
- **CI flake exorcised: the scheduled-update worker vs the request-counting
  mock.** `updater_never_runs_on_hook_but_runs_on_status` asserted a v0.1.9
  premise that v0.1.9 itself had retired: hooks *do* fire updates now —
  detached and scheduled. When the worker's GET landed before the mock's
  verification (a race that favored slow CI runners), the count failed.
  The worker now honors `STATEROOT_DISABLE_SCHEDULED_UPDATE` (test/CI
  seam); the test documents that the hook's inline fast-path is the
  property under test. Flaked twice on GitHub, never locally.
- **Shared Capabilities in every digest — delegate, never refuse.** The
  reference-only pool (imagegen → codex, automate → cursor, …) existed on
  disk, but its triggers required the user to *already know* to ask for
  delegation — so an agent asked "can you do X" answered from its own tool
  list and refused (the four-harness imagegen trial: codex native yes,
  kimi offered delegation, cursor discovered-then-claimed, claude flat
  no). Every resume/hook digest now carries a bounded **Shared
  Capabilities** section (8 entries + a "+N more" tail; empty pool → no
  section), the session skill gains a hard rule — name the path and offer
  to delegate instead of answering "I can't", never claim another
  harness's capability as your own — and the generated reference skills
  now fire on the capability ask itself, not only on explicit delegation
  requests. (A `wrapper_version` in the projection metadata makes template
  changes regenerate existing wrappers — the old dedup compared only the
  source package digest, so v1's refusal-shaped wording would have lived
  on disk forever.)
- **Capture-chain honesty.** reqwest now trusts the OS certificate store
  (`rustls-tls-native-roots`, webpki fallback kept) — enterprise GitHub via
  `STATEROOT_GITHUB_API_BASE`, corporate MITM proxies, and private PKI no
  longer fail TLS the way Mozilla-bundle-only clients do. The vestigial
  server-sync writers are gone: hook heartbeats, hook observation ops, and
  handoff-accept ops were appended to `.stateroot/outbox.jsonl` for a server
  this local-first variant does not have — with no drain anywhere, the queue
  grew forever, silently. And `stateroot doctor` gains a **continuity
  chain**: per hooked harness, a duplicate-block lint on the managed hook
  config plus the last captured checkpoint attributed to it (hook
  checkpoints now record the firing harness id; older `cli`-attributed
  records fall back to note parsing), and a legacy-outbox warning when a
  pre-fix queue still exists (safe to delete).
- **Soul federation — personality authored anywhere lands everywhere.**
  `stateroot soul sync` is a two-way bridge between the canonical soul and
  harness-native persona files (openclaw `IDENTITY.md` + `SOUL.md`, hermes
  `SOUL.md`): a persona edit made inside OpenClaw or Hermes is adopted into
  the canonical soul (history-snapshotted) and pushed outward to the other
  harnesses; a canonical edit (`soul propose` from any harness) is pushed
  back into the native files (backup first, `stateroot:synced` marker).
  Three-way baseline hashing keeps round trips stable; both-sides-changed
  is a surfaced conflict in the digest, resolved explicitly with
  `--accept-theirs|--accept-mine <source>` — never silently. Session hooks
  run one pass per hour of activity automatically, so the bridge needs no
  command. A persona change from any harness now re-anchors every other
  harness on its next session (the adoption changes the identity hash,
  which itself forces a FULL injection).
- **Persona re-injection after compaction — delivered where it can land.**
  On harnesses whose compact-boundary stdout is discarded (kimi ignores
  PreCompact return values outright), the scheduler printed a FULL identity
  into the void and marked it delivered — the state believed the persona had
  been refreshed while the model got nothing, and post-compaction sessions
  decayed to the 30-token pointer. Compact boundaries on those harnesses now
  only arm a `pending_compaction` flag, and the first event that can
  actually carry identity (prompt_submit on kimi/claude, session_start on
  cursor/gemini) injects FULL, bypassing dedupe. `compact_injection`
  harnesses (claude-code) keep their working channel — the bounded digest
  already re-injects identity at compaction — so no redundant FULL arms
  there.
- **kimi `PostCompact` wired** into the event map and installed hooks.
- **Scheduler state is per-key** (`~/.stateroot/local/persona-injection/
  <sha256(key)>.json`): concurrent hooks from multiple harnesses no longer
  clobber each other's counters through the whole-map load/save race (a live
  kimi session's record was observed fossilized four days old while its
  hooks fired daily). State resets once on upgrade; each live session
  re-anchors with a single FULL.
- **`stateroot install` TOML hook idempotency fixed**: the strip matcher
  looked for a bare `command = "stateroot hook` prefix and never matched the
  absolute-path commands install actually writes, so every re-arm —
  including scheduled self-update's — appended another full set of
  `[[hooks]]` blocks (152 on the dogfood machine) and kept churning
  `config.toml` underneath running kimi sessions. Reinstall is a true no-op
  now and dedupes existing piles, bare/absolute/Windows command forms.
- **Embedded session skill frontmatter repaired**: the v0.1.9 description
  was an unquoted YAML scalar containing `agents: …`, which made harnesses
  skip the installed skill on parse.

## v0.1.9 — 2026-08-26

- **Reframed public copy**: "Switch harnesses. Keep the agent." — the README,
  GitHub About, stateroot.dev (landing, tagline, intro, `llms.txt`), and all
  skill descriptions (marketplace, CLI-embedded, plugin) now open by anchoring
  the AI-coding-agent domain in the first screen: cross-harness continuity
  across Claude Code, Codex, Cursor, Kimi Code, Pi, and DeepSeek Harness.
- **BREAKING: `stateroot delegate` is async-only.** The sync contract
  (blocking run, `--timeout-secs`, `--max-output-chars`, caller-receives-a-
  tail) is gone. `delegate --to H --task "…"` now writes a
  `stateroot.delegation.v1` record with `status: "running"` and a pid,
  launches a detached worker (the same binary in hidden `--_worker` mode,
  output redirected into `.stateroot/delegations/<file>.log`), and exits 0
  immediately. **No timeout anywhere and no blocking**: the harness runs to
  its natural end; the worker finalizes the record (`outcome:
  completed|failed`, `exit_code`, `duration_ms`) plus an episodic lineage
  note. Observation is pull-based — `stateroot delegate list` (live status
  `running|completed|failed|lost`; a dead pid with no final outcome is
  reaped to `lost`) and `stateroot delegate status <id>` (record + bounded
  log tail) — and completions surface in the digest's new `## Recent
  Delegations` section. The depth cap still refuses at
  `STATEROOT_DELEGATION_DEPTH >= 2` before anything is spawned.
- **Automatic scheduled self-update**: session-boundary hooks now fire a
  detached `stateroot self-update` whenever the release cache is stale
  (gated by `[update] check_interval_hours`, one worker at a time via a
  lock file). Updates keep machines current through agent activity alone —
  no command invocation and no agent action required. The digest's update
  notice stays as the visible layer; this is the layer that acts on it.
  Failures land in `<config>/update-scheduled.log`.
- **Memory federation** (`stateroot memory sync`): pull harness-native memories
  into the StateRoot pool as `observed` tier — claude
  (`~/.claude/projects/<slug>/memory/*.md`), codex (`~/.codex/memories/*.md`),
  and openclaw daily logs (`~/.openclaw/workspace/memory/*.md`). Claude and
  codex notes become wiki pages under `memories/pages/harness/<harness>/`
  (provenance header, content-hash dedup, conflicts preserved alongside as
  `title__hash8.md`); openclaw logs land in the episodic tier. `--dry-run`
  reports without writing. `--push` writes a compact managed brief
  (`<!-- stateroot:managed v1 -->`) into each harness's native memory home —
  only when the file is absent or already managed; an unmarked pre-existing
  file is reported as a conflict and left untouched.

## v0.1.8 — 2026-08-26

- `stateroot install` (and thus `self-update`'s re-arm) now refreshes the
  project convenience layers — the AGENTS.md block and harness command/rule
  stubs — for every registered project still on disk. Init writes those stubs
  once and the protocol text evolves with the binary; without this, projects
  keep stale stubs indefinitely (the Aug-20 `.claude/commands/stateroot.md`
  found stale during the claude-code continuity test).
- **Latest Activity in every digest** (resume and hook): the newest observed
  activity anywhere — last checkpoint or latest root — with harness and
  timestamp, plus an explicit stale-handoff note when activity postdates the
  formal handoff (`activity continues after formal handoff #2 by codex…`). A
  long-running session that never writes a formal handoff is now visible to
  every harness that arrives after it (the claude-code/codex misattribution
  incident). `checkpoint` and `snap` also stamp `last_activity
  {harness, kind, at}` into the current handoff in place (additive; history
  files stay immutable).
- **Periodic self-update for agents**: the digest now carries a cache-only
  `**Update available: <tag> — run \`stateroot self-update\`**` notice when
  the release cache knows a newer tag (never network at hook time), and the
  post-install skill instructs agents to act on it (or to run
  `self-update --check` occasionally) — the tool keeps its own freshness.

## v0.1.7 — 2026-08-25

- `stateroot projects` [--json] [--prune]: the global registry window —
  every initialized project on the machine with live hints (phase, objective,
  handoff seq, active plan, last root, on-disk). Missing directories are
  marked MISSING, never silently dropped; `--prune` unregisters them. MCP
  gains `projects_list` over the same listing. This is what lets a
  fixed-workspace personal agent (openclaw) discover a project by name and
  then work it — and enables cross-project operations for every harness.
- `memory recall` / MCP `memory_recall` hits are now excerpted to a ~1600-char
  window around the match (indexed transcript docs can run ~100KB and blew
  past Claude Code's MCP tool-result cap mid-continuity-test).
- Hook project resolution now prefers the event payload's `cwd` /
  `workspace_roots` over the hook process's own cwd — gateway daemons and
  IDE hosts run hooks with *their* working directory, so the digest
  previously described the wrong project (the OpenClaw gateway served the
  repo it was launched from). New env-gated forensics: `STATEROOT_HOOK_DEBUG=1`
  appends every hook payload to `/tmp/stateroot-hook-payloads.jsonl`.
- The digest (resume AND in-band hook injection) now carries the freshest
  actionable state: a `## Recent Checkpoints` section (last five episodic
  notes) and the `## Active Plan` section in the hook digest too — previously
  resume-only, so hook-injected agents never saw the plan store. A live
  OpenClaw probe caught both gaps: the agent received persona and rules but
  no plan and no checkpoint notes.

## v0.1.6 — 2026-08-25

- Hook latency hardening (cursor session-start timeouts on slow
  filesystems): the session-start hook now prints the digest FIRST and runs
  the session-boundary federation syncs (skills/MCP/rules) and the compiler
  AFTER output, so a killed hook can't take the injection with it; the
  installer now writes `timeout: 30` into cursor hook entries (cursor kills
  hooks at a short default — our session-start took ~11s on drvfs).
- `self-update` now re-arms harness wiring after a successful update (spawns
  `stateroot install` from the new binary; background auto-update does it
  quietly). Binaries and wiring no longer drift apart across versions.
- `stateroot doctor` now checks the binary every installed hook config
  points at (all harness hook formats): each distinct stateroot hook binary
  gets a `--version` probe — `[ok]` when it matches the running CLI, a soft
  `[!!]` warning naming the version when it differs (`<harness> hook binary
  is stateroot X.Y.Z — run \`stateroot self-update\` on this machine`) or
  when the command cannot be executed at all. Bare `stateroot` commands
  resolve through the same PATH-probing seam as the rest of the codebase.
  This catches fail-open staleness — hooks wired to an old binary silently
  doing nothing (the Cursor-on-Windows 0.1.1-vs-0.1.5 incident).
- Cursor delivery routing fixed to Cursor's real hook contract:
  `beforeSubmitPrompt` is continue-only and cannot carry
  `additional_context`, so digest + persona now ride `sessionStart`
  exclusively (`prompt_submit_injects: false`, `session_start_marks: true`).
  Previously stateroot emitted the digest on prompt submits where Cursor
  silently discarded it — and recorded false deliveries in the ledger.
- Skill protocol: a truncated tool *display* is not a truncated digest —
  agents must not re-fetch state the digest already carries (the cursor
  scavenger-hunt lesson).
- Doctor: new per-harness hook-binary version check — resolves the stateroot
  binary each installed hook config points at and warns when it is stale or
  unrunnable (the Windows-Cursor-on-0.1.1 incident).

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
