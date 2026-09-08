# Changelog

## 0.2.18

- Bundle installers with the extension so macOS and PATH fixes no longer depend
  on a separate CLI release publishing an updated installation script.
- Allow slow GitHub downloads to complete, retry transient failures, and stream
  installation output with explicit timeout errors.
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
