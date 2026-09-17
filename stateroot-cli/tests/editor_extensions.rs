//! Editor discovery / VSIX reconciliation (network mocked via wiremock).

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::json;
use sha2::{Digest, Sha256};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const VSIX_BYTES: &[u8] = b"vsix-bytes-v1";
const EXT_ID: &str = "CognizTech.stateroot";
const EXT_VER: &str = "0.2.19";
const VSIX_NAME: &str = "stateroot-vscode-0.2.19.vsix";

fn stateroot(config_home: &Path, user_home: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::cargo_bin("stateroot").expect("binary");
    cmd.env("STATEROOT_HOME", config_home)
        .env("STATEROOT_TEST_HOME", user_home)
        .env("STATEROOT_TEST_CMD_PROBES", "")
        .env("STATEROOT_DISABLE_SCHEDULED_UPDATE", "1")
        .env("STATEROOT_TEST_BUILD_VERSION", "0.2.2")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("STATEROOT_SYNTHESIS_API_KEY")
        .current_dir(cwd);
    cmd
}

fn seed_config(home: &Path) {
    std::fs::create_dir_all(home).expect("config home");
    std::fs::write(
        home.join("config.toml"),
        "user_id = \"default\"\nagent_id = \"default\"\n[update]\nrepo = \"stateroot-dev/stateroot\"\n",
    )
    .expect("config.toml");
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn manifest_body(digest: &str) -> String {
    serde_json::to_string(&json!({
        "schema_version": 1,
        "extension_id": EXT_ID,
        "extension_version": EXT_VER,
        "vsix_filename": VSIX_NAME,
        "digest": digest,
    }))
    .expect("manifest json")
}

fn checksums_text(manifest: &[u8], vsix: &[u8]) -> String {
    format!(
        "{}  stateroot-linux-x64\n{}  stateroot-windows-x64.exe\n{}  stateroot-macos-aarch64\n{}  {VSIX_NAME}\n{}  stateroot-extension.json\n",
        sha256_hex(b"cli"),
        sha256_hex(b"cli"),
        sha256_hex(b"cli"),
        sha256_hex(vsix),
        sha256_hex(manifest),
    )
}

fn unix_stub() -> &'static str {
    r#"#!/bin/sh
dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
if [ "$1" = "--list-extensions" ]; then
  if [ -f "$dir/list.txt" ]; then cat "$dir/list.txt"; fi
  exit 0
fi
if [ "$1" = "--install-extension" ]; then
  if [ -f "$dir/fail-install" ]; then exit 1; fi
  if [ -f "$dir/install-to.txt" ]; then cp "$dir/install-to.txt" "$dir/list.txt"; fi
  echo "installed $2" >> "$dir/installs.txt"
  exit 0
fi
exit 1
"#
}

fn windows_stub() -> &'static str {
    r#"@echo off
set DIR=%~dp0
if "%~1"=="--list-extensions" (
  if exist "%DIR%list.txt" type "%DIR%list.txt"
  exit /b 0
)
if "%~1"=="--install-extension" (
  if exist "%DIR%fail-install" exit /b 1
  if exist "%DIR%install-to.txt" copy /Y "%DIR%install-to.txt" "%DIR%list.txt" >nul
  echo installed %2>>"%DIR%installs.txt"
  exit /b 0
)
exit /b 1
"#
}

fn write_stub(
    dir: &Path,
    name: &str,
    list_line: Option<&str>,
    install_to: Option<&str>,
    fail_install: bool,
) -> PathBuf {
    std::fs::create_dir_all(dir).expect("stub dir");
    if let Some(line) = list_line {
        std::fs::write(dir.join("list.txt"), format!("{line}\n")).expect("list.txt");
    }
    if let Some(line) = install_to {
        std::fs::write(dir.join("install-to.txt"), format!("{line}\n")).expect("install-to");
    }
    if fail_install {
        std::fs::write(dir.join("fail-install"), "1").expect("fail-install");
    }
    if cfg!(windows) {
        let path = dir.join(format!("{name}.cmd"));
        std::fs::write(&path, windows_stub()).expect("cmd stub");
        path
    } else {
        let path = dir.join(name);
        std::fs::write(&path, unix_stub()).expect("sh stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).expect("meta").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).expect("chmod");
        }
        path
    }
}

