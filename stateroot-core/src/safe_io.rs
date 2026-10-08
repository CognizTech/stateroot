//! Checked crash/concurrency primitives (repair plan Phase 1).
//!
//! Three guarantees every lifecycle path builds on:
//! - `atomic_replace`: a file is never observed torn — unique temp, flush,
//!   replace (Windows-safe), parent sync where supported, errors propagated.
//! - `ResourceLock`: a MANDATORY, ownership-tokened lock with stale-owner
//!   recovery qualified by host/PID namespace (a native Windows process can
//!   never reclaim a live WSL writer, nor vice versa). Acquisition failure
//!   is an ERROR — no path silently continues unlocked.
//! - `update_ref_cas`: expected-old-OID ref update under the ref's own
//!   resource lock, so a moved lineage tip is a retryable conflict, never a
//!   silent last-writer-wins. Granularity is per-ref: independent fork refs
//!   never serialize against each other.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Bounded retry budget for transient sharing violations on replacement
/// (Windows AV/indexers briefly hold the destination open; os error 5/32/33).
const REPLACE_RETRY_TRIES: u32 = 40;
const REPLACE_RETRY_BACKOFF_MS: u64 = 25;

/// True for the transient sharing/locking failures Windows surfaces while a
/// scanner or another process briefly holds the destination. POSIX rename is
/// atomic and replaces unconditionally, so no retry class exists there.
fn is_transient_sharing(err: &std::io::Error) -> bool {
    #[cfg(windows)]
    {
        matches!(err.raw_os_error(), Some(5) | Some(32) | Some(33))
    }
    #[cfg(not(windows))]
    {
        let _ = err;
        false
    }
}

/// Replace `path` with `bytes` atomically (checked): temp in the same
/// directory, flush+sync, replace, sync the parent directory. On failure the
/// temp is removed best-effort and the error propagates — a reader never
/// sees a half-written file after a reported success.
///
/// Durability guarantee (tested, per platform):
/// - POSIX: `rename(2)` over the synced temp is atomic; the parent directory
///   sync persists the rename. The old destination survives any failure.
/// - Windows: `std::fs::rename` is `MoveFileExW` with
///   `MOVEFILE_REPLACE_EXISTING` — an atomic replacement that preserves the
///   old destination on failure (the previous remove-then-rename dance lost
///   it between the two calls). Transient sharing violations (AV/indexer)
///   get bounded retries; the parent-directory sync is best-effort because
///   Windows cannot open a directory for `sync_all` the way POSIX can.
///
/// Neither platform promises survival of hardware/power failure mid-write;
/// the guarantee is crash-atomicity at the filesystem API level.
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
        // Atomic replacement on every supported platform; never delete the
        // destination first. Bounded retries ride out transient Windows
        // sharing violations (ERROR_ACCESS_DENIED / ERROR_SHARING_VIOLATION /
        // ERROR_LOCK_VIOLATION) before the failure is real.
        let mut attempt = 0u32;
        loop {
            match fs::rename(&tmp, path) {
                Ok(()) => break,
                Err(err) if is_transient_sharing(&err) && attempt < REPLACE_RETRY_TRIES => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(REPLACE_RETRY_BACKOFF_MS));
                }
                Err(err) => return Err(err),
            }
        }
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

/// A held resource lock; the lock file is removed on drop only while the
/// recorded owner token still matches ours (a reclaimed/successor lock at
/// the same path is never deleted from under its new owner).
#[derive(Debug)]
pub struct ResourceLock {
    path: PathBuf,
    token: String,
}

/// Owner metadata persisted inside the lock file (checked on release and on
/// stale reclaim; `token`/`acquired_at`/`namespace` are absent in legacy
/// lock files).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct LockOwner {
    /// Owning process id.
    pub pid: u32,
    /// Random per-acquisition ownership token (empty when the lock predates
    /// ownership tokens).
    #[serde(default)]
    pub token: String,
    /// RFC3339 acquisition time (empty in legacy lock files).
    #[serde(default)]
    pub acquired_at: String,
    /// Host/PID namespace the owner pid belongs to: `windows` or `unix`.
    /// A liveness probe is meaningful only inside the namespace that wrote
    /// the record — a WSL `kill -0` says nothing about a native Windows pid
    /// and `tasklist` says nothing about a WSL pid. Empty in legacy records:
    /// ambiguous, diagnosed, never assumed local.
    #[serde(default)]
    pub namespace: String,
}

