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

Handoffs follow the canonical schema (`stateroot.handoff.v1`; see `technical/stateroot_canonical_schema.md`). A handoff write MUST cover five core sections:

1. **objective** — the current top-level goal in one sentence.
2. **state** — status (`active|blocked|paused|done`), current phase, and a compact implementation-status summary.
3. **decisions** — each decision with the *why*. This section includes **failed_approaches**: every approach tried and rejected or abandoned, with the reason it failed, so the next harness does not repeat it.
4. **next_actions** — ordered, concrete, executable without re-discovery.
5. **failed_approaches** — surfaced as its own section (and searched before new attempts); never bury failures inside `decisions` prose only.

Supporting fields to populate when known: `changed_files[]`, `tests_run[]`, `bugs_found[]`, `blockers[]`, `open_questions[]`, `warnings[]`, `relevant_memories[]`, `relevant_skills[]`, `artifacts[]`, `traces[]`, `context_summary`. The CLI sets `last_harness` and `recommended_next_harness` from `--to`.

Quality bar: a handoff without `next_actions` or `failed_approaches` is a failed handoff. Write the handoff early enough that a usage-limit cutoff still leaves a complete packet — do not wait for the last moment.

## Offline Outbox Semantics

1. When the server is unreachable, the CLI writes operations to a local outbox under `.stateroot/` and still exits successfully. Queued operations sync automatically when connectivity returns.
2. Ordering is preserved per project. Do not re-issue the same checkpoint or handoff in a retry loop — queued operations are not lost, and duplicates pollute the episodic log.
3. Never edit, dedupe, or "clean up" outbox files by hand; the CLI owns them.
4. `stateroot status` shows outbox depth and project attachment; `stateroot doctor` diagnoses sync and auth problems.
5. After reconnecting, run `stateroot resume` to see synced state. If a write reports a conflict or stale revision, re-read with `resume` and retry the write exactly once.
