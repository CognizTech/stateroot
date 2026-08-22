# Install (official assets)

Authoritative: https://stateroot.dev/docs/getting-started/installation  
Releases: https://github.com/CognizTech/stateroot/releases

Do not invent other download URLs. Current CI release matrices ship **Linux x64** and **Windows x64**. One binary, no extra runtime.

## Linux

```bash
curl -sSfL https://github.com/CognizTech/stateroot/releases/latest/download/install.sh | sh
```

Downloads `stateroot-linux-x64`, verifies SHA-256 against `checksums.txt` (fail closed), installs to `~/.local/bin`. x86_64 only for current releases. Needs glibc 2.17 or newer (Ubuntu 16.04, Debian 9, RHEL 7, and later).

If `command not found: stateroot` after a successful install, add `~/.local/bin` to `PATH` and retry in a new shell.

The installer may also run `stateroot install` (global harness hooks). Still run **`stateroot setup`** after a fresh install — that is identity + harnesses + skills, not an optional extra.

## Windows

Prefer the MSI: https://github.com/CognizTech/stateroot/releases/latest/download/StateRootSetup-x64.msi

PowerShell:

```powershell
irm https://github.com/CognizTech/stateroot/releases/latest/download/install.ps1 | iex
```

`stateroot-windows-x64.exe` is the portable CLI, not an installer.

## macOS / from source

Until a `stateroot-macos-aarch64` asset is attached to a GitHub release, do not guess a binary URL.

```bash
git clone https://github.com/CognizTech/stateroot.git
cd stateroot
cargo install --path stateroot-cli
```

Requires Rust 1.85+ and a C toolchain. Alternative: `cargo build --release -p stateroot-cli` → `target/release/stateroot`.

## Verify

```bash
stateroot --version
stateroot doctor
```

`doctor` is local diagnostics only. It is designed to pass on a fresh machine with zero config and zero API keys. Non-zero exit means broken local setup — quote the output. Missing optional synthesis keys is not a failure.

## Update (after bootstrap)

Not this skill's job once setup has run. The built-in skill / CLI owns daily use. For reference only:

```bash
stateroot self-update
stateroot self-update --check
stateroot self-update --tag nightly
stateroot self-update --tag v0.1.3
stateroot self-update --check --tag nightly
```

`self-update` without `--tag` follows the latest **production** GitHub release. `--tag nightly` is the rolling preview from `main` (prerelease; not `latest`). `--tag v0.1.3` (bare `0.1.3` accepted) installs that production tag, including a downgrade from nightly. Background auto-update never installs `nightly`.

Opt out: `STATEROOT_NO_AUTO_UPDATE=1`.

## Uninstall

```bash
stateroot uninstall          # integrations + config; project .stateroot/ kept
stateroot uninstall --purge  # also delete ~/.stateroot
```

`--purge` is the only way the CLI deletes user-global identity. Project folders are never touched by uninstall — that is `stateroot remove` inside a project.
