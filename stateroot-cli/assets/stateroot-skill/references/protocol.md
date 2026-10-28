# StateRoot Protocol

Operational details behind the rules in `SKILL.md`. Read this when the top-level rules leave a question open.

## Resume digest (never truncate)

`stateroot resume` and the session-start hook digest are the project state of record. Run them as a bare command — no pipe, pager, or line/byte cap.

Forbidden (including equivalents):

```bash
stateroot resume --harness kimi 2>&1 | head -100
stateroot resume --harness cursor | tail -50
stateroot resume | less
```

Do not invent `--budget`. The CLI already sized the digest. A 2–3 sentence user summary after you have ingested every section is fine; clipping the digest itself is not. If a tool UI offers "limit output" or auto-pipes through `head`, disable it for `resume`, `status`, `doctor`, `log`, and `handoff show`.

**A truncated tool *display* is not a truncated digest.** Harness UIs may elide long tool results visually while the model still receives the full text. If the digest looks cut off in the UI, treat it as complete anyway: it is the state of record, and its sections already answer "where are we". Do NOT re-fetch the same state via `handoff show`, `handoff list`, `memory recall`, or session tools to compensate — one `resume` (or one injected hook digest) is the whole answer. Reach for other tools only for things the digest does not carry.

## Checkpoint Cadence

1. Checkpoint after every step that changes project state:
   - files written, edited, moved, or deleted
   - decisions made (architecture, library choice, approach selected)
   - milestones reached (tests passing, feature complete, phase done)
   - blockers discovered or approaches abandoned
2. Granularity is one logical step, not one tool call. A step that takes twenty tool calls gets one checkpoint at the end.
3. Order matters: checkpoint the finished step BEFORE starting the next one. Never batch several finished steps into a single checkpoint.
4. Note format: `<what changed> — <why> — <what it unblocks>`. Keep it specific and short; use `--files a,b` to name the touched files instead of listing them in prose.
5. Checkpoints are append-only and offline-safe. When in doubt, checkpoint — a noisy log is recoverable, a missing one is not.

## Work-State Lineage (snap / revert / fork)

Handoff carries **session continuity**. **Root lineage** records verified project-tree state under `refs/stateroot`:

1. Automatic: checkpoints and finished turns snap the working tree whenever project files changed (store bookkeeping never creates a root). Run `stateroot snap [--reason "..."]` for explicit milestones.
2. Resume and hook digests show the current root, prior transition when available, last actor, and verified tree delta when recorded.
3. Run `stateroot revert <root>` only for verified restoration — append-only, creates a new root.
4. Run `stateroot fork <root>` when work should diverge from an earlier root.
5. Use `stateroot log`, `stateroot show`, `stateroot diff`, and `stateroot compare` to inspect lineage — do not invent history the CLI has not verified.

## Scoped audit and evidence reuse

Use this procedure for non-trivial changes, before proposing additions. Scale effort to task risk, the actual contract and uncertainty; reuse valid intelligence first and widen only as evidence warrants. Routine small edits need no new command ritual or full inventory.

