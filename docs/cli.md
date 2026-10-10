# StateRoot — cli

User-facing CLI documentation is published at **https://stateroot.dev/docs/reference/cli**.

`stateroot --help` and `stateroot <command> --help` remain the in-binary reference.

## `stateroot init` — init seeding

`init` no longer leaves `.stateroot/` empty. After writing the skeleton it
**seeds** project state from what the repo already declares, writing only
into placeholder/empty slots (user content is never overwritten; re-running
`init` is safe):

- **Deterministic seed (always, zero LLM)** — objective from the README title
  + first paragraph, next actions from `TODO.md` checkboxes / roadmap
  bullets, memory facts (top-level layout, observed docs, git origin remote,
  recent commits) into `memories/MEMORY.md` under `## Seed (observed at
  init)`, and a seq-1 `handoffs/current.json` labeled `"provenance":
  "observed"`. Empty repo → `nothing to seed` and no handoff.
- **`--synthesize` (opt-in LLM enrichment)** — asks a backend for a richer
  seed. Auto backend order: local harness CLIs first (`claude`, `codex`,
  `kimi`, `gemini`, `opencode`, `openclaw`, `hermes`, `pi`, `grok`, `zero`,
  `antigravity`, `omp`, `devin` — first whose registry delegation spec is a
  CLI whose binary is on PATH, run non-interactively with piped stdout), then
  the `DEEPSEEK_API_KEY` / `OPENAI_API_KEY` API path. `--synthesize-with
  <backend>` forces one backend (a harness id, `deepseek`, or `openai`).
  Synthesized fields replace the same-origin init seed and are labeled
  `synthesized — unverified (<backend>)`. Synthesis problems never fail
  `init`: a note is printed, the deterministic seed stands, exit stays 0.

## `stateroot delegate` — cross-harness subagents, async-only

`stateroot delegate --to <harness> --task "<bounded task>"` spawns another
harness's CLI as a **detached** subagent inside the current project, writes a
`stateroot.delegation.v1` record with `status: "running"` and a pid, prints
the delegation id and exits 0 immediately. Async was always the right
architecture: launch detached, observe until done, completions surface in the
record and the digest. **There is no sync mode, and no timeout anywhere —
nothing is ever killed or blocked on.** The harness runs to its natural end;
its own internal limits belong to the harness.

```bash
stateroot delegate --to codex --task "add the failing parser test"
stateroot delegate list                    # every delegation with live status
stateroot delegate status <id>             # the record + a bounded log tail
```

- **The worker** — the spawn launches a detached copy of the same binary
  (hidden `--_worker`) with stdout/stderr redirected into
  `.stateroot/delegations/<ts>-<h>-d<depth>.log`. The worker runs the full
  path (resolve → depth guard → prompt wrap → capture, no kill condition)
  and finalizes the record (`outcome: completed|failed`, `exit_code`,
  `duration_ms`, `ended_at`) plus an episodic lineage note.
- **Live status** — `list` reports `running | completed | failed | lost`.
  `lost` means the worker died before writing an outcome (dead pid, no final
  record): `list`/`status` probe pid liveness and reap the record to
  `lost` — never a silent running-forever.
- **Completions surface asynchronously** — the digest gains a
  `## Recent Delegations` section (last few with status + task), so a parent
  harness learns on its next session or prompt that labor finished.
- **Resolution** — the target must be a registry cli-mode harness whose
  binary probes on PATH. Unknown harnesses, handoff-only harnesses (e.g.
  `cursor`), and missing binaries are loud errors listing the available
  cli-mode harnesses.
- **Depth cap** — `STATEROOT_DELEGATION_DEPTH` guards recursion: at depth ≥
  2 the spawn refuses ("a subagent may not spawn further subagents") and
  nothing is spawned or recorded. The worker runs at parent depth + 1; its
  own depth guard then enforces the cap inside the delegation as well.
- **Flags** — `--skill <slug>` (repeatable) projects StateRoot skill
  packages into the run; `--ambient-skills` opts into the harness's own
  skill discovery; `--json` prints the running record as the spawn envelope.
  `--timeout-secs` and `--max-output-chars` no longer exist (the sync
  contract they belonged to is gone).
