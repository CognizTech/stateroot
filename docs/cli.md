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
  init)`, and a numbered `handoffs/current.json` labeled `"provenance":
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

## Optional API transports — provider call hardening

The two optional API paths — the synthesis caller (`--synthesize`, continuity
advisory, memory distill) and the local-ingest caller — share one hardened
HTTP client, used only when an operator key (`DEEPSEEK_API_KEY` /
`OPENAI_API_KEY`) is configured:

- **Redirects** — exact-origin hops only (scheme + host + port must all
  match; an explicit default port still matches). An HTTPS→HTTP downgrade is
  refused even on the same host, a cross-origin target is refused outright,
  and a 5-redirect ceiling makes loops fail fast instead of walking the
  request body onto an origin the operator never configured.
- **Response bound** — bodies are capped at 1 MiB: an honest over-cap
  Content-Length fails before body bytes are consumed, and streamed reads
  enforce the same cap, so a pathological endpoint is reported, never read
  whole into memory.
- **Error excerpts** — diagnostics quote at most the first 300 chars of an
  upstream error body, cut on Unicode char boundaries with a `…[truncated]`
  marker. These are raw upstream excerpts: they can echo whatever the
  provider chose to send, so they are not a guarantee that an error body can
  never contain sensitive data — the CLI simply never adds request auth
  headers or the prompt to them.
- **Fallback unchanged** — every API path stays optional: missing keys,
  refusals, and provider failures degrade to the existing deterministic
  observed context pack. No scanner, no capture restriction, no new
  approval, and no always-on LLM.

## `stateroot wiki compile` — retained generation and retry

The deterministic inbox stage remains keyless and automatic at the existing
session-end/pre-compaction boundaries. It records its own completed input
identity. Optional configured generation has a separate successful identity:
provider or apply failures leave that baseline intact, and retrying does not
repeat completed mining. Unchanged input skips generation. `--force` repeats
optional generation; it does not require repeated deterministic mining.

Native agents can declare relevant inputs through the Wiki owner:

```sh
stateroot wiki compile --dependencies '[{"kind":"source","path":"src/parser.rs"},{"kind":"instruction","path":"AGENTS.md"},{"kind":"tool_config","path":"tool-config.json"},{"kind":"skill","selector":"parser-check","file":"references/checks.md"}]'
```

This replaces the persisted `compile_dependencies` array in the inbox's
frontmatter; `--dependencies '[]'` clears it. Omission retains it. File paths
are project-relative. Skill selectors use the existing I2 package inspection
contract, including package/resource digests, metadata and visible unavailable
states. Only declared references are resolved; their content and identities
enter the actual generation payload. Installing or editing an unrelated
Skill does not invalidate it. References express native-authored dependencies,
not observed Skill application, correctness, or automatic relevance selection.

`ingest-gov.json` retains the floor identity/count, optional attempt count and
status, prior successful provenance, generated response awaiting preparation,
and any prepared apply. A valid generated response is reused after preparation
failure; `--force` can request a new response. A prepared apply
contains before/after content identities and exact prepared output bytes; publication uses the existing
memory/wiki/episodic locks. The archive publishes before apex demotion, and
durable publication intent is saved before the first output write. An
interrupted retry completes that saved mutation without another provider call,
even when keys/configuration or input sources have since changed. New episodic
appends are preserved; the saved append operation is applied once. Changed
inputs supersede a proposal only before publication starts. Changed authored
outputs report a conflict and remain intact; a started recovery journal stays
available even with `--force`. A fresh proposal can be requested after an
unpublished output conflict. Changed learning-scope or source inputs remain
due for actual deterministic mining after recovery. If authored output blocks
optional recovery, new notes still pass through the keyless floor. Its newer
inbox/catalog/log writes remain intact and do not rebase or discard the older
optional journal; the optional conflict stays visible. The optional Wiki log
entry publishes through an idempotent append, preserving newer floor entries.
Saved journals from the whole-log writer remain recoverable: an already-applied
old append is recognized, append-only extensions are preserved, and changed
existing log entries remain explicit conflicts. Memory capacity and archive
behavior use the existing memory mutators. Successful publication means the
recorded synthesized outputs were applied, not that their meaning was verified.

