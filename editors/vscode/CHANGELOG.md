# Changelog

## 0.2.22

_GitHub-release artifact only; registries intentionally remain on 0.2.21
while the campaign measurement window is open._

- A full editor-event queue no longer silently loses a version's
  `editor_seen`: the version marker advances only after the event is
  durably enqueued (or telemetry is opted out), so a full queue retries on
  the next activation.
- Dead installer-ping code removed.

## 0.2.21

- The extension is now a measurable recovery controller. On install or update
  it captures an immutable preflight snapshot (prior marker and receipt,
  editor host, workspace presence, CLI status missing/unrunnable/working with
  version, path class — never the path), classifies the profile
  (verified_legacy, existing_cli, verified_project, unknown_first_seen —
  preserved across retries), and runs exactly one recovery path: bundled
  stable installer for a missing or unrunnable CLI, self-update for a stale
  working one, a `doctor` health check with a single repair pass for a
  current receipt, and the bundled fallback only for the default stable
  destination — never custom, Cargo, or nightly paths.
- Exactly one integration: extension-driven installers skip their internal
  integration and extension-driven self-update skips automatic rearming, then
  the controller performs one final `stateroot install` with
  `STATEROOT_INSTALL_VIA=extension`.
- Anonymous editor recovery telemetry (opt out with `STATEROOT_NO_PING=1`):
  `editor_seen` once per extension version, `setup_started`, and
  `setup_finished` with ready/failed, a bounded failure stage, and the CLI
  install_id once linked. Events persist with stable ids in bounded
  globalState, retry until acknowledged, and are removed only after 2xx —
  the queue never evicts unacknowledged events, flushes are single-flight,
  and a failed identity link stays pending and retries on the next
  activation.
- Project initialization stays separately retryable so a recovered machine
  cannot hide a failed workspace init.
- Marketplace description realigned: persistent, federated meta-harness for
  AI agents.

## 0.2.18

- Bundle installers with the extension so macOS and PATH fixes no longer depend
  on a separate CLI release publishing an updated installation script.
- Allow slow GitHub downloads to complete, retry transient failures, and stream
  installation output with explicit timeout errors.
- Use the macOS system HTTPS proxy and bypass list for downloads when no proxy
  environment variable is configured. Explicit proxy settings take precedence.
- Make the resolved CLI available in new integrated terminals and discover
  source installations under `~/.cargo/bin`.
- Create a missing zsh profile when configuring PATH, respecting `ZDOTDIR`.
- Share concurrent installation attempts and verify the installed binary.
- Add regression tests for platform selection, installation failures, checksum
  verification, packaging, and terminal/shell PATH handling.

## 0.2.17

- CLI auto-install now works on macOS: `detectPlatform` still claimed "macOS
  release binaries are not shipped yet" even though releases have shipped
  `stateroot-macos-aarch64` since v0.1.15 — so Apple Silicon machines never
  got the CLI. arm64 now uses `install.sh`; Intel Macs get an honest
  Apple-Silicon-only message.

## 0.2.16

- The extension now activates on editor startup (`onStartupFinished`) —
  previously it only activated inside an existing StateRoot project or via
  its view/commands, so new users who installed from the marketplace never
  activated it and the CLI auto-install never fired. This was the funnel
  leak behind "downloads but no installs".

## 0.2.15

- CLI auto-installs are tagged (`STATEROOT_INSTALL_VIA=extension`) so the
  anonymous install counter can tell extension-driven installs from script
  installs.

## 0.2.14

- After the CLI auto-installs, `stateroot init` runs automatically in the
  open project (skipped when a manifest already exists) — the extension
  works on first sight, not first command.

## 0.2.13

- The CLI installs itself: when no `stateroot` binary is found (on extension
  activation or first use), the latest stable release is downloaded and
  installed automatically — no confirmation gate. The extension is no longer
  dead weight on a fresh machine.
- The continuity demo gif now opens the store overview.

## 0.2.12

- Store overview rewritten: what the StateRoot CLI is and does, what the
  extension adds on top, requirements, and links.

## 0.2.11

- Wiki page listing reads the OKF bundle location (`wiki/pages/`) with a
  legacy fallback for pre-migration projects.
- Store readiness: PNG gallery icon, gallery banner, keywords, changelog.

## 0.2.10

- Sidebar + workbench (Control, Plans, Todos, Crew, Learnings, Memory,
  Lineage) for the human directing several harnesses.