- **Idempotency** — `--key <k>` (1–64 chars of `A–Z a–z 0–9 . _ -`) makes
  the record id the key: a replayed spawn re-attaches to a live worker or
  refuses a finished one; a same-key request that differs is rejected.
- **Lifecycle** — records run one state machine per key (`starting` →
  `running` → terminal) with a bounded event history. Every outcome —
  completion, failure, cancellation — captures the work as an immutable
  root before the task is terminal (`outcome_root`).
- **Cancel** — `stateroot delegate cancel <id>` persists `cancelling`,
  stops the whole process tree (group TERM, escalate KILL, verify),
  snapshots partial work, and records `cancelled_with_root` with
  `cancel_confirmed` honest. A repeated `cancel` resumes an interrupted one.
- **Fork worktrees** — `--worktree <path>` runs the subagent inside a
  registered fork checkout (validated against the project, not just any
  directory with `.stateroot`); records stay with the calling project.

For an interactive harness session use `stateroot harness run` instead;
`delegate` is the detached, recorded, non-interactive route.

## `stateroot fork` / `stateroot merge` — parallel plan execution

```bash
stateroot fork <root> --worktree <path> [--plan ID]
stateroot merge <fork> [<fork>…]
stateroot merge --cleanup <fork> [<fork>…]
stateroot merge --prepare <fork> [<fork>…] --json
stateroot merge --status <attempt> --json
stateroot merge --continue <attempt>
stateroot merge --abort <attempt>
```

`fork --worktree` materializes an isolated checkout of the fork root's
tree at `<path>` — validated (destination free and outside the project,
plan exists and unclaimed) and rolled back on partial failure. The
snapshot's `.stateroot/` state (plans, handoffs, memory) travels with it;
HEAD is detached at the root commit (user branches are never created or
moved). A machine-local `fork-context.json` stamp makes snaps inside the
worktree chain on the fork ref, never on the trunk's `latest`.
`.stateroot/worktrees/` is hardcoded-ignored; keep checkouts outside the
project tree.

`stateroot merge` folds fork tips back into the trunk: each fork is
3-way-merged into the accumulated union, then committed as ONE root with N
parents — trunk + every merged fork tip. Conflicts produce a per-path
report and no merge root; contained forks are reported as
nothing-to-merge. The merged tree is materialized into the trunk
filesystem before the ref advances (uncommitted changes on paths the
merge would overwrite refuse with the exact paths), verified on disk, and
the merged fork worktrees are retained. Fork refs and records stay as
history; each registered worktree is marked `cleanup_pending` so merge
returns without waiting on deletion. Retry with
`stateroot merge --cleanup <fork>…` — missing paths count as already
removed, success is bounded and idempotent, and a timeout or failure
leaves retryable state intact. Merge refuses to run from inside a fork
worktree.

For an appointed agent coordinating an integration, `stateroot merge
--prepare <fork>… --json` freezes the trunk and fork tips and reports either
a `ready` attempt or structured `attention` conflicts (path, conflict kind,
plus base/ours/theirs blob identities). It changes no refs or worktrees of
the trunk. A conflicted attempt additionally materializes an isolated
reconciliation worktree under machine-local state: the accumulated clean
fold with each text conflict rendered with standard conflict markers. The
appointed agent edits and tests in that worktree, then `--continue
<attempt>` revalidates every frozen ref, refuses while conflict markers
remain, folds any forks selected after the conflicted one, and publishes
exactly the reconciled tree as one N+1-parent root — recording the attempt
id, resolved conflicts, and any `--evidence "<tests run>"` strings in the
transition. `--status` is read-only and `--abort` removes only the local
attempt record and its worktree. StateRoot never chooses a model or
silently chooses a source side.

`stateroot handoff write --worktree <path>` binds a handoff to a
registered fork: the packet carries the opaque `fork_id` (never the raw
path), the receiver's digest opens with **Work in fork \<id\>** and the
registry-resolved path, and `resume` anywhere else fails closed with the
exact recovery command.

## Extension subcommands — git-style `stateroot-<name>` on PATH

Any executable named `stateroot-<name>` on PATH becomes `stateroot <name>
[args…]`. There is no registry or install step: an agent can write a small
script and the CLI immediately grows a command.

