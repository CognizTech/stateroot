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

/// Fire a DETACHED `self-update` when the release cache is stale — the
/// automatic, agent-independent update path. Session-boundary hooks (already
/// the slow-work zone) call this: when the check interval has passed, we
/// spawn the updater in the background and return instantly. The hook never
/// blocks, no agent is asked to act, and a lock prevents concurrent workers.
pub fn maybe_spawn_scheduled_update(config_dir: &Path, interval_hours: i64) {
    // Test/CI seam: integration tests drive the real binary against a mock
    // release server and assert exact request counts — the detached worker
    // makes that nondeterministic by design.
    if std::env::var_os("STATEROOT_DISABLE_SCHEDULED_UPDATE").is_some() {
        return;
    }
    if let Ok(worker) = std::env::current_exe() {
        spawn_scheduled_update(config_dir, interval_hours, &worker);
    }
}

fn spawn_scheduled_update(config_dir: &Path, interval_hours: i64, worker: &Path) {
    // Gate 1: a fresh cache means a check already happened recently.
    let cache_fresh = std::fs::read_to_string(config_dir.join("update-check.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|cached| {
            cached
                .get("checked_at")
                .and_then(|v| v.as_str())
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                .map(|at| {
                    (chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_hours()
                        < interval_hours.max(1)
                })
        })
        .unwrap_or(false);
    if cache_fresh {
        return;
    }
    // Gate 2: one worker at a time (lock younger than an hour counts as live).
    let lock = config_dir.join("update-in-progress");
    let lock_live = std::fs::read_to_string(&lock)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|entry| {
            entry
                .get("started_at")
                .and_then(|v| v.as_str())
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                .map(|at| (chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_hours() < 1)
        })
        .unwrap_or(false);
    if lock_live {
        return;
    }
    let Ok(log) = std::fs::File::create(config_dir.join("update-scheduled.log")) else {
        return;
    };
    let Ok(log_err) = log.try_clone() else {
        return;
    };
    let spawned = std::process::Command::new(worker)
        .arg("self-update")
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_err)
        .spawn();
    if let Ok(child) = spawned {
        let entry = serde_json::json!({
            "pid": child.id(),
            "started_at": chrono::Utc::now().to_rfc3339(),
        });
        let _ = std::fs::write(&lock, format!("{entry}\n"));
    }
}

/// Digest update notice (cache-only, NEVER network): a one-liner when the
/// cached release check knows a newer tag than this binary. The background
/// auto-update refreshes the cache on its own cadence; hooks stay fast and
/// offline. Agents act on what they see — this is the periodic-update nudge.
pub(crate) fn update_notice(config_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(config_dir.join("update-check.json")).ok()?;
    let cached: Value = serde_json::from_str(&text).ok()?;
    let tag = cached.get("latest_tag").and_then(|v| v.as_str())?;
    if !is_newer(tag) {
        return None;
    }
    Some(format!(
        "**Update available: {tag} — run `stateroot self-update` (it re-arms wiring automatically).**\n\n"
    ))
}

fn api_base() -> String {
    std::env::var("STATEROOT_GITHUB_API_BASE")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://api.github.com".into())
}

// ---------------------------------------------------------------------------
// GitHub auth (unauthenticated API calls share a 60/hr per-IP rate limit —
// behind a shared egress that quota is gone in minutes; authenticated calls
// get 5000/hr). Token resolution order: GH_TOKEN env → GITHUB_TOKEN env →
// the stateroot credential store (the Phase-1 device-flow login store,
// `<config>/credentials.json`) → the gh CLI's own hosts.yml.
// ---------------------------------------------------------------------------

/// Resolve a GitHub token for release API + asset calls. `None` is fine —
/// public repos work unauthenticated, just rate-limited.
fn resolve_github_token(ctx: &Ctx) -> Option<String> {
    token_from_env()
        .or_else(|| token_from_store(ctx))
        .or_else(token_from_gh_hosts)
}

