---
name: stateroot-dot
description: Explicit project continuity in a shared cloud dot environment using StateRoot. Use when the user provides a StateRoot project path and asks to resume, checkpoint or hand off work in that environment.
---

# StateRoot dot proof of concept

Use only the explicitly supplied project directory. Do not infer it from host
sessions, registry entries or other projects. Ask for the project path if missing.
Do not run global setup/install or the normal host-integrating resume/import/sync.

1. Read `stateroot dot --project /path/to/project resume` in full at task start.
2. After meaningful progress, run `stateroot dot --project /path/to/project checkpoint "Progress, evidence, blockers and next steps"`.
3. Recall supplied facts with `stateroot dot --project /path/to/project recall "query"`.
4. Before stopping or handing off, pipe a JSON input on stdin with nonempty `objective`,
   `task`, `context_summary`, and `next_actions` (array of strings), then run
   `stateroot dot --project /path/to/project handoff --input -`.
5. When a working-tree snapshot is needed, explicitly run
   `stateroot dot --project /path/to/project snap --reason "milestone"`.

Initialize only if authorized: `stateroot dot --project /path/to/project init`.
No hooks, compaction callbacks, automatic transcript capture
are provided. Checkpoints snapshot changed project files automatically. Never fabricate missing context. CLI availability and a supported
skill loader are required; arbitrary local skills do not automatically load in dots.
Repo skills can be used by supported cloud coding tasks, which are distinct from
the root dot. If no loader is available, have the agent read this file explicitly.

Different devices accessing the same environment share its on-disk project state.
This is not cross-machine synchronization or a cloud durability guarantee. Preserve
`.stateroot/` explicitly before losing the VM; Git snapshots additionally require
Git objects and `refs/stateroot/*`. Ordinary Git commits omit personal continuity
files by default. Do not upload private state without the user's authorization.

Use an unbound project store without symlinks/reparse points. Dot rejects foreign
project packets and fork-bound handoffs; use ordinary StateRoot for registered
fork workflows. Handoff input accepts only the existing author-controlled content
fields; do not supply provenance, plan refs or timestamps. Use ordinary
`stateroot handoff repair` if current JSON is corrupt. Avoid concurrent ordinary
and dot handoff writers. Existing snapshot restore commands remain explicit.

Resume includes stored personality, rules, active learnings, current plan guidance
and catalogs of plans, skill packages, tools and memory. Read catalog entries with
`stateroot dot --project /path/to/project read plans/PLAN_ID.md` (or the listed
store-relative path). A stored tool or skill is not evidence of runtime availability.
If the canonical user store is already available and authorized in this environment,
add `--shared-state /path/to/.stateroot` before the action; use `read --shared`
for its catalog entries. This reads shared intelligence without host integration
or synchronization. Missing shared data stays missing. Before choosing an approach,
recall failed approaches. After compaction, explicitly run resume again in full.