- **Discovery** — every PATH dir is scanned for `stateroot-*` files. Unix:
  any executable bit. Windows: the extension must be in `PATHEXT` (and is not
  part of the command name). The bare `stateroot` binary itself never
  matches. Duplicate names dedup first-PATH-hit-wins.
- **Execution** — extensions run with inherited stdio (they may be
  interactive) and the child's exit code becomes the CLI's.
- **Env contract** — additive over the inherited environment:
  `STATEROOT_HOME`, `STATEROOT_VERSION`, and inside a project
  `STATEROOT_PROJECT_DIR` + `STATEROOT_PROJECT_ID` (from the manifest).
  `STATEROOT_DELEGATION_DEPTH` passes through untouched, so extensions inside
  delegate flows keep the recursion cap.
- **Shadowing** — builtins always win: a `stateroot-status` executable never
  intercepts `stateroot status`; `stateroot ext list` marks such entries
  `shadowed builtin (ignored)`.
- **Unknown subcommands** — a name that is neither builtin nor extension is a
  clap-styled `error: unrecognized subcommand` with a did-you-mean tip over
  builtins and discovered extensions, exit code 2.
- `stateroot ext list` prints each discovered extension as `name — path`, or
  `no extensions found on PATH (stateroot-*)`.

Write your first extension:

```sh
#!/bin/sh
# stateroot-hello — drop anywhere on PATH; runs as `stateroot hello`.
set -eu
if [ -z "${STATEROOT_PROJECT_DIR:-}" ]; then
  echo "not a stateroot project" >&2
  exit 1
fi
stateroot checkpoint --note "hello extension ran ($*)"
echo "checkpoint recorded in $STATEROOT_PROJECT_DIR"
```

## `stateroot session` — canonical sessions & cross-harness transfer

Sessions belong to StateRoot: standardized, shared, portable across
harnesses. `stateroot session sync` canonicalizes sessions from every
harness store — claude (`~/.claude/projects/**`), codex (rollout + archived
stores), kimi (wire files + session index — stateroot's own harness,
dogfooded), openclaw, cursor and hermes (sqlite state stores, opened
immutable), pi (`$PI_CODING_AGENT_DIR` or `~/.pi/agent/sessions`), and dsh
(`$DSH_HOME` or `~/.dsh/sessions`) — into `.stateroot/local/sessions/` as
`stateroot.session.v1` JSONL: a header line, then one full-fidelity entry
per line (`message`, `tool_call`, `tool_result`, `compaction`, `plan`,
`meta`). Entries are never content-capped (display paths cap); native
ids/parents are kept where the format has them; unmapped native types are
kept as `meta` with `native_type` — nothing silently vanishes (injected
envelopes, thinking blocks, harness control records, and cursor's
unverified `toolResults` are all preserved and marked). Sync is idempotent
(each session file is rewritten whole; codex active-store copies win over
archived duplicates).

- **The `local/` boundary** — canonical sessions live under
  `.stateroot/local/`, never pinned into roots (same rule as
  `local/memory.sqlite`): full session logs stay out of snapshots to
  protect root size. Promotion into synced state is a later,
  retention-tiered decision.
- **Honest skips** — DSH `.jsonl.zstd` artifacts are counted and skipped
  (no zstd in the dependency tree); torn tails and seq gaps are recorded,
  not hidden; `assistant/chunk` stream deltas are skipped (assembled text
  lives in `assistant/message`) with the omission counted.
- `stateroot session list [--harness H]` — id, harness, span, entries,
  outcome. `stateroot session show <id>` — header, first user message, last
  entries (capped for display).

### `stateroot session transfer <id> --to pi|dsh [--dry-run]`

Transfer translates strings to strings: a canonical session becomes a real,
resumable session file in the target harness's native store (Pi v3 tree
with a fresh linear id/parentId spine — branches flatten to the imported
timeline; DSH v0 event log with contiguous seq and a clean completed/
interrupted tail). The source session is never mutated, an existing target
is never clobbered, and the fidelity report is always printed:

