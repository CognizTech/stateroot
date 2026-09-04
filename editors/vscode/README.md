# StateRoot — VS Code / Cursor extension

StateRoot in the Coding window: a sidebar glance plus an editor workbench (Control, Plans, Todos, Crew, Learnings, Memory, Lineage). Agents already use the CLI and digest. This UI is for the human directing several harnesses.

Ships in this repo. Install the packaged `.vsix` (built by `npm run package`), or run from source below.

## Surfaces

- **Activity bar lamp** → glance: Now, Needs you, current root. Empty workspace: Initialize.
- **Workbench** (`StateRoot: Open workbench`): Control inbox, Plans (approve / assign cli-mode harness / delegate), Crew (reassign / log), Lineage (compare, native diff, restore, fork).
- Palette commands remain as escape hatches.

Writes go through the `stateroot` CLI only.

## Development

```bash
npm install
npm run compile
```

Launch **Run StateRoot Extension** from this folder, or:

```bash
cursor --extensionDevelopmentPath=<repo>/editors/vscode <repo>
```