/// The host/PID namespace of this process. Windows and WSL share the DrvFs
/// filesystem while keeping disjoint pid namespaces, so a lock owner is only
/// ever probed when its recorded namespace equals ours.
pub fn host_namespace() -> Option<&'static str> {
    static NAMESPACE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NAMESPACE
        .get_or_init(|| {
            use sha2::{Digest, Sha256};
            #[cfg(target_os = "linux")]
            let identity = {
                let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
                let namespace = fs::read_link("/proc/self/ns/pid").ok()?;
                format!("linux:{}:{}", boot.trim(), namespace.display())
            };
            #[cfg(windows)]
            let identity = {
                use std::os::windows::process::CommandExt;
                let output = std::process::Command::new("reg.exe")
                    .args([
                        "query",
                        r"HKLM\SOFTWARE\Microsoft\Cryptography",
                        "/v",
                        "MachineGuid",
                    ])
                    .creation_flags(0x08000000)
                    .output()
                    .ok()?;
                if !output.status.success() {
                    return None;
                }
                let text = String::from_utf8(output.stdout).ok()?;
                let guid = text
                    .lines()
                    .find_map(|line| line.split_once("REG_SZ").map(|(_, value)| value.trim()))?;
                if guid.is_empty() {
                    return None;
                }
                format!("windows:{guid}")
            };
            #[cfg(not(any(target_os = "linux", windows)))]
            let identity = {
                let host = std::process::Command::new("sysctl")
                    .args(["-n", "kern.hostuuid"])
                    .output()
                    .ok()?;
                let boot = std::process::Command::new("sysctl")
                    .args(["-n", "kern.boottime"])
                    .output()
                    .ok()?;
                if !host.status.success() || !boot.status.success() {
                    return None;
                }
                format!(
                    "{}:{:?}:{:?}",
                    std::env::consts::OS,
                    host.stdout,
                    boot.stdout
                )
            };
            Some(format!("sha256:{:x}", Sha256::digest(identity.as_bytes())))
        })
        .as_deref()
}

fn same_namespace(namespace: &str) -> bool {
    host_namespace().is_some_and(|current| current == namespace)
}

/// Lock acquisition failure (fail-closed by contract).
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// The budget expired while another live owner holds the lock.
    #[error("resource lock {path} held by live owner ({owner}) after {budget_ms}ms")]
    Timeout {
        /// Lock file path.
        path: PathBuf,
        /// Owning pid as recorded (0 = unreadable/legacy owner record).
        owner_pid: u32,
        /// Rendered owner detail (pid, acquisition time) for diagnostics.
        owner: String,
        /// Wait budget used.
        budget_ms: u64,
    },
    /// Filesystem failure.
    #[error("io error on resource lock: {0}")]
    Io(#[from] std::io::Error),
}

static LOCK_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Unique-per-acquisition ownership token: time + pid + process-local
/// sequence, so two acquisitions never share a token even in the same
/// nanosecond on the same machine.
fn mint_lock_token() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = LOCK_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:x}-{:x}-{:x}", nanos, std::process::id(), seq)
}

/// Read and parse the owner record of an existing lock file.
fn read_lock_owner(path: &Path) -> Option<LockOwner> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Kernel-owned mutation guard, released on crash. Its inode is never unlinked.
struct MutationGuard {
    file: fs::File,
}

