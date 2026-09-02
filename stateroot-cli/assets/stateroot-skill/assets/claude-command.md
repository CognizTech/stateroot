---
description: Resume StateRoot project state and bind the stateroot protocol for this session
---

Prefer the auto-injected StateRoot digest from hooks when present. Only run `stateroot resume --harness claude` if no digest appeared. Manual resume is the last fallback. Treat that **entire** output — current handoff, hot-apex memory, context pack — as the project state of record for this session. Do not run resume twice unless the user explicitly asks (`--force` reprints). Never pipe resume through `head`, `tail`, or any line limiter.

Then follow the stateroot skill protocol mechanically:

1. After every step that changes project state (files written, decisions made, milestones, blockers), run `stateroot checkpoint --note "<what changed and why>"`.
2. Before attempting a non-trivial approach, run `stateroot memory recall "failed approach <topic>"` and do not repeat recorded failures.
3. After meaningful real-tree changes, run `stateroot snap`; use `stateroot revert` / `stateroot fork` when lineage recovery or branching is needed.
3. Before ending the session, when approaching usage limits, or when asked to switch harness, prefer `stateroot handoff write --from claude [--to <harness>] --objective "…" --task "…" --context-summary "…" [--next "…"]` (one command, no temp JSON). Omit `--to` for continuity-only. Use `--input` only for large payloads. Never write under `.stateroot/handoffs/` by hand. Field is `--task`, not `immediate_task`.
3b. Planning for another harness to implement? Record the plan in the shared store, not only in claude's plan mode: `stateroot plan record --stdin --title "…"` (or `--file`), then hand off with `--to <harness>` — the executor's digest says *execute it; do not re-plan*.
4. Never edit files under `.stateroot/` directly — all state access goes through the CLI. The CLI is offline-safe and queues operations locally when the network is down.
