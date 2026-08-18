## StateRoot

This project uses StateRoot for persistent, harness-neutral project state (`.stateroot/`). Follow this protocol mechanically, every session:

1. **Prefer the hook digest** at session start. Only run `stateroot resume --harness <id>` if no StateRoot digest appeared yet. Run it **unpiped and untruncated** — never `2>&1 | head -100`, `| tail`, or any line limiter. The full digest is the state of record.
2. **After every state-changing step** — files written, decisions made, milestones reached, blockers discovered — run `stateroot checkpoint --note "<what changed and why>" [--files a,b]`. Hooks do not fire on every write.
3. **Before attempting an approach** — run `stateroot memory recall "failed approach <topic>"` (or read `failed_approaches` in the current handoff) and do not repeat recorded failures.
4. **After meaningful real-tree changes** — run `stateroot snap`; use `stateroot revert` / `stateroot fork` for verified restoration or divergent work.
4. **Session end / usage limit / harness switch** — prefer `stateroot handoff write --from <resolved-current-harness> [--to <harness>] --objective "…" --task "…" --context-summary "…" [--next "…"]`, **or** rely on session_end/stop hook finalize when it ran. Omit `--to` for continuity-only. Use `--input` only for large payloads. Never write under `.stateroot/handoffs/` by hand. Thin fields warn; they do not refuse the write.
5. **Learnings are taste, not facts.** Each note is a judgment (`prefer X over Y` / `never Z`) plus when it applies. Scopes: `--user`, `--workspace`, project (default), `--domain <slug>`. After init, seed an empty layer with 2–7 evidenced judgments. Facts go to `memory_save`. All activate immediately — no classify→approve story.
6. **Shared rules** — product-intent ships by default (full body in the digest). `stateroot rules sync` pulls Cursor/Codex/Claude/Gemini instruction files into the same pool. Preserve product intent; do not replace agent judgment with classifiers or approval gates.
7. **Never edit `.stateroot/` directly** — all state access goes through the `stateroot` CLI. The CLI is offline-safe: when the network is down it queues operations in a local outbox and still succeeds.
8. **Privacy** — files matching root `.gitignore` or `.staterootignore` never enter snap/root trees (plus hardcoded `.git/` and `.stateroot/local/`). `.staterootignore` is extra patterns for things git still tracks.

Session-start resume is harness-specific (global integration / Cursor rule / Claude command). Run it **once** per session — never from this block *and* another surface.

If `stateroot` is not on PATH, tell the user to install the CLI. If this is not yet a stateroot project, suggest `stateroot init`.