```
transferred session <id> → pi
  entries: 84 native · 6 adapted (compaction→branch_summary) · 3 dropped (model_change)
  wrote: ~/.pi/agent/sessions/<dir>/<file>.jsonl
  resume with: pi (in <cwd>)
```

`--dry-run` prints the same plan with `would write:` and touches nothing.
Every transfer appends an episodic lineage note.

## `stateroot plan` — central plan artifacts + lifecycle

The plan/implement split, doctrine-shaped: StateRoot owns the plan
**artifact and its lifecycle** (strings above the runtime); each harness
keeps its own plan **mode**. A strong model in harness A authors a plan;
it lands in the project plan store with provenance; the user (or a
delegating agent) approves it; harness B's digest points at the file with
an execute directive. Full-fidelity markdown on disk, pointer + directive
in the prompt path (token razor).

- **Store** — `.stateroot/plans/<id>.md` (the plan, verbatim markdown) plus
  a `stateroot.plan.v1` sidecar (`<id>.json`: title, status, author
  harness, timestamps, `root_ref` from `refs/stateroot/latest`, source
  path, notes). `stateroot plan record --file <path>` / `--stdin` creates a
  **draft**; `list` / `show <id>` inspect (show prints the raw markdown —
  that is how other harnesses read a plan).
- **Lifecycle** — `draft → approved → active → done`; `abandoned` from any
  non-terminal state. Wrong-state transitions are clear errors (same-state
  included). At most one plan is **active**: `plan activate` demotes the
  currently active plan to `approved`, recorded in its notes — never
  silent.
