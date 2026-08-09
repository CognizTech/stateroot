# StateRoot

**Version control for the complete state of AI-assisted work, plus a portable
working identity.** StateRoot gives every coding agent a shared, durable
substrate: git-backed roots of your project files and machine state,
transitions with receipts, handoffs between harnesses, a canonical soul
(working relationship), scoped learnings and memory, and federated skills and
MCP servers — all verifiable on disk, all readable by any tool.

**Agents are temporary. The state of the work — and the working
relationship — is permanent.** Harnesses come and go (Claude today, Codex
tomorrow, Cursor next week); what persists is the lineage of what was tried,
decided, failed, and shipped, and the identity that tells the next harness
*who you are to it, and who it is to you*. StateRoot makes that state
first-class: snapshotted, diffable, revertible, forkable — and never locked
inside one vendor's runtime.

## No server. Ever.

StateRoot is fully local. There is no hosted service, no account, no sync
daemon, no telemetry. Everything lives in two places you own:

- `.stateroot/` inside your project (state, handoffs, transitions, learnings,
  roots metadata), and
- `~/.stateroot/` for user-global identity (soul, learnings, memory) — plus
  real git commits under `refs/stateroot/` in your project's own repo.

Your branch log is never touched: roots are plumbing-only commits
(`commit-tree`) outside `refs/heads/`, and the working tree is never
rewritten — revert is a *new* root, append-only, always.

## Quickstart (3 minutes)

```bash
# install from source (single static binary; you need nothing but git)
cargo install --path stateroot-cli        # from this repo
# or download a release binary and put `stateroot` on your PATH

cd your-project
stateroot init          # creates .stateroot/ + a silent git repo if needed
stateroot snap --reason "starting the parser refactor"

# ... work in Claude for a while ...
stateroot checkpoint --note "lexer done, parser at 60%"
stateroot handoff write --to codex --objective "finish the parser"

# ... switch to Codex (or Cursor, Kimi, OpenClaw, Hermes) ...
stateroot resume        # the digest: objective, state, failures, skills

stateroot log           # root lineage with coverage + fork markers
stateroot diff a1b2 c3d4 --content
stateroot receipt <transition-id>   # verified tier: the git delta itself
```

## Feature map

| Area | What you get |
|---|---|
| **Continuity** | Import transcripts from six harnesses (Claude, Codex, Cursor, Kimi, OpenClaw, Hermes) into a local handoff; `resume` renders the working digest anywhere; session hooks inject it automatically where supported. |
| **Git-backed roots** | `snap / log / show / diff / revert / fork / receipt` on libgit2 plumbing. The diff IS the verified delta. Append-only history; forks are real branch refs under `refs/stateroot/forks/`. |
| **Working identity** | Canonical soul at `~/.stateroot/soul/` (versioned, provenance-tracked), per-harness projections, deterministic Q&A generate, imports from OpenClaw/Hermes — evolution always through approval-gated proposals. |
| **Learnings & memory** | Scoped (`user`/`project`) category-markdown learnings with lifecycle (candidate → proposed → active), a deterministic distiller over your checkpoints and hook spool, and serve-time gates: candidates and private notes surface nowhere foreign. |
| **Skill & MCP federation** | Skills are discovered across all six harnesses into one portable registry — new foreign skills arrive *quarantined* as candidates and project into harness roots only after you approve. MCP servers pool and project across configs. |
| **Shared self-improvement tools** | `stateroot mcp-stdio` is a local stdio MCP server exposing `memory_save / memory_recall / learn_record / skill_propose / soul_read / learnings_list` to every harness — "any agent can teach; all inherit". Writes arrive quarantined. |
| **Local synthesis** | `stateroot synthesize` condenses transcript bundles into handoff sections using *your own* provider key — OpenAI, DeepSeek, Ollama, litellm, anything OpenAI-compatible. No key? The deterministic digest always works. |

## GitHub-backed sync (optional)

StateRoot syncs `refs/stateroot/*` over plain git — roots are commit-trees,
so state and files travel inside the commits; no translation layer, no
hosted service of ours.

```bash
stateroot login --via github          # OAuth device flow
stateroot repo link owner/repo        # verify + bind (writes to the manifest)
stateroot sync                        # push + pull refs/stateroot/*
```

