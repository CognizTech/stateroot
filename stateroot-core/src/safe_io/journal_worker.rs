//! Single writer for one checkout's boundary journal. A complete owner
//! record is published exclusively so even an older or foreign-runtime
//! worker sees it. The kernel guard stays locked for the entire operation.
//! Windows and WSL locks need not interoperate on a shared drive.

mod shared_drive;

use super::{native_process_start_token, same_namespace, LockOwner, MutationGuard, ResourceLock};
use crate::local_store;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

const PROTOCOL: &str = "stateroot.journal-worker.v1";

#[derive(Serialize, Deserialize)]
struct Record {
    #[serde(flatten)]
    owner: LockOwner,
    #[serde(default)]
    worker_protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worker_guard_domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shared_drive: Option<shared_drive::Binding>,
}

/// Observer-relative state. Only a positively identified local owner or
/// an unavailable kernel guard is ordinary contention.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Available,
    Busy,
    Recoverable,
    Unverified,
    Malformed,
    IoError,
}

#[derive(Debug, Serialize)]
pub struct Inspection {
    pub state: State,
    pub detail: String,
    pub owner_sha256: Option<String>,
    pub pid: Option<u32>,
    pub namespace: Option<String>,
    pub acquired_at: Option<String>,
}

impl Inspection {
    pub fn blocked(&self) -> bool {
        matches!(
            self.state,
            State::Unverified | State::Malformed | State::IoError
        )
    }

    pub fn recovery_hint(&self) -> Option<String> {
        if !matches!(self.state, State::Unverified | State::Malformed) {
            return None;
        }
        self.owner_sha256.as_ref().map(|sha| format!(
            "After confirming every writer in all runtimes has stopped, use stateroot handoff recover-worker --expected-owner {sha} --writers-stopped --reason 'how all writers were confirmed stopped'"
        ))
    }

    fn message(&self) -> String {
        format!(
            "boundary worker {:?}: {}. Inspect with stateroot handoff inspect",
            self.state, self.detail
        )
    }
}

fn path(project: &Path) -> PathBuf {
    local_store::root(project).join(local_store::FINALIZE_LOCK_PATH)
}

fn evidence_path(project: &Path, hash: &str) -> PathBuf {
    local_store::root(project)
        .join("local/finalize-worker-recovery")
        .join(format!("{hash}.json"))
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn domain(guard: &MutationGuard) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // Only trust a free lease in the same native kernel and on the
        // same guard inode. A PID namespace can disappear within one boot.
        let boot = super::native_epoch().1?;
        let file = guard.file.metadata().ok()?;
        Some(sha(format!(
            "{}:{boot}:{}:{}",
            std::env::consts::OS,
            file.dev(),
            file.ino()
        )
        .as_bytes()))
    }
    #[cfg(windows)]
    {
        let _ = guard;
        super::host_namespace().map(str::to_owned)
    }
}

fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => match fs::read(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            result => result.map(Some),
        },
        Ok(_) => Err(io::Error::other(format!(
            "{} is not a regular owner file",
            path.display()
        ))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn classify(bytes: Option<&[u8]>, guard: Option<&MutationGuard>, path: &Path) -> Inspection {
    let mut view = Inspection {
        state: State::Available,
        detail: "no recorded owner".into(),
        owner_sha256: bytes.map(sha),
        pid: None,
        namespace: None,
        acquired_at: None,
    };
    let Some(bytes) = bytes else { return view };
    let Ok(record) = serde_json::from_slice::<Record>(bytes) else {
        view.state = State::Malformed;
        view.detail = "owner record cannot be decoded".into();
        return view;
    };
    let owner = record.owner;
    view.pid = Some(owner.pid);
    view.namespace = Some(owner.namespace.clone());
    view.acquired_at = Some(owner.acquired_at.clone());
    if record
        .worker_protocol
        .as_deref()
        .is_some_and(|protocol| protocol != PROTOCOL)
    {
        view.state = State::Unverified;
        view.detail = "unrecognized worker protocol; owner retained".into();
        return view;
    }
    let kernel_released = record.worker_protocol.as_deref() == Some(PROTOCOL)
        && guard.is_some_and(|guard| {
            record
                .worker_guard_domain
                .as_deref()
                .is_some_and(|recorded| domain(guard).as_deref() == Some(recorded))
                || record
                    .shared_drive
                    .as_ref()
                    .is_some_and(|binding| binding.peer_released(guard, path))
        });
    (view.state, view.detail) = if kernel_released || owner.provably_stale() {
        (
            State::Recoverable,
            "owner ended, was replaced, or released its native lifetime lock".into(),
        )
    } else if owner.pid != 0
        && same_namespace(&owner.namespace)
        && owner
            .process_start
            .filter(|start| *start != 0)
            .zip(native_process_start_token(owner.pid))
            .is_some_and(|(a, b)| a == b)
    {
        (
            State::Busy,
            format!("verified active owner PID {}", owner.pid),
        )
    } else {
        (
            State::Unverified,
            format!(
                "ownership of PID {} cannot be verified from this runtime",
                owner.pid
            ),
        )
    };
    view
}

// Keep the lifetime handle apart from the original short mutation guard.
// An old Windows client's blocking Drop must still be able to open that
// guard while a new client is inspecting or attempting to claim the journal.
fn lease_path(resource: &Path) -> PathBuf {
    resource.with_extension("lease")
}

fn guard_error(bytes: Option<&[u8]>, path: &Path, error: io::Error) -> Inspection {
    let mut view = classify(bytes, None, path);
    view.state = State::IoError;
    view.detail = if error.kind() == io::ErrorKind::PermissionDenied {
        format!("guard access denied (permissions or an exclusive peer handle): {error}")
    } else {
        format!("worker ownership unavailable: {error}")
    };
    view
}

/// Read-only. Inspection never creates a guard or changes an owner or job.
pub fn inspect(project: &Path) -> Inspection {
    let resource = path(project);
    let bytes = match read(&resource) {
        Ok(bytes) => bytes,
        Err(error) => return guard_error(None, &resource, error),
    };
    let busy = || {
        let mut view = classify(bytes.as_deref(), None, &resource);
        view.state = State::Busy;
        view.detail = "native guard is busy".into();
        view
    };
    let lease = match MutationGuard::worker(&lease_path(&resource), false) {
        Ok(Some(lease)) => Some(lease),
        Ok(None) => return busy(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return guard_error(bytes.as_deref(), &resource, error),
    };
    // A broken or inaccessible legacy guard can block acquisition even
    // before an owner record exists. Observe it without creating it.
    match MutationGuard::open(&resource, false, false, false) {
        Ok(Some(_mutation)) => classify(bytes.as_deref(), lease.as_ref(), &resource),
        Ok(None) => busy(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            classify(bytes.as_deref(), lease.as_ref(), &resource)
        }
        Err(error) => guard_error(bytes.as_deref(), &resource, error),
    }
}

fn publish_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing parent directory"))?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist_noclobber(path).map_err(|error| error.error)?;
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Acquire the new lifetime guard and the existing cross-runtime marker.
/// None means contention. Ownership uncertainty and disk errors propagate.
pub fn try_claim(project: &Path) -> io::Result<Option<ResourceLock>> {
    try_claim_inner(project, true)
}

fn try_claim_inner(project: &Path, hold_handle: bool) -> io::Result<Option<ResourceLock>> {
    let path = path(project);
    fs::create_dir_all(path.parent().unwrap())?;
    let owner = LockOwner::current();
    let Some(guard) = MutationGuard::worker(&lease_path(&path), true)? else {
        return Ok(None);
    };
    let Some(_mutation) = MutationGuard::acquire(&path, false)? else {
        return Ok(None);
    };
    let bytes = read(&path)?;
    let view = classify(bytes.as_deref(), Some(&guard), &path);
    match view.state {
        State::Available => {}
        State::Recoverable => {
            // Compare the complete record, then retry publication immediately
            // in this invocation. A legacy one-attempt drainer only removed it.
            if read(&path)? != bytes {
                return Err(io::Error::other("worker owner changed during recovery"));
            }
            fs::remove_file(&path)?;
        }
        State::Busy => return Ok(None),
        _ => return Err(io::Error::other(view.message())),
    }
    let record = Record {
        owner,
        worker_protocol: Some(PROTOCOL.into()),
        shared_drive: if hold_handle {
            shared_drive::Binding::observe(&guard, &path, true)
        } else {
            None
        },
        worker_guard_domain: if hold_handle { domain(&guard) } else { None },
    };
    let body = serde_json::to_vec(&record).map_err(io::Error::other)?;
    match publish_new(&path, &body) {
        Ok(()) => Ok(Some(ResourceLock {
            path,
            token: record.owner.token,
            _lifetime_guard: hold_handle.then_some(guard),
        })),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            // Another kernel can win exclusive creation on a shared drive.
            // Only a verified owner is a normal busy result.
            let view = classify(read(&path)?.as_deref(), None, &path);
            if view.blocked() {
                Err(io::Error::other(view.message()))
            } else {
                Ok(None)
            }
        }
        Err(e) => Err(e),
    }
}

pub fn claim(project: &Path) -> io::Result<ResourceLock> {
    claim_with_budget(project, true)
}

/// Closing a checkout must exclude workers while moving its directory. Hold
/// the compatible owner marker, without a descriptor under the directory.
/// This marker never claims the kernel-lifetime recovery protocol.
pub(crate) fn reserve_for_checkout_move(project: &Path) -> io::Result<ResourceLock> {
    claim_with_budget(project, false)
}

fn claim_with_budget(project: &Path, hold_handle: bool) -> io::Result<ResourceLock> {
    for _ in 0..40 {
        if let Some(claim) = try_claim_inner(project, hold_handle)? {
            return Ok(claim);
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "boundary worker busy",
    ))
}

#[derive(Serialize, Deserialize)]
pub struct Recovery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    by: Option<String>,
    owner_sha256: String,
    owner_bytes_base64: String,
    reason: String,
    at: String,
}

