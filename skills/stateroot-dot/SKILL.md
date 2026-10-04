---
name: stateroot-dot
description: Use the full StateRoot continuity workflow from a ChatGPT dot through an accessible computer or execution environment. Resume project context, preserve progress, and share personality, plans, tools, skills, memory and learnings with other registered harnesses.
---

# StateRoot for ChatGPT dots

Use the project's existing StateRoot CLI and guidance. Select the actual project
path on the computer executing the command; a dot's cloud computer and connected
personal computer are different environments. If the path or CLI is unavailable,
resolve that execution prerequisite before claiming continuity is loaded.

Run `stateroot dot --project PROJECT resume` at task start and consume the complete
digest, unpiped and untruncated. This uses the normal registered project, canonical
user state and fork binding. Follow the inherited product intent, personality,
active plan, learnings, skill router and tool availability guidance. Never read
`.stateroot/` storage files as a substitute for the CLI.

Before attempting an approach, run
`stateroot dot --project PROJECT recall "failed approach TOPIC"`.
After meaningful changes, decisions or blockers, run
`stateroot dot --project PROJECT checkpoint "what changed and why"`.
Changed project files enter automatic lineage; explicit milestones can use
`stateroot dot --project PROJECT snap --reason "milestone"`.
After compaction explicitly run resume again in full. Cloud orchestration does
not load ordinary local command hooks: these calls are the supported lifecycle.

All normal CLI commands are available, with dot provenance:

- Facts: `stateroot dot --project PROJECT memory add "fact"` and `memory recall QUERY`.
- Judgments: `stateroot dot --project PROJECT learn record "prefer X when Y"`;
  use the normal user/workspace/domain scopes when appropriate.
- Personality: pipe the requested persona to
  `stateroot dot --project PROJECT soul propose --stdin`, then use `soul sync`.
- Plans: pipe the plan to `stateroot dot --project PROJECT plan record --stdin --title TITLE`;
  use `plan show`, `approve`, `activate`, `done` and `todo list` normally.
- Shared rules: `rules list`, `rules show SLUG` and `rules sync`.
- Skills and tools: `stateroot dot --project PROJECT run -- skill list`,
  `run -- skill show SLUG`, `mcp status`, `mcp sync` and `mcp-stdio`.
  Stored capabilities are not proof of runtime connection or authorization.
- Work lineage: normal `log`, `show`, `compare`, `fork` and `revert` commands;
  inspect the selected root and preserve unrelated work before restoring.

`run -- COMMAND ...` is the escape hatch for the complete normal CLI, including
commands whose names overlap the dot convenience commands. The equivalent native
form is `stateroot --project PROJECT --actor dot COMMAND ...`.

At a session boundary run the normal handoff writer, for example:
`stateroot dot --project PROJECT run -- handoff write --from dot --objective "..." --task "..." --context-summary "..." --next "..."`.
Omit `--to` for continuity, or name the registered destination for a transfer.
For a large packet use `handoff --input -` with JSON on stdin. Never stage the
payload in the project tree. Normal handoff acceptance, plan refs, fork routing,
repair, provenance and history remain available.

For an authorized new project use `stateroot dot --project PROJECT init`.
`install` installs/refreshes the bundled skill through canonical federation;
`uninstall` removes dot skill delivery while retaining project intelligence.
Normal setup/install/self-update also deliver the bundled dot skill.
Discover it through the connected computer's supported local skill loader, or
explicitly read `stateroot dot --project PROJECT skill` when no loader is available.
A repo skill in a delegated coding task does not prove root-dot skill discovery.

There is no dot transcript reader or verified automatic cloud callback. Explicit
checkpoints and handoffs capture the agent's evidence. Devices using the same
computer/project share that state; independent computers need an authorized
transfer of state and lineage through existing mechanisms. This integration adds
no sync service, cloud durability promise, accounts or billing.

`--portable` retains the previous isolated project-store compatibility mode;
`--shared-state DIR` selects its read-only explicit user pool. Those modes have
narrower capabilities and are not the normal registered-harness workflow.