- **Layouts**: `same-repo` (default — refs live in your repo, invisible to
  the branch list) or `--layout companion` (a dedicated
  `<project>-stateroot` repo you create first).
- **Never destructive**: divergence forks (both tips kept as
  `refs/stateroot/forks/sync-diverged-*`); pushes are never forced, remote
  refs are never deleted. A non-fast-forward push fails honestly — pull
  first, or fork on purpose.
- **Scope decision**: the OAuth App asks for `repo` (refs push needs it for
  private repos). Public-only users can set `public_repo`:
  `[github] scope = "public_repo"` in `config.toml`.
- **Client id**: the OAuth App is registered by the project owner; until
  then set `STATEROOT_GITHUB_CLIENT_ID` (or `[github] client_id`) — the
  shipped placeholder fails with a pointer here instead of a broken flow.
- `.stateroot/local/` (sync state, machine-local notes) never enters roots
  and never syncs. Trees beyond 200 MB earn a `.staterootignore` hint.

## Cloud runs (optional, paid product)

`stateroot run --cloud "<objective>" [--from <root>] [--harness <id>] [--verification <cmd>] [--watch]`
hands an objective to StateSmith Cloud: it clones your repo + refs at the
root, hydrates the environment, runs the agent headless against the state
the refs carried (soul, learnings, skills, active goal), executes the
verification surface, and pushes a new root + transition + receipt back.

```bash
stateroot run --cloud "port the lexer" --harness codex --watch
stateroot runs list
stateroot runs status <run-id>
```

- Requires `stateroot login` (the Phase-1 credential is the bearer token);
  without one the commands fail with exactly that message.
- Endpoint: `[cloud] base_url` in `config.toml` (default the StateSmith
  deployment) or the `STATEROOT_CLOUD_URL` env override.
- `--watch` polls until a terminal state with a compact event tail; on
  success the result root id prints with the `stateroot sync --pull`
  reminder.

## The truth contract

StateRoot never blurs provenance. Every artifact carries its tier:

- **verified** — backed by git or files you can inspect (root diffs, receipts,
  import records);
- **observed** — recorded from a harness or the distiller, marked as such;
- **synthesized** — LLM-produced sections, always labeled
  `synthesized — not verified`, with model and bundle hash in provenance.

Candidates (learnings, skills, memory from foreign harnesses) are quarantined
until a human approves them through the proposals flow. Nothing an agent
writes activates itself.

## Configuration

Config lives at `~/.config/stateroot/config.toml` (or `$STATEROOT_HOME/config.toml`).
Everything works with zero config; synthesis needs your own provider key:

```toml
[synthesis]
enabled = true
api_key = "sk-..."                    # or env STATEROOT_SYNTHESIS_API_KEY
base_url = "https://api.openai.com/v1"      # OpenAI
# base_url = "https://api.deepseek.com/v1"  # DeepSeek
# base_url = "http://127.0.0.1:11434/v1"    # Ollama
# base_url = "http://127.0.0.1:8080/v1"     # litellm
model = "gpt-4o-mini"
min_interval_seconds = 600            # governance
daily_cap = 20
# extra_body is merged into the request verbatim (non-thinking passthrough,
# vendor flags, temperature):
# [synthesis.extra_body]
# temperature = 0.2
```

Deterministic mode is the floor, never the fallback-of-shame: with no key and
no network, resume, roots, learnings, and the review loop all behave
identically.

## License

StateRoot is source-available under the **Business Source License 1.1**. In
plain language: you can read, modify, and self-host it freely today; using it
to offer a competing hosted service requires a commercial license. On the
**Change Date** — the second anniversary of the first public release of each
version — that version converts automatically to **Apache License 2.0** and
becomes fully open source forever. See [`LICENSE`](LICENSE).

## Relationship to StateSmith

StateRoot is a deliberate copy-and-own fork of the local core of the
StateSmith monorepo (which provides the hosted StateSmith control plane).
The fork exists for open-source self-containment: no path dependencies, no
server coupling, its own repo, its own license. File formats (soul,
learnings, skill packages, handoff v1, transitions, receipts) remain
compatible by contract, so a future sync bridge can reconcile local and
cloud without migrations. The monorepo remains the upstream reference;
drift here is intentional and documented in [`CHANGELOG.md`](CHANGELOG.md).