fn checked_receipt(file: &Path, expected: &str) -> io::Result<Option<Recovery>> {
    let Some(bytes) = read(file)? else {
        return Ok(None);
    };
    let receipt: Recovery = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if receipt.owner_sha256 != expected
        || sha(&STANDARD
            .decode(&receipt.owner_bytes_base64)
            .map_err(io::Error::other)?)
            != expected
    {
        return Err(io::Error::other(
            "worker recovery receipt does not match its identity",
        ));
    }
    Ok(Some(receipt))
}

fn valid_digest(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn recovery(project: &Path, id: &str) -> io::Result<Recovery> {
    if !valid_digest(id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid worker owner SHA256",
        ));
    }
    checked_receipt(&evidence_path(project, id), id)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "worker recovery receipt unavailable",
        )
    })
}

/// Explicit legacy recovery after the operator has quiesced all writers,
/// including old binaries on the other side of a shared Windows/WSL mount.
pub fn recover_stopped(
    project: &Path,
    expected: &str,
    reason: &str,
    actor: &str,
) -> io::Result<()> {
    if !valid_digest(expected) || reason.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "exact owner SHA256 and recovery reason required",
        ));
    }
    let resource = path(project);
    let receipt = evidence_path(project, expected);
    fs::create_dir_all(resource.parent().unwrap())?;
    // The same crash-released guard covers drain, recovery and retirement.
    let Some(guard) = MutationGuard::worker(&lease_path(&resource), true)? else {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "boundary worker busy",
        ));
    };
    let Some(_mutation) = MutationGuard::acquire(&resource, false)? else {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "boundary owner transition busy",
        ));
    };
    let Some(bytes) = read(&resource)? else {
        if checked_receipt(&receipt, expected)?.is_some() {
            return Ok(());
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no matching worker owner or recovery receipt",
        ));
    };
    let view = classify(Some(&bytes), Some(&guard), &resource);
    if view.owner_sha256.as_deref() != Some(expected) {
        return Err(io::Error::other("worker owner changed; inspect again"));
    }
    if view.state == State::Busy {
        return Err(io::Error::other(view.message()));
    }
    let evidence = Recovery {
        by: Some(actor.into()),
        owner_sha256: expected.into(),
        owner_bytes_base64: STANDARD.encode(&bytes),
        reason: reason.trim().into(),
        at: local_store::now_rfc3339(),
    };
    match publish_new(
        &receipt,
        &serde_json::to_vec(&evidence).map_err(io::Error::other)?,
    ) {
        Ok(()) => {}
        Err(e)
            if e.kind() == io::ErrorKind::AlreadyExists
                && checked_receipt(&receipt, expected)?.is_some() => {}
        Err(e) => return Err(e),
    }
    if read(&resource)?.as_deref() != Some(&bytes) {
        return Err(io::Error::other("worker owner changed before release"));
    }
    fs::remove_file(resource)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_owner_can_release_while_new_client_holds_lifetime_gate() {
        let project = tempfile::tempdir().unwrap();
        let marker = path(project.path());
        let old = ResourceLock::acquire(&marker).unwrap();
        let gate = MutationGuard::worker(&lease_path(&marker), true)
            .unwrap()
            .unwrap();
        drop(old);
        assert!(!marker.exists());
        drop(gate);
        assert!(try_claim(project.path()).unwrap().is_some());
    }
}
