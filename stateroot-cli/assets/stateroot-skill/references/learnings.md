# Learnings quality bar

Operational detail for the Learnings section in `SKILL.md`. Read this before seeding an empty layer or recording a correction.

Learnings are CommandCode-style **durable preferences** (taste): judgment rules another harness can apply. StateRoot stores them as learnings, not a separate taste subsystem.

## Stores (do not mix)

| Store | Tool | What belongs |
|---|---|---|
| Learnings | `learn_record` / `stateroot learn record` | Prefer X over Y; never Z; quality bars; anti-patterns |
| Memory | `memory` / `memory_save` | Curated facts in MEMORY.md (add/replace/remove); recall via FTS |
| Wiki | `wiki_show` / distill | Compiled pages — catalog in digest, bodies on pull |
| Skill | `skill_propose` | A procedure that worked end-to-end |
| Soul | soul commands | Identity / working relationship |

`learn_record` always writes a learning. It does not classify the sentence into another store.

## A good learning

A later agent, in a different harness, with no transcript, can use it as a decision rule.

It names:

1. the **choice** (prefer X over Y) or the **ban** (never Z)
2. **when** it applies
3. what **not** to do, if that is the point

It is still true next month. It is not a snapshot of the tree.

## Examples (format to copy)

These are format exemplars, not instructions to copy into every project:

```
Prefer small, reviewable diffs over rewrites. Touch only files that implement the asked change; do not restyle or restructure adjacent code.
```

```
Server-side tools must never include arbitrary command execution. exec belongs on the client, never on the server.
```

```
Always read .gitignore before exploring a codebase so ignored directories do not pollute context.
```

```
Configuration belongs in YAML, not hardcoded defaults. API flavor, model selection, and execution policy should be configurable.
```

```
Do not over-engineer. Prefer a practical working solution over architecture copied from another project that does not fit this one.
```

```
When two designs are valid, prefer restrained, provenance-rich interfaces over decorative generic AI chrome.
```

## Anti-examples (do not record these as learnings)

```
Laiq is a TypeScript/Python monorepo
```

That is a fact. If it must persist: `memory_save`. A learning would be: `Keep Python and TypeScript package boundaries explicit. Do not collapse the monorepo into one toolchain or share config that only one language owns.`

```
prefer evidence over assertion
```

Too thin. A learning would be: `Do not claim a change works without evidence. Cite the test, command, or file you ran; do not assert from the diff alone.`

```
the deploy uses systemd
```

Inventory. Memory if needed. A learning would be: `Ship deploy changes as systemd units, not ad-hoc shell. Do not add a second process supervisor.`

```
be careful / write good code / follow best practices
```

Slogans. Discard them.

## Seeding an empty layer

After `stateroot init`, if `learnings list` (project) or `learnings list --user` (global) is empty:

1. Read the repo (or the user's stated preferences) until you have **evidence**
2. Write **2–7** learnings that meet the bar above — not one inventory sentence
3. Stop. Do not pad. Do not duplicate. Do not dump the README

Global seed: how this human wants agents to work everywhere.
Project seed: how *this* codebase should be built and what it punishes.

## Maintenance

- Read both layers before writing.
- If a new note is the same judgment as an existing one, skip it.
- If the user corrected you, record the **rule**, not the incident.
- One note per `learn record` call.