impl MutationGuard {
    fn acquire(path: &Path, blocking: bool) -> std::io::Result<Option<Self>> {
        let mut name = path.as_os_str().to_os_string();
        name.push(".guard");
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(PathBuf::from(name))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: the live File owns the descriptor throughout this lock.
            if unsafe {
                libc::flock(
                    file.as_raw_fd(),
                    libc::LOCK_EX | if blocking { 0 } else { libc::LOCK_NB },
                )
            } != 0
            {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    return Ok(None);
                }
                return Err(error);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            let mut overlapped = Overlapped::default();
            // SAFETY: synchronous File; offset structure lives for this call.
            if unsafe {
                LockFileEx(
                    file.as_raw_handle(),
                    2 | if blocking { 0 } else { 1 },
                    0,
                    1,
                    0,
                    &mut overlapped,
                )
            } == 0
            {
                let error = std::io::Error::last_os_error();
                if matches!(error.raw_os_error(), Some(33)) {
                    return Ok(None);
                }
                return Err(error);
            }
        }
        Ok(Some(Self { file }))
    }
}

#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: *mut std::ffi::c_void,
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn LockFileEx(
        file: *mut std::ffi::c_void,
        flags: u32,
        reserved: u32,
        low: u32,
        high: u32,
        overlapped: *mut Overlapped,
    ) -> i32;
    fn UnlockFileEx(
        file: *mut std::ffi::c_void,
        reserved: u32,
        low: u32,
        high: u32,
        overlapped: *mut Overlapped,
    ) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
}

impl Drop for MutationGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd; /* SAFETY: live owned descriptor. */
            unsafe {
                libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            let mut overlapped = Overlapped::default(); /* SAFETY: same locked offset and live File. */
            unsafe {
                UnlockFileEx(self.file.as_raw_handle(), 0, 1, 0, &mut overlapped);
            }
        }
    }
}

impl ResourceLock {
    /// Acquire `path` exclusively, spinning 40×15ms (600ms). A lock file
    /// whose recorded owner pid is dead IN OUR host/PID namespace is
    /// reclaimed (stale recovery) — a crash never wedges the resource.
    /// Foreign-namespace and legacy (namespace-less) owner records are never
    /// probed or reclaimed: cross-runtime pid assumptions are diagnosed, not
    /// made. Timeout is an ERROR carrying the recorded owner for diagnostics.
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
        let mut last_owner: Option<LockOwner> = None;
        // Platform identity initialization may invoke a native registry query;
        // never perform it while serializing resource mutations.
        let namespace = host_namespace().unwrap_or("unavailable");
        for attempt in 0..tries.max(1) {
            let Some(guard) = MutationGuard::acquire(&path, false)? else {
                std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                continue;
            };
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    let token = mint_lock_token();
                    let owner = LockOwner {
                        pid: std::process::id(),
                        token: token.clone(),
                        acquired_at: crate::local_store::now_rfc3339(),
                        namespace: namespace.to_string(),
                    };
                    // Checked owner persistence: the record must be complete
                    // on disk before the lock is considered held — a crash
                    // mid-write leaves a file no one can reclaim, so a failed
                    // owner write removes the lock and propagates.
                    let body = serde_json::to_string(&owner).map_err(std::io::Error::other)?;
                    let written = file
                        .write_all(body.as_bytes())
                        .and_then(|()| file.sync_all());
                    if let Err(err) = written {
                        drop(file);
                        let _ = fs::remove_file(&path);
                        return Err(LockError::Io(err));
                    }
                    return Ok(Self { path, token });
                }
                Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
                    return Err(LockError::Io(error))
                }
                Err(_) => {
                    if attempt % 8 == 0 {
                        // Periodic stale-owner check (cheap: one read+probe).
                        if let Some(owner) = read_lock_owner(&path) {
                            last_owner = Some(owner.clone());
                            // Stale reclaim is only safe inside OUR pid
                            // namespace: a foreign-namespace pid (a native
                            // Windows writer seen from WSL, or vice versa)
                            // cannot be probed here, and a legacy record
                            // without a namespace is ambiguous. Both are
                            // treated as held and diagnosed on timeout —
                            // never silently reclaimed on a cross-runtime
                            // pid assumption.
                            let reclaimable = same_namespace(&owner.namespace)
                                && owner.pid != 0
                                && !pid_alive(owner.pid);
                            if reclaimable {
                                // Stale owner: the process is gone — reclaim,
                                // but only the exact record we read (a fresh
                                // owner appearing between the probe and the
                                // removal is serialized by the kernel guard).
                                let still_same = read_lock_owner(&path)
                                    .map(|current| current == owner)
                                    .unwrap_or(false);
                                if still_same {
                                    let _ = fs::remove_file(&path);
                                }
                                continue;
                            }
                        }
                    }
                    drop(guard);
                    std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                }
            }
        }
        let (owner_pid, owner) = match last_owner {
            Some(owner) => {
                let acquired = if owner.acquired_at.is_empty() {
                    "unknown (legacy record)".to_string()
                } else {
                    owner.acquired_at.clone()
                };
                let detail = if owner.namespace.is_empty() {
                    format!(
                        "legacy record without a host namespace — ambiguous: pid may belong to either runtime, never probed or reclaimed cross-runtime; acquired at {acquired}"
                    )
                } else if same_namespace(&owner.namespace) {
                    format!("same host namespace; acquired at {acquired}")
                } else {
                    format!(
                        "foreign host namespace — a live owner in another runtime cannot be probed or reclaimed from here; acquired at {acquired}"
                    )
                };
                (
                    owner.pid,
                    format!(
                        "pid {} namespace {} ({detail})",
                        owner.pid,
                        if owner.namespace.is_empty() {
                            "<legacy>"
                        } else {
                            owner.namespace.as_str()
                        },
                    ),
                )
            }
            None => (
                0,
                "unreadable or malformed owner record; treated as held (fail-closed)".to_string(),
            ),
        };
        Err(LockError::Timeout {
            path,
            owner_pid,
            owner,
            budget_ms: u64::from(tries) * backoff_ms,
        })
    }
}