The identity covers the actual inbox/catalog payload, declared source and
package bytes, and effective provider/model/endpoint/request configuration. Successful
provenance separates prepared and observed output identities and includes
copied/prepared/payload byte counts and stage elapsed times;
these are local costs, not billed tokens or evidence of semantic savings.
Output identities are SHA-256 of JSON-encoded optional UTF-8 text, so an absent
file and an empty file remain distinct.
It does not claim awareness of undeclared source dependencies or infer semantic
impact from a root label. Output identities cover the existing Wiki catalog;
materialization copies only proposed pages and the fixed memory/archive/index
surfaces, with appended episodic history retained as a delta. This is one retained ingest assessment; it does not
introduce a per-symbol dependency graph or schedule work on unrelated queries.

Handoff synthesis also binds its effective request configuration and validates
its frozen bundle before publication. Its compare/update uses the ordinary
handoff lock, preserving a concurrently authored packet or newly created
handoff. Incidental activity and acceptance stamps remain receiver-owned.

## `stateroot skill` — native-agent discovery and progressive package reads

Local `mcp-stdio` exposes `skill_inspect` for catalog, package digest, resource
and native-route inspection. `skill_propose` accepts the existing `slug`,
`skill_md`, `name` and `rationale`, plus optional `files_base64` and `evidence`.
Package inspection records delivery, not whether an agent applied the Skill.

Read-only MCP tools `root_inspect`, `root_compare`, `transition_inspect`,
`handoff_list`, `handoff_inspect`, `verification_inspect`, `plan_inspect` and `plan_graph`
expose canonical continuity readers. Handoff history accepts an exact packet
ID or unambiguous sequence; paginated history lists IDs for older packets.
`native_session_inspect` reads an explicit archive and optional saved operation
receipt. It reports the last recorded native observation without contacting
Codex or revalidating the current destination; missing archives remain unavailable.
`stateroot rules list --json` emits canonical rule metadata, and
`stateroot mcp tools --json` emits local StateRoot MCP tool descriptions.
`stateroot mcp doctor --json` reports conflicts and dry-run projection actions.
The extension reads these with `skill scan --json`, `mcp status --json`,
`mcp doctor --json`, `doctor --json` and `plan graph --json` when the corresponding workbench
view opens or is refreshed. Discovery does not establish that a harness
read, invoked, or benefited from a capability.
Verification reports observed commands and input
freshness, not semantic acceptance. Tool failures set MCP `isError`.

`skill list` and `skill scan --json` expose the full discovered catalog,
including native/reference-only and incompatible capabilities. The native
agent chooses applicability. An empty catalog, missing package and an agent
choosing no Skill are different states; StateRoot does not select a top-k
subset, call a model or infer use from a projection marker. Inspection reports
`selector_state` as `no_selector` or `explicit_selector`; neither proves a
task-level choice. `selection` stays `unobserved`. The native agent can record
an actual selection or explicit no-selection decision in its normal handoff.

`skill show <slug-or-identity> --json` inspects one package's metadata, source
references, native route and observed projections. Default catalog entries
omit full sidecar bodies. Use `identity_key` and `package_digest` from the scan
for an exact progressive read:

```sh
stateroot skill show psi_<identity> --json
stateroot skill show psi_<identity> --digest <package-digest> --file references/example.md
stateroot skill show psi_<identity> --digest <package-digest> --file assets/example.bin --json
```

Plain `show <slug>` still prints SKILL.md when the current identity/version is
unambiguous (including canonical directory aliases and legacy lower-case entrypoints). Ambiguous names expose
all candidates through `--json`; a changed digest returns `changed_package`
and no bytes. JSON file reads carry `content_base64`, including binary
resources; plain reads write the original bytes. Missing files and hash-excluded
resources remain visibly unavailable for a digest-bound read. Reference-only
implementations remain in their native harness; inspection gives the existing
delegation route. Compatibility is reported without hiding readable portable
bytes. `--harness pi` reports the registry's isolation declaration for
StateRoot-owned launches; other harnesses do not inherit that claim.

