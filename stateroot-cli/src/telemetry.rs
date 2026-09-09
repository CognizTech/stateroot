//! First-run install/update telemetry: one anonymous GET when the binary's
//! version differs from the last seen one. This is the channel-agnostic floor
//! — installers, extension flows, direct binary copies, and manual updates all
//! end with someone *running* the CLI, so the CLI itself is the only place
//! that counts every path.
//!
//! Contract: detached (never blocks the command), 3s cap, every error
//! swallowed, `STATEROOT_NO_PING=1` opts out, and development builds
//! (`-dev.*`) never ping. One attempt per version change per machine: the
//! marker is written before firing, so an offline machine is not retried —
//! the floor is installs that happened and could reach us, never an exact
//! census.

use std::path::{Path, PathBuf};

/// Default ping endpoint (override in tests via STATEROOT_PING_URL).
const PING_URL: &str = "https://stateroot.dev/api/install-ping";
const MARKER_NAME: &str = "last_seen_version";

fn marker_path(config_dir: &Path) -> PathBuf {
    config_dir.join("local").join(MARKER_NAME)
}

fn os_target() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows-x64"
    } else if cfg!(target_os = "macos") {
        "macos-aarch64"
    } else {
        "linux-x64"
    }
}

fn ping_url(version: &str, kind: &str, from: &str) -> String {
    let base = std::env::var("STATEROOT_PING_URL").unwrap_or_else(|_| PING_URL.to_string());
    let mut url = format!("{base}?os={}&v={version}&via=cli&kind={kind}", os_target());
    if !from.is_empty() {
        url.push_str(&format!("&from={from}"));
    }
    url
}

/// Fire one fail-silent ping when `version` differs from the marker.
/// The returned task must be awaited (bounded by the client's 3s timeout) —
/// otherwise a fast command like `--version` would exit before the ping
/// lands, and it is the most common first command after a manual install.
pub fn maybe_ping(config_dir: &Path, version: &str) -> Option<tokio::task::JoinHandle<()>> {
    if std::env::var_os("STATEROOT_NO_PING").is_some() || version.contains("-dev.") {
        return None;
    }
    let marker = marker_path(config_dir);
    let last = std::fs::read_to_string(&marker)
        .ok()
        .map(|s| s.trim().to_string());
    if last.as_deref() == Some(version) {
        return None;
    }
    let kind = if last.is_none() { "install" } else { "update" };
    let from = last.unwrap_or_default();
    // Record before firing: one attempt per version change, even offline.
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&marker, format!("{version}\n"));
    let url = ping_url(version, kind, &from);
    Some(tokio::spawn(async move {
        if let Ok(client) = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
        {
            let _ = client.get(url).send().await;
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// One-shot HTTP sink: accepts a single request and yields its target
    /// (`/path?query`). No framework, no async, no real network.
    fn one_shot_server() -> (String, std::sync::mpsc::Receiver<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                use std::io::Read as _;
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let target = request
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("")
                    .to_string();
                use std::io::Write as _;
                let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
                let _ = tx.send(target);
            }
        });
        (addr, rx)
    }

    #[tokio::test]
    async fn install_pings_once_with_install_kind_and_records_marker() {
        let (url, rx) = one_shot_server();
        let dir = tempfile::tempdir().unwrap();
        // The ping URL is captured synchronously inside maybe_ping, so the
        // guards only need to live for the call — never across the await.
        let handle = {
            let _lock = ENV_LOCK.lock().unwrap();
            let _url = EnvGuard::set("STATEROOT_PING_URL", &url);
            maybe_ping(dir.path(), "9.9.9")
        };
        handle.expect("install pings").await.unwrap();
        let target = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("one request");
        assert!(target.contains("v=9.9.9"), "{target}");
        assert!(target.contains("via=cli"), "{target}");
        assert!(target.contains("kind=install"), "{target}");
        assert!(target.contains(&format!("os={}", os_target())), "{target}");
        assert_eq!(
            std::fs::read_to_string(marker_path(dir.path()))
                .unwrap()
                .trim(),
            "9.9.9"
        );

        // Same version again: silent.
        assert!(maybe_ping(dir.path(), "9.9.9").is_none());
        assert!(rx
            .recv_timeout(std::time::Duration::from_millis(300))
            .is_err());
    }

    #[tokio::test]
    async fn update_pings_with_update_kind_and_from() {
        let (url, rx) = one_shot_server();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("local")).unwrap();
        std::fs::write(marker_path(dir.path()), "0.1.0\n").unwrap();
        let handle = {
            let _lock = ENV_LOCK.lock().unwrap();
            let _url = EnvGuard::set("STATEROOT_PING_URL", &url);
            maybe_ping(dir.path(), "0.2.0")
        };
        handle.expect("update pings").await.unwrap();
        let target = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("one request");
        assert!(target.contains("kind=update"), "{target}");
        assert!(target.contains("from=0.1.0"), "{target}");
        assert!(target.contains("v=0.2.0"), "{target}");
    }

    #[test]
    fn opt_out_and_dev_builds_never_ping() {
        let _lock = ENV_LOCK.lock().unwrap();
        let (url, rx) = one_shot_server();
        let _url = EnvGuard::set("STATEROOT_PING_URL", &url);
        let dir = tempfile::tempdir().unwrap();

        let no_ping = EnvGuard::set("STATEROOT_NO_PING", "1");
        assert!(maybe_ping(dir.path(), "9.9.9").is_none());
        drop(no_ping);
        assert!(maybe_ping(dir.path(), "9.9.9-dev.local").is_none());
        assert!(maybe_ping(dir.path(), "9.9.9-dev.171").is_none());
        assert!(rx
            .recv_timeout(std::time::Duration::from_millis(300))
            .is_err());
    }

    #[tokio::test]
    async fn marker_is_recorded_even_when_the_endpoint_fails() {
        let dir = tempfile::tempdir().unwrap();
        // Nothing listens here — the ping fails, the marker still lands.
        let handle = {
            let _lock = ENV_LOCK.lock().unwrap();
            let _url = EnvGuard::set("STATEROOT_PING_URL", "http://127.0.0.1:9");
            maybe_ping(dir.path(), "9.9.9")
        };
        if let Some(handle) = handle {
            handle.await.unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(marker_path(dir.path()))
                .unwrap()
                .trim(),
            "9.9.9"
        );
    }
}
