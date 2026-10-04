# Dot compatibility proof of concept

StateRoot can keep explicit project context in a cloud dot VM. A laptop and phone
that access **the same project in the same environment** see the same on-disk
handoffs and checkpoints. This is a single-environment continuity POC; it does not
implement device synchronization, hosted storage, accounts, or billing.

## Build and try in your VM

Requires Rust 1.85+ and the normal native build prerequisites. Build locally:

```sh
cargo build --release -p stateroot-cli
SR="$PWD/target/release/stateroot"
PROJECT=/absolute/path/to/your/project
"$SR" dot --project "$PROJECT" init
"$SR" dot --project "$PROJECT" resume
"$SR" dot --project "$PROJECT" checkpoint "Implemented feature; tests passed; next: review"
"$SR" dot --project "$PROJECT" recall "feature"
```

No global installation or `setup` is required. Init creates only `.stateroot/`
using the existing canonical schema and preserves existing files. It does not
register a project, seed host identity, install hooks, modify AGENTS.md, or install
skills. Every command requires the exact project directory, including commands
run from another working directory. Relative paths resolve against the shell cwd.

Write a handoff from explicit evidence:

```sh
cat <<'JSON' | "$SR" dot --project "$PROJECT" handoff --input -
{
  "objective": "Ship the feature",
  "task": "Implement and verify",
  "context_summary": "Implementation complete; focused tests passed. Review pending.",
  "next_actions": ["Review the diff", "Run integration tests"]
}
JSON
"$SR" dot --project "$PROJECT" resume
"$SR" dot --project "$PROJECT" snap --reason "verified milestone"
```

Handoff assembly, like ordinary StateRoot, may initialize local project Git metadata
when resolving the latest root. Handoffs use the ordinary command's input parser, packet assembly, bounded fields,
and immutable history-first writer. Envelope/provenance overrides, unknown fields
and worktree routing are rejected; restored older packets do not reset sequence
numbers. Ordinary `stateroot handoff show` and `handoff repair` work on this state. Dot assigns project id,
sequence, actor and timestamps; input content is supplied by the agent. Checkpoints
append episodic notes and use the existing automatic lineage mechanism: changed
project files create a root; StateRoot bookkeeping alone does not. Snap uses the existing Git plumbing
under `refs/stateroot/*`; it can initialize Git in a non-Git project. Review ignore
rules before snapshots. Recall is literal, case-insensitive line search of project
memory, wiki and episodic notes, not the host transcript FTS index. Resume prints
stored state, objectives, instructions, memory, episodic notes and the current
handoff; large journals may produce large output. It does not reconstruct context
that was never explicitly recorded. Dot commands bypass host configuration,
telemetry, updater, federation, transcript readers and detached workers.

## Skill loading

The portable skill is [skills/stateroot-dot/SKILL.md](../skills/stateroot-dot/SKILL.md).
`stateroot dot --project "$PROJECT" skill` prints the identical embedded copy.
Load it through the environment's supported skill mechanism, or explicitly ask the
agent to read it and supply the project path. For a supported Codex cloud coding
task, place the file at `.agents/skills/stateroot-dot/SKILL.md` in that task's repo.
Do this as a deliberate project configuration change; StateRoot does not install it.
Personal local skills are not automatically synced into cloud tasks, and a repo
skill in a coding task is not proof it loads into the root dot.