`skill write --input - --from <harness>` consumes JSON and immediately writes
and projects a canonical project Skill, using the same existing federation
owner. It requires `slug` and `skill_md`; optional `files_base64` maps relative
resource paths to complete base64 bytes. Omitted resources survive updates. Writes stage the complete package before
publication, so invalid/conflicting resources leave an existing active package
and its projections intact. Resource input carries bytes, not a portable mode,
symlink or ACL manifest. Existing file permissions survive updates; new scripts
should be invoked through their interpreter unless executable modes are set
separately. The existing package digest covers bytes, not executable modes.
Optional `name`, `rationale` and `evidence` preserve author context. No plan
receipt, candidate queue or promotion step is required. For example:

```json
{
  "slug": "check-fixture",
  "skill_md": "---\nname: check-fixture\n---\nRead references/example.txt.\n",
  "files_base64": {"references/example.txt": "ZXhhbXBsZQo="},
  "evidence": [
    {"kind": "plan", "reference": "plan_<exact-id>", "body_digest": "<sha256>", "status": "historical", "note": "Procedure derived by the native agent"},
    {"kind": "session", "reference": "<exact-session-id>", "project_id": "<source-project>", "status": "declared"}
  ]
}
```

Evidence entries retain optional plan/body/root/delegation/session/verification
references and unknown future fields. Their supplied status is an author
claim. Detail inspection separately reports unavailable, foreign and stale
references. Historical evidence stays historical; an exact available command
observation can establish only that command's exit, never the Skill's
applicability, generalizability or successful application. Delegation evidence
is retained with `unavailable_delegation_inspection` because its record reader
belongs to the CLI runtime. File delivery does not prove the agent read or
applied it; application/outcome stay `unobserved` until supported by separate
native-harness evidence. A projected wrapper proves only route projection.

`package_digest` keeps its existing sorted file-path/file-SHA256 contract.
`skill.federation.json` and managed projection metadata are excluded; other
existing size/vendor exclusions remain listed in `hash_exclusions`. Full local
resources survive canonical/projection copies; federation report content is
the hashed file manifest, with exclusions visible. Sidecar metadata has a
separate `metadata_digest` (identity, not a semantic certificate). Evidence
updates bind to the current package digest; later procedure changes keep old
evidence visible as a stale package binding. Unknown sidecar fields survive
scanner, writer, sync, projection and explicit report/detail inspection.

The shared Rust adapter is `skill_federation::{author_skill, inspect_package}`
with `SkillWriteRequest` and `SkillInspection`. The CLI uses it now. The legacy
MCP `skill_propose` writer remains pending the separately owned MCP adapter
integration; its old arguments and immediate activation are unchanged.

## `stateroot delegate` — cross-harness subagents, async-only

`stateroot delegate --to <harness> --task "<bounded task>"` spawns another
harness's CLI as a **detached** subagent inside the current project, writes a
`stateroot.delegation.v2` record with `status: "running"` and a pid, prints
the delegation id and exits 0 immediately. Async was always the right
architecture: launch detached, observe until done, completions surface in the
record and the digest. **There is no sync mode, and no automatic timeout
anywhere — a worker is never killed on a timer or blocked on.** The harness
runs to its natural end; its own internal limits belong to the harness. The
only stop is an explicit `stateroot delegate cancel` (below), which preserves
the partial work root.

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
  `duration_ms`, `ended_at`) plus an episodic lineage note. `completed`
  means the worker PROCESS exited — not plan acceptance: judge the actual
  work by the retained `outcome_root`, the test evidence, and the plan's
  completion receipt.
