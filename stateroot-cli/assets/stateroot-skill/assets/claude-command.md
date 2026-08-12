---
description: Resume StateRoot project state and bind the stateroot protocol for this session
---

Prefer the auto-injected StateRoot digest from hooks when present. Only run `stateroot resume --harness claude` if no digest appeared yet. Treat that output — current handoff, hot-apex memory, context pack — as the project state of record for this session. Do not run resume twice unless the user explicitly asks (`--force` reprints).

Then follow the stateroot skill protocol mechanically:

1. After every step that changes project state (files written, decisions made, milestones, blockers), run `stateroot checkpoint --note "<what changed and why>"`.
2. Before attempting a non-trivial approach, run `stateroot search "failed approach <topic>"` and do not repeat recorded failures.
3. Before ending the session, when approaching usage limits, or when asked to switch harness, write strict structured JSON and run `stateroot handoff write --from claude [--to <harness>] --input <handoff.json>`. Omit `--to` for continuity-only; use it only when orchestrating a cross-harness switch. Include durable objective, immediate task, a detailed continuity narrative in `context_summary`, decisions with rationale, truthful failures, and next actions; verified recent conversation is auto-captured. Do not paste giant dumps into legacy `--note`. Normal Windows paths work; `--input -` is optional stdin convenience.
4. Never edit files under `.stateroot/` directly — all state access goes through the CLI. The CLI is offline-safe and queues operations locally when the network is down.
