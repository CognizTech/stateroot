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

## Verification and publication

Run `stateroot plan verify <id> -- <project-check-command> [args...]` on the actual source to retain command evidence. The observation binds eligible source bytes and the plan body; its source-root pointer may predate unsnapped edits, so compare the observed product tree, not just the root pointer. Exit zero is command evidence, not semantic acceptance.

For a reconciliation candidate, verify there using its owning repository, then restage the observation metadata and publish from the owner. Do not snapshot the candidate or create a private repository. Unchanged source may reuse evidence across metadata-only restaging; changed source needs new checks. Raw stdout/stderr stay in the owner's local verification lane after publication and candidate cleanup.

When automating installation, parse the complete stdout of `stateroot install --json` as one document and inspect its `install` outcome. Human scheduling and editor-reconciliation messages belong to stderr. Malformed output is not success and must not be repaired by stripping arbitrary text or recording a Ready receipt.

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
