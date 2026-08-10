//! Phase-1 tests — GitHub device flow (mocked endpoints), repo link, and
//! refs sync against a local file remote (fork-on-divergence, quarantine).

use std::path::Path;

use assert_cmd::Command;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn stateroot(config_home: &Path, user_home: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::cargo_bin("stateroot").expect("binary");
    cmd.env("STATEROOT_HOME", config_home)
        .env("STATEROOT_TEST_HOME", user_home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env("STATEROOT_CREDENTIALS", "file")
        // These tests exercise the real cloud paths (the gate is tested
        // separately in tests/update.rs).
        .env("STATEROOT_CLOUD_PREVIEW", "1")
        .current_dir(cwd);
    cmd
}

fn init_project(config_home: &Path, user_home: &Path, project: &Path) {
    std::fs::create_dir_all(project).expect("project dir");
    stateroot(config_home, user_home, project)
        .arg("init")
        .assert()
        .success();
}

/// `file://` URL for a local path that git2 accepts on Windows and Unix.
fn path_as_file_url(path: &Path) -> String {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut s = abs.to_string_lossy().replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_string();
    }
    if cfg!(windows) {
        // Drive paths need the third slash: file:///C:/...
        format!("file:///{s}")
    } else {
        // Absolute POSIX path already starts with `/` → file:///tmp/...
        format!("file://{s}")
    }
}

#[tokio::test]
async fn device_flow_login_polls_until_approved_and_logout_clears() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login/device/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dc-1",
            "user_code": "ABCD-1234",
            "verification_uri": "https://github.com/login/device",
            "interval": 0,
            "expires_in": 120
        })))
        .expect(1)
        .mount(&server)
        .await;
    // Two pending polls, then the token (first-mounted wins for the first 2).
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": "authorization_pending"
        })))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "gho_test_token",
            "token_type": "bearer",
            "scope": "repo"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let cwd = tempfile::tempdir().expect("cwd");
    let mut cmd = stateroot(config_home.path(), user_home.path(), cwd.path());
    cmd.env("STATEROOT_GITHUB_WEB_BASE", server.uri())
        .env("STATEROOT_GITHUB_CLIENT_ID", "test-client-id");
    let out = cmd.args(["login", "--via", "github"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("Enter code: ABCD-1234"), "{stdout}");
    assert!(
        stdout.contains("logged in via github (scope: repo)"),
        "{stdout}"
    );
    let creds = std::fs::read_to_string(config_home.path().join("credentials.json"))
        .expect("credentials file");
    assert!(creds.contains("gho_test_token"), "{creds}");
    server.verify().await;

    stateroot(config_home.path(), user_home.path(), cwd.path())
        .arg("logout")
        .assert()
        .success();
    let creds = std::fs::read_to_string(config_home.path().join("credentials.json"))
        .expect("credentials file");
    assert!(!creds.contains("gho_test_token"), "logout clears: {creds}");
}

#[tokio::test]
async fn login_without_client_id_fails_honestly() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let cwd = tempfile::tempdir().expect("cwd");
    let out = stateroot(config_home.path(), user_home.path(), cwd.path())
        .args(["login", "--via", "github"])
        .env_remove("STATEROOT_GITHUB_CLIENT_ID")
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("utf8");
    assert!(stderr.contains("STATEROOT_GITHUB_CLIENT_ID"), "{stderr}");
}

#[tokio::test]
async fn repo_link_binds_and_verifies_access() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "full_name": "acme/widgets", "private": true
        })))
        .expect(1)
        .mount(&server)
        .await;

    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());
    // seed a credential (file store)
    std::fs::write(
        config_home.path().join("credentials.json"),
        json!({"github": {"access_token": "gho_x", "obtained_at": "2026-08-08T00:00:00Z"}})
            .to_string(),
    )
    .expect("creds");

    let mut cmd = stateroot(config_home.path(), user_home.path(), project.path());
    cmd.env("STATEROOT_GITHUB_API_BASE", server.uri());
    let out = cmd
        .args(["repo", "link", "acme/widgets"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("linked: acme/widgets (same-repo)"),
        "{stdout}"
    );
    let manifest =
        std::fs::read_to_string(project.path().join(".stateroot/manifest.json")).expect("manifest");
    assert!(manifest.contains("acme/widgets"), "{manifest}");
    server.verify().await;

    // status reflects the binding
    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["repo", "status"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("acme/widgets"), "{stdout}");
}