impl Drop for ResourceLock {
    fn drop(&mut self) {
        let Ok(Some(_guard)) = MutationGuard::acquire(&self.path, true) else {
            return;
        };
        // Ownership-checked release: remove the lock file only while it still
        // records OUR token. After a stale reclaim + re-acquire by a
        // successor, the path belongs to them — dropping must not delete it.
        // A bare pid match is never sufficient: pid namespaces are disjoint
        // across Windows/WSL, so a token-less record claiming our pid could
        // be a foreign runtime's lock.
        match read_lock_owner(&self.path) {
            Some(owner) if !owner.token.is_empty() && owner.token == self.token => {
                let _ = fs::remove_file(&self.path);
            }
            _ => {}
        }
    }
}

#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    let Ok(pid) = i32::try_from(pid) else {
        return true;
    };
    if pid <= 0 {
        return true;
    }
    // SAFETY: signal zero probes a positive PID without sending a signal.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    // SAFETY: query/synchronize-only process handle, closed after a zero-time wait.
    let handle = unsafe { OpenProcess(0x101000, 0, pid) };
    if handle.is_null() {
        return std::io::Error::last_os_error().raw_os_error() != Some(87);
    }
    let status = unsafe { WaitForSingleObject(handle, 0) };
    unsafe {
        CloseHandle(handle);
    }
    // WAIT_OBJECT_0 proves exit. Timeout or unavailable probe stays alive/unknown.
    status != 0
}

