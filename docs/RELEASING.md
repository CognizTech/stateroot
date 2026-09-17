# Releasing StateRoot

Process for cutting a release (maintainer runbook; M5 stub — refine on the
first real tag).

## Versioning

Pre-1.0: minor versions per milestone (0.1.0 = M1–M4). After 1.0: semver.

## Checklist

1. Gate locally: `cargo fmt --all -- --check &&
   cargo clippy --workspace --all-targets -- -D warnings &&
   cargo test --workspace` — all green on a clean tree.
2. Update `CHANGELOG.md` (move the milestone notes into a versioned entry,
   date it).
3. Bump `version` in the workspace `Cargo.toml` (`[workspace.package]`).
4. Commit, tag: `git tag -a v0.1.1 -m "v0.1.1"` and
   `git push origin main v0.1.1`. A commit message containing `[skip tests]`
   skips fmt/clippy/test and the rolling `nightly` prerelease. Tagged `v*`
   pushes skip those jobs and run `build-release` + `release` only.
   Ordinary `main` pushes publish the rolling `nightly` preview after tests.
   Install it with `stateroot self-update --tag nightly`. Production stays
   on the latest `v*` GitHub release (`stateroot self-update`, no `--tag`).
5. CI (`release` job) builds `stateroot-linux-x64` and
   `stateroot-windows-x64.exe`, plus `StateRootSetup-x64.msi`, the
   verified `stateroot-vscode-<extension-version>.vsix`, and
   `stateroot-extension.json`, and attaches them with `checksums.txt`
   and the install scripts. Linux is linked with
   `cargo zigbuild` for **glibc 2.17** (Ubuntu 16.04 / Debian 9 / RHEL 7
   and newer); `packaging/linux/check-glibc.sh` fails the job if the binary
   needs a newer glibc. Windows Authenticode
   signing runs only when the GitHub `release` environment variable
   `AZURE_ARTIFACT_SIGNING_ENABLED` is `true`.
   The VSIX is the same byte-identical artifact used for GitHub
   reconciliation, VS Code Marketplace, and Open VSX/Cursor. Publishing
   that VSIX to either registry is an owner-authorized operation and is
   never implied by attaching it to a GitHub release.
6. Smoke-test the attached binary on a scratch project:
   `stateroot init && stateroot snap && stateroot doctor` — the doctor run
   must pass with zero config and zero keys (deterministic first-run UX).
   Then `stateroot editor status` should report each detected editor
   against the release-declared extension version; `stateroot editor reconcile`
   is the explicit retry if an automatic install/update left a target
   missing or stale.
7. Announce in README if the feature map changed materially.

## Notes

- Binaries are statically self-contained (vendored libgit2); users need
  nothing installed but git.
- Release artifacts carry no provider keys, ever. The synthesis layer always
  asks for the user's own key at runtime.