- **Digest** — resume renders `## Active Plan` before `## Plan State`:
  title, status, provenance, the `.md` path, and a state-aware directive.
  Unfinished bound work → the executor directive ("Execute it as written;
  do not re-plan or re-explore"); structurally complete work (all
  plan-bound todos done, or an open plan-closure obligation) → the closure
  directive ("do not restart implementation; record completion evidence or
  state concrete remaining work"); an approved plan with no executor →
  "assign or claim execution"; only a draft → the planner directive
  ("refine the plan file; do not implement yet"). The transcript
  `## Plan State` remains as the fallback tier and is suppressed while a
  central plan exists. The plan body never enters the digest — the
  executor reads one file.
- **Completion requires evidence** — `stateroot plan done <id> --evidence
  "…"` first creates/verifies a completion snapshot, then records an
  additive completion receipt (completion time, actor, exact plan-body
  digest, completion root, evidence) and transitions the plan in one
  command; any failure leaves the plan active. Plans are never
  auto-completed: all-completed plan-bound todos open a plan-closure
  obligation instead, and the plan stays active until the explicit
  receipt.
- **Approval pins the body** — `plan approve` records `approved_digest`
  (sha256 of the plan body) in the sidecar. When the body on disk no longer
  matches, `## Active Plan` warns `**Warning: plan body changed since
  approval**` — review the body, then re-approve (`stateroot plan approve`)
  or restore it; the executor never silently runs a substituted plan. Plans
  approved before this field existed carry no digest and make no drift
  claim.
- **Handoff** — `handoff write` auto-attaches `plan_ref: {id, title,
  status}` when an active/approved plan exists.
- **v1 has no tool-gating** — hooks do not deny write tools while a draft
  exists. Enforcement is a policy decision for the user (optional hook
  hardening later); StateRoot ships the strings, not a runtime cage.

## `stateroot obligation` — durable, federated obligations

Explicit future-work items (campaign reviews, plan-closure receipts,
scheduled follow-ups) shared across harnesses through the project store:

- `add --task TEXT [--due RFC3339 | --in DURATION] [--assign HARNESS]
  [--plan ID] [--operation-id ID]` — create. One-shot due dates only;
  milestone series use multiple obligations. `--in` accepts `30m`, `24h`,
  `7d`, `2w` and compounds (`1h30m`). The operation id makes retries
  idempotent: the same key returns the existing obligation.
- `list [--all] [--json]`, `show ID` — inspect. Corrupt event lines are
  preserved and counted, never silently dropped.
- `done ID --evidence TEXT` / `snooze ID --until RFC3339` / `cancel ID
  --reason TEXT` — lifecycle. States: `open | snoozed | done | cancelled`;
  a lapsed snooze is open again. UUIDv7 ids (prefix allowed), UTC
  timestamps.

Definitions live at `.stateroot/obligations/<id>.json`; lifecycle changes
append to `.stateroot/obligations/events.jsonl` (merge-union, synced).

## `stateroot service` — the background continuity service

A deterministic per-user resident process reconciles every registered
project on the `[continuity] poll_interval_seconds` cadence (default 30s):
due obligations, plan lifecycle contradictions, handoff freshness,
boundary-journal health — one atomic machine-local projection per project
at `.stateroot/local/projections/continuity.v1.json`. It never embeds or
runs an AI agent.

- `service install|remove|start|stop|restart|status [--json]|run` —
  registration is a per-user systemd service (Linux), LaunchAgent (macOS),
  or logon Scheduled Task (native Windows, hidden wscript launcher with
  persistent log); under WSL a functional user-systemd wins, otherwise a
  Windows-host task launches the service through the current WSL
  distribution. Scheduled-task names are scoped to the owning user, config
  home AND WSL distribution (`StateRoot Continuity (<user>-<hash8>)`), so
  the same path/user in two distros never collides; install/remove repair
  the pre-scoping global task name only when its descriptor verifiably
  references a stateroot launcher. Every descriptor pins the config home it
  was registered for (systemd `Environment=`, launchd
  `EnvironmentVariables`, the Windows launchers via `set STATEROOT_HOME` /
  `WSLENV`). Descriptors quote spaced / ampersand / Unicode paths (systemd
  `%`/quote escaping, launchd XML escaping, Windows command-line quoting).
  Registration records the exact binary + config home; a registration is
  "current" only while the on-disk descriptor CONTENT matches what this
  binary + config home would generate (and the manager still runs it), and
  `install` re-registers and replaces the verified running instance when
  the binary drifts (self-update rearm). `stop` signals only a pid verified
  end-to-end as ours — exact recorded binary running exactly `service run`,
  same host namespace, the recorded process-start token (a reused pid
  fails), and the pinned config home; a legacy or unverifiable identity is
  never killed. A signalled service that does not exit within the shutdown
  bound is a FAILED stop (nonzero exit, records preserved) — never a
  printed "stopped" that a restart/remove would build on. The resident loop
  distinguishes live-lock contention ("already running (pid N)"), an
  unverifiable lock owner (held, not started, honestly worded), and lock
  I/O failures (a hard error, never already-running). All OS manager
  operations run with a wall-clock budget. `stateroot install`, self-update
  rearm, and uninstall manage it automatically. When OS registration is
  unavailable the service runs detached, hooks/CLI keep reconciling on
  activity, and `doctor` reports degraded background coverage.
- Single-instance per user (lock + heartbeat in the config dir), capped
  log, per-project locks. `[continuity] enabled = false` disables the
  runtime; `[continuity] synthesis = true` (requires the existing
  synthesis credentials) may attach one hash-idempotent, provenance-
  labeled advisory line — advisory-only, never state-changing.

## `## Needs Attention` — the push section

Injected digests (resume + hooks) carry `## Needs Attention` before the
plan section: the highest-priority five derived items plus an overflow
count. Items are deterministic — due obligations, a structurally complete
active plan awaiting its completion receipt, an approved plan with no
executor, a routed handoff awaiting acceptance or one made stale by newer
activity, a failed/lost delegation against an open plan, boundary-journal
jobs parked for manual attention, an unhealthy continuity service,
registry projects missing from disk. Attention IDs are stable
(`<kind>:<entity>`); nothing is derived from keyword classifiers or model
calls. `stateroot status` renders the same projection (human or `--json`).

## `stateroot handoff` — write flags and accept gates

`handoff list` shows each distinct packet's full body SHA256, with only the
exact current packet pinned and marked. Identical copies from merged forks
are collapsed in this read view; immutable history files are unchanged.
`handoff show <seq>` works when that number identifies one distinct packet.
If fork histories contain different packets with the same sequence, it
refuses to guess and lists exact `handoff show --id <SHA256>` selectors with
their provenance. The selector reuses the acceptance body hash below, works
for legacy packets, and stays unchanged by acceptance/checkpoint bookkeeping.
Omitting both selectors still shows the current packet.

`handoff write` owns the packet envelope (schema, sequence, provenance,
timestamps); the author owns the content. Beyond the core fields, two
structured channels carry what prose blurs:

- **`--failed-approach "<approach> → <outcome>: <reason>"`** (repeatable) —
  a structured failed-approach record. The outcome vocabulary is fixed:
  `success` | `partial` | `failed`, parsed case-insensitively and stored
  canonical — any other word is a hard error, so a typo never reads as a
  real outcome. The digest renders them as `## Failed approaches`, distinct
  from the free-text `## Failed Approaches / Bugs`. The JSON input channel
  (`failed_approaches: [{"approach","outcome","reason"}]`) applies the same
  vocabulary.
- **`--context-only "<fact>"`** (repeatable) — an authority-labeled
  background fact: the receiver may rely on it but must not execute it. The
  digest renders them as `## Context (not instructions)` under a hard cap
  (see digest budgets below).
- **Budget warnings write anyway** — a `--context-summary` past the
  6000-char digest budget, an empty `objective`/`task`/`context_summary`,
  and an identical task/summary pair all warn on stderr; none refuses the
  write (continuity beats form-filling).

`handoff accept --by <harness>` is deliberately not a rubber stamp:

- **Fail-closed staleness gate** — when the newest observed activity
  (checkpoint or root) postdates the handoff's `written_at` boundary, accept
  refuses and names the newer activity: accepting would anchor the receiver
  on dead state. Re-read with `stateroot resume`, or pass `--force` to
  accept anyway — a forced acceptance is recorded with `forced: true`.
- **Append-only acceptance records** — every accept appends `{by, at,
  body_sha256}` (plus `operation_id` / `forced` when given) to the packet's
  `acceptances`. The body hash excludes local acceptance bookkeeping, so
  checkpoint stamps and accept marks never read as drift; a real body change
  since the last acceptance warns but records.
- **`--operation-id <id>`** — idempotent re-accept: an accept whose
  operation id is already recorded is a no-op (`already accepted …
  idempotent no-op`), checked before the staleness gate so a retried
  operation never newly fails.

## `stateroot resume` — digest budgets

The resume/hook digest is prompt-path real estate, so the bulky sections are
bounded — pointer + shape, never silent loss. The work body (objective,
active plan, next actions, handoff fields) stays fully inline.

- **Shared Rules** — a rule whose body fits 1200 chars renders whole; a
  larger rule renders as title + a deterministic outline (every markdown
  heading, one indented line each) + `… full rule: \`stateroot rules show
  <slug>\``. Past an 8000-char section budget, later rules collapse to
  title + pointer. Never truncated mid-line.
- **Federated Skills** — the same package discovered from several scopes
  lists once (deduped by slug + route + description); the header count and
  the 40-line cap apply to the deduped list.
- **Work-since-handoff overlay** — the observed conversation tail is the
  last 8 entries, each ≤ 400 chars with an ellipsis when cut (same bound in
  resume and hooks).
- **Context pack** — per-doc cap stays 8000 chars; repo docs additionally
  share a 16000-char total budget in pack order, and docs past the budget
  appear as a one-line title listing: `(capped — N more docs on disk)`. The
  top-level tree listing is unbounded (it is short by construction).
- **Failed approaches** — structured `approach → outcome: reason` records
  (`handoff write --failed-approach`) render as `## Failed approaches`, kept
  distinct from the free-text `## Failed Approaches / Bugs` section.
- **Context (not instructions)** — author-asserted background facts
  (`handoff write --context-only`) render as `## Context (not
  instructions)`, hard-capped at 8 items and 4000 chars; what does not fit
  appears as a `- … +N more` tail — bounded in the digest, never dropped
  from the packet.

## The digest's freshness lines — Latest Activity & update notice

Two one-line sections keep every arriving harness oriented:

- **Latest Activity** — the `## Latest Activity` section names the newest
  observed activity anywhere (last checkpoint or latest root) with harness
  and timestamp. A long-running session that
  never writes a formal handoff is no longer invisible: when activity
  postdates the handoff boundary, the digest says so plainly (`activity
  continues after formal handoff #2 by codex — the formal handoff is stale`).
  `checkpoint` and `snap` also stamp `last_activity {harness, kind, at}` into
  `handoffs/current.json` in place (additive; history stays immutable).
- **Update notice** — when the release cache (`update-check.json`, refreshed
  by the background auto-update on its own cadence) knows a newer tag than
  the running binary, the digest carries `**Update available: <tag> — run
  \`stateroot self-update\`**`. Cache-only: the digest never touches the
  network. The post-install skill tells agents to act on this line (or to run
  `stateroot self-update --check` occasionally).

## Scheduled self-update (automatic, agent-independent)

Machines stay current without anyone asking. On every session-boundary hook
(already the slow-work zone), stateroot checks the release cache's age; when
`[update] check_interval_hours` has passed, it spawns a **detached**
`stateroot self-update` and returns instantly — the hook never blocks and no
agent is asked to act. One worker at a time (`update-in-progress` lock, one
hour liveness); the child updates the binary and re-arms harness wiring as
usual, logging to `update-scheduled.log`. The digest's update notice is the
visible layer; this is the layer that acts.

The swap itself is fail-closed and journaled. `self-update` fingerprints the
running binary before anything in its directory is touched — a binary whose
`--version` does not answer a `stateroot ` line is left completely untouched
— then writes `<config_home>/update-journal.json` (`status: in_progress`,
from/to versions) before parking the old binary, and clears the journal only
after the new binary's `--version` readback confirms the target tag. A clean
rollback rewrites the journal as `rolled_back` — a recovered failure, not an
interruption. A leftover in-progress journal means the process died
mid-swap: `stateroot doctor` surfaces it as a soft `self-update` warning
(`update interrupted (from … to … at …) — rerun \`stateroot self-update\``),
never a hard failure.

## `stateroot doctor` — hook-binary health

`stateroot doctor [--json]` — the `--json` document
(`stateroot.doctor.v1`) carries every check as a typed
`{label, ok, hard, detail, repair}` row plus an `integrations` section with
the per-harness integration-health document (`stateroot.integration-health.v1`:
per harness — detection evidence, hook/instruction/MCP/skill projection
state, identity-delivery tier, last digest delivery/capture, problems and
repair commands) and an `integrations_summary` section. Base checks and
integration readiness are separate verdicts: `ok` covers base hard failures
only, and a missing-but-undetected harness is never a hard failure. Status
is truthful: `configured` never reads as `observed_working` without a
delivered digest or a durable captured observation (the WS1 store — an
authored checkpoint or a prose "via hook" note is never capture evidence),
and unreadable/corrupt capture or delivery evidence is diagnosed in
`evidence_problems`/per-row `problems`, never silently absent. Hook configs
are read with each format's real grammar (TOML is TOML-decoded, so escaped
quoted `stateroot.exe` paths with spaces/Unicode resolve correctly).
`stateroot install --json` prints the same integration document plus an
`install` outcome (`configured` / `failed` / `cli_only`) — the actual
result of the pass; human progress moves to stderr, and a partial install
is explicit in the document rather than a prose summary. `stateroot init`
closes with the one-line integration summary. The editor setup flow
consumes `install --json`: a partial integration fails without a Ready
receipt, a no-agent machine is an explicit CLI-only success, and the
human `Installed for:` summary fallback runs only when the CLI provably
rejected the `--json` flag (never on timeout/failure/malformed output). A
cached receipt's health probe consumes `doctor --json` the same way: base
`ok` AND no detected integration row reading `missing` — base-ok alone
never counts as integration readiness, a timeout or malformed document is
not proof of health, and the human doctor exit-code fallback runs only on
a proven parse-time `--json` rejection.