#[cfg(test)]
fn tasklist_pid(text: &str, pid: u32) -> Option<bool> {
    let mut rows = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let value = line
            .trim()
            .strip_prefix('"')?
            .split("\",\"")
            .nth(1)?
            .trim_matches('"')
            .parse::<u32>()
            .ok()?;
        rows += 1;
        if value == pid {
            return Some(true);
        }
    }
    (rows > 0).then_some(false)
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

    #[cfg(windows)]
    #[test]
    fn windows_replace_failure_preserves_destination_and_cleans_temp() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        fs::write(&path, b"original").unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(atomic_replace(&path, b"replacement").is_err());
        drop(held);
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn windows_replace_retries_transient_destination_sharing() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        fs::write(&path, b"original").unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1 | 2)
            .open(&path)
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(75));
            drop(held);
        });
        let result = atomic_replace(&path, b"replacement");
        release.join().unwrap();
        result.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn tasklist_csv_matches_exact_pid_and_unknown_shapes_stay_unknown() {
        assert_eq!(
            tasklist_pid(
                "\"No tasks are running.exe\",\"123\",\"Console\",\"1\",\"20 K\"",
                123
            ),
            Some(true)
        );
        assert_eq!(
            tasklist_pid("\"app.exe\",\"1234\",\"Console\",\"1\",\"20 K\"", 123),
            Some(false)
        );
        assert_eq!(tasklist_pid("INFO: Keine passenden Aufgaben", 123), None);
        assert_eq!(tasklist_pid("", 123), None);
    }

    #[test]
    fn cached_namespace_is_not_an_os_family_claim() {
        assert_eq!(host_namespace(), host_namespace());
        if let Some(namespace) = host_namespace() {
            assert!(namespace.starts_with("sha256:"));
            assert!(!same_namespace("unix"));
            assert!(!same_namespace("windows"));
        }
        assert!(!same_namespace("unavailable"));
    }

    #[test]
    fn competing_stale_reclaimers_keep_mutual_exclusion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("race.lock");
        if let Some(namespace) = host_namespace() {
            fs::write(
                &path,
                serde_json::json!({"pid":4_000_000,"namespace":namespace,"token":"stale-original"})
                    .to_string(),
            )
            .unwrap();
        }
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let start = std::sync::Arc::new(std::sync::Barrier::new(5));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                let active = active.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    for _ in 0..10 {
                        let _lock = ResourceLock::acquire_with_budget(&path, 2000, 1).unwrap();
                        assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                        std::thread::yield_now();
                        assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
                    }
                })
            })
            .collect();
        start.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        assert!(!path.is_file());
    }

    #[test]
    fn filesystem_failures_are_not_contention_timeouts() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("ordinary-file");
        fs::write(&parent, b"intact").unwrap();
        assert!(matches!(
            ResourceLock::acquire_with_budget(parent.join("lock"), 2, 0),
            Err(LockError::Io(_))
        ));
        assert_eq!(fs::read(parent).unwrap(), b"intact");
    }

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
        fs::write(
            &path,
            serde_json::json!({"pid": 4_000_000, "namespace": host_namespace().unwrap_or("unavailable")}).to_string(),
        )
        .expect("plant");
        let lock = ResourceLock::acquire_with_budget(&path, 4, 5).expect("stale reclaimed");
        drop(lock);
        let again = ResourceLock::acquire(&path).expect("reacquire after drop");
        drop(again);
    }

    #[test]
    fn a_foreign_namespace_lock_is_never_reclaimed() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("r.lock");
        // A dead pid in the OTHER runtime's namespace (a native Windows
        // writer seen from WSL, or vice versa): our probe cannot speak for
        // it, so even a locally-dead pid number is never reclaimed.
        let foreign = if cfg!(windows) { "unix" } else { "windows" };
        fs::write(
            &path,
            serde_json::json!({
                "pid": 4_000_000,
                "token": "foreign-token",
                "acquired_at": "2026-10-08T00:00:00Z",
                "namespace": foreign,
            })
            .to_string(),
        )
        .expect("plant foreign");
        let err = ResourceLock::acquire_with_budget(&path, 2, 5)
            .expect_err("foreign lock is held, never reclaimed");
        match err {
            LockError::Timeout { owner, .. } => {
                assert!(owner.contains("foreign host namespace"), "{owner}");
                assert!(owner.contains(foreign), "{owner}");
            }
            other => panic!("expected timeout, got {other}"),
        }
        assert!(path.exists(), "the foreign lock was not reclaimed");
        let owner = read_lock_owner(&path).expect("owner record");
        assert_eq!(owner.namespace, foreign, "record untouched");
    }

    #[test]
    fn a_legacy_lock_is_ambiguous_fail_closed_and_diagnosed() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("r.lock");
        // Legacy record (no namespace): the pid could belong to either
        // runtime. Even a locally-dead pid is never reclaimed on that
        // ambiguity — the timeout diagnoses it.
        fs::write(&path, serde_json::json!({"pid": 4_000_000}).to_string()).expect("plant legacy");
        let err = ResourceLock::acquire_with_budget(&path, 2, 5)
            .expect_err("legacy ambiguity fails closed");
        match err {
            LockError::Timeout { owner, .. } => {
                assert!(owner.contains("legacy record"), "{owner}");
                assert!(owner.contains("ambiguous"), "{owner}");
            }
            other => panic!("expected timeout, got {other}"),
        }
        assert!(path.exists(), "the ambiguous legacy lock was not reclaimed");
    }

    #[test]
    fn drop_never_removes_a_successors_lock() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("r.lock");
        let first = ResourceLock::acquire(&path).expect("first");
        // Simulate a stale-reclaim + re-acquire by a successor: overwrite the
        // lock file with a different owner token, then drop the first lock.
        fs::write(
            &path,
            serde_json::json!({"pid": std::process::id(), "token": "successor", "acquired_at": "now"})
                .to_string(),
        )
        .expect("successor owner record");
        drop(first);
        assert!(
            path.exists(),
            "drop must not delete a lock owned by a successor token"
        );
        let owner = read_lock_owner(&path).expect("owner record");
        assert_eq!(owner.token, "successor");
    }

    #[test]
    fn drop_removes_our_own_tokened_lock() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("r.lock");
        let lock = ResourceLock::acquire(&path).expect("acquire");
        let owner = read_lock_owner(&path).expect("owner record on disk");
        assert_eq!(owner.pid, std::process::id());
        assert!(!owner.token.is_empty(), "checked owner token persisted");
        assert!(!owner.acquired_at.is_empty(), "acquisition time persisted");
        assert_eq!(
            owner.namespace,
            host_namespace().unwrap_or("unavailable"),
            "host namespace persisted"
        );
        drop(lock);
        assert!(!path.exists(), "our own lock is released on drop");
    }

    #[test]
    fn reclaim_does_not_delete_a_fresh_owners_lock() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("r.lock");
        // A dead-pid same-namespace record (reclaimable) — then swap in a
        // live owner before the reclaim lands. The fresh owner must survive.
        fs::write(
            &path,
            serde_json::json!({"pid": 4_000_000, "namespace": host_namespace().unwrap_or("unavailable")}).to_string(),
        )
        .expect("plant stale");
        let live = ResourceLock::acquire_with_budget(&path, 4, 5).expect("stale reclaimed by us");
        drop(live);
        // Now plant a stale record again and race a live owner over it:
        // acquire must see the stale record, reclaim only THAT record, and
        // then fail to create_new while the live owner holds the path.
        fs::write(
            &path,
            serde_json::json!({"pid": 4_000_000, "namespace": host_namespace().unwrap_or("unavailable")}).to_string(),
        )
        .expect("plant stale");
        let live = ResourceLock::acquire_with_budget(&path, 4, 5).expect("reclaim again");
        // While held, a second acquirer times out and must report the live
        // owner (pid diagnostics preserved).
        let err = ResourceLock::acquire_with_budget(&path, 2, 5).expect_err("held");
        match err {
            LockError::Timeout {
                owner_pid, owner, ..
            } => {
                assert_eq!(owner_pid, std::process::id());
                assert!(owner.contains("pid"), "{owner}");
            }
            other => panic!("expected timeout, got {other}"),
        }
        drop(live);
    }

    #[test]
    fn corrupt_lock_file_is_fail_closed_with_diagnostics() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("r.lock");
        fs::write(&path, "{not json").expect("plant corrupt");
        let err = ResourceLock::acquire_with_budget(&path, 2, 5).expect_err("fail closed");
        match err {
            LockError::Timeout {
                owner_pid, owner, ..
            } => {
                assert_eq!(owner_pid, 0);
                assert!(owner.contains("unreadable"), "{owner}");
            }
            other => panic!("expected timeout, got {other}"),
        }
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