- **Live status** — `list` reports `running | completed | failed | lost |
  unknown`. Status reads are read-only: they probe pid liveness and REPORT,
  never rewrite the worker's record. `lost` means the worker died before
  writing an outcome (dead pid, no final record) — never a silent
  running-forever; `unknown` means the recorded pid belongs to a foreign or
  legacy process namespace this host cannot verify — only the worker's own
  final outcome is authoritative.
  CLI and continuity use the same interpretation. Unknown status does not
  establish worker failure, absence of an executor, or plan completion.
  Malformed, unreadable and unsupported records remain intact and appear as
  degraded entries rather than disappearing from the status view.
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
  the record id the key: a replayed spawn re-attaches to a live worker
  ("already running — no double-spawn"), resubmits a `failed`/`lost` worker
  as the next attempt under the same key (a compact retry history is kept
  on the replacement record), and refuses a terminal `completed`/`cancelled`
  one — finished records are immutable, so a genuinely new run needs a new
  key. A same-key request that differs is rejected.
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

For a reviewed candidate, `stateroot merge --stage <attempt>` folds the
remaining forks into the reconciliation worktree without publishing. A
fully staged candidate has a sealed tree for acceptance checks; edit it,
then stage and check it again. `--stage <attempt> --keep-main <path>` retains
one specifically reviewed live operational record that staging would
otherwise overwrite (repeat the flag for additional exact paths). Publish
only the gated candidate with `--continue <attempt>`.

Run `plan verify` inside the reconciliation worktree to bind observations to
the candidate's eligible source. The recorded attempt identifies its owning
repository; verification uses that repository without creating a private
`.git` directory or advancing the frozen trunk or fork tips. Run `--stage`
and `--continue` from the owning project. Candidate snapshots, restoration,
fork creation and merge mutations refuse; the owner publishes the result.
Restage newly recorded verification metadata before publication. Metadata-only
changes preserve the observed source identity; changed source requires new checks.

A legacy candidate with its own `.git` directory is rejected with recovery
guidance. Preserve that attempt and its evidence, prepare a new attempt from
the owning project, and rerun verification in the new candidate.

New snapshot, merge and restoration roots derive provenance for the resulting
state; immutable parents retain their original declarations. Restoration
records its historical source without claiming that the old environment was
reproduced. Supported generated declarations need no manual conflict choice;
unsupported declarations and genuine authored conflicts still refuse.

Fork bindings remain retained as exact historical versions without replacing
the receiver's live session routing. Receiving-checkout cursors and current
projections retain their own state, including absence. Captured evidence,
verification records and immutable acknowledgement frontiers remain retained;
different bodies under one immutable identity require explicit reconciliation.
New bindings carry checkout ownership; existing unqualified trunk bindings
remain readable, and the binding API used by hooks is unchanged.

`stateroot handoff write --worktree <path>` is PARENT-side routing: run from
the trunk (or any other) checkout, it binds the handoff to a registered fork
of this project and delivers the packet into that fork's own current slot
(the sender keeps an immutable audit record). The packet carries the opaque
`fork_id` (never the raw path), the receiver's digest opens with **Work in
fork \<id\>** and the registry-resolved path, and `resume` anywhere else
fails closed with the exact recovery command. When writing a handoff from
ALREADY INSIDE the receiving fork, omit `--worktree` — a plain
`handoff write` updates the fork's own packet (the fork is not a registered
fork of itself, so the flag would only fail validation).

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

### Qualified native Codex recovery

`session sync` also retains supported settled Codex history under
`.stateroot/local/sessions/native-codex/`. This runs through the existing
automatic session import; a final handoff is not required. `session list`
and `session show <id>` list exact boundary archive IDs. The listing verifies
manifest identity; selecting a boundary verifies its retained bytes.

Settled boundaries include completed, failed and interrupted native turns.
Their original statuses and error/abort payloads survive recovery; a later
successful turn can be captured after a failed or interrupted one. An
in-progress, malformed or otherwise unqualified suffix is reported as partial
capture through the exact older boundary, separately from full capture counts.
It is never silently presented as fresh capture of the newer source.
Standalone legacy `event_msg.error` records are retained, but native recovery
is unavailable for that source: the qualified runner drops those records when
persisting a fork, violating the required exact historical payload prefix.
Normal terminal `task_complete.error` records survive and remain supported.