1. **Locate the responsibility.** Start with the digest and relevant retained findings, decisions and failures; recall missing context with `stateroot memory recall "<query>"`. Use root/working-tree differences, cheap broad searches and targeted reads of relevant callers, consumers and sibling implementations. Trace who owns the behavior, duplicate decisions, reuse opportunities and obsolete paths. Read actual producer output and consumer expectations, not only types or mocks. A product direction statement does not authorize invented UI or other features.
2. **Repair the cause.** Prefer the existing owner or mechanism when it fits. Remove superseded paths and consolidate duplicate responsibility in the same scoped change. Preserve necessary compatibility and independent failure boundaries, recording why each exception remains. Similar code can serve different contracts; no deletion quota, blanket DRY rule, speculative refactor or compulsory generic abstraction.
3. **Evolve coverage.** Update tests with the accepted contract. Consolidate duplicated setup and policy matrices; retire obsolete assertions. For a removed meaningful case, identify the retained check that covers it or explain why the contract no longer requires it. Preserve real process, wire, persistence, platform and interrupted-work checks wherever those boundaries matter. Passing mocks and test counts cannot establish integration. Run focused checks while iterating; broaden when changed dependencies or remaining uncertainty warrant it, including any required final combined gate. Do not reread the whole repository with an LLM or rerun every suite after each edit.
4. **Retain and reuse evidence.** In existing checkpoints/handoffs, keep concise findings with source paths/locations and root or working-tree identity, relevant dependencies/configuration/tools, assumptions, accepted exceptions, and check commands/outcomes/limits. Keep full evidence and context available at their existing sources and retrieve details progressively through locators; this is retrieval, not data pruning, and preserves required full resume ingestion. Another harness starts from this record and the current delta rather than replaying all discovery. Reuse a finding only after checking its validity boundary: a changed caller, dependency/interface, configuration, tool or assumption can invalidate it even when the named file is unchanged. Recheck affected conclusions and checks, reusing the rest. Notes are context, not automatic verification authority; exact-source command observations retain the stricter semantics below.
5. **Account for total cost.** Include token use, runtime and avoidable rework in the result. Use measured native usage/timing when available; otherwise label proxies (such as bytes, words, tool calls or scoped command timing) and unavailable data. Compare against the starting point and explain necessary net increases. Savings never justify hiding failures, omitting relevant context or silently skipping validation. Reasoning belongs in the native harness; local StateRoot only stores and executes deterministic continuity operations. Use existing records, not a new audit subsystem or local AI service.

Examples of applying the procedure:

- **Producer/parser mismatch:** a producer emits a shorter degraded-status row than its consumer expects. Trace the shifted fields through downstream views; exercise actual output through the consumer. Preserve degraded evidence and the shared status owner's semantics. Repair the boundary rather than invent a dashboard or duplicate status policy.
- **Small dependency change:** the parser file is unchanged, but its producer's format changes. Invalidate the affected compatibility finding, inspect the delta and rerun that boundary check. Reuse unrelated findings whose assumptions still hold; prior exact-source verification does not cover the changed tree.

For optional Wiki compilation, declare relevant source/instruction/tool files
and selected Skill resources with `stateroot wiki compile --dependencies
'[...]'`; the array replaces the retained declarations (`[]` clears them).
Use project-relative file references or the existing Skill selector/file
contract. Resolved content enters the generation payload. Do not hand-edit
inbox frontmatter. The completed deterministic floor and optional successful
generation have separate identities. Failed or interrupted optional apply
retains the previous success and can retry at the existing boundary; output
conflicts preserve authored bytes. Once publication starts, recovery material
survives later source/configuration changes and `--force`; archive preservation
precedes apex demotion. Changed learning inputs still require actual mining.
An optional output conflict does not block new notes from the keyless floor;
its newer Wiki writes remain intact alongside the retained conflict journal.
Optional log publication appends once; it never restores an older full log
over newer floor entries, including when recovering previously saved journals.
A successful apply is synthesized output,
not semantic verification. Undeclared dependencies remain unknown.

## Progressive Skill reuse and source-linked authoring

Without shell access, local MCP `skill_inspect` reads package identity and
resources; `skill_propose` supports optional evidence and base64 resources with
immediate activation. MCP `root_inspect`, `root_compare`, `transition_inspect`,
`handoff_list`, `handoff_inspect`, `verification_inspect`, `plan_inspect` and `plan_graph`
expose existing continuity readers. Verification is an observation, not
semantic acceptance; use exact handoff IDs when a sequence is ambiguous.
MCP `native_session_inspect` reads an explicit retained archive and optional
saved operation receipt without launching the native harness. Its operation
state is the last recorded observation, not a fresh destination verification.

The native agent chooses applicability from `stateroot skill list` or
`stateroot skill scan --json`; StateRoot does not select Skills or judge
whether completed work generalizes. Keep reference-only/native routes visible
and delegate through their existing routes when appropriate.
The existing workbench Intelligence view reads that inventory alongside
`rules list --json`, `mcp status --json`, `mcp doctor --json`,
`mcp tools --json`, and `doctor --json`.
It shows configured state separately from observed use; unknown loading or
invocation is never presented as success.

