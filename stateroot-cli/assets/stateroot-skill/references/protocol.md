# StateRoot Protocol

Operational details behind the rules in `SKILL.md`. Read this when the top-level rules leave a question open.

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

## Handoff Packet Fields

Handoffs use strict content JSON passed with `stateroot handoff write --from <resolved-current-harness> --to <harness> --input <handoff.json>`. Resolve `--from` explicitly to the actual current harness. The CLI rejects unknown keys and owns schema, project, sequence, timestamps, source, destination, provenance, and transcript-derived fields.

1. **objective** — the durable goal that can span multiple sessions.
2. **task** — the immediate work boundary for the receiving agent.
3. **context_summary** — a detailed continuity narrative for the receiving agent: present state, verified evidence, decisions and rationale, constraints, failed approaches, and implications for the next agent. Distinct from the task; structured arrays complement the prose rather than replacing it.
4. **decisions** — each decision with the *why*.
5. **next_actions** — ordered, concrete, executable without re-discovery; required when switching to another harness.
6. **failures** and **bugs_found** — truthful observed failures and known bugs; an explicit empty `failures` array means none were observed.

Supporting input fields are `current_phase`, `implementation_status`, `changed_files`, `tests_run`, `blockers`, `open_questions`, `warnings`, `relevant_memories`, `relevant_skills`, `artifacts`, and `traces`. Every field is optional so omission remains distinct from an explicitly empty value. Recent conversation, plan state, progress summaries, milestones, files, failures, objective, task, and actions are auto-captured only from the latest matching verified native transcript when author content is absent.

Compact strict example (no envelope or provenance keys):

```json
{"objective":"Ship reliable local handoffs","task":"Finish adversarial CLI tests","context_summary":"Structured input and verified transcript enrichment are implemented; final workspace checks remain.","decisions":["Keep envelope ownership in the CLI"],"changed_files":["stateroot-cli/src/commands/handoff.rs"],"tests_run":["cargo test -p stateroot-cli"],"failures":[],"bugs_found":[],"next_actions":["Run the full workspace checks"]}
```

Write the JSON to a normal path (Windows paths are supported) and pass that path to `--input`. `--input -` may read stdin when convenient, but shell piping is optional. Do not paste giant transcript or state dumps into `--note`; unstructured notes become only the summary, while exact legacy section labels receive conservative migration.

## Offline Outbox Semantics

1. When the server is unreachable, the CLI writes operations to a local outbox under `.stateroot/` and still exits successfully. Queued operations sync automatically when connectivity returns.
2. Ordering is preserved per project. Do not re-issue the same checkpoint or handoff in a retry loop — queued operations are not lost, and duplicates pollute the episodic log.
3. Never edit, dedupe, or "clean up" outbox files by hand; the CLI owns them.
4. `stateroot status` shows outbox depth and project attachment; `stateroot doctor` diagnoses sync and auth problems.
5. After reconnecting, run `stateroot resume` to see synced state. If a write reports a conflict or stale revision, re-read with `resume` and retry the write exactly once.