Doctor inspects the binary every installed hook config actually points at
(all harness hook formats: nested/flat JSON, TOML, exec-form, named groups,
and the generated OpenClaw plugin). For each distinct stateroot hook binary
it runs `--version` with a wall-clock budget (a hung custom binary reports
not-runnable, never blocks doctor): a match with the running CLI reports `[ok]`; a
mismatched or unrunnable binary is a soft `[!!]` warning (never a hard
failure) — e.g. `cursor hook binary is stateroot 0.1.1 — run \`stateroot
self-update\` on this machine`. This is the check for fail-open staleness:
hooks that resolve to an old `stateroot` silently do nothing, and nothing
else reports it.

Doctor also reads the self-update crash journal
(`<config_home>/update-journal.json`): a leftover in-progress swap reports
`update interrupted … rerun \`stateroot self-update\`` as a soft warning,
a completed rollback reports as a recovered failure, and an unreadable
journal comes with a delete instruction — see Scheduled self-update above.

## `stateroot projects` — the global registry window

`stateroot init` registers every initialized project in the machine-global
`projects.toml`. `stateroot projects` prints the window: name, phase, handoff
seq, active plan, last root, and path — with live hints read cheaply from
each project store (no scans). `--json` for machine consumers; the same
listing is exposed to agents as the `projects_list` MCP tool.

