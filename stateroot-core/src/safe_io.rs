//! Checked crash/concurrency primitives (repair plan Phase 1).
//!
//! Three guarantees every lifecycle path builds on:
//! - `atomic_replace`: a file is never observed torn — unique temp, flush,
//!   replace (Windows-safe), parent sync where supported, errors propagated.
//! - `ResourceLock`: a MANDATORY, ownership-tokened lock with stale-owner
//!   recovery. Acquisition failure is an ERROR — no path silently continues
//!   unlocked.
//! - `update_ref_cas`: expected-old-OID ref update under the ref's own
//!   resource lock, so a moved lineage tip is a retryable conflict, never a
//!   silent last-writer-wins. Granularity is per-ref: independent fork refs
//!   never serialize against each other.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Replace `path` with `bytes` atomically (checked): temp in the same
/// directory, flush+sync, replace, sync the parent directory. On failure the
/// temp is removed best-effort and the error propagates — a reader never
/// sees a half-written file after a reported success.
pub fn atomic_replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent"))?;
    fs::create_dir_all(parent)?;
    let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        seq
    ));
    let result = (|| -> std::io::Result<()> {
        {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        // Windows cannot rename over an existing destination; POSIX can.
        if cfg!(windows) && path.exists() {
            fs::remove_file(path)?;
        }
        fs::rename(&tmp, path)?;
        // Durability of the rename itself: sync the parent where supported.
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// JSON convenience wrapper around [`atomic_replace`].
pub fn atomic_replace_json(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    atomic_replace(path, format!("{text}\n").as_bytes())
}

/// A held resource lock; the lock file is removed on drop.
#[derive(Debug)]
pub struct ResourceLock {
    path: PathBuf,
}

/// Lock acquisition failure (fail-closed by contract).
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// The budget expired while another live owner holds the lock.
    #[error("resource lock {path} held by live owner (pid {owner_pid}) after {budget_ms}ms")]
    Timeout {
        /// Lock file path.
        path: PathBuf,
        /// Owning pid as recorded.
        owner_pid: u32,
        /// Wait budget used.
        budget_ms: u64,
    },
    /// Filesystem failure.
    #[error("io error on resource lock: {0}")]
    Io(#[from] std::io::Error),
}

impl ResourceLock {
    /// Acquire `path` exclusively, spinning 40×15ms (600ms). A lock file
    /// whose recorded owner pid is dead is reclaimed (stale recovery) —
    /// a crash never wedges the resource. Timeout is an ERROR.
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self, LockError> {
        Self::acquire_with_budget(path, 40, 15)
    }

    /// Budget-tunable variant (tests use short budgets).
    pub fn acquire_with_budget(
        path: impl AsRef<Path>,
        tries: u32,
        backoff_ms: u64,
    ) -> Result<Self, LockError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut last_owner: Option<serde_json::Value> = None;
        for attempt in 0..tries.max(1) {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    let owner = serde_json::json!({
                        "pid": std::process::id(),
                        "acquired_at": crate::local_store::now_rfc3339(),
                    });
                    let _ = file.write_all(owner.to_string().as_bytes());
                    return Ok(Self { path });
                }
                Err(_) => {
                    if attempt % 8 == 0 {
                        // Periodic stale-owner check (cheap: one read+probe).
                        if let Ok(text) = fs::read_to_string(&path) {
                            let owner: Option<serde_json::Value> = serde_json::from_str(&text).ok();
                            last_owner = owner.clone();
                            let pid = owner
                                .as_ref()
                                .and_then(|o| o.get("pid"))
                                .and_then(|p| p.as_u64())
                                .unwrap_or(0) as u32;
                            if pid != 0 && !pid_alive(pid) {
                                // Stale owner: the process is gone — reclaim.
                                let _ = fs::remove_file(&path);
                                continue;
                            }
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                }
            }
        }
        let owner_pid = last_owner
            .as_ref()
            .and_then(|o| o.get("pid"))
            .and_then(|p| p.as_u64())
            .unwrap_or(0) as u32;
        Err(LockError::Timeout {
            path,
            owner_pid,
            budget_ms: u64::from(tries) * backoff_ms,
        })
    }
}

impl Drop for ResourceLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            out.status.success() && !text.contains("No tasks are running")
        })
        .unwrap_or(false)
}