```text
stateroot session transfer <codex-session-id> --to codex --archive <full-digest> --operation-id <stable-uuid>
codex resume <returned-destination-id>
stateroot session show <codex-session-id> --operation-id <stable-uuid>
```

The transfer invokes the installed native `codex app-server` fork API. It
creates a distinct native session, checks source ancestry, every full historical
turn ID, workspace, effective model and permissions, and the historical prefix
before returning a resume command. StateRoot does not submit a model prompt.
After work continues in native Codex, `session show --operation-id` reads the
native turns and records completed output as observed evidence. The reviewing
agent assesses whether that output demonstrates useful retained context.
Concurrent inspections compare their entire receipt snapshot before publication;
a stale inspection refuses to replace newer continuation evidence and can retry
from the current receipt.

The initial qualified envelope is **Codex CLI 0.149.1**, its experimental v2
`thread/fork.path` API, legacy rollout history, and Linux x86_64. The original
workspace path must exist. A fresh native home and empty native registry work
when the captured configuration files match; the live source rollout is not
required. Changed runner versions, file configuration, model, permissions,
unsupported history, missing native evidence, and missing/corrupt retained
archives report native recovery unavailable. Generic intelligence and project
restoration remain available independently. Current configuration is never
overwritten; credentials, software, remote policy, and historical launch
arguments are not reconstructed.

Each boundary binds the native source identities, configuration fingerprints,
and known project/lineage/root context at capture. Raw SHA-256 checks the exact
retained bytes. A separate BLAKE3 historical-prefix digest includes every JSON
field except the outer transport timestamp, which the qualified native fork
rewrites. It includes source metadata and all payload fields. Destination
metadata and new turns do not have to equal the source container. Completed
native turns, a verified archive, a verified native fork, generic intelligence,
and project restoration are reported as separate facts; root fidelity never
guesses a conversation from the latest archive.

Source bytes append once as history grows. A native rewrite or divergent prefix
creates a separate retained generation, preserving earlier boundaries. Restore
materializes only the explicitly selected byte prefix, removes that temporary
materialization after verification, and leaves the retained source unchanged.
The stable operation UUID journals fork intent before the native request. Retry
reconciles the same destination; an uncertain unlocated or ambiguous result does
not launch another fork. `--dry-run` verifies the selected archive without
starting the native runner. All archives and operation receipts remain local;
project roots do not carry them to another device. `session purge` retains these
native archives, just as it retains native harness transcripts.