This is the discovery half of cross-project work: a personal agent with a
fixed workspace (openclaw) or any harness juggling repos lists the projects
here, then moves into the one requested and resumes it there. A registered
project whose directory was deleted is marked `MISSING`, never silently
dropped; `stateroot projects --prune` unregisters those entries (prints each
one; project state on disk is never touched — the dirs are already gone).

## `stateroot memory sync` — memory federation

Harness-native memory systems (claude memory, codex memories, openclaw session
logs) are both a conflict and an opportunity. `stateroot memory sync` makes
StateRoot the memory **pool**: pull harness memories in as `observed` tier, and
push a curated brief back into harness-native formats so even hook-limited
harnesses know the project.

### Sources → tiers

| Harness | Reads | Lands as |
| --- | --- | --- |
| `claude` | `~/.claude/projects/<slug>/memory/*.md` (slug decodes to the cwd, matched with walk-up/walk-down tolerance) | wiki pages `wiki/pages/harness/claude/*.md` |
| `codex` | `~/.codex/memories/*.md` (flat; the sqlite is pipeline state, never read) | wiki pages `wiki/pages/harness/codex/*.md` |
| `openclaw` | `~/.openclaw/workspace/memory/*.md` (daily logs) | episodic records (`harness-memory:openclaw:<hash>` source id) |

