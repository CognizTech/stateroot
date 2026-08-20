//! Auto-update (detect → download → verify → replace) + `stateroot
//! self-update`. The self-replace mechanics are ported from the monorepo's
//! `self_update.rs` (rustup's rename-park trick): park the current exe as
//! `<name>.old`, copy the new binary in, verify, roll back on failure.
//!
//! Guarantees (owner's rules): a user command is NEVER blocked or broken by
//! the updater — the background path swallows every failure into debug
//! logs and always keeps the old binary. The updater NEVER runs on `hook`
//! or `mcp-stdio` paths (enforced in main's dispatch whitelist, tested).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _};
use serde_json::{json, Value};

use super::Ctx;

/// Current binary version (crate version at build time).
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Release asset name per platform.
pub const fn asset_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "stateroot-windows-x64.exe"
    } else if cfg!(target_os = "macos") {
        "stateroot-macos-aarch64"
    } else {
        "stateroot-linux-x64"
    }
}

fn cache_path(ctx: &Ctx) -> PathBuf {
    ctx.config_dir.join("update-check.json")
}

fn api_base() -> String {
    std::env::var("STATEROOT_GITHUB_API_BASE")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://api.github.com".into())
}

/// True when auto-update is disabled (config off or env opt-out).
pub fn disabled(ctx: &Ctx) -> bool {
    if std::env::var("STATEROOT_NO_AUTO_UPDATE")
        .ok()
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .unwrap_or(false)
    {
        return true;
    }
    !ctx.config.update.enabled
}

/// Parse `v1.2.3` / `1.2.3` into a comparable tuple (None when not semver).
pub fn parse_semver(tag: &str) -> Option<(u64, u64, u64)> {
    let trimmed = tag.trim().trim_start_matches('v');
    let mut parts = trimmed.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// Normalize a user-supplied release tag.
///
/// `nightly` (any case) is the rolling preview. A three-part semver with or
/// without a `v` prefix becomes `vMAJOR.MINOR.PATCH`. Other names are kept
/// as typed (trimmed) so unusual tags still resolve.
pub fn normalize_release_tag(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("nightly") {
        return "nightly".into();
    }
    if let Some((major, minor, patch)) = parse_semver(trimmed) {
        return format!("v{major}.{minor}.{patch}");
    }
    trimmed.to_string()
}

/// Rolling preview GitHub tag (`nightly`).
pub fn is_rolling_preview_tag(tag: &str) -> bool {
    tag.eq_ignore_ascii_case("nightly")
}

fn release_api_url(repo: &str, tag: Option<&str>) -> String {
    match tag {
        None => format!("{}/repos/{repo}/releases/latest", api_base()),
        Some(tag) => format!("{}/repos/{repo}/releases/tags/{tag}", api_base()),
    }
}

fn assets_from_body(body: &Value) -> Option<ReleaseInfo> {
    let tag = body.get("tag_name").and_then(|v| v.as_str())?.to_string();
    let assets = body.get("assets").and_then(|v| v.as_array())?;
    let mut asset_url = None;
    let mut checksums_url = None;
    for asset in assets {
        let name = asset.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let url = asset
            .get("browser_download_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if name == asset_name() {
            asset_url = Some(url.to_string());
        } else if name == "checksums.txt" {
            checksums_url = Some(url.to_string());
        }
    }
    Some(ReleaseInfo {
        tag,
        asset_url: asset_url?,
        checksums_url: checksums_url?,
    })
}

/// One release the updater can act on.
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    /// Release tag (e.g. `v0.2.0`).
    pub tag: String,
    /// Download URL of the platform asset.
    pub asset_url: String,
    /// Download URL of `checksums.txt`.
    pub checksums_url: String,
}

