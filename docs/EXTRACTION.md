# Extracting StateRoot into its own public repo

This folder is designed to become the standalone public StateRoot repository
via `git subtree split`. It was a self-contained git repo from day one
(`git init` inside `stateroot/` at M1), so two extraction paths exist.

## Path A — keep the existing repo (preferred, zero scrubbing)

The `stateroot/` directory is already its own repository with the full M1–M5
history and nothing monorepo-specific inside it. Simply:

```bash
cd stateroot
git remote add public git@github.com:stateroot/stateroot.git
git push public main
```

## Path B — split from the monorepo (if you ever nested it differently)

```bash
# from the monorepo root (one-time)
git subtree split --prefix stateroot -b stateroot-extract
git clone -b stateroot-extract . stateroot-public
```

## Scrub checklist (verify before pushing public)

- [ ] **No monorepo paths**: the workspace has no path dependencies outside
      `stateroot/` (`cargo metadata` shows only crates.io + workspace members).
- [ ] **Contracts copy is fine**: `contracts/stateroot_harness_registry.v1.json`
      is intentionally shared (the file-format contract) — it belongs in the
      public repo verbatim.
- [ ] **No secrets**: no provider keys, tokens, or user-specific config in
      git history (`git log -p | grep -iE 'api_key|token|secret'` should show
      only code references, never values).
- [ ] **No user data**: nothing under `.stateroot/` of a real project, no
      transcripts, no personas beyond embedded test fixtures.
- [ ] **License + readme present**: `LICENSE` (BSL-1.1 → Apache-2.0),
      `README.md`, `CHANGELOG.md`, `.github/workflows/ci.yml`.

## Remote setup (suggested)

- Default branch `main`; require CI green (ubuntu + windows) for merges.
- Tag protection: releases only via tags pushed by maintainers (the release
  job attaches binaries automatically).
- `git clone` + `cargo install --path stateroot-cli` must work on a clean
  machine with nothing but Rust + git — keep that invariant sacred.
