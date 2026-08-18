# StateRoot Agent Plugin (transport wrapper)

This directory packages StateRoot for [Agent Plugins 1.0](https://agent-plugins.org) discovery ecosystems.

## What it includes

- **MCP:** `stateroot mcp-stdio` (see `plugin.json` → `mcpServers.stateroot`)
- **Bootstrap skill:** `../skill/` (marketplace/onboarding only)

## What it does *not* replace

Run **`stateroot install`** on each machine for harness-native lifecycle hooks. Hooks remain required for:

- automatic observation capture into `.stateroot/spool/observations.jsonl`
- session-start resume / working-intelligence projection
- checkpoint and handoff finalize on stop/session end

Agent Plugins is transport + discovery; StateRoot harness integration stays in the CLI.

## Typical setup

```bash
# After installing the stateroot binary:
stateroot init
stateroot install
```

Then register this plugin in your agent marketplace, or point MCP clients at:

```json
{"command": "stateroot", "args": ["mcp-stdio"]}
```