/// A ref move detected between expectation and update.
#[derive(Debug, thiserror::Error)]
pub enum RefCasError {
    /// The ref's current tip differs from the expected one.
    #[error("ref {refname} moved: expected {expected}, found {current}")]
    Moved {
        /// Ref name.
        refname: String,
        /// Tip the caller based its work on (`<none>` = expected absent).
        expected: String,
        /// Tip actually present (`<none>` = currently absent).
        current: String,
    },
    /// The resource lock could not be acquired.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// git2 plumbing failure.
    #[error(transparent)]
    Git(#[from] git2::Error),
}

/// The resource-lock path for a lineage ref (shared by every writer so a
/// ref's whole write span — per-hash ref, tip check, tip write — serializes
/// under ONE held lock, not just the final reference call).
pub fn ref_lock_path(lock_dir: &Path, refname: &str) -> PathBuf {
    let sanitized: String = refname
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    lock_dir.join(format!("root-ref-{sanitized}.lock"))
}

/// Compare-and-swap a ref under its own resource lock
/// (`<lock_dir>/root-ref-<sanitized refname>.lock`): inside the lock, the
/// current tip must equal `expected` (None = ref must be absent) or the
/// update is rejected as retryable-conflict. Independent refs take
/// independent locks — fork refs never serialize against each other.
pub fn update_ref_cas(
    repo: &git2::Repository,
    lock_dir: &Path,
    refname: &str,
    expected: Option<git2::Oid>,
    new_oid: git2::Oid,
    log_message: &str,
) -> Result<(), RefCasError> {
    let _lock = ResourceLock::acquire(ref_lock_path(lock_dir, refname))?;
    let current = repo.refname_to_id(refname).ok();
    if current != expected {
        return Err(RefCasError::Moved {
            refname: refname.to_string(),
            expected: expected
                .map(|o| o.to_string())
                .unwrap_or_else(|| "<none>".into()),
            current: current
                .map(|o| o.to_string())
                .unwrap_or_else(|| "<none>".into()),
        });
    }
    repo.reference(refname, new_oid, true, log_message)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_replace_lands_content_and_leaves_no_temp() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("f.json");
        atomic_replace(&path, b"one").expect("first");
        atomic_replace(&path, b"two").expect("second");
        assert_eq!(fs::read_to_string(&path).expect("read"), "two");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .expect("dir")
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files leaked: {leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_replace_failure_keeps_the_original() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tmp");
        let sub = dir.path().join("ro");
        fs::create_dir(&sub).expect("sub");
        let path = sub.join("f.json");
        atomic_replace(&path, b"original").expect("seed");
        let mut perms = sub.metadata().expect("meta").permissions();
        perms.set_mode(0o555);
        fs::set_permissions(&sub, perms).expect("ro");
        let err = atomic_replace(&path, b"new").expect_err("must fail in a read-only dir");
        let _ = err;
        let mut perms = sub.metadata().expect("meta").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&sub, perms).expect("rw");
        assert_eq!(fs::read_to_string(&path).expect("read"), "original");
    }

    #[test]
    fn second_lock_times_out_as_an_error_never_a_silent_pass() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("r.lock");
        let _held = ResourceLock::acquire(&path).expect("first");
        let err = ResourceLock::acquire_with_budget(&path, 2, 5).expect_err("must fail closed");
        assert!(matches!(err, LockError::Timeout { .. }), "{err}");
    }

    #[test]
    fn a_stale_lock_is_reclaimed() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("r.lock");
        fs::write(&path, serde_json::json!({"pid": 4_000_000}).to_string()).expect("plant");
        let lock = ResourceLock::acquire_with_budget(&path, 4, 5).expect("stale reclaimed");
        drop(lock);
        let again = ResourceLock::acquire(&path).expect("reacquire after drop");
        drop(again);
    }

    #[test]
    fn ref_cas_updates_on_match_and_reports_moves() {
        let dir = tempfile::tempdir().expect("tmp");
        let repo = git2::Repository::init(dir.path()).expect("repo");
        let sig = git2::Signature::now("t", "t@t").expect("sig");
        let tree = repo
            .find_tree(repo.treebuilder(None).expect("tb").write().expect("tree"))
            .expect("tree");
        let c1 = repo
            .commit(None, &sig, &sig, "one", &tree, &[])
            .expect("c1");
        let c2 = repo
            .commit(None, &sig, &sig, "two", &tree, &[])
            .expect("c2");
        let lock_dir = dir.path().join("locks");
        update_ref_cas(&repo, &lock_dir, "refs/x/tip", None, c1, "first").expect("create");
        let err = update_ref_cas(&repo, &lock_dir, "refs/x/tip", None, c2, "clobber")
            .expect_err("expected-absent must conflict");
        assert!(matches!(err, RefCasError::Moved { .. }), "{err}");
        update_ref_cas(&repo, &lock_dir, "refs/x/tip", Some(c1), c2, "advance").expect("advance");
        assert_eq!(
            repo.refname_to_id("refs/x/tip").expect("tip").to_string(),
            c2.to_string()
        );
    }
}
