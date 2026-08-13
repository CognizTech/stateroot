# Changelog

All notable changes to StateRoot. Format loosely follows Keep a Changelog;
StateRoot is pre-1.0 and milestones land as minor versions.

## Unreleased

- `learn record` / MCP `learn_record` activate learnings and memories
  immediately so the next harness inherits them. Soul and skill still file
  a proposal. Distill remains gated (inferred notes).

## [0.1.0] — 2026-08 (first public release)

The local-first, open-source StateRoot variant: a fully local substrate for
AI-assisted work — no server anywhere.

### Continuity core (M1)

- New self-contained workspace (`stateroot-core` + `stateroot-cli`), seeded by
  copy-and-own lifting of the StateSmith monorepo's proven local modules:
  six harness transcript readers (Claude, Codex, Cursor, Kimi, OpenClaw,
  Hermes) + bundle builder, `.stateroot/` local store, skill & MCP
  federation engines, harness installers, ignore rules, canonical hashing.
- Fully offline command surface: `init`, `import`, `resume`, `checkpoint`,
  `handoff write/list/show/accept`, `log`, `status`, `doctor` (local checks),
  `hook`, `install`/`uninstall`, `setup` (harnesses + skills), `skill` and
  `mcp` federation.
- BSL-1.1 license with automatic conversion to Apache-2.0 on the change date.

### Git-backed roots (M2)

- Roots as `git commit-tree` of the working state (project files honoring
  `.gitignore`/`.staterootignore` + the `.stateroot/` tree), stored under
  `refs/stateroot/roots/` — the user's branch log and index never touched;
  non-git folders auto-`git init`.
- Transitions (`.stateroot/transitions/`) linking roots; receipts rendered
  from transition + the git delta (verified tier for free).
- `snap`, `log` (lineage, coverage, fork markers), `show`, `diff --content`,
  append-only `revert`, `fork` (branch materialization), `receipt`.
- Coverage honesty: `files: N pinned` vs `state-only`.

### Identity, learnings, synthesis (M3)

- Canonical soul at `~/.stateroot/soul/` (history snapshots, provenance),
  project overlay, imports (OpenClaw/Hermes/file), deterministic generate,
  per-harness projections.
- Scoped learnings (category-md, user + project), deterministic distiller,
  lifecycle candidate → proposed → active; memory notes with serve-time gates.
- Local proposals engine (`.stateroot/proposals/`) as the shared approval
  gate; `learn record` review-loop entry (classify → proposal, never direct).
- `stateroot synthesize` — direct OpenAI-compatible synthesis with your own
  key (DeepSeek/OpenAI/Ollama/litellm), hash-idempotent governance, honest
  unavailability without a key.

### Federation + shared MCP tools (M4)

- Foreign skills arrive quarantined as candidates; activation only via
  proposals (`skill promote` → `proposals approve` → projected into harness
  roots).
- `stateroot mcp-stdio`: local stdio MCP server exposing `memory_save`,
  `memory_recall`, `learn_record`, `skill_propose`, `soul_read`,
  `learnings_list` — external-harness writes quarantined
  (session-candidate/private) until approved.
- Harness instruction blocks carry the two-sentence self-improvement
  guidance; install registers the stdio server into harness configs.

[0.1.0]: https://github.com/stateroot/stateroot/releases/tag/v0.1.0
