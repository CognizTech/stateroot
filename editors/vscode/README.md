# StateRoot — VSCode extension

Project continuity inside the editor: the shared state of record your coding
agents read and write, visible in a sidebar and writable from the palette.

**Switch harnesses. Keep the agent.** StateRoot carries your project's goal,
plans, memory, skills, sessions, and personality across Claude Code, Codex,
Cursor, Kimi Code, Pi, DeepSeek Harness and friends. This extension is the
editor surface: it watches `.stateroot/` and shows what every harness sees.

## What you get

- **Project Continuity sidebar** (lamp icon in the activity bar): project +
  phase, the active plan, the current handoff (with latest activity and next
  actions), and recent checkpoints — refreshing live as any harness writes.
- **Status bar** entry with the project phase.
- **Commands** (`Ctrl+Shift+P` → `StateRoot:`): Initialize, Checkpoint,
  Snapshot, Resume Digest, Write Handoff, Doctor, Refresh.
- No project yet? The sidebar offers **Initialize** in one click.

## Requirements

The [`stateroot` CLI](https://github.com/CognizTech/stateroot) on your PATH
(one local binary). Set `stateroot.cliPath` if it lives elsewhere.

## Design contract

The extension is a thin client: it reads `.stateroot/` files directly (plain
JSON/JSONL/Markdown — the documented state of record) and writes only through
the CLI. It never reimplements engine logic, never edits state files.

## Development

```bash
npm install
npm run compile     # tsc → out/
npm run package     # vsce → stateroot-<version>.vsix
```