1. Inspect a chosen Skill with `stateroot skill show <identity_key> --json`.
   Read the needed entrypoint/resource using `--digest <package_digest>
   --file <relative-path>`. A changed digest or ambiguous name requires a fresh
   inspection; do not silently use different bytes. Full metadata/source
   evidence is available on detail inspection, not repeated in the catalog.
2. Report actual evidence: projection is availability, file delivery is not
   proof of reading, and reading is not application or a successful outcome.
   A missing or explicit inspection selector does not establish task-level
   selection; report an actual choice separately through the normal handoff.
   Name the native harness/model when known, the bounded task, resources read,
   actual action, result and limitations. Only Pi currently declares isolated
   Skill discovery for StateRoot-owned launches.
3. When a useful procedure is worth preserving, `stateroot skill write
   --input - --from <harness>` accepts the existing explicit authoring content
   (`slug`, `skill_md`) plus optional `files_base64` resources and `evidence`
   references. Preserve actual source plan/body/root/session/delegation/
   verification locators and declared/observed/historical status. Historical
   success is not current verification; missing or foreign references remain
   context. Supplied status never makes an assertion authoritative.
4. Explicit writing activates immediately, with no candidate/promotion gate.
   Retain relevant resources and context; omitted resources survive updates.
   Supplied resources carry bytes; new scripts need their interpreter unless
   executable permissions are established separately. Existing modes survive
   updates, but package hashes do not certify them.
   Sidecar provenance is outside the package byte digest and has a separate
   metadata digest. Inspect `hash_exclusions` before claiming complete hash
   coverage. Checkpoint the actual result through the normal protocol.

The current CLI uses the shared package writer; the legacy MCP `skill_propose`
adapter is pending its separately owned integration. Keep that distinction
visible when qualifying a particular transport.

## Raw evidence in recall

`memory recall` searches bounded raw observations even when the derived index
is unavailable. Raw events are labeled `(raw observation)`, with stable
`cap_…`/`obs_N` references for `observations show <id>`. Equal text does not
mean the same captured event. Raw availability is not an index backlog claim.
The requested hit limit includes up to three reserved raw-event slots.
Source resolution preserves intact records among corrupt siblings and keeps
legacy physical line locators. An unresolved ID with incomplete archive
coverage is unavailable, not confirmed absent; inspect `observations health`.

Read index degradation and raw coverage separately, including on no-match.
Discovery, file/byte windows, unreadable/corrupt records, and result limits can
make coverage partial. `observations search` and `observations health` inspect
retained evidence beyond the recall window; recall never deletes that evidence.
Core consumers use `memory_index::search_report` to carry these facts. Do not
claim installed MCP coverage until its handler has adopted that report.

## Verification and publication

Run `stateroot plan verify <id> -- <project-check-command> [args...]` on the actual source to retain command evidence. The observation binds eligible source bytes and the plan body; its source-root pointer may predate unsnapped edits, so compare the observed product tree, not just the root pointer. Exit zero is command evidence, not semantic acceptance.

For a reconciliation candidate, verify there using its owning repository, then restage the observation metadata and publish from the owner. Do not snapshot the candidate or create a private repository. Unchanged source may reuse evidence across metadata-only restaging; changed source needs new checks. Raw stdout/stderr stay in the owner's local verification lane after publication and candidate cleanup.

When automating installation, parse the complete stdout of `stateroot install --json` as one document and inspect its `install` outcome. Human scheduling and editor-reconciliation messages belong to stderr. Malformed output is not success and must not be repaired by stripping arbitrary text or recording a Ready receipt.

## Dependency execution

