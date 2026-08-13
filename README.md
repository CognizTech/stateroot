# StateRoot

**Version control for AI-assisted work.**
**Switch agents. Nothing is lost.**
**Local-first. No account required.**

[![Release](https://img.shields.io/github/v/release/CognizTech/stateroot?color=7ee0c8&labelColor=0c1016&logo=github&style=flat-square)](https://github.com/CognizTech/stateroot/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/CognizTech/stateroot/ci.yml?branch=main&label=CI&labelColor=0c1016&style=flat-square)](https://github.com/CognizTech/stateroot/actions)
[![License](https://img.shields.io/badge/license-Apache--2.0-7ee0c8?labelColor=0c1016&style=flat-square)](LICENSE)

Website · [Docs](https://stateroot.dev/docs/intro) · [CLI reference](https://stateroot.dev/docs/reference/cli) · [Discord](https://discord.gg/SfbKEPRD7) · [Releases](https://github.com/CognizTech/stateroot/releases) · [Issues](https://github.com/CognizTech/stateroot/issues)

StateRoot is the local continuity layer for AI-assisted work. It sits beside Git and beside the coding agents you already use. Git versions trees. StateRoot versions *agentic work* — the snapshots agents create, the handoffs they leave, and the memory and working identity that should follow you from Claude Code to Codex to Cursor without a rebuild.

```text
Claude Code ──→ State A ──→ State B
                             │
                             ├── continue with Codex
                             ├── branch with Cursor
                             └── restore an earlier state
```

One binary. No account. No hosted server. Your project stays on your machine.

## What StateRoot is

| | |
| --- | --- |
| **Work lineage** | Append-only roots (Git plumbing, never your branches). Inspect, diff, fork, restore. |
| **Cross-agent continuity** | Checkpoints, structured handoffs, and a resume digest injected at session start. |
| **Portable working identity** | Soul + USER.md delivered in full — not truncated to fit a token budget. |
| **Layered memory** | Curated hot-apex facts, a compiled wiki, local FTS recall. Taste stays learnings. |
| **Federation** | Skills, MCP servers, and harness rules pooled across tools; conflicts are not overwritten. |
| **Honest provenance** | Verified (git) vs observed (transcripts) vs synthesized (LLM) — empty stays empty. |

It is not a hosted agent, not a Git replacement, and not a cloud account.

Full capability map: **[stateroot.dev](https://stateroot.dev/docs/intro)**.

## Install

### Linux

```bash
curl -sSfL https://github.com/CognizTech/stateroot/releases/latest/download/install.sh | sh
```

### Windows (PowerShell)

```powershell
irm https://github.com/CognizTech/stateroot/releases/latest/download/install.ps1 | iex
```

Prefer **`StateRootSetup-x64.msi`** from [Releases](https://github.com/CognizTech/stateroot/releases). `stateroot-windows-x64.exe` is the portable CLI, not an installer.

Current tagged builds ship **Linux x64** and **Windows x64**. macOS: build from source until a release asset is published.

```bash
stateroot --version
stateroot doctor     # passes with zero config and zero keys
```

## Quickstart

```bash
cd my-project
stateroot init
stateroot setup      # once per machine: identity, harnesses, skills
```

Keep working in your usual agent. Session hooks inject a digest. When you switch tools — or hit a usage limit mid-task — the next agent picks up the same state of record:

```bash
stateroot status
stateroot log
stateroot resume --harness cursor
stateroot checkpoint --note "what changed — why — what it unblocks" --files src/lib.rs
stateroot handoff write --from cursor --objective "…" --task "…" --context-summary "…"
```

## Supported harnesses

**Hooks + transcripts:** Claude Code · Codex · Cursor · Kimi Code · OpenClaw · Hermes

**Instruction files / federation:** OpenCode · GitHub Copilot · Crush · others detected at install

Per-harness notes: [Harnesses](https://stateroot.dev/docs/harnesses/overview).

## Documentation

All user docs live at **[stateroot.dev](https://stateroot.dev)**:

| Section | Contents |
| --- | --- |
| [Quickstart](https://stateroot.dev/docs/getting-started/quickstart) | Init → setup → first resume |
| [Concepts](https://stateroot.dev/docs/concepts/overview) | Roots, continuity, identity, memory, provenance |
| [Capabilities](https://stateroot.dev/docs/features/roots) | Lineage, handoffs, memory, wiki, skills, MCP, rules, compiler |
| [Harnesses](https://stateroot.dev/docs/harnesses/overview) | Per-tool install and protocol |
| [CLI reference](https://stateroot.dev/docs/reference/cli) | Every command |
| [MCP tools](https://stateroot.dev/docs/reference/mcp-tools) | Local stdio server |
| [Architecture](https://stateroot.dev/docs/developer-guide/architecture) | Crates and planes |

Machine-readable index: [stateroot.dev/llms.txt](https://stateroot.dev/llms.txt).

## Privacy

Your project never leaves the machine unless you copy it. Snap/root payloads honor `.staterootignore` plus hardcoded `.git/` and `.stateroot/local/`. Root `.gitignore` is **not** unioned into lineage trees. Optional LLM synthesis calls only *your* OpenAI-compatible endpoint, and only if you set a key.

Details: [Privacy](https://stateroot.dev/docs/guides/privacy).

## Contributing

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Docs: **[stateroot.dev](https://stateroot.dev/docs/developer-guide/contributing)**.

Please preserve [product intent](https://stateroot.dev/docs/features/rules): do not replace agent judgment with classifiers, or truncate identity to look conservative.

## License

Apache-2.0 — see [LICENSE](LICENSE).