See [building skills](https://learn.chatgpt.com/docs/build-skills) and
[cloud environment limitations](https://learn.chatgpt.com/docs/environments/cloud-environments#current-limitations).
Cloud-orchestrated dots cannot load local command hooks/config/plugins; enterprise
managed remote hooks have separate requirements. This POC needs only explicit CLI
calls and does not require that enterprise path. See
[managed hooks](https://learn.chatgpt.com/docs/hooks#managed-hooks-from-requirementstoml).

## Storage and portability boundaries

State lives on disk in the selected project's `.stateroot/`. Devices accessing the
same running environment need no copy or sync. VM loss can lose the state; this
implementation makes no durability or retention promise. Saved coding-task
workspace persistence must not be generalized to the dot VM itself.

For an explicit private backup, stop concurrent writers and archive the project's
`.stateroot/` with a normal filesystem backup tool. Restore it only into the intended
project, then call dot resume with its new path. An ordinary Git commit is not a
complete continuity backup: `.stateroot/.gitignore` excludes current handoff,
curated memory, episodic notes and local artifacts by default. Working-tree snapshots
also need the project's Git objects and `refs/stateroot/*`; a `.stateroot/` archive
alone does not transport that lineage. Existing Git ref transfer and filesystem
backup primitives remain available; there is no new export/sync service. Shared persona and user data are available through an explicitly selected existing
canonical user store (`--shared-state /path/to/.stateroot`); no discovery or transfer
is performed. Review private content before
sharing any backup.

No automatic session-start/stop or compaction callbacks, transcript import,
automatic context reinjection, hosted MCP service, or multiwriter synchronization
across independent machines is provided. A skill asks the agent to checkpoint;
it cannot guarantee the agent always does so.

## Verification

```sh
cargo test -p stateroot-cli --test dot
cargo fmt --all -- --check
```

For the shared-environment test, checkpoint from one device and call dot resume
from the other with the same VM project path. Confirm the note and current handoff
are visible. This verifies same-environment continuity, not independent-machine sync.

## Boundary and recovery notes

Dot rejects symlinks/reparse points anywhere inside `.stateroot/`, including
junctions and linked wiki directories, before reading or writing the store. The
explicit project argument itself is canonicalized. This check prevents accidental
link traversal; it is not a sandbox against another process racing filesystem
changes. Dot snapshots also reject eligible linked working-tree paths (and linked
ignore files), respecting the existing root ignore rules. Ignored linked paths are
not captured. The snapshot format and Git engine remain unchanged.
Unsupported manifests, foreign-project current packets, and fork-bound current
handoffs fail closed. Use ordinary StateRoot's registered-worktree workflow for
fork routing; dot POC does not implement it.

Partial init retains the existing project identity and never replaces existing
project files. Malformed current/history JSON stops handoff writes; use the existing
`stateroot handoff repair` recovery rather than manually discarding history. Dot
snapshots are ordinary StateRoot roots: existing revert/fork/compare commands can
operate on them. Existing `revert` appends a root with the selected tree; it does not check out
that tree into the current working directory. To inspect/recover snapshot files,
use the existing `fork HASH --worktree NEW_DIR` materialization workflow. Dot adds
no automatic restore command or independent snapshot format.

Dot handoff writers serialize against one another, but the ordinary lifecycle
writers have their own behavior; do not run ordinary and dot handoff writers
concurrently. On Windows, the reused ordinary handoff writer retains its existing
truncate/write current-file behavior; history is durable first and repair can
recover an interrupted current write. Resume reads all episodic notes and recall
is literal line search, so this POC has no bounded context budget or FTS ranking.

## Shared intelligence without host integration

The POC retains the project's shared personality, plans, rules, learnings, memories,
skill packages and tool definitions. Resume prints stored soul/USER/overlay content
and rules, active learnings with their labels, the current plan directive, and
catalogs for plans, skills, tools and memory files. The current handoff retains
its labeled synthesis and context-only sections. Catalog entries expose stored
capabilities; they do not claim a native runtime or MCP server is available.

If the canonical user pool is already present in this environment, select it
explicitly (the directory is the StateRoot store, not the user home):

```sh
"$SR" dot --project "$PROJECT" --shared-state /path/to/.stateroot resume
"$SR" dot --project "$PROJECT" read plans/PLAN_ID.md
"$SR" dot --project "$PROJECT" --shared-state /path/to/.stateroot read --shared skills/SKILL/SKILL.md
```

`read` uses store-relative paths and rejects absolute paths, traversal and links.
The shared store is read-only in this mode. Missing global state is not replaced
with the VM host's persona. Run resume again explicitly after context compaction;
this is an agent instruction, not a platform callback. No existing native harness
automation is changed. Cloud-VM and two-device validation remain pending.