fn token_from_env() -> Option<String> {
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(var) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn token_from_store(ctx: &Ctx) -> Option<String> {
    let text = std::fs::read_to_string(ctx.config_dir.join("credentials.json")).ok()?;
    parse_store_token(&text)
}

/// Lenient reader for the login credential store: a `github` object or flat
/// keys, any of the common token field names.
fn parse_store_token(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    for obj in [value.get("github"), Some(&value)].into_iter().flatten() {
        for key in ["oauth_token", "access_token", "github_token", "token"] {
            if let Some(token) = obj
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
            {
                return Some(token.to_string());
            }
        }
    }
    None
}

fn token_from_gh_hosts() -> Option<String> {
    let home = stateroot_core::harness_install::home_dir().ok()?;
    let text = std::fs::read_to_string(home.join(".config/gh/hosts.yml")).ok()?;
    parse_gh_hosts_token(&text)
}

/// Lenient `~/.config/gh/hosts.yml` parse — line-based, tolerant of
/// indentation and quotes (it is the user's own credential file, not a
/// schema we control).
fn parse_gh_hosts_token(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("oauth_token:") {
            let token = rest.trim().trim_matches(|c| c == '"' || c == '\'');
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

/// GET against the GitHub API with the CLI's standard headers and a bearer
/// token when one resolved.
fn github_get(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
    accept: &str,
) -> reqwest::RequestBuilder {
    let mut req = client
        .get(url)
        .header("Accept", accept)
        .header("User-Agent", "stateroot-cli");
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    req
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
        // Prefer the API asset URL: downloads go through it with
        // `Accept: application/octet-stream` (+ bearer when a token
        // resolved), which keeps working when the unauthenticated rate
        // limit is exhausted. `browser_download_url` is the fallback for
        // lean fixtures that only carry the public URL.
        let url = asset
            .get("url")
            .and_then(|v| v.as_str())
            .or_else(|| asset.get("browser_download_url").and_then(|v| v.as_str()))
            .unwrap_or("");
        if name == asset_name() {
            asset_url = Some(url.to_string());
        } else if name == "checksums.txt" {
            checksums_url = Some(url.to_string());
        }
    }
    Some(ReleaseInfo {
        tag,
        name: body
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        asset_url: asset_url?,
        checksums_url: checksums_url?,
    })
}

/// One release the updater can act on.
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    /// Release tag (e.g. `v0.2.0`).
    pub tag: String,
    /// Release display name (the nightly carries the dev version here:
    /// `StateRoot 0.1.10-dev.125 (rolling preview)`).
    pub name: String,
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
                        name: String::new(),
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
    let token = resolve_github_token(ctx);
    let resp = github_get(
        &client,
        &release_api_url(repo, None),
        token.as_deref(),
        "application/vnd.github+json",
    )
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
    let token = resolve_github_token(ctx);
    let url = release_api_url(repo, Some(&tag));
    let resp = github_get(
        &client,
        &url,
        token.as_deref(),
        "application/vnd.github+json",
    )
    .send()
    .await
    .with_context(|| format!("requesting {url}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("no GitHub release tagged `{tag}`");
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body = body.trim();
        if token.is_none() && status == reqwest::StatusCode::FORBIDDEN {
            // Verified common cause: the unauthenticated 60/hr per-IP quota
            // is shared per egress IP — behind a proxy it is gone fast.
            anyhow::bail!(
                "release lookup for `{tag}` failed (HTTP {status}): GitHub API rate limit \
                 hit (60/hr unauthenticated). Set GH_TOKEN (e.g. `gh auth token`) for a \
                 higher limit, or retry after the hourly window."
            );
        }
        let detail = if body.is_empty() {
            String::new()
        } else {
            let excerpt: String = body.chars().take(300).collect();
            format!(" — {excerpt}")
        };
        anyhow::bail!("release lookup for `{tag}` failed (HTTP {status}){detail}");
    }
    let body: Value = resp.json().await.context("parsing GitHub release JSON")?;
    assets_from_body(&body).ok_or_else(|| {
        anyhow!(
            "release `{tag}` has no {asset} + checksums.txt",
            asset = asset_name()
        )
    })
}

/// True when the running binary is a dev/nightly build (`0.1.9-dev.122`).
/// Detected from BUILD_VERSION — the binary's true identity (git-describe
/// suffix); CURRENT_VERSION (CARGO_PKG_VERSION) is always plain `0.1.x` and
/// would hide the channel.
pub fn current_is_dev() -> bool {
    crate::cli::BUILD_VERSION.contains("-dev.")
}

/// Parse a dev-build version (`0.1.9-dev.122`) into (base, counter).
/// Tolerant of prefixes (`stateroot 0.1.9-dev.122`) and suffixes (release
/// names like `StateRoot 0.1.10-dev.125 (rolling preview)`): scans tokens
/// for one carrying `-dev.`.
pub fn parse_dev_version(text: &str) -> Option<((u64, u64, u64), u64)> {
    for token in text.split_whitespace() {
        let Some(pos) = token.find("-dev.") else {
            continue;
        };
        let base_text: String = token[..pos]
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .collect();
        let counter_text: String = token[pos + 5..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let (Some(base), Ok(counter)) = (parse_semver(&base_text), counter_text.parse::<u64>()) {
            return Some((base, counter));
        }
    }
    None
}

/// True when `latest` is a newer version than `current`, compared by base
/// version only. A dev build is *ahead* of its base release, so
/// `0.1.9-dev.122` vs `v0.1.9` is NOT an upgrade path (and must never
/// downgrade) — while `v0.1.10` genuinely is one.
pub fn is_newer_than(current: &str, latest: &str) -> bool {
    let current_base = parse_dev_version(current)
        .map(|(base, _)| base)
        .or_else(|| parse_semver(current));
    let latest_base = parse_semver(latest.trim_start_matches('v'));
    match (current_base, latest_base) {
        (Some(current), Some(latest)) => latest > current,
        _ => false,
    }
}

/// True when `latest` is a newer version than the running binary.
pub fn is_newer(latest: &str) -> bool {
    is_newer_than(crate::cli::BUILD_VERSION, latest)
}

/// Order a dev build against the nightly release's dev version (carried in
/// its display name). None when either side is unparseable — callers stay
/// honest and do nothing rather than guess.
pub fn dev_update_order(current: &str, release_name: &str) -> Option<std::cmp::Ordering> {
    let current = parse_dev_version(current)?;
    let nightly = parse_dev_version(release_name)?;
    Some(current.cmp(&nightly))
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
    let token = resolve_github_token(ctx);
    // API asset URLs require `Accept: application/octet-stream` to return
    // the bytes (otherwise they answer JSON metadata); the bearer token
    // keeps downloads working when the unauthenticated quota is gone.
    let asset = github_get(
        &client,
        asset_url,
        token.as_deref(),
        "application/octet-stream",
    )
    .send()
    .await?;
    if !asset.status().is_success() {
        anyhow::bail!("asset download failed (HTTP {})", asset.status());
    }
    let bytes = asset.bytes().await?;
    let checksums = github_get(
        &client,
        checksums_url,
        token.as_deref(),
        "application/octet-stream",
    )
    .send()
    .await?;
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
    download_and_install_quiet(ctx, info, false).await
}

/// [`download_and_install`] with a quiet switch for the background path
/// (no install output, failures only traced).
pub async fn download_and_install_quiet(
    ctx: &Ctx,
    info: &ReleaseInfo,
    quiet: bool,
) -> anyhow::Result<PathBuf> {
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
    rearm_install(&outcome.installed_path, quiet);
    Ok(outcome.installed_path)
}

/// A successful update re-arms harness wiring with the NEW binary: hook
/// formats, plugins, and MCP registrations go stale across versions (the
/// 0.1.1 hooks.json incident — the binary moved on, the wiring never
/// migrated). Best-effort: a failed re-arm never fails the update.
fn rearm_install(installed: &Path, quiet: bool) {
    let mut cmd = std::process::Command::new(installed);
    cmd.arg("install").env("STATEROOT_NO_AUTO_UPDATE", "1");
    let result = if quiet {
        cmd.output().map(|o| o.status.success())
    } else {
        cmd.status().map(|s| s.success())
    };
    match result {
        Ok(true) => {
            if !quiet {
                note_update("harness wiring re-armed (`stateroot install`)");
            }
        }
        _ => {
            let message = "warning: could not re-run `stateroot install` — run it manually to refresh harness wiring";
            if quiet {
                tracing::warn!("{message}");
            } else {
                note_update(message);
            }
        }
    }
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
        download_and_install_quiet(ctx, &info, true).await.ok()
    }
    .await;
    // Deliberately discarded: silent background update — every failure is
    // invisible to the user command that just ran.
    let _ = attempt;
}

/// `stateroot self-update [--check] [--tag nightly|v0.1.2]`.
///
/// Channel stickiness: plain `self-update` follows the running binary's
/// channel — a dev build tracks the rolling preview (`nightly`), a release
/// build tracks the latest production release. `--tag` always wins and
/// switches channels explicitly. A dev build is also *offered* a genuinely
/// newer production release (base-version compare), never downgraded to
/// its own base.
pub async fn self_update(ctx: &Ctx, check_only: bool, tag: Option<&str>) -> anyhow::Result<()> {
    if disabled(ctx) {
        println!("auto-update is disabled ([update] enabled = false or STATEROOT_NO_AUTO_UPDATE)");
        return Ok(());
    }
    let explicit = tag.is_some();
    let follow_nightly = !explicit && current_is_dev();
    let info = if let Some(tag) = tag {
        fetch_tagged_release(ctx, tag).await?
    } else if follow_nightly {
        fetch_tagged_release(ctx, "nightly").await?
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
    println!(
        "release:  {} ({channel}){}",
        info.tag,
        if follow_nightly {
            " — followed because this build is a dev variant"
        } else {
            ""
        }
    );
    if check_only {
        if explicit {
            println!(
                "run `stateroot self-update --tag {}` to install it",
                info.tag
            );
        } else if follow_nightly {
            match dev_update_order(current, &info.name) {
                Some(std::cmp::Ordering::Less) => println!(
                    "a newer rolling preview is available — run `stateroot self-update` to install it"
                ),
                Some(_) => println!("already ahead of the rolling preview"),
                None => println!(
                    "could not compare nightly versions — `stateroot self-update --tag nightly` forces a reinstall"
                ),
            }
        } else if is_newer(&info.tag) {
            println!("an update is available — run `stateroot self-update` to install it");
        } else {
            println!("already on the latest production release");
        }
        return Ok(());
    }
    if !explicit {
        if follow_nightly {
            match dev_update_order(current, &info.name) {
                Some(std::cmp::Ordering::Less) => {}
                Some(_) => {
                    println!("already ahead of the rolling preview");
                    return Ok(());
                }
                None => {
                    println!(
                        "could not compare nightly versions — `stateroot self-update --tag nightly` forces a reinstall"
                    );
                    return Ok(());
                }
            }
        } else if !is_newer(&info.tag) {
            println!("already on the latest production release");
            return Ok(());
        }
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
    rename_retrying(&current_exe, &parked).with_context(|| {
        format!(
            "could not rename {} → {} (is the install directory writable?)",
            current_exe.display(),
            parked.display()
        )
    })?;

    let outcome = install_and_verify(&current_exe, &new_binary, old_version);
    if let Err(err) = outcome {
        let _ = std::fs::remove_file(&current_exe);
        if let Err(rollback_err) = rename_retrying(&parked, &current_exe) {
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

/// Rename with a bounded retry on ETXTBSY/EBUSY: drvfs (WSL `/mnt/*`)
/// releases executable handles asynchronously, so renaming a just-run binary
/// can transiently fail with os error 26/16. Real product hardening for
/// self-update on WSL — and it deflakes the tests at the source.
fn rename_retrying(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(err) if matches!(err.raw_os_error(), Some(16) | Some(26)) && attempt < 5 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(50 * attempt));
            }
            Err(err) => return Err(err),
        }
    }
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
///
/// Retries ETXTBSY/EBUSY on spawn: drvfs (WSL `/mnt/*`) releases executable
/// handles asynchronously, so executing a just-written binary can transiently
/// fail with os error 26 — without this, a WSL self-update spuriously fails
/// verification and rolls back.
fn verify_binary(path: &Path) -> Result<String, String> {
    let mut attempt = 0;
    let output = loop {
        match std::process::Command::new(path)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .output()
        {
            Err(err) if matches!(err.raw_os_error(), Some(16) | Some(26)) && attempt < 5 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(50 * attempt));
            }
            other => break other.map_err(|err| format!("could not execute: {err}"))?,
        }
    };
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

    #[test]
    fn scheduled_update_gates_on_cache_freshness_and_the_lock() {
        let dir = tempfile::tempdir().expect("dir");
        let worker = Path::new("/bin/true");
        if !worker.is_file() {
            eprintln!("skipping: /bin/true unavailable");
            return;
        }
        let now = chrono::Utc::now().to_rfc3339();
        // Fresh cache → never fires.
        std::fs::write(
            dir.path().join("update-check.json"),
            format!(r#"{{"latest_tag": "v9.9.9", "checked_at": "{now}"}}"#),
        )
        .expect("cache");
        spawn_scheduled_update(dir.path(), 6, worker);
        assert!(
            !dir.path().join("update-in-progress").exists(),
            "fresh cache"
        );
        // Stale cache → fires once, writes the lock.
        std::fs::write(
            dir.path().join("update-check.json"),
            r#"{"latest_tag": "v9.9.9", "checked_at": "2020-01-01T00:00:00Z"}"#,
        )
        .expect("cache");
        spawn_scheduled_update(dir.path(), 6, worker);
        assert!(
            dir.path().join("update-in-progress").exists(),
            "stale fires"
        );
        // Live lock → no second fire (lock content unchanged).
        let before = std::fs::read_to_string(dir.path().join("update-in-progress")).expect("lock");
        spawn_scheduled_update(dir.path(), 6, worker);
        let after = std::fs::read_to_string(dir.path().join("update-in-progress")).expect("lock");
        assert_eq!(before, after, "live lock blocks respawn");
    }

    #[test]
    fn update_notice_reads_the_cache_and_stays_honest() {
        let dir = tempfile::tempdir().expect("dir");
        assert!(update_notice(dir.path()).is_none(), "no cache file");
        std::fs::write(
            dir.path().join("update-check.json"),
            r#"{"latest_tag": "v999.0.0", "checked_at": "2026-08-25T00:00:00Z"}"#,
        )
        .expect("cache");
        let notice = update_notice(dir.path()).expect("notice");
        assert!(notice.contains("v999.0.0"), "{notice}");
        assert!(notice.contains("self-update"), "{notice}");
        // Same version as this binary → no nudge.
        std::fs::write(
            dir.path().join("update-check.json"),
            format!(
                r#"{{"latest_tag": "v{}", "checked_at": "2026-08-25T00:00:00Z"}}"#,
                crate::cli::BUILD_VERSION
            ),
        )
        .expect("cache");
        assert!(
            update_notice(dir.path()).is_none(),
            "same version, no nudge"
        );
    }

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

    fn test_ctx_with_repo(dir: &Path, repo: &str) -> crate::commands::Ctx {
        let mut ctx = test_ctx(dir);
        ctx.config.update.repo = repo.to_string();
        ctx
    }

    static UPDATE_TEST_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// SAFETY for every set/remove below: serialized by UPDATE_TEST_ENV.
    fn set_env(key: &str, value: Option<&str>) -> Option<String> {
        let prior = std::env::var(key).ok();
        unsafe {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        prior
    }

    /// Process-env guard for token-resolution tests: clears GH_TOKEN /
    /// GITHUB_TOKEN and points STATEROOT_TEST_HOME at an empty temp home
    /// (so the real ~/.config/gh/hosts.yml is never read). Restores
    /// everything on drop.
    struct TokenEnv {
        priors: Vec<(&'static str, Option<String>)>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    fn clean_token_env() -> (TokenEnv, tempfile::TempDir) {
        let guard = UPDATE_TEST_ENV.lock().expect("env lock");
        let home = tempfile::tempdir().expect("home");
        let home_str = home.path().to_string_lossy().into_owned();
        let priors = vec![
            ("GH_TOKEN", set_env("GH_TOKEN", None)),
            ("GITHUB_TOKEN", set_env("GITHUB_TOKEN", None)),
            (
                "STATEROOT_TEST_HOME",
                set_env("STATEROOT_TEST_HOME", Some(&home_str)),
            ),
            // Captured, not overridden — each test points it at its mock.
            (
                "STATEROOT_GITHUB_API_BASE",
                std::env::var("STATEROOT_GITHUB_API_BASE").ok(),
            ),
        ];
        (
            TokenEnv {
                priors,
                _guard: guard,
            },
            home,
        )
    }

    impl Drop for TokenEnv {
        fn drop(&mut self) {
            for (key, prior) in self.priors.drain(..) {
                let _ = set_env(key, prior.as_deref());
            }
        }
    }

    /// Release payload carrying BOTH the API asset url and the browser url —
    /// the updater must prefer the API one (works with octet-stream + auth).
    fn release_json(server: &wiremock::MockServer, tag: &str) -> Value {
        json!({
            "tag_name": tag,
            "assets": [
                {
                    "name": asset_name(),
                    "url": format!("{}/api-asset", server.uri()),
                    "browser_download_url": format!("{}/browser-asset", server.uri()),
                },
                {
                    "name": "checksums.txt",
                    "url": format!("{}/api-checksums", server.uri()),
                    "browser_download_url": format!("{}/browser-checksums", server.uri()),
                }
            ]
        })
    }

    #[tokio::test]
    async fn checksum_mismatch_keeps_old_binary() {
        let (_env, _home) = clean_token_env();
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
        let (_env, _home) = clean_token_env();
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
    fn token_env_precedence_and_trim() {
        let (_env, _home) = clean_token_env();
        let _ = set_env("GH_TOKEN", Some("  gh-tok  "));
        let _ = set_env("GITHUB_TOKEN", Some("github-tok"));
        assert_eq!(token_from_env().as_deref(), Some("gh-tok"));
        let _ = set_env("GH_TOKEN", None);
        assert_eq!(token_from_env().as_deref(), Some("github-tok"));
    }

    #[test]
    fn store_token_parses_nested_and_flat() {
        assert_eq!(
            parse_store_token(r#"{"github":{"oauth_token":"nested-tok"}}"#).as_deref(),
            Some("nested-tok")
        );
        assert_eq!(
            parse_store_token(r#"{"github_token":"flat-tok"}"#).as_deref(),
            Some("flat-tok")
        );
        assert_eq!(parse_store_token("not json"), None);
        assert_eq!(parse_store_token(r#"{"github":{}}"#), None);
    }

    #[test]
    fn gh_hosts_token_parses_leniently() {
        let text = "github.com:\n    oauth_token: gho_abc123\n    user: octo\n";
        assert_eq!(parse_gh_hosts_token(text).as_deref(), Some("gho_abc123"));
        let quoted = "github.com:\n  oauth_token: \"quoted-tok\"\n";
        assert_eq!(parse_gh_hosts_token(quoted).as_deref(), Some("quoted-tok"));
        assert_eq!(parse_gh_hosts_token("github.com:\n  user: octo\n"), None);
    }

    #[tokio::test]
    async fn release_lookup_attaches_env_token() {
        let (_env, _home) = clean_token_env();
        let _ = set_env("GH_TOKEN", Some("env-tok"));
        let server = wiremock::MockServer::start().await;
        let _ = set_env("STATEROOT_GITHUB_API_BASE", Some(&server.uri()));
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/o/r/releases/tags/nightly"))
            .and(wiremock::matchers::header(
                "Authorization",
                "Bearer env-tok",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(release_json(&server, "nightly")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().expect("tmp");
        let ctx = test_ctx_with_repo(dir.path(), "o/r");
        let info = fetch_tagged_release(&ctx, "nightly")
            .await
            .expect("release");
        assert_eq!(info.tag, "nightly");
        // API asset URL preferred over browser_download_url.
        assert!(info.asset_url.ends_with("/api-asset"), "{}", info.asset_url);
        assert!(
            info.checksums_url.ends_with("/api-checksums"),
            "{}",
            info.checksums_url
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn release_lookup_attaches_store_token() {
        let (_env, _home) = clean_token_env();
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(
            dir.path().join("credentials.json"),
            r#"{"github":{"oauth_token":"store-tok"}}"#,
        )
        .expect("credentials");
        let server = wiremock::MockServer::start().await;
        let _ = set_env("STATEROOT_GITHUB_API_BASE", Some(&server.uri()));
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/o/r/releases/tags/nightly"))
            .and(wiremock::matchers::header(
                "Authorization",
                "Bearer store-tok",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(release_json(&server, "nightly")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let ctx = test_ctx_with_repo(dir.path(), "o/r");
        fetch_tagged_release(&ctx, "nightly")
            .await
            .expect("release");
        server.verify().await;
    }

    #[tokio::test]
    async fn release_lookup_attaches_gh_hosts_token() {
        let (_env, home) = clean_token_env();
        let gh_dir = home.path().join(".config/gh");
        std::fs::create_dir_all(&gh_dir).expect("gh dir");
        std::fs::write(
            gh_dir.join("hosts.yml"),
            "github.com:\n  oauth_token: gho_ghcli\n  user: octo\n",
        )
        .expect("hosts.yml");
        let server = wiremock::MockServer::start().await;
        let _ = set_env("STATEROOT_GITHUB_API_BASE", Some(&server.uri()));
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/o/r/releases/tags/nightly"))
            .and(wiremock::matchers::header(
                "Authorization",
                "Bearer gho_ghcli",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(release_json(&server, "nightly")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().expect("tmp");
        let ctx = test_ctx_with_repo(dir.path(), "o/r");
        fetch_tagged_release(&ctx, "nightly")
            .await
            .expect("release");
        server.verify().await;
    }

    #[tokio::test]
    async fn no_token_403_names_rate_limit_not_private() {
        let (_env, _home) = clean_token_env();
        let server = wiremock::MockServer::start().await;
        let _ = set_env("STATEROOT_GITHUB_API_BASE", Some(&server.uri()));
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/o/r/releases/tags/nightly"))
            .respond_with(
                wiremock::ResponseTemplate::new(403)
                    .set_body_string("{\"message\":\"API rate limit exceeded for 203.0.113.7.\"}"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().expect("tmp");
        let ctx = test_ctx_with_repo(dir.path(), "o/r");
        let err = fetch_tagged_release(&ctx, "nightly")
            .await
            .expect_err("403 must fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("rate limit"), "{msg}");
        assert!(msg.contains("GH_TOKEN"), "{msg}");
        assert!(!msg.contains("private"), "{msg}");
        server.verify().await;
    }

    #[tokio::test]
    async fn token_present_403_surfaces_real_status_and_body() {
        let (_env, _home) = clean_token_env();
        let _ = set_env("GH_TOKEN", Some("env-tok"));
        let server = wiremock::MockServer::start().await;
        let _ = set_env("STATEROOT_GITHUB_API_BASE", Some(&server.uri()));
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/o/r/releases/tags/nightly"))
            .respond_with(wiremock::ResponseTemplate::new(403).set_body_string(
                "{\"message\":\"You have triggered an abuse detection mechanism.\"}",
            ))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().expect("tmp");
        let ctx = test_ctx_with_repo(dir.path(), "o/r");
        let err = fetch_tagged_release(&ctx, "nightly")
            .await
            .expect_err("403 must fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("HTTP 403"), "{msg}");
        assert!(msg.contains("abuse detection"), "{msg}");
        // The no-token rate-limit hint must NOT fire with a token attached.
        assert!(!msg.contains("60/hr"), "{msg}");
        server.verify().await;
    }

    #[tokio::test]
    async fn asset_download_attaches_token_and_octet_stream() {
        use sha2::Digest as _;
        let (_env, _home) = clean_token_env();
        let _ = set_env("GH_TOKEN", Some("dl-tok"));
        let asset_bytes = b"download with auth";
        let sha = format!("{:x}", sha2::Sha256::digest(asset_bytes));
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/asset"))
            .and(wiremock::matchers::header("Authorization", "Bearer dl-tok"))
            .and(wiremock::matchers::header(
                "Accept",
                "application/octet-stream",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(asset_bytes.to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/checksums.txt"))
            .and(wiremock::matchers::header("Authorization", "Bearer dl-tok"))
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

    #[tokio::test]
    async fn public_download_without_token_still_works() {
        // Every token source cleared — the public path needs no auth and
        // must behave exactly as before.
        use sha2::Digest as _;
        let (_env, _home) = clean_token_env();
        let asset_bytes = b"public payload";
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

    #[test]
    fn parse_dev_version_reads_binary_and_release_name_forms() {
        assert_eq!(parse_dev_version("0.1.9-dev.122"), Some(((0, 1, 9), 122)));
        assert_eq!(
            parse_dev_version("stateroot 0.1.9-dev.122"),
            Some(((0, 1, 9), 122))
        );
        assert_eq!(
            parse_dev_version("StateRoot 0.1.10-dev.125 (rolling preview)"),
            Some(((0, 1, 10), 125))
        );
        assert_eq!(parse_dev_version("v0.1.10"), None);
        assert_eq!(parse_dev_version("nightly"), None);
    }

    #[test]
    fn is_newer_than_never_downgrades_dev_but_offers_newer_production() {
        // A dev build is ahead of its base release: no downgrade path.
        assert!(!is_newer_than("0.1.9-dev.122", "v0.1.9"));
        // …but a genuinely newer production release is offered.
        assert!(is_newer_than("0.1.9-dev.122", "v0.1.10"));
        assert!(is_newer_than("stateroot 0.1.9-dev.122", "v0.1.10"));
        // Production current behaves exactly as before.
        assert!(is_newer_than("0.1.9", "v0.1.10"));
        assert!(!is_newer_than("0.1.10", "v0.1.10"));
        assert!(!is_newer_than("0.1.10", "v0.1.9"));
    }

    #[test]
    fn dev_update_order_compares_base_then_counter() {
        use std::cmp::Ordering;
        // Newer base wins regardless of counter.
        assert_eq!(
            dev_update_order(
                "0.1.9-dev.122",
                "StateRoot 0.1.10-dev.125 (rolling preview)"
            ),
            Some(Ordering::Less)
        );
        // Same base: higher counter is newer.
        assert_eq!(
            dev_update_order(
                "0.1.10-dev.125",
                "StateRoot 0.1.10-dev.130 (rolling preview)"
            ),
            Some(Ordering::Less)
        );
        // Local source builds with a higher counter are never clobbered.
        assert_eq!(
            dev_update_order(
                "0.1.10-dev.999",
                "StateRoot 0.1.10-dev.125 (rolling preview)"
            ),
            Some(Ordering::Greater)
        );
        assert_eq!(
            dev_update_order(
                "0.1.10-dev.125",
                "StateRoot 0.1.10-dev.125 (rolling preview)"
            ),
            Some(Ordering::Equal)
        );
        // Unparseable sides stay honest: None, not a guess.
        assert_eq!(dev_update_order("0.1.10", "StateRoot nightly"), None);
        assert_eq!(dev_update_order("0.1.10-dev.1", "nightly"), None);
    }
}