/// Two projects sharing one file remote: push, pull, then divergence forks.
#[test]
fn sync_push_pull_and_divergence_forks() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let remote_dir = tempfile::tempdir().expect("remote");
    let remote_repo = remote_dir.path().join("acme/widgets.git");
    git2::Repository::init_bare(&remote_repo).expect("bare remote");
    let git_base = path_as_file_url(remote_dir.path());

    let p1 = tempfile::tempdir().expect("p1");
    let p2 = tempfile::tempdir().expect("p2");
    init_project(config_home.path(), user_home.path(), p1.path());
    init_project(config_home.path(), user_home.path(), p2.path());

    // Link both to the same "remote" (skip the REST verify for this test by
    // writing the binding directly).
    for project in [p1.path(), p2.path()] {
        let manifest_path = project.join(".stateroot/manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest["github"] = json!({"repo": "acme/widgets", "layout": "same-repo"});
        std::fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    let sync = |project: &Path, extra: &[&str]| {
        let mut cmd = stateroot(config_home.path(), user_home.path(), project);
        cmd.env("STATEROOT_GITHUB_GIT_BASE", &git_base);
        let mut args = vec!["sync"];
        args.extend_from_slice(extra);
        cmd.args(args);
        cmd
    };

    // P1: snap A + push.
    std::fs::write(p1.path().join("a.txt"), "from p1\n").unwrap();
    stateroot(config_home.path(), user_home.path(), p1.path())
        .args(["snap", "--reason", "p1-A"])
        .assert()
        .success();
    let out = sync(p1.path(), &["--push"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("sync:"), "{stdout}");
    let remote = git2::Repository::open_bare(&remote_repo).unwrap();
    assert!(remote.refname_to_id("refs/stateroot/latest").is_ok());

    // P2: pull (adopts A), snap C on top, push (fast-forward).
    let out = sync(p2.path(), &["--pull"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    assert!(stdout.contains("sync:"), "{stdout}");
    let p2_state = std::fs::read_to_string(p2.path().join(".stateroot/local/sync-state.json"));
    assert!(p2_state.is_ok(), "sync-state recorded");
    std::fs::write(p2.path().join("b.txt"), "from p2\n").unwrap();
    stateroot(config_home.path(), user_home.path(), p2.path())
        .args(["snap", "--reason", "p2-C"])
        .assert()
        .success();
    sync(p2.path(), &["--push"]).assert().success();

    // P1: snap B on its own tip (diverged from C), pull → fork, local tip kept.
    std::fs::write(p1.path().join("c.txt"), "p1 again\n").unwrap();
    stateroot(config_home.path(), user_home.path(), p1.path())
        .args(["snap", "--reason", "p1-B"])
        .assert()
        .success();
    let local_tip_before = git2::Repository::open(p1.path())
        .unwrap()
        .refname_to_id("refs/stateroot/latest")
        .unwrap();
    let out = sync(p1.path(), &["--pull"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    let repo = git2::Repository::open(p1.path()).unwrap();
    let local_tip_after = repo.refname_to_id("refs/stateroot/latest").unwrap();
    assert_eq!(
        local_tip_before, local_tip_after,
        "local tip never force-moved"
    );
    let forks: Vec<String> = repo
        .references_glob("refs/stateroot/forks/*")
        .unwrap()
        .flatten()
        .filter_map(|r| r.name().map(str::to_string))
        .collect();
    assert!(
        forks.iter().any(|f| f.contains("sync-diverged-")),
        "divergence fork kept: {forks:?} / {stdout}"
    );
    // The remote still has its own tip (never deleted, never forced).
    assert!(remote.refname_to_id("refs/stateroot/latest").is_ok());
    // P1's non-fast-forward push is honestly rejected (never forced).
    sync(p1.path(), &["--push"]).assert().failure();
}

#[test]
fn quarantine_never_enters_roots() {
    let config_home = tempfile::tempdir().expect("config home");
    let user_home = tempfile::tempdir().expect("user home");
    let project = tempfile::tempdir().expect("project");
    init_project(config_home.path(), user_home.path(), project.path());

    // .stateroot/local/ content + a sync-state file
    let local_dir = project.path().join(".stateroot/local");
    std::fs::create_dir_all(&local_dir).unwrap();
    std::fs::write(local_dir.join("notes.txt"), "machine-local only\n").unwrap();
    std::fs::write(local_dir.join("sync-state.json"), "{}\n").unwrap();

    let out = stateroot(config_home.path(), user_home.path(), project.path())
        .args(["snap", "--reason", "quarantine check"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    let hash = stdout
        .lines()
        .find(|l| l.starts_with("root "))
        .expect("root line")
        .trim_start_matches("root ")
        .trim()
        .to_string();

    let repo = git2::Repository::open(project.path()).unwrap();
    let commit = repo
        .find_commit(git2::Oid::from_str(&hash).unwrap())
        .unwrap();
    let tree = commit.tree().unwrap();
    assert!(
        tree.get_path(Path::new(".stateroot/local")).is_err(),
        ".stateroot/local must never enter roots"
    );
}
