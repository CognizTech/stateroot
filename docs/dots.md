# ChatGPT dot integration

Dot is a registered StateRoot harness (`dot`, aliases `chatgpt-dot` and
`chatgpt-dots`). It uses the normal local CLI, canonical user intelligence,
project registry, skill/tool federation, handoffs and work lineage. It requires
an execution environment that can run StateRoot against the selected project.

## Install and use

Use the normal binary for the computer's operating system. No separate dot
executable or service is required. From an existing project:

```sh
stateroot dot --project /absolute/project install
stateroot dot --project /absolute/project resume
stateroot dot --project /absolute/project checkpoint "Implemented change; tests passed"
stateroot dot --project /absolute/project memory recall "change"
stateroot dot --project /absolute/project run -- skill list
stateroot dot --project /absolute/project plan list
stateroot dot --project /absolute/project mcp status
stateroot dot --project /absolute/project run -- handoff write --from dot \
  --objective "Ship change" --task "Review" --context-summary "Tests passed" --next "Review diff"
```

`init` initializes a new project using the normal registry, canonical identity,
rules and product skill workflow. Existing project files and identity are reused.
`install` enrolls dot skill delivery without installing cloud command hooks.
The bundled skill is seeded in the canonical user and selected project skill
stores and projected to `.agents/skills/stateroot-dot`. Normal install/setup and
self-update refresh the bundled package through the existing projection engine.
`dot uninstall` removes dot-owned skill packages/projections and enrollment;
project state and other harness packages remain. Normal complete uninstall also
cleans dot's user delivery files.

Use `dot --project DIR run -- COMMAND ...` for any ordinary StateRoot command,
including full checkpoint/handoff options or `skill show`. Unrecognized dot
subcommands also dispatch to the native CLI. Arguments and stdin pass directly
without a shell; native failure exit codes are preserved. The equivalent native
form is `stateroot --project DIR --actor dot COMMAND ...`. Project paths are
resolved in the execution environment. The root `--project` option precedes the
command; it selects cwd before loading the normal project context.

## Platform capabilities

| Capability | Dot integration |
| --- | --- |
| Identity and shared personality | Native canonical soul, project overlays, explicit soul sync |
| Plans and tasks | Native plan record/show/approve/activate/done, todo federation |
| Memory and learnings | Native curated memory, indexed recall, user/workspace/project/domain judgments |
| Shared rules | Native product intent and rule federation |
| Skills and tools | Native catalogs, federation and MCP CLI; runtime availability checked separately |
| Context and provenance | Explicit resume/checkpoint/handoff, registered aliases and dot actor |
| Work lineage | Native automatic roots, snap/log/compare/revert/fork and bound worktrees |
| Skill delivery | Bundled package, canonical ownership metadata and local `.agents/skills` projections |
| Lifecycle hooks | Explicit calls; no ordinary local hooks in cloud orchestration |
| Transcript ingestion | No dot-specific reader; explicitly recorded evidence |
| Delegation to dot | Handoff-only; no invented dot launch executable |

[Connected computers](https://learn.chatgpt.com/docs/dots/computers-and-apps)
provide the supported local execution/skill route. A dot cloud computer is a
separate environment: the binary, project and authorized user state must actually
be accessible there. [Skill documentation](https://learn.chatgpt.com/docs/build-skills)
distinguishes local skills and account-installed plugins; projecting a local skill
does not install an account plugin or guarantee root-dot cloud discovery. A
supported delegated coding task can discover repository `.agents/skills`.
When no skill loader is available, explicitly read `stateroot dot --project DIR skill`.

[Managed hook documentation](https://learn.chatgpt.com/docs/hooks#managed-hooks-from-requirementstoml)
states that cloud-orchestrated dots do not load ordinary local command hooks.
Enterprise managed remote hooks are a separate facility. This adapter declares
no local hooks, session/compaction events, fake dot binary or transcript source.
Resume explicitly at start and after compaction, checkpoint meaningful changes,
and write a handoff before ending. Configured skill discovery is evidence of
installation, not evidence that a dot session is running.

## Compatibility and state boundaries

The prior isolated store interface remains under `--portable` for callers that
need to avoid host context. Existing `--shared-state DIR` also selects that
interface and reads the explicit user store without transfer. Its limitations
(symlink rejection, literal recall, unbound handoffs and the small command subset)
do not apply to the default native integration. Do not use portable mode for a
registered fork. Use the native handoff repair and bound-worktree workflow.

Devices accessing the same computer/project use its existing on-disk state.
This does not add independent-machine synchronization, hosted storage, billing,
or a VM retention guarantee. State transfer requires the user's authorization.
An ordinary Git commit omits private continuity files by default; lineage also
requires Git objects and `refs/stateroot/*`. Preserve the existing state through
normal backup/transfer mechanisms before discarding an environment.

## Verification

```sh
cargo test -p stateroot-cli --test dot
cargo test -p stateroot-core harness_install
cargo test -p stateroot-core skill_federation
cargo fmt --all -- --check
```

Repository tests cover native routing, identity, shared-state workflows and skill
lifecycle as well as portable compatibility. Actual root-dot skill loading,
cloud-VM execution and two-device access must be tested in the authorized target
environment; local test results do not establish those platform observations.
