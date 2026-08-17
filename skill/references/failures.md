# Bootstrap failures

Authoritative: https://stateroot.dev/docs/reference/faq

Quote CLI output. Do not hand-write `.stateroot/`, `~/.stateroot/`, or harness stubs.

| Symptom | Action |
| --- | --- |
| `command not found: stateroot` | Install from official assets. On Linux, ensure `~/.local/bin` is on `PATH`. |
| `stateroot --version` works, harnesses have no hooks | Run `stateroot setup` (this skill still applies). `stateroot install` is harness wiring only. |
| SmartScreen / unsigned warning | Expected on Windows for now. Use the MSI from the GitHub release. |
| macOS / no binary | Do not guess a release URL. Build from source (see install.md). |
| `doctor` non-zero | Broken local setup. Quote the output. Missing synthesis keys are not a failure. |
| Checksum mismatch / installer refuse | Fail closed. Re-download from the official latest release. Do not skip verification. |
| `not a stateroot project` | Project command on a tree with no `.stateroot/`. Run `stateroot init` from the repo root if the user wants this project on StateRoot — then expire this skill. |
| User hesitated to pipe `install.sh` | Stop. Give them the URL and the MSI/script to run themselves. |
| Setup already configured | Default is skip-unless-reconfigure. Do not pass `--blank-slate` unless they asked. |
| Want resume / checkpoint / memory | This skill has expired if setup ran. `stateroot skill show stateroot`. |