Protocol reference: [OpenAI Codex App Server](https://developers.openai.com/codex/app-server).
The exact experimental schema was checked with
`codex app-server generate-json-schema --experimental --out <temporary-dir>`.

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

### Explicit dependencies and readiness

`stateroot plan graph --json` and `plan ready --json` return the same
`stateroot.plan-graph.v1` projection: exact plan/body identities, prerequisites,
blocker codes, declared assignments, observed workers, executor fork/root,
parallel-ready IDs and declared ownership overlaps. Human `ready` output shows
only the ready set. A fork claim means assigned/awaiting native activity;
activation alone never proves a worker is running. Legacy plans remain usable
and report `dependencies_not_declared`, rather than invented independence.

Use `plan declare <exact-id> --revision <revision-from-graph>` with an
`ExecutionDeclaration` JSON document on stdin. This replaces the declaration,
preserves the plan body/approval, records the editor and increments its revision.
Only the edited sidecar becomes `stateroot.plan.v2`; older writers refuse it.
Stale revisions, self/cyclic edges and terminal declaration edits are rejected.
Imported malformed/unsupported graphs remain visible and untouched. Divergent
execution declarations from parallel forks require explicit reconciliation;
line-based merging must not combine dependencies with a different assignment.

```json
{
  "dependencies": [{
    "plan_id": "<exact-prerequisite-id>",
    "body_digest": "<sha256-of-prerequisite-body>",
    "kind": "source",
    "outputs": [{"path": "src/contract.rs", "sha256": "<required-file-sha256>"}]
  }],
  "assignment": {"harness": "codex", "fork": "consumer-fork"},
  "owned_paths": ["src/consumer.rs"]
}
```

A `completion` edge needs a matching evidence-bearing completion receipt. A
`source` edge also needs the declared outputs in the completion root, the
executor's **published** root and its current files. Completion elsewhere,
retained objects, staged merges and ancestry alone cannot satisfy it. JSON
`evidence_basis` distinguishes required-output identity from native compatibility
acceptance. For compatible changed implementations, the native coordinator can
use `plan accept-source <id> <prerequisite> --revision <revision> --evidence
"<compatibility judgment>"`; this records an explicit judgment bound to the
upstream completion root and unchanged published executor source. Dirty or
unavailable source cannot silently pass. No similarly titled replacement is
inferred for a missing, abandoned or explicitly superseded prerequisite.

To advance existing authorization, an assignment can include `dispatch` with
`operation_id` (stable across observations), `task` and `authorization` (the
existing owner/programme instruction). The service and native hooks then
launch ready approved/active bindings through the existing detached delegate.
`plan advance` performs the same reconciliation immediately. Readiness does not
grant new authority or spending. Drafts never dispatch; declared overlaps remain
visible for native judgment. Launch rechecks the graph/source/assignment under
the delegation key lock. Automatic operations use one reservation in the verified
trunk owner's delegation store, including when triggered from an executor fork.
The shared Git location, registered fork path and StateRoot project identity must
agree; independent projects do not inherit that authority. The current owner
must still approve the same declaration/body (approved-owner/active-fork is
compatible). A stale fork cannot revive abandoned or reassigned owner work.
Fork graphs read this same authoritative reservation; manual delegation keeps
its caller-owned store. Automatic observations never retry existing attempts.
Unknown/starting attempts require reconciliation from their original runtime;
a deliberate manual invocation may still retry a proven failed/lost attempt
under the same key, preserving its attempt history.

CLI and native continuity consume this projection. Dedicated MCP inspection and
workbench dependency views remain integration work; no separate UI readiness
logic or store is implied.

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
`delegation_status_unknown` requests inspection from the original runtime;
`delegation_record_degraded` identifies a preserved record that cannot be
interpreted. Neither is evidence that the worker failed or completed its plan.

## Optional plan command observations and state compatibility

`stateroot plan verify <id> -- <command> [args...]` explicitly runs a local
command and records immutable started/finished observations. It does not
complete a plan or certify semantic acceptance. Bindings cover project
identity, exact plan body and eligible before/after project bytes using current
ignore declarations. Raw stdout/stderr remains under excluded
`.stateroot/local/verification/`; shared records contain digests and outcomes.
For a reconciliation candidate, that directory belongs to the owning project,
and the command prints its actual location. Candidate publication and cleanup
therefore preserve the local output. Verification source trees are retained in
the owning repository so ordinary Git garbage collection preserves their objects.
Source fingerprinting reads eligible project bytes without traversing the
StateRoot store; full snapshots continue to capture the retained project state.
Argument metadata is bounded; oversized metadata is rejected before execution.
Snapshots and checkpoints never run verification commands automatically.

`stateroot plan done <id> --evidence "..." --verification <v_id>` optionally
attaches an observed successful command. Wrong-project/plan/attempt, unsupported
records, failed outcomes and mismatched source/body are rejected before closure.
Exact unchanged inputs can reuse observations; source capture/root bookkeeping
changes do not by themselves invalidate them. This is before/after observation,
not execution isolation, credential/environment reproduction or proof that every
acceptance criterion was satisfied. Omitting `--verification` retains explicit
author-declared completion; old completions are never retroactively verified.

`doctor` adds a bounded read-only authoritative-state summary to human output
and `authoritative_state` in its JSON document. Base health remains separate
from retained root/direct-parent availability, metadata compatibility, capture
facts, declared/command evidence and restoration limits. Unknown schemas remain
visible; supported legacy bytes stay inspectable. Incompatible root/merge/plan
operations refuse rather than silently migrating or overwriting state. Package
downgrade is not historical state restoration. No-root, partial scans, unknown
metadata and missing verification are not reported as verified acceptance.

## Capture exclusions and original-evidence recall

Root `.gitignore` and `.staterootignore` exclude matching project files from
snapshots. Tool results are a separate observation stream: by default their
full payloads remain shared evidence, even when a result quotes an excluded
file. For known file-tool inputs whose full result must stay on this machine,
create a root `.staterootcaptureignore` with gitignore-style relative paths
(for example, `private/`). A matching existing file path routes the raw hook
payload to `.stateroot/local/captured-tool-output/` and shares only a labeled
observation with its event identity, local locator and digest. An invalid
declaration or a reported file path that cannot be verified also keeps that
event local. `stateroot observations show <id>` points at the local evidence;
`observations health` counts local-only and missing sources separately.

This rule inspects explicit file-path fields in hook inputs. It does not parse
shell command strings, infer the source of arbitrary output, change native
transcript files, filter authored handoffs/memories, or rewrite older roots.
An unproven shell result is still shared by default. It is a declaration about
capture routing, not secret-pattern scanning.

`stateroot memory recall` indexes a bounded opening and recent window from
long native sessions (4,096 opening characters plus 27,904 recent characters
when a middle is omitted). Hits retain the native source reference and label
the omitted middle. Recall also searches a bounded raw observation window,
independently of the derived index. At most `--limit` hits are returned: up to
three slots are reserved for matching raw events, with the remaining slots
keeping the indexed ranking. Raw hits are labeled `(raw observation)` and
retain their `cap_…` or legacy `obs_N` locator for `observations show <id>`.
Identical text from different captures keeps distinct event locators. Raw
availability does not mean a projection is pending or that the event is newer
than every indexed source.

The raw window uses existing recent-capture references plus at most 256
directory entries and one overflow probe. Candidate modification times are
sampled once; the newest eight discovered files contribute at most 512 KiB
each, ending at the file length observed when opened. Complete records are
aligned before UTF-8 decoding. Capture timestamps order matching raw events.
Oversized records, omitted history, corrupt/unreadable sources, and result
budget omissions are reported as partial coverage, including on no-match.
Legacy tails cannot supply stable line numbers and are explicitly omitted.
`stateroot observations search`, `observations health`, and direct native
sources remain available for evidence outside these windows. Canonical
capture files are never rewritten by recall.

Source resolution decodes intact records individually, so a corrupt sibling
does not invalidate a returned capture ID. Legacy `obs_N` references retain
their physical line numbers. If corruption or a read failure prevents proving
an unmatched capture absent, `observations show` reports it unavailable.
`observations health` retains the corruption/read-failure diagnostics. The
existing visibility rules apply before the fallback hit limit, so a private
match cannot consume an external caller's shared-result slot.

The core producer `memory_index::search_report` returns `RecallReport` with
`hits`, `index_state` (`not_searched`, `current`, `stale`, or `unavailable`),
`index_detail` (nullable failure detail), and `raw_coverage`. Coverage contains
`partial`, `directory_entries`, `files_read`, `bytes_read`, `matches_omitted`,
`diagnostics` (at most 16), and `diagnostics_omitted`. Hit `kind=observation`
identifies raw evidence; `path` is the stable observation locator and `source`
carries harness/session provenance. `stale` describes indexed-source fallback,
not raw capture age. Consumers must expose index and raw coverage independently,
even when there are no hits. The compatibility `search` API returns bounded
hits without this report; MCP wire adoption is a separate integration step.

Unspaced-script recall (Han/Kana/Hangul): the FTS tokenizer folds an unspaced
run into ONE token, so a bare FTS query only matches exact run boundaries.
Query terms from those scripts therefore also run a bounded substring pass
over the derived index docs — independent of the old newest-1000 fallback
scan, so an old CJK document outside that window is still found — and never
re-reading the native sources. Query punctuation is normalized away first
(alphanumerics plus `_`/`-` survive; `支付服务？` and `（支付服务）` carry the
same intended run), and typed wildcards never widen the match. The FTS stream
and the substring stream are merged by reciprocal-rank fusion, so each stream
keeps its own relevance order without comparing incompatible raw score
scales; a doc found by both appears once with the summed rank
contribution. Reported scores are retrieval ranking within one result set,
not confidence. The pass is a limited derived-index scan with result limits —
not an indexed substring accelerator and not an unbounded-completeness
guarantee.

## Recovery diagnostics — an interrupted-work example

The durable observation, finalize-journal and resume primitives compose into
one inspectable recovery flow. An already-queued boundary job retains its
frozen identity across interruption, and a safe entrypoint can retry that
retained job. A writer killed before durable publication may leave no record
or job. Inspect the retained evidence without changing it:

```bash
stateroot observations health        # torn/corrupt capture tails counted, bytes retained
stateroot handoff inspect --job <id> # the retained boundary job as JSON — no retry, no rewrite
stateroot resume                     # inspect current root, retained references, and outstanding work
stateroot doctor                     # authoritative-state summary: root availability, capture facts
```

This composition is proven by an acceptance test over composed local fixtures
(owned fixture kills, journal phase rewind, exact-occurrence replay) — it is
not evidence of real quota-exhaustion or native-harness recovery in the wild.

## `stateroot handoff` — write flags and accept gates

New handoffs reserve a project-wide number across checkouts sharing the same
Git common directory. The counter is machine-local, locked and atomically
updated; failed publication may leave a harmless gap. Initial allocation (or
recovery of a missing counter) starts above known checkout and retained fork
history. Existing packets are never renumbered. All writers must run the
updated CLI: older writers and disconnected clones can still produce duplicate
numbers, so packet IDs remain the exact identity across those histories.

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

Hook delivery is decided before rendering the full context. A deduplicated
event does not build a discarded full digest; a delivered full injection
builds it once. Session-boundary maintenance remains active even when the
identity output is intentionally silent. Silence alone is not a reason to
force another resume or disable maintenance.

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

Automatic update routes skip Cargo build artifacts and the existing test-harness
marker before release lookup or worker launch. An explicit `self-update --check`
still inspects metadata; a forbidden development-binary installation refuses
before downloading assets/checksums or creating a download temporary file.
Normal installed-CLI automatic updating is unchanged.

Release lookups share one path for production, nightly and explicit tags.
Anonymous downloads prefer public GitHub release URLs rather than consuming
REST API quota for each asset. If GitHub confirms an API rate limit (`403`
with rate-limit evidence, or `429`), the updater tries the public
`stateroot-release.json` asset once. Release jobs publish its real tag, nightly
build name and asset URLs; downloaded binaries and extension assets still
require the existing checksum checks. An arbitrary `403` is not labeled a
rate limit. Missing/invalid public metadata remains an error, with GitHub's
retry/reset guidance when supplied; old releases may not have this asset.

For an already-installed client blocked by API quota, this PowerShell workaround
uses an existing GitHub CLI login without printing its token:

```powershell
# If needed first: gh auth login --hostname github.com --web
$env:GH_TOKEN = gh auth token --hostname github.com
if ($LASTEXITCODE -ne 0) { throw "GitHub CLI authentication is required for this workaround" }
stateroot self-update
```

`GH_TOKEN` applies to this terminal and its child processes. GitHub CLI normally
uses the OS credential store; merely being signed into VS Code, or having a
GitHub CLI login with no plaintext token file, does not set this variable.
Authentication is a workaround for older clients/release metadata, not a new
requirement for public updates. New fallback behavior requires both the updated
CLI and a release carrying `stateroot-release.json`.

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
result of the pass; human progress (including daily update scheduling and
VS Code/Cursor reconciliation) moves to stderr, and a partial install
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