/// Check for a newer release. Background checks honor the cache (at most one
/// network call per `check_interval_hours`); an explicit caller can force a
/// fresh lookup. Returns None on any failure or when the repo is a placeholder.
pub async fn check_latest(ctx: &Ctx, force: bool) -> Option<ReleaseInfo> {
    let repo = ctx.config.update.repo.trim();
    if repo.is_empty() || repo.contains("OWNER") || repo.contains("placeholder") {
        // Placeholder default: no public repo yet — honest silence.
        return None;
    }
    let interval = ctx.config.update.check_interval_hours.max(1);
    if !force {
        if let Ok(text) = std::fs::read_to_string(cache_path(ctx)) {
            if let Ok(cached) = serde_json::from_str::<Value>(&text) {
                let fresh = cached
                    .get("checked_at")
                    .and_then(|v| v.as_str())
                    .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                    .map(|at| {
                        (chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_hours() < interval
                    })
                    .unwrap_or(false);
                if fresh {
                    let tag = cached.get("latest_tag").and_then(|v| v.as_str())?;
                    let asset_url = cached.get("asset_url").and_then(|v| v.as_str())?;
                    let checksums_url = cached.get("checksums_url").and_then(|v| v.as_str())?;
                    return Some(ReleaseInfo {
                        tag: tag.into(),
                        asset_url: asset_url.into(),
                        checksums_url: checksums_url.into(),
                    });
                }
            }
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;
    let resp = client
        .get(release_api_url(repo, None))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "stateroot-cli")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    let info = assets_from_body(&body)?;
    // Always record the check timestamp so a missing platform asset does not
    // re-hit the network on every invocation within the interval.
    let _ = std::fs::create_dir_all(&ctx.config_dir);
    let _ = std::fs::write(
        cache_path(ctx),
        serde_json::to_string_pretty(&json!({
            "checked_at": stateroot_core::local_store::now_rfc3339(),
            "latest_tag": info.tag,
            "asset_url": info.asset_url,
            "checksums_url": info.checksums_url,
        }))
        .ok()?,
    );
    Some(info)
}

/// Fetch one GitHub release by tag (`nightly`, `v0.1.2`, …). Never writes the
/// production `update-check.json` cache.
pub async fn fetch_tagged_release(ctx: &Ctx, tag: &str) -> anyhow::Result<ReleaseInfo> {
    let repo = ctx.config.update.repo.trim();
    if repo.is_empty() || repo.contains("OWNER") || repo.contains("placeholder") {
        anyhow::bail!("could not check for updates (no public release repo configured yet)");
    }
    let tag = normalize_release_tag(tag);
    if tag.is_empty() {
        anyhow::bail!("release tag must not be empty");
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()?;
    let url = release_api_url(repo, Some(&tag));
    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "stateroot-cli")
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("no GitHub release tagged `{tag}`");
    }
    if !resp.status().is_success() {
        anyhow::bail!("release lookup for `{tag}` failed (HTTP {})", resp.status());
    }
    let body: Value = resp.json().await.context("parsing GitHub release JSON")?;
    assets_from_body(&body).ok_or_else(|| {
        anyhow!(
            "release `{tag}` has no {asset} + checksums.txt",
            asset = asset_name()
        )
    })
}

/// True when `latest` is a newer version than the running binary.
pub fn is_newer(latest: &str) -> bool {
    match (parse_semver(latest), parse_semver(CURRENT_VERSION)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// Download the asset + checksums.txt and verify the sha256. Returns the
/// verified temp file path; on failure nothing is written to the install
/// path (callers keep the old binary).
pub async fn download_verified(
    ctx: &Ctx,
    asset_url: &str,
    checksums_url: &str,
) -> anyhow::Result<PathBuf> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let asset = client.get(asset_url).send().await?;
    if !asset.status().is_success() {
        anyhow::bail!("asset download failed (HTTP {})", asset.status());
    }
    let bytes = asset.bytes().await?;
    let checksums = client.get(checksums_url).send().await?;
    if !checksums.status().is_success() {
        anyhow::bail!(
            "checksums.txt download failed (HTTP {})",
            checksums.status()
        );
    }
    let checksums_text = checksums.text().await?;
    let expected = checksums_text
        .lines()
        .find(|line| line.contains(asset_name()))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| anyhow!("checksums.txt has no entry for {}", asset_name()))?
        .to_string();
    use sha2::Digest as _;
    let actual = format!("{:x}", sha2::Sha256::digest(&bytes));
    anyhow::ensure!(
        actual == expected,
        "checksum mismatch for {} (expected {}, got {})",
        asset_name(),
        expected,
        actual
    );
    let tmp = ctx
        .config_dir
        .join(format!("update-download-{}", std::process::id()));
    std::fs::write(&tmp, &bytes)?;
    Ok(tmp)
}

/// Download the asset + checksums.txt, verify the sha256, and self-replace.
/// On ANY failure the old binary stays in place (the self-replace rolls
/// back internally; download failures never touch the install).
pub async fn download_and_install(ctx: &Ctx, info: &ReleaseInfo) -> anyhow::Result<PathBuf> {
    let tmp = download_verified(ctx, &info.asset_url, &info.checksums_url).await?;
    let current_exe = std::env::current_exe().context("resolving current exe")?;
    let outcome = self_replace(&current_exe, &tmp)?;
    let _ = std::fs::remove_file(&tmp);
    let detail = match &outcome.old_version {
        Some(old) if old != &outcome.new_version => format!("{old} → {}", outcome.new_version),
        _ => outcome.new_version.clone(),
    };
    note_update(&format!(
        "updated to {} ({detail}) at {}",
        info.tag,
        outcome.installed_path.display()
    ));
    Ok(outcome.installed_path)
}

fn note_update(message: &str) {
    println!("stateroot self-update: {message}");
}

/// Background entry point (post-dispatch, whitelisted commands only):
/// silent on every failure; never blocks past short timeouts.
pub async fn maybe_auto_update(ctx: &Ctx) {
    if disabled(ctx) {
        return;
    }
    let attempt = async {
        let info = check_latest(ctx, false).await?;
        if !is_newer(&info.tag) {
            return None;
        }
        download_and_install(ctx, &info).await.ok()
    }
    .await;
    // Deliberately discarded: silent background update — every failure is
    // invisible to the user command that just ran.
    let _ = attempt;
}

/// `stateroot self-update [--check] [--tag nightly|v0.1.2]`.
///
/// Omit `--tag` to follow the latest production release (`/releases/latest`).
/// Pass `--tag nightly` for the rolling preview, or a production tag to
/// install/downgrade that exact release. Background auto-update never uses
/// `--tag` and never follows `nightly`.
pub async fn self_update(ctx: &Ctx, check_only: bool, tag: Option<&str>) -> anyhow::Result<()> {
    if disabled(ctx) {
        println!("auto-update is disabled ([update] enabled = false or STATEROOT_NO_AUTO_UPDATE)");
        return Ok(());
    }
    let explicit = tag.is_some();
    let info = if let Some(tag) = tag {
        fetch_tagged_release(ctx, tag).await?
    } else {
        // A user explicitly asked to check or update. Never report stale
        // cached release metadata as the current production release.
        match check_latest(ctx, true).await {
            Some(info) => info,
            None => {
                println!("could not check for updates (no public release repo configured yet)");
                return Ok(());
            }
        }
    };
    let current = crate::cli::BUILD_VERSION;
    let channel = if is_rolling_preview_tag(&info.tag) {
        "rolling preview"
    } else {
        "production"
    };
    println!("current:  {current}");
    println!("release:  {} ({channel})", info.tag);
    if check_only {
        if explicit {
            println!(
                "run `stateroot self-update --tag {}` to install it",
                info.tag
            );
        } else if is_newer(&info.tag) {
            println!("an update is available — run `stateroot self-update` to install it");
        } else {
            println!("already on the latest production release");
        }
        return Ok(());
    }
    if !explicit && !is_newer(&info.tag) {
        println!("already on the latest production release");
        return Ok(());
    }
    download_and_install(ctx, &info).await.map(|_| ())
}

// ---------------------------------------------------------------------------
// self-replace (ported from the monorepo's self_update.rs — rename-park)
// ---------------------------------------------------------------------------

/// What a successful self-update did.
#[derive(Debug)]
pub struct SelfUpdateOutcome {
    /// Version line of the replaced binary (when it answered `--version`).
    pub old_version: Option<String>,
    /// Version line of the freshly installed binary.
    pub new_version: String,
    /// Where the new binary now lives (the original exe path).
    pub installed_path: PathBuf,
}

/// Replace `current_exe` with `new_binary`, rolling back on any failure.
pub fn self_replace(current_exe: &Path, new_binary: &Path) -> anyhow::Result<SelfUpdateOutcome> {
    let current_exe = std::fs::canonicalize(current_exe)
        .with_context(|| format!("resolving {}", current_exe.display()))?;
    let new_binary = std::fs::canonicalize(new_binary)
        .with_context(|| format!("resolving {}", new_binary.display()))?;
    if current_exe == new_binary {
        anyhow::bail!("binary resolves to the current executable itself — nothing to do");
    }
    let dir = current_exe
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", current_exe.display()))?;
    let file_name = current_exe
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("{}: non-UTF8 file name", current_exe.display()))?;
    let stem = current_exe
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or(file_name);

    // Cleanup pass: delete stale `<stem>*.old*` siblings from previous runs.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with(stem) && name.contains(".old") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    let old_version = version_of(&current_exe);
    let parked = park_target(dir, file_name);
    std::fs::rename(&current_exe, &parked).with_context(|| {
        format!(
            "could not rename {} → {} (is the install directory writable?)",
            current_exe.display(),
            parked.display()
        )
    })?;

    let outcome = install_and_verify(&current_exe, &new_binary, old_version);
    if let Err(err) = outcome {
        let _ = std::fs::remove_file(&current_exe);
        if let Err(rollback_err) = std::fs::rename(&parked, &current_exe) {
            return Err(anyhow!(
                "self-update failed ({err:#}) AND rollback failed ({rollback_err:#}) — \
                 the previous binary is parked at {}; restore it manually",
                parked.display()
            ));
        }
        return Err(anyhow!(
            "self-update failed, previous binary restored: {err:#}"
        ));
    }
    let mut outcome = outcome?;
    outcome.installed_path = current_exe;
    let _ = std::fs::remove_file(&parked);
    Ok(outcome)
}