fn ext_line(version: &str) -> String {
    format!("{EXT_ID}@{version}")
}

struct ReleaseMock {
    server: MockServer,
    vsix_url: String,
}

async fn mount_release(tag: &str, vsix: &[u8], manifest: &[u8], vsix_expect: u64) -> ReleaseMock {
    let server = MockServer::start().await;
    let checksums = checksums_text(manifest, vsix);
    let body = json!({
        "tag_name": tag,
        "name": format!("StateRoot {tag}"),
        "assets": [
            {"name": "stateroot-linux-x64", "browser_download_url": format!("{}/cli", server.uri())},
            {"name": "stateroot-windows-x64.exe", "browser_download_url": format!("{}/cli", server.uri())},
            {"name": "stateroot-macos-aarch64", "browser_download_url": format!("{}/cli", server.uri())},
            {"name": "checksums.txt", "browser_download_url": format!("{}/checksums.txt", server.uri())},
            {"name": "stateroot-extension.json", "browser_download_url": format!("{}/stateroot-extension.json", server.uri())},
            {"name": VSIX_NAME, "browser_download_url": format!("{}/{VSIX_NAME}", server.uri())},
        ]
    });
    let release_path = if tag == "nightly" {
        "/repos/stateroot-dev/stateroot/releases/tags/nightly".to_string()
    } else {
        "/repos/stateroot-dev/stateroot/releases/latest".to_string()
    };
    Mock::given(method("GET"))
        .and(path(release_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/checksums.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(checksums))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/stateroot-extension.json"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(manifest.to_vec()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/{VSIX_NAME}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vsix.to_vec()))
        .expect(vsix_expect)
        .mount(&server)
        .await;
    ReleaseMock {
        vsix_url: format!("{}/{VSIX_NAME}", server.uri()),
        server,
    }
}

fn wanted() -> String {
    ext_line(EXT_VER)
}

#[tokio::test]
async fn no_editor_is_clean_noop_without_vsix_download() {
    let vsix = VSIX_BYTES;
    let digest = sha256_hex(vsix);
    let manifest = manifest_body(&digest);
    let mock = mount_release("v0.2.2", vsix, manifest.as_bytes(), 0).await;
    let config = tempfile::tempdir().unwrap();
    seed_config(config.path());
    let user = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    stateroot(config.path(), user.path(), cwd.path())
        .env("STATEROOT_GITHUB_API_BASE", mock.server.uri())
        .args(["editor", "reconcile"])
        .assert()
        .success()
        .stdout(predicates::str::contains("not detected"));
    mock.server.verify().await;
}

#[tokio::test]
async fn vscode_missing_installs_and_cursor_absent_stays_absent() {
    let vsix = VSIX_BYTES;
    let digest = sha256_hex(vsix);
    let manifest = manifest_body(&digest);
    let mock = mount_release("v0.2.2", vsix, manifest.as_bytes(), 1).await;
    let config = tempfile::tempdir().unwrap();
    seed_config(config.path());
    let user = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let code_dir = tempfile::tempdir().unwrap();
    let code = write_stub(code_dir.path(), "code", None, Some(&wanted()), false);
    stateroot(config.path(), user.path(), cwd.path())
        .env("STATEROOT_GITHUB_API_BASE", mock.server.uri())
        .env("STATEROOT_TEST_EDITOR_CODE", &code)
        .args(["editor", "reconcile"])
        .assert()
        .success()
        .stdout(predicates::str::contains("updated"));
    let installs = std::fs::read_to_string(code_dir.path().join("installs.txt")).unwrap();
    assert!(installs.contains("installed"));
    mock.server.verify().await;
    let _ = mock.vsix_url;
}

#[tokio::test]
async fn exact_and_newer_do_not_install() {
    let vsix = VSIX_BYTES;
    let digest = sha256_hex(vsix);
    let manifest = manifest_body(&digest);
    let mock = mount_release("v0.2.2", vsix, manifest.as_bytes(), 0).await;
    let config = tempfile::tempdir().unwrap();
    seed_config(config.path());
    let user = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let code_dir = tempfile::tempdir().unwrap();
    let cursor_dir = tempfile::tempdir().unwrap();
    let code = write_stub(
        code_dir.path(),
        "code",
        Some(&wanted()),
        Some(&wanted()),
        false,
    );
    let cursor = write_stub(
        cursor_dir.path(),
        "cursor",
        Some(&ext_line("9.9.9")),
        Some(&wanted()),
        false,
    );
    stateroot(config.path(), user.path(), cwd.path())
        .env("STATEROOT_GITHUB_API_BASE", mock.server.uri())
        .env("STATEROOT_TEST_EDITOR_CODE", &code)
        .env("STATEROOT_TEST_EDITOR_CURSOR", &cursor)
        .args(["editor", "reconcile"])
        .assert()
        .success()
        .stdout(predicates::str::contains("exact"))
        .stdout(predicates::str::contains("newer"));
    assert!(!code_dir.path().join("installs.txt").is_file());
    assert!(!cursor_dir.path().join("installs.txt").is_file());
    mock.server.verify().await;
}

#[tokio::test]
async fn stale_cursor_updates_while_exact_vscode_is_noop() {
    let vsix = VSIX_BYTES;
    let digest = sha256_hex(vsix);
    let manifest = manifest_body(&digest);
    let mock = mount_release("v0.2.2", vsix, manifest.as_bytes(), 1).await;
    let config = tempfile::tempdir().unwrap();
    seed_config(config.path());
    let user = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let code_dir = tempfile::tempdir().unwrap();
    let cursor_dir = tempfile::tempdir().unwrap();
    let code = write_stub(
        code_dir.path(),
        "code",
        Some(&wanted()),
        Some(&wanted()),
        false,
    );
    let cursor = write_stub(
        cursor_dir.path(),
        "cursor",
        Some("cogniztech.STATEROOT@0.2.18"),
        Some(&wanted()),
        false,
    );
    stateroot(config.path(), user.path(), cwd.path())
        .env("STATEROOT_GITHUB_API_BASE", mock.server.uri())
        .env("STATEROOT_TEST_EDITOR_CODE", &code)
        .env("STATEROOT_TEST_EDITOR_CURSOR", &cursor)
        .args(["editor", "reconcile"])
        .assert()
        .success();
    assert!(!code_dir.path().join("installs.txt").is_file());
    assert!(cursor_dir.path().join("installs.txt").is_file());
    mock.server.verify().await;
}

#[tokio::test]
async fn one_editor_failure_preserves_the_other() {
    let vsix = VSIX_BYTES;
    let digest = sha256_hex(vsix);
    let manifest = manifest_body(&digest);
    let mock = mount_release("v0.2.2", vsix, manifest.as_bytes(), 1).await;
    let config = tempfile::tempdir().unwrap();
    seed_config(config.path());
    let user = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let code_dir = tempfile::tempdir().unwrap();
    let cursor_dir = tempfile::tempdir().unwrap();
    let code = write_stub(code_dir.path(), "code", None, Some(&wanted()), true);
    let cursor = write_stub(cursor_dir.path(), "cursor", None, Some(&wanted()), false);
    stateroot(config.path(), user.path(), cwd.path())
        .env("STATEROOT_GITHUB_API_BASE", mock.server.uri())
        .env("STATEROOT_TEST_EDITOR_CODE", &code)
        .env("STATEROOT_TEST_EDITOR_CURSOR", &cursor)
        .args(["editor", "reconcile"])
        .assert()
        .failure();
    assert!(cursor_dir.path().join("installs.txt").is_file());
    mock.server.verify().await;
}

#[tokio::test]
async fn explicit_reconcile_runs_when_auto_update_disabled() {
    let vsix = VSIX_BYTES;
    let digest = sha256_hex(vsix);
    let manifest = manifest_body(&digest);
    let mock = mount_release("v0.2.2", vsix, manifest.as_bytes(), 1).await;
    let config = tempfile::tempdir().unwrap();
    seed_config(config.path());
    let user = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let code_dir = tempfile::tempdir().unwrap();
    let code = write_stub(code_dir.path(), "code", None, Some(&wanted()), false);
    stateroot(config.path(), user.path(), cwd.path())
        .env("STATEROOT_GITHUB_API_BASE", mock.server.uri())
        .env("STATEROOT_TEST_EDITOR_CODE", &code)
        .env("STATEROOT_NO_AUTO_UPDATE", "1")
        .args(["editor", "reconcile"])
        .assert()
        .success();
    assert!(code_dir.path().join("installs.txt").is_file());
    mock.server.verify().await;
}

#[tokio::test]
async fn checksum_mismatch_blocks_install() {
    let vsix = VSIX_BYTES;
    let digest = sha256_hex(b"other-bytes");
    let manifest = manifest_body(&digest);
    let mock = mount_release("v0.2.2", vsix, manifest.as_bytes(), 1).await;
    let config = tempfile::tempdir().unwrap();
    seed_config(config.path());
    let user = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let code_dir = tempfile::tempdir().unwrap();
    let code = write_stub(code_dir.path(), "code", None, Some(&wanted()), false);
    stateroot(config.path(), user.path(), cwd.path())
        .env("STATEROOT_GITHUB_API_BASE", mock.server.uri())
        .env("STATEROOT_TEST_EDITOR_CODE", &code)
        .args(["editor", "reconcile"])
        .assert()
        .failure();
    assert!(!code_dir.path().join("installs.txt").is_file());
}

#[tokio::test]
async fn missing_manifest_is_unavailable_not_version_zero() {
    let server = MockServer::start().await;
    let body = json!({
        "tag_name": "v0.2.2",
        "assets": [
            {"name": "stateroot-linux-x64", "browser_download_url": format!("{}/cli", server.uri())},
            {"name": "stateroot-windows-x64.exe", "browser_download_url": format!("{}/cli", server.uri())},
            {"name": "stateroot-macos-aarch64", "browser_download_url": format!("{}/cli", server.uri())},
            {"name": "checksums.txt", "browser_download_url": format!("{}/checksums.txt", server.uri())},
        ]
    });
    Mock::given(method("GET"))
        .and(path("/repos/stateroot-dev/stateroot/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/checksums.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("deadbeef  stateroot-linux-x64\n"))
        .mount(&server)
        .await;
    let config = tempfile::tempdir().unwrap();
    seed_config(config.path());
    let user = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let code_dir = tempfile::tempdir().unwrap();
    let code = write_stub(
        code_dir.path(),
        "code",
        Some(&wanted()),
        Some(&wanted()),
        false,
    );
    stateroot(config.path(), user.path(), cwd.path())
        .env("STATEROOT_GITHUB_API_BASE", server.uri())
        .env("STATEROOT_TEST_EDITOR_CODE", &code)
        .args(["editor", "status"])
        .assert()
        .success()
        .stdout(predicates::str::contains("unavailable"));
    assert!(!code_dir.path().join("installs.txt").is_file());
}

#[tokio::test]
async fn nightly_build_selects_nightly_assets_not_production() {
    let vsix = VSIX_BYTES;
    let digest = sha256_hex(vsix);
    let manifest = manifest_body(&digest);
    let mock = mount_release("nightly", vsix, manifest.as_bytes(), 0).await;
    Mock::given(method("GET"))
        .and(path("/repos/stateroot-dev/stateroot/releases/latest"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&mock.server)
        .await;
    let config = tempfile::tempdir().unwrap();
    seed_config(config.path());
    let user = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    stateroot(config.path(), user.path(), cwd.path())
        .env("STATEROOT_GITHUB_API_BASE", mock.server.uri())
        .env("STATEROOT_TEST_BUILD_VERSION", "0.2.2-dev.10")
        .args(["editor", "status"])
        .assert()
        .success();
    mock.server.verify().await;
}

#[tokio::test]
async fn concurrent_reconcile_downloads_vsix_once() {
    let vsix = VSIX_BYTES;
    let digest = sha256_hex(vsix);
    let manifest = manifest_body(&digest);
    let mock = mount_release("v0.2.2", vsix, manifest.as_bytes(), 1).await;
    let config = tempfile::tempdir().unwrap();
    seed_config(config.path());
    let user = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let code_dir = tempfile::tempdir().unwrap();
    let code = write_stub(code_dir.path(), "code", None, Some(&wanted()), false);
    let mut first = stateroot(config.path(), user.path(), cwd.path());
    first
        .env("STATEROOT_GITHUB_API_BASE", mock.server.uri())
        .env("STATEROOT_TEST_EDITOR_CODE", &code)
        .args(["editor", "reconcile"]);
    let mut second = stateroot(config.path(), user.path(), cwd.path());
    second
        .env("STATEROOT_GITHUB_API_BASE", mock.server.uri())
        .env("STATEROOT_TEST_EDITOR_CODE", &code)
        .args(["editor", "reconcile"]);
    let a = std::thread::spawn(move || first.assert().success());
    let b = std::thread::spawn(move || second.assert().success());
    a.join().expect("first");
    b.join().expect("second");
    mock.server.verify().await;
}

#[tokio::test]
async fn already_current_status_repairs_stale_editor() {
    let vsix = VSIX_BYTES;
    let digest = sha256_hex(vsix);
    let manifest = manifest_body(&digest);
    let mock = mount_release("v0.2.2", vsix, manifest.as_bytes(), 1).await;
    let config = tempfile::tempdir().unwrap();
    seed_config(config.path());
    let user = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    stateroot(config.path(), user.path(), project.path())
        .env("STATEROOT_NO_AUTO_UPDATE", "1")
        .arg("init")
        .assert()
        .success();
    let code_dir = tempfile::tempdir().unwrap();
    let code = write_stub(
        code_dir.path(),
        "code",
        Some(&ext_line("0.2.18")),
        Some(&wanted()),
        false,
    );
    stateroot(config.path(), user.path(), project.path())
        .env("STATEROOT_GITHUB_API_BASE", mock.server.uri())
        .env("STATEROOT_TEST_EDITOR_CODE", &code)
        .arg("status")
        .assert()
        .success();
    assert!(code_dir.path().join("installs.txt").is_file());
    mock.server.verify().await;
}

#[tokio::test]
async fn auto_update_disabled_status_does_not_mutate() {
    let vsix = VSIX_BYTES;
    let digest = sha256_hex(vsix);
    let manifest = manifest_body(&digest);
    let mock = mount_release("v0.2.2", vsix, manifest.as_bytes(), 0).await;
    let config = tempfile::tempdir().unwrap();
    seed_config(config.path());
    let user = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    stateroot(config.path(), user.path(), project.path())
        .env("STATEROOT_NO_AUTO_UPDATE", "1")
        .arg("init")
        .assert()
        .success();
    let code_dir = tempfile::tempdir().unwrap();
    let code = write_stub(
        code_dir.path(),
        "code",
        Some(&ext_line("0.2.18")),
        Some(&wanted()),
        false,
    );
    stateroot(config.path(), user.path(), project.path())
        .env("STATEROOT_GITHUB_API_BASE", mock.server.uri())
        .env("STATEROOT_TEST_EDITOR_CODE", &code)
        .env("STATEROOT_NO_AUTO_UPDATE", "1")
        .arg("status")
        .assert()
        .success();
    assert!(!code_dir.path().join("installs.txt").is_file());
}
