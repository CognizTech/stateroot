//! `stateroot login --via github` / `stateroot logout` — GitHub OAuth device
//! flow with a local credential store (keyring with file fallback).
//!
//! The owner's OAuth App registration may not exist yet: the client id is
//! config/env driven (`STATEROOT_GITHUB_CLIENT_ID` env → `[github] client_id`
//! in config.toml → documented placeholder). No client id → an honest error
//! that points at the README instead of a broken flow.
//!
//! Scope decision (documented): default `repo` (refs push needs it for
//! private repos). `public_repo` works for public-only users — set
//! `[github] scope = "public_repo"`.

use serde_json::{json, Value};

use super::{note, stdin_is_tty, Ctx};

const DEVICE_CODE_PATH: &str = "/login/device/code";
const ACCESS_TOKEN_PATH: &str = "/login/oauth/access_token";
const PLACEHOLDER: &str = "STATEROOT_GITHUB_CLIENT_ID_PLACEHOLDER";

/// Env/config resolution helpers shared with `repo`/`sync`.
pub fn client_id(ctx: &Ctx) -> Option<String> {
    if let Ok(raw) = std::env::var("STATEROOT_GITHUB_CLIENT_ID") {
        let raw = raw.trim().to_string();
        if !raw.is_empty() {
            return Some(raw);
        }
    }
    let configured = ctx.config.github.client_id.trim();
    if configured.is_empty() || configured == PLACEHOLDER {
        None
    } else {
        Some(configured.to_string())
    }
}

/// Web base (device flow endpoints): github.com, overridable for tests.
pub fn web_base() -> String {
    std::env::var("STATEROOT_GITHUB_WEB_BASE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "https://github.com".into())
}

/// REST base (api.github.com), overridable for tests.
pub fn api_base() -> String {
    std::env::var("STATEROOT_GITHUB_API_BASE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "https://api.github.com".into())
}

/// Git base for clone/push URLs, overridable for tests/local remotes.
pub fn git_base() -> String {
    std::env::var("STATEROOT_GITHUB_GIT_BASE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "https://github.com".into())
}

// ---------------------------------------------------------------------------
// credential store (keyring with file fallback — the existing pattern)
// ---------------------------------------------------------------------------

const KEYRING_SERVICE: &str = "stateroot";
const KEYRING_USER: &str = "github";

/// Resolve the stored GitHub token (keyring → file fallback), if any.
pub fn github_token(ctx: &Ctx) -> Option<String> {
    if std::env::var("STATEROOT_CREDENTIALS").ok().as_deref() != Some("file") {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
            if let Ok(token) = entry.get_password() {
                if !token.trim().is_empty() {
                    return Some(token);
                }
            }
        }
    }
    read_token_file(ctx)
}

fn read_token_file(ctx: &Ctx) -> Option<String> {
    let text = std::fs::read_to_string(ctx.config_dir.join("credentials.json")).ok()?;
    let parsed: Value = serde_json::from_str(&text).ok()?;
    parsed
        .pointer("/github/access_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

fn store_token(ctx: &Ctx, token: &str) -> anyhow::Result<()> {
    if std::env::var("STATEROOT_CREDENTIALS").ok().as_deref() != Some("file") {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
            if entry.set_password(token).is_ok() {
                return Ok(());
            }
        }
    }
    // File fallback (0600).
    let path = ctx.config_dir.join("credentials.json");
    std::fs::create_dir_all(&ctx.config_dir)?;
    let mut parsed: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(json!({}));
    parsed["github"] = json!({
        "access_token": token,
        "obtained_at": stateroot_core::local_store::now_rfc3339(),
    });
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&parsed)?),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn clear_token(ctx: &Ctx) -> anyhow::Result<()> {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
        let _ = entry.delete_credential();
    }
    let path = ctx.config_dir.join("credentials.json");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let mut parsed: Value = serde_json::from_str(&text)?;
        if let Some(obj) = parsed.as_object_mut() {
            obj.remove("github");
        }
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&parsed)?),
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// device flow
// ---------------------------------------------------------------------------

/// `stateroot login --via github`
pub async fn login(ctx: &Ctx, via: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        via == "github",
        "only --via github exists (others are out of scope)"
    );
    let Some(client_id) = client_id(ctx) else {
        anyhow::bail!(
            "no GitHub OAuth App client id configured — set STATEROOT_GITHUB_CLIENT_ID or [github] client_id in config.toml (owner registers the app; see README)"
        );
    };
    let scope = ctx.config.github.scope.trim().to_string();
    let base = web_base();
    let client = reqwest::Client::new();

    let device: Value = client
        .post(format!("{base}{DEVICE_CODE_PATH}"))
        .header("Accept", "application/json")
        .form(&[("client_id", client_id.as_str()), ("scope", scope.as_str())])
        .send()
        .await?
        .json()
        .await?;
    let user_code = device
        .get("user_code")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("device flow: response missing user_code"))?;
    let verification_uri = device
        .get("verification_uri")
        .and_then(|v| v.as_str())
        .unwrap_or("https://github.com/login/device");
    let device_code = device
        .get("device_code")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("device flow: response missing device_code"))?
        .to_string();
    let mut interval = device
        .get("interval")
        .and_then(|v| v.as_u64())
        .unwrap_or(5)
        .max(1);
    let expires_in = device
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(900);

    println!("Go to: {verification_uri}");
    println!("Enter code: {user_code}");
    if !stdin_is_tty() {
        note!("(non-interactive: approve the code in a browser on any device)");
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(expires_in);
    loop {
        if std::time::Instant::now() > deadline {
            anyhow::bail!("device code expired — run `stateroot login` again");
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        let poll: Value = client
            .post(format!("{base}{ACCESS_TOKEN_PATH}"))
            .header("Accept", "application/json")
            .form(&[
                ("client_id", client_id.as_str()),
                ("device_code", device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await?
            .json()
            .await?;
        if let Some(token) = poll.get("access_token").and_then(|v| v.as_str()) {
            store_token(ctx, token)?;
            let granted = poll.get("scope").and_then(|v| v.as_str()).unwrap_or(&scope);
            println!("logged in via github (scope: {granted})");
            return Ok(());
        }
        match poll.get("error").and_then(|v| v.as_str()).unwrap_or("") {
            "authorization_pending" => {}
            "slow_down" => interval += 5,
            "access_denied" => anyhow::bail!("authorization denied by the user"),
            "expired_token" => anyhow::bail!("device code expired — run `stateroot login` again"),
            other => anyhow::bail!("device flow error: {other}"),
        }
    }
}

/// `stateroot logout`
pub fn logout(ctx: &Ctx) -> anyhow::Result<()> {
    clear_token(ctx)?;
    println!("logged out (local github credential cleared)");
    Ok(())
}
