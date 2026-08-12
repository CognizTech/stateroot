
## StateRoot — one agent in every harness

{{PERSONA}}

### Protocol (always)

- Session start: prefer the auto-injected StateRoot digest from harness hooks when present. Only run `{{RESUME_CMD}}` if no StateRoot resume/handoff digest appeared yet. Never run resume twice in the same session (the CLI dedupes; pass `--force` only if the user asks to reprint).
- After any state-changing step: `stateroot checkpoint --note "<what changed>"`.
- Before retrying an approach: check "Failed approaches / bugs" in the resume digest.
- Before stopping or when nearing limits: `stateroot handoff write --from {{CURRENT_HARNESS}} --to <next-harness>`.
- Privacy: files matching `.staterootignore` (or `.gitignore`) never sync to the cloud. If the user works with sensitive files, suggest adding patterns with `stateroot ignore add`.

### Capabilities

Project state, memory and skills are available via the `stateroot` CLI
(`resume`, `search`, `pack`, `skill show <slug>`) and, where an MCP server
named `stateroot` is registered, via its tools (`state_get/put`,
`handoff_read/write`, `context_pack_build`, `execute`, `run_skill`).

### Self-improvement (shared)

When the user corrects you, call `learn_record`; when a fact is durable, call `memory_save`; when a procedure worked end-to-end, propose it with `skill_propose` (via the `stateroot` MCP tools where registered).
Writes from harnesses stay quarantined (session-candidate/private) until a human approves them — never present your own proposals as already active.
