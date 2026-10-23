//! Shared CLI integration fixture — the one home of the test command
//! contract (M5 consolidation; the M4 updater-isolation fixture lives here
//! too). Every suite's spawned command gets hermetic homes, the
//! test-harness marker, and scrubbed provider keys from these helpers.
//!
//! Contract:
//! - Per-test state isolation stays: every helper call builds a fresh
//!   `Command`; local overrides chain `.env(...)`/`.env_remove(...)` off the
//!   returned value. No shared mutable state, no framework.
//! - The `STATEROOT_TEST_CMD_PROBES` marker suppresses the automatic
//!   updater on EVERY route (M4): awaited and detached. Suites that test the
//!   updater itself release the marker explicitly and use
//!   [`installed_stateroot`] for the automatic paths.
//! - Provider/synthesis keys are always scrubbed so a test can never reach
//!   a real API through ambient user configuration.
//!
//! Include from a sibling test file with:
//! `#[path = "support/mod.rs"] mod support;`

use std::path::Path;

use assert_cmd::Command;
use tempfile::TempDir;

/// The canonical spawned command: cargo-built binary, hermetic homes, the
/// test-harness marker, provider keys scrubbed.
#[allow(dead_code)]
pub fn stateroot(config_home: &Path, user_home: &Path, cwd: &Path) -> Command {
    isolated_command(
        Command::cargo_bin("stateroot").expect("binary"),
        config_home,
        user_home,
        cwd,
    )
}

fn isolated_command(mut cmd: Command, config_home: &Path, user_home: &Path, cwd: &Path) -> Command {
    cmd.env("STATEROOT_HOME", config_home)
        .env("STATEROOT_TEST_HOME", user_home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_BASE")
        .current_dir(cwd);
    cmd
}

/// The canonical hermetic homes pair (config home, user home).
#[allow(dead_code)]
pub fn homes() -> (TempDir, TempDir) {
    (
        tempfile::tempdir().expect("config home"),
        tempfile::tempdir().expect("user home"),
    )
}

/// `stateroot init` a fresh project directory.
#[allow(dead_code)]
pub fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
}

/// An "installed" copy of the CLI in a disposable directory (the M4
/// fixture contract): admitted by the updater's executable eligibility, so
/// live automatic-update paths stay exercisable end to end — and a
/// self-replace can only ever touch this throwaway copy, never a user's
/// binary. Rules: copy the built binary out of the target dir, keep
/// homes/projects hermetic, count requests against a local endpoint, and
/// never point a fixture at live GitHub.
#[allow(dead_code)]
pub fn installed_stateroot(
    install_dir: &Path,
    config_home: &Path,
    user_home: &Path,
    cwd: &Path,
) -> Command {
    let source = assert_cmd::cargo::cargo_bin("stateroot");
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("install bin dir");
    let installed = bin_dir.join(if cfg!(windows) {
        "stateroot.exe"
    } else {
        "stateroot"
    });
    if !installed.exists() {
        std::fs::copy(&source, &installed).expect("install copy");
    }
    let mut cmd = isolated_command(Command::new(installed), config_home, user_home, cwd);
    cmd.env_remove("STATEROOT_TEST_CMD_PROBES");
    cmd
}
