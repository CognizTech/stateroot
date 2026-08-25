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
  init)`, and a seq-1 `handoffs/current.json` labeled `"provenance":
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

## `stateroot delegate` — cross-harness subagents

`stateroot delegate --to <harness> --task "<bounded task>"` spawns another
harness's CLI as a subagent inside the current project: the task goes out as
a prompt (prefixed with a short subagent contract), the child runs with piped
stdout under a timeout, and the caller receives only a bounded tail of its
final output — never a transcript dump.

- **Resolution** — the target must be a registry cli-mode harness whose
  binary probes on PATH. Unknown harnesses, handoff-only harnesses (e.g.
  `cursor`), and missing binaries are loud errors listing the available
  cli-mode harnesses (unlike `init --synthesize`, which notes and stands
  down).
- **Depth cap** — `STATEROOT_DELEGATION_DEPTH` guards recursion: at depth ≥
  2 the command refuses ("a subagent may not spawn further subagents") and
  nothing is spawned; children always run with the depth incremented.
- **Bounds** — `--timeout-secs` (default 600) kills the child past the
  deadline; `--max-output-chars` (default 8000) caps the stdout tail returned
  to the caller. `--skill <slug>` (repeatable) projects StateRoot skill
  packages into the run per the registry policy; `--ambient-skills` opts into
  the harness's own skill discovery. `--json` emits the delegation record
  plus tails as one machine-readable envelope.
- **Records** — every run writes the full stdout/stderr log and a
  `stateroot.delegation.v1` record (harness, task, command, exit code,
  duration, outcome `completed|failed|timed_out`) under
  `.stateroot/delegations/`, and appends an episodic lineage note so the
  delegation shows up in digests like any other activity.
- **Exit codes** — 0 on success; the child's own exit code when it fails
  (its stderr tail is included in the output); 1 on timeout, refusal, or an
  empty-stdout run (pty-marked harnesses may misbehave when piped — the full
  log path is printed either way).

For an interactive harness session use `stateroot harness run` instead;
`delegate` is the bounded, recorded, non-interactive route.

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
  title, status, provenance, the `.md` path, and a directive. Approved or
  active → the executor directive ("Execute it as written; do not re-plan
  or re-explore"); only a draft → the planner directive ("refine the plan
  file; do not implement yet"). The transcript `## Plan State` remains as
  the fallback tier and is suppressed while a central plan exists. The plan
  body never enters the digest — the executor reads one file.
- **Handoff** — `handoff write` auto-attaches `plan_ref: {id, title,
  status}` when an active/approved plan exists.
- **v1 has no tool-gating** — hooks do not deny write tools while a draft
  exists. Enforcement is a policy decision for the user (optional hook
  hardening later); StateRoot ships the strings, not a runtime cage.

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

## The digest's freshness lines — Latest Activity & update notice

Two one-line sections keep every arriving harness oriented:

- **Latest Activity** — the newest observed activity anywhere (last checkpoint
  or latest root) with harness and timestamp. A long-running session that
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

## `stateroot doctor` — hook-binary health

Doctor inspects the binary every installed hook config actually points at
(all harness hook formats: nested/flat JSON, TOML, exec-form, named groups,
and the generated OpenClaw plugin). For each distinct stateroot hook binary
it runs `--version`: a match with the running CLI reports `[ok]`; a
mismatched or unrunnable binary is a soft `[!!]` warning (never a hard
failure) — e.g. `cursor hook binary is stateroot 0.1.1 — run \`stateroot
self-update\` on this machine`. This is the check for fail-open staleness:
hooks that resolve to an old `stateroot` silently do nothing, and nothing
else reports it.

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
