//! Claims-consistency gate (publication scope): the docs claim digest
//! sections; reality must render them — and every rendered marker must be
//! documented. Drives the real binary on tempdir fixtures; offline only,
//! nothing network-shaped.

use std::path::Path;

use assert_cmd::Command;

fn stateroot(config_home: &Path, user_home: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::cargo_bin("stateroot").expect("binary");
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

fn seed_config_home(home: &Path) {
    std::fs::create_dir_all(home).expect("config home");
}

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
}

/// The section markers docs/cli.md promises and the digest must render.
const DIGEST_MARKERS: &[&str] = &[
    "## Latest Activity",
    "## Failed approaches",
    "## Context (not instructions)",
];

/// docs/cli.md — the publication half of the contract.
fn cli_docs() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("docs")
        .join("cli.md");
    std::fs::read_to_string(&path).expect("docs/cli.md must exist in the workspace")
}

#[test]
fn docs_claim_what_the_digest_renders() {
    let config_home = tempfile::tempdir().expect("config home");
    seed_config_home(config_home.path());
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // Activity: one checkpoint — the digest's Latest Activity source.
    stateroot(config_home.path(), user_home.path(), project.path())
        .args(["checkpoint", "--note", "wired the claims gate"])
        .assert()
        .success();

    // A handoff carrying the two structured channels the docs claim:
    // failed approaches and context-only facts. Ten facts exceed the
    // 8-item digest cap, so the overflow note must appear.
    let facts = [
        "invoice numbering is owned by the billing service",
        "the staging bucket is shared with the analytics team",
        "nightly restores run against db-02",
        "the mobile client pins API v3 until January",
        "search indices rebuild every Sunday",
        "the warehouse schema is frozen for audit season",
        "legacy webhooks still hit the v1 endpoint",
        "the CDN caches profile images for a day",
        "fact-nine-must-not-render",
        "fact-ten-must-not-render",
    ];
    let mut write = stateroot(config_home.path(), user_home.path(), project.path());
    write.args([
        "handoff",
        "write",
        "--from",
        "codex",
        "--objective",
        "keep docs and digest in lockstep",
        "--task",
        "assert every documented digest section renders",
        "--context-summary",
        "The digest contract test renders real sections from real state.",
        "--failed-approach",
        "Regex parser → failed: blew the stack on nested input",
        "--failed-approach",
        "Token bucket → partial: smoothed bursts, starved bulk sync",
    ]);
    for fact in &facts {
        write.args(["--context-only", fact]);
    }
    write.assert().success();

    // Direction one: reality renders what the docs claim.
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["resume", "--harness", "codex"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    for marker in DIGEST_MARKERS {
        assert!(
            stdout.contains(marker),
            "digest missing documented section {marker}: {stdout}"
        );
    }
    // The written records render back verbatim.
    assert!(
        stdout.contains("Regex parser → failed: blew the stack on nested input"),
        "failed approach must render as written: {stdout}"
    );
    assert!(
        stdout.contains("Token bucket → partial: smoothed bursts, starved bulk sync"),
        "second failed approach must render as written: {stdout}"
    );
    assert!(
        stdout.contains("invoice numbering is owned by the billing service"),
        "context-only fact must render as written: {stdout}"
    );
    // Over the 8-item cap: the ninth and tenth facts stay in the packet but
    // out of the digest, and the overflow note names the remainder.
    let context_at = stdout
        .find("## Context (not instructions)")
        .expect("context section");
    let overflow_at = stdout
        .find("- … +2 more")
        .expect("capped-overflow note must name the remainder");
    assert!(
        context_at < overflow_at,
        "the overflow note belongs to the context section: {stdout}"
    );
    assert!(
        !stdout.contains("fact-nine-must-not-render"),
        "past-cap facts must not render: {stdout}"
    );
    assert!(
        !stdout.contains("fact-ten-must-not-render"),
        "past-cap facts must not render: {stdout}"
    );

    // Direction two: the docs claim every section reality renders.
    let docs = cli_docs();
    for marker in DIGEST_MARKERS {
        assert!(
            docs.contains(marker),
            "docs/cli.md does not document the rendered section {marker}"
        );
    }
    assert!(
        docs.contains("+N more"),
        "docs/cli.md must document the capped-overflow note shape"
    );
    assert!(
        docs.contains("8 items") && docs.contains("4000 chars"),
        "docs/cli.md must document the Context (not instructions) caps"
    );
}