`stateroot memory sync [--harness claude|codex|openclaw] [--dry-run]` — no
harness filter means all three.

### Dedup, conflicts, provenance

- Every imported artifact carries a provenance header
  `<!-- stateroot:imported harness=… source=… hash=… -->` and is `observed`.
- Dedup is by **content hash** (sha256 over normalized text), recorded in the
  import ledger `.stateroot/memories/federation.json` — never by title.
- Same title + different content is **preserved, never overwritten**: the new
  note lands as `<title>__<hash8>.md` and the conflict is recorded.

### Push (`--push`)

`stateroot memory sync --push` writes a compact managed brief
(`<!-- stateroot:managed v1 -->` + objective, phase, active plan, latest
checkpoints, hot-apex memory — capped ~4000 chars) into:

- `~/.claude/projects/<slug>/memory/stateroot.md`
- `~/.codex/memories/stateroot.md`
- `~/.openclaw/workspace/memory/stateroot.md`

Managed files are written only when absent or already carrying the marker; an
unmarked pre-existing file is a conflict, reported and left untouched.
`--dry-run` prints each target and the would-be size without writing.

## `stateroot editor` — VS Code / Cursor extension reconciliation

`stateroot editor status` is read-only: it reports each detected stable
VS Code and Cursor launcher, the installed StateRoot extension version, and
the release-declared desired version (production CLI → production release;
nightly/dev CLI → rolling `nightly`).

`stateroot editor reconcile` installs a missing extension or updates a
stale one from the verified GitHub VSIX. Exact and newer installs are
strict no-ops (never downgrade). One editor's failure does not undo the
CLI or the other editor; retry with the same command.

Automatic reconciliation also runs after `stateroot install` and after an
already-current scheduled CLI update check. It honors `[update] enabled`
and `STATEROOT_NO_AUTO_UPDATE` on the automatic path; the explicit
`editor reconcile` command still runs when background auto-update is off.
It does not run from hooks or every ordinary CLI command.