The plan sidecar is the sole declaration owner. `plan graph --json` exposes a
stable observation identity and each plan's current revision; `plan declare
<exact-id> --revision <revision>` reads a replacement declaration from stdin.
Explicit edits use v2 metadata so older writers refuse them. Do not infer edges
from Markdown or upgrade live programme records while testing schema changes.
Completion edges bind the exact prerequisite body/receipt. Source edges also
check declared output hashes against the completion root, executor publication
and current files. A staged merge or a published merge retaining main's bytes
is not proof of required output availability. Native compatibility judgment is
recorded with `plan accept-source`, tied to unchanged published target source.

Keep assignments separate from worker evidence. An authorized dispatch carries
its existing authorization, task and stable operation identity in the assignment;
service/hooks and `plan advance` use the same detached-worker key lock and
revalidate readiness at launch. Automatic reservations belong to the verified
trunk owner across registered executor forks; Git location, fork registration,
project identity and current owner authorization must agree. Fork graphs read
that shared reservation. Keep automatic programme declarations synchronized with
the owner; a stale fork cannot revive an abandoned or reassigned operation.
Manual delegation retains its caller-owned record. Observations never retry an existing attempt.
Inspect unknown/starting attempts from the original runtime; do not invent a new
key to bypass uncertainty. Manual retries of proven failed/lost workers retain
the existing attempt history. Ownership overlap is visible for native judgment,
not a promise that graph-independent work can edit the same files safely.

## Update troubleshooting

Release API quota is shared by public IP, so repeated Windows update failures
can also affect other users behind the same network. Updated clients try the
public `stateroot-release.json` asset on a confirmed API rate limit and retain
checksum verification. Older clients/releases may lack this fallback; do not
assume every `403` is a rate limit or retry in a loop before GitHub's reset.

In PowerShell, an existing GitHub CLI login can supply a token without printing
it: `$env:GH_TOKEN = gh auth token --hostname github.com`, then check
`$LASTEXITCODE` before running `stateroot self-update`. If not signed in, the
user can run `gh auth login --hostname github.com --web`. VS Code sign-in does
not automatically authenticate the updater; GitHub CLI may store its token in
the OS credential store rather than a readable `hosts.yml`. Do not ask users
to paste tokens into chat. Keep login optional for normal public updates.

## Handoff Packet Fields

Near usage limits or harness switches, prefer a **flag-first** write (one command, no temp JSON). Use `--input` only when the payload is large.

```bash
stateroot handoff write --from <resolved-current-harness> [--to <harness>] \
  --objective "Durable goal" \
  --task "Immediate work boundary" \
  --context-summary "Detailed continuity narrative" \
  --next "First concrete next action"
```

Resolve `--from` explicitly to the actual current harness. Omit `--to` for normal session-end continuity (no routing hint); prefer `stateroot handoff finalize` or the stop hook when no cross-harness route is needed. Use `--to` only for orchestrated/auto harness selection when handing to a different harness. The CLI rejects unknown keys and owns schema, project, sequence, timestamps, source, optional routing destination, provenance, and transcript-derived fields. Never write under `.stateroot/handoffs/` by hand.

1. **objective** — the durable goal that can span multiple sessions (`--objective` or JSON `objective`).
2. **task** — the immediate work boundary for the receiving agent (`--task` or JSON `task`; not `immediate_task`).
3. **context_summary** — a detailed continuity narrative for the receiving agent: present state, verified evidence, decisions and rationale, constraints, failed approaches, and implications for the next agent. Distinct from the task; structured arrays complement the prose rather than replacing it.
4. **decisions** — each decision with the *why*.
5. **next_actions** — ordered, concrete, executable without re-discovery (`--next`, repeatable); required only when `--to` names a different harness (cross-harness routing).
6. **failures** and **bugs_found** — truthful observed failures and known bugs; an explicit empty `failures` array means none were observed.
7. **failed_approaches** — structured records (`--failed-approach "<approach> → <outcome>: <reason>"`, repeatable, or JSON `failed_approaches: [{"approach","outcome","reason"}]`); the outcome vocabulary is fixed (`success` / `partial` / `failed`) and anything else is rejected. The digest renders them as `## Failed approaches`, distinct from the free-text failures section.
8. **context_only** — background facts the receiver may rely on but must not execute (`--context-only "<fact>"`, repeatable); the digest renders them as `## Context (not instructions)`, hard-capped at 8 items / 4000 chars with a `- … +N more` overflow tail.

