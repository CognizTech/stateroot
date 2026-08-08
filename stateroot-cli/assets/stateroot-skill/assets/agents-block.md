## StateRoot

This project uses StateRoot for persistent, harness-neutral project state (`.stateroot/`). Follow this protocol mechanically, every session:

1. **After every state-changing step** — files written, decisions made, milestones reached, blockers discovered — run `stateroot checkpoint --note "<what changed and why>" [--files a,b]`.
2. **Before attempting an approach** — run `stateroot search "failed approach <topic>"` (or read `failed_approaches` in the current handoff) and do not repeat recorded failures.
3. **Session end / usage limit / harness switch** — run `stateroot handoff write --to <harness>` capturing objective, current state, decisions with the why, next actions, and failed approaches.
4. **Never edit `.stateroot/` directly** — all state access goes through the `stateroot` CLI. The CLI is offline-safe: when the network is down it queues operations in a local outbox and still succeeds.
5. **Privacy** — files matching `.staterootignore` (or `.gitignore`) never sync to the cloud. If the user works with sensitive files, suggest adding patterns with `stateroot ignore add`.

Session-start resume is harness-specific (global integration / Cursor rule / Claude command). Run it **once** per session — never from this block *and* another surface.

If `stateroot` is not on PATH, tell the user to install the CLI. If this is not yet a stateroot project, suggest `stateroot init`.