/// Where to park the old exe: `<file-name>.old`, falling back to a
/// timestamped variant when that name exists and can't be removed.
fn park_target(dir: &Path, file_name: &str) -> PathBuf {
    let plain = dir.join(format!("{file_name}.old"));
    if !plain.exists() || std::fs::remove_file(&plain).is_ok() {
        return plain;
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    dir.join(format!("{file_name}.old-{secs}"))
}

fn install_and_verify(
    installed: &Path,
    new_binary: &Path,
    old_version: Option<String>,
) -> anyhow::Result<SelfUpdateOutcome> {
    {
        let mut source = std::fs::File::open(new_binary)
            .with_context(|| format!("opening {}", new_binary.display()))?;
        let mut target = std::fs::File::create(installed)
            .with_context(|| format!("creating {}", installed.display()))?;
        std::io::copy(&mut source, &mut target)
            .with_context(|| format!("copying into {}", installed.display()))?;
        use std::io::Write as _;
        target.flush()?;
    }
    set_executable_if_unix(installed, new_binary)?;
    let new_version = verify_binary(installed).map_err(|err| {
        anyhow!(
            "installed binary failed verification ({err}) — {}",
            installed.display()
        )
    })?;
    Ok(SelfUpdateOutcome {
        old_version,
        new_version,
        installed_path: installed.to_path_buf(),
    })
}

#[cfg(unix)]
fn set_executable_if_unix(installed: &Path, source: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(source)
        .map(|m| m.permissions().mode())
        // HTTP downloads are materialized as regular files (normally 0644),
        // even though their release asset is an executable. Preserve the
        // source permissions while always making the replacement runnable.
        .unwrap_or(0o755)
        | 0o111;
    std::fs::set_permissions(installed, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable_if_unix(_installed: &Path, _source: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// Run `path --version`; Ok(first stdout line) on exit 0.
fn verify_binary(path: &Path) -> Result<String, String> {
    let output = std::process::Command::new(path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| format!("could not execute: {err}"))?;
    if !output.status.success() {
        return Err(format!("`--version` exited with {}", output.status));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().next().unwrap_or("").trim().to_string())
}

/// Version line of a runnable binary (None when it can't answer).
fn version_of(path: &Path) -> Option<String> {
    let version = verify_binary(path).ok();
    version.filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_binary(dir: &Path, name: &str) -> Option<PathBuf> {
        let source = Path::new("/bin/true");
        if !source.is_file() {
            return None;
        }
        let path = dir.join(name);
        std::fs::copy(source, &path).expect("stub");
        Some(path)
    }

    #[test]
    fn self_replace_happy_path_replaces_and_cleans_stale() {
        let dir = tempfile::tempdir().expect("tmp");
        let installed = dir.path().join("stateroot.exe");
        std::fs::write(&installed, b"OLD-CONTENT").expect("old exe");
        let stale = dir.path().join("stateroot.exe.old-prev");
        std::fs::write(&stale, b"stale").expect("stale");
        let Some(new_binary) = stub_binary(dir.path(), "new-stateroot.exe") else {
            eprintln!("skipping: /bin/true unavailable");
            return;
        };
        let outcome = self_replace(&installed, &new_binary).expect("self_replace");
        assert_eq!(
            std::fs::read(&installed).expect("installed"),
            std::fs::read(&new_binary).expect("source")
        );
        assert!(!dir.path().join("stateroot.exe.old").exists());
        assert!(!stale.exists());
        let _ = outcome;
    }

    #[cfg(unix)]
    #[test]
    fn self_replace_makes_a_downloaded_non_executable_asset_runnable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tmp");
        let installed = dir.path().join("stateroot");
        std::fs::write(&installed, b"OLD-CONTENT").expect("old exe");
        let Some(new_binary) = stub_binary(dir.path(), "update-download") else {
            eprintln!("skipping: /bin/true unavailable");
            return;
        };
        std::fs::set_permissions(&new_binary, std::fs::Permissions::from_mode(0o644))
            .expect("make download non-executable");

        self_replace(&installed, &new_binary).expect("self_replace");

        let mode = std::fs::metadata(&installed)
            .expect("installed metadata")
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "installed binary must be executable");
        verify_binary(&installed).expect("installed binary must run");
    }

    #[test]
    fn self_replace_bogus_binary_rolls_back() {
        let dir = tempfile::tempdir().expect("tmp");
        let installed = dir.path().join("stateroot.exe");
        std::fs::write(&installed, b"OLD-CONTENT").expect("old exe");
        let bogus = dir.path().join("bogus.exe");
        std::fs::write(&bogus, b"definitely not an executable").expect("bogus");
        let err = self_replace(&installed, &bogus).expect_err("must fail");
        assert!(format!("{err:#}").contains("previous binary restored"));
        assert_eq!(
            std::fs::read(&installed).expect("installed"),
            b"OLD-CONTENT"
        );
        assert!(!dir.path().join("stateroot.exe.old").exists());
    }

    #[test]
    fn self_replace_same_path_refusal() {
        let dir = tempfile::tempdir().expect("tmp");
        let installed = dir.path().join("stateroot.exe");
        std::fs::write(&installed, b"OLD-CONTENT").expect("old exe");
        let err = self_replace(&installed, &installed).expect_err("must refuse");
        assert!(format!("{err:#}").contains("current executable itself"));
        assert_eq!(
            std::fs::read(&installed).expect("installed"),
            b"OLD-CONTENT"
        );
    }

    fn test_ctx(dir: &Path) -> crate::commands::Ctx {
        crate::commands::Ctx {
            cwd: dir.to_path_buf(),
            config_dir: dir.to_path_buf(),
            config: stateroot_core::config::AppConfig::default(),
        }
    }

    #[tokio::test]
    async fn checksum_mismatch_keeps_old_binary() {
        let server = wiremock::MockServer::start().await;
        let asset_bytes = b"new binary content";
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/asset"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(asset_bytes.to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/checksums.txt"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_string(format!(
                    "0000000000000000000000000000000000000000000000000000000000000000  {}
",
                    asset_name()
                )),
            )
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().expect("tmp");
        let ctx = test_ctx(dir.path());
        let err = download_verified(
            &ctx,
            &format!("{}/asset", server.uri()),
            &format!("{}/checksums.txt", server.uri()),
        )
        .await
        .expect_err("checksum mismatch must fail");
        assert!(format!("{err:#}").contains("checksum mismatch"), "{err:#}");
        assert!(
            !dir.path()
                .join(format!("update-download-{}", std::process::id()))
                .exists(),
            "no partial download left behind"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn correct_checksum_passes_verification() {
        use sha2::Digest as _;
        let asset_bytes = b"verified binary payload";
        let sha = format!("{:x}", sha2::Sha256::digest(asset_bytes));
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/asset"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(asset_bytes.to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/checksums.txt"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_string(format!(
                    "{sha}  {}
",
                    asset_name()
                )),
            )
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().expect("tmp");
        let ctx = test_ctx(dir.path());
        let path = download_verified(
            &ctx,
            &format!("{}/asset", server.uri()),
            &format!("{}/checksums.txt", server.uri()),
        )
        .await
        .expect("verified download");
        assert_eq!(std::fs::read(&path).expect("tmp"), asset_bytes);
        server.verify().await;
    }

    #[test]
    fn semver_parse_and_compare() {
        assert_eq!(parse_semver("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_semver("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("nightly"), None);
        assert!(is_newer(&format!(
            "v{}.{}.{}",
            env!("CARGO_PKG_VERSION_MAJOR"),
            env!("CARGO_PKG_VERSION_MINOR"),
            99
        )));
        assert!(!is_newer("v0.0.1"));
    }

    #[test]
    fn normalize_release_tag_maps_channels() {
        assert_eq!(normalize_release_tag("nightly"), "nightly");
        assert_eq!(normalize_release_tag("Nightly"), "nightly");
        assert_eq!(normalize_release_tag("v0.1.2"), "v0.1.2");
        assert_eq!(normalize_release_tag("0.1.2"), "v0.1.2");
        assert_eq!(normalize_release_tag("  v1.0.0  "), "v1.0.0");
        assert!(is_rolling_preview_tag("nightly"));
        assert!(!is_rolling_preview_tag("v0.1.2"));
    }
}