Supporting input fields are `current_phase`, `implementation_status`, `changed_files`, `tests_run`, `blockers`, `open_questions`, `warnings`, `relevant_memories`, `relevant_skills`, `artifacts`, and `traces`. Every field is optional so omission remains distinct from an explicitly empty value. Recent conversation, plan state, progress summaries, milestones, files, failures, objective, task, and actions are auto-captured only from the latest matching verified native transcript when author content is absent.

Compact strict example (no envelope or provenance keys):

```json
{"objective":"Ship reliable local handoffs","task":"Finish adversarial CLI tests","context_summary":"Structured input and verified transcript enrichment are implemented; final workspace checks remain.","decisions":["Keep envelope ownership in the CLI"],"changed_files":["stateroot-cli/src/commands/handoff.rs"],"tests_run":["cargo test -p stateroot-cli"],"failures":[],"bugs_found":[],"next_actions":["Run the full workspace checks"]}
```

Write the JSON to a normal path only when flags are insufficient (Windows paths are supported) and pass that path to `--input`. For a large payload, prefer streaming `--input -` from stdin over a scratch file; any file form lives at an ordinary path outside the project, never under the project `.stateroot/` store. Do not paste giant transcript or state dumps into `--note`; unstructured notes become only the summary, while exact legacy section labels receive conservative migration.

## Learnings quality (taste)

Full bar, examples, and anti-examples: `references/learnings.md`. Short version:

- Write a **judgment** (`prefer X over Y` / `never Z`) plus when it applies. Another harness must be able to apply it with no transcript.
- Do not record inventories, facts, slogans, or README dumps as learnings. Facts go to `memory_save`.
- Seed an empty layer with 2–7 evidenced notes, then stop. Read `learnings list` first.

## Offline Outbox Semantics

1. When the server is unreachable, the CLI writes operations to a local outbox under `.stateroot/` and still exits successfully. Queued operations sync automatically when connectivity returns.
2. Ordering is preserved per project. Do not re-issue the same checkpoint or handoff in a retry loop — queued operations are not lost, and duplicates pollute the episodic log.
3. Never edit, dedupe, or "clean up" outbox files by hand; the CLI owns them.
4. `stateroot status` shows outbox depth and project attachment; `stateroot doctor` diagnoses sync and auth problems.
5. After reconnecting, run `stateroot resume` to see synced state. If a write reports a conflict or stale revision, re-read with `resume` and retry the write exactly once.

## Qualified native Codex recovery

Existing session import automatically retains supported settled native Codex
boundaries locally. A final handoff is not required. Use `session show <id>` to
select the exact archive digest, then `session transfer <id> --to codex --archive
<digest> --operation-id <stable-uuid>`. Reuse that UUID for an interrupted retry;
uncertain outcomes reconcile without creating another native fork.

Failed and interrupted turns retain their original status and do not prevent
capturing a later settled turn. Partial capture explicitly names its older
boundary and uncovered suffix; do not treat it as fresh full-source capture.
Concurrent inspection refuses stale receipt publication, preserving newer
observed continuation evidence.
Standalone legacy error events stay in the raw archive but are explicitly
unqualified for native recovery because Codex 0.149.1 drops them during fork
persistence. Terminal task-completion errors remain supported.

Qualification currently covers Codex CLI 0.149.1 experimental app-server on
Linux x86_64, legacy rollout history and the original workspace path. A fresh
native home/registry works with matching captured file configuration. The
native model/provider/permission settings must match. A missing live rollout
can recover from retained bytes; missing/corrupt retained bytes cannot. Do not
substitute a newer live conversation or treat project-root restoration as
native history recovery. Archives and receipts stay under local sessions and
are not transported by roots.

Transfer verifies raw SHA-256, source ancestry, full historical turn identities
and the payload prefix before returning a native resume command. Its prefix
digest excludes only the outer transport timestamp that native fork rewrites;
new destination metadata and later turns need not equal source bytes. Native
Codex performs continuation. `session show <id> --operation-id <uuid>` observes
completed native output separately; assess useful retained context from the
actual output, never file creation alone. StateRoot does not restore credentials,
install software, switch accounts or reconstruct historical launch arguments.
