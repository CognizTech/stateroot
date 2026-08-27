# Security Policy

## Supported Versions

StateRoot is pre-1.0. Only the latest release line receives fixes:

| Version | Supported |
| --- | --- |
| Latest `v*` release | ✅ |
| `nightly` prerelease | ⚠️ best effort |
| anything older | ❌ |

## Reporting a Vulnerability

**Please do not open a public issue for security reports.**

Use GitHub's private vulnerability reporting: this repository's **Security** tab
→ **Advisories** → **Report a vulnerability**. If that channel is unavailable,
contact the maintainers via GitHub ([@usama04](https://github.com/usama04)).

Include, when you can:

- the affected version (`stateroot --version`) and asset (CLI, MSI, install
  scripts, skill packages)
- platform and harness (Claude Code, Codex, Cursor, Kimi Code, Pi, DSH, …)
- reproduction steps or a proof of concept
- the impact you see (data exposure, code execution, integrity, privacy)

You will get an acknowledgment within 72 hours. We fix confirmed
vulnerabilities in the next patch release and credit reporters in the release
notes when they want it.

## Scope Notes

- StateRoot is a **local-first CLI**; release artifacts carry **no provider
  keys, ever**. Optional LLM synthesis runs only with your own keys
  (`DEEPSEEK_API_KEY` preferred, `OPENAI_API_KEY` fallback), supplied at
  runtime by you and never uploaded or embedded.
- Snapshots honor the root `.gitignore` and `.staterootignore`, plus `.git/`
  and `.stateroot/local/`. If your secret or credential still slipped into a
  snapshot, digest, or state file — that is a security report; tell us what
  slipped and where.
- Search stays in `.stateroot/local/` and is never included in snapshots.
  Project data stays in the repo; persona and USER.md live in `~/.stateroot/`.
