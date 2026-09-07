# Changelog

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
