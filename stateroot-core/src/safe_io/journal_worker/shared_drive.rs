//! A Windows deny-sharing handle and a WSL DrvFs open file exclude each
//! other even when the two kernels' byte locks do not. Bind this proof to
//! the same permanent guard, and only to this qualified pair of backends.

use super::MutationGuard;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use uuid::Uuid;

#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Backend {
    Windows,
    Drvfs,
}

#[derive(Serialize, Deserialize)]
pub(super) struct Binding {
    backend: Backend,
    guard_id: [u8; 16],
}

impl Binding {
    pub(super) fn peer_released(&self, guard: &MutationGuard, path: &Path) -> bool {
        Self::observe(guard, path, false)
            .is_some_and(|now| now.backend != self.backend && now.guard_id == self.guard_id)
    }

    pub(super) fn observe(guard: &MutationGuard, path: &Path, initialize: bool) -> Option<Self> {
        let backend = backend(guard, path)?;
        let mut file = &guard.file;
        let len = file.metadata().ok()?.len();
        file.seek(SeekFrom::Start(0)).ok()?;
        let guard_id = match len {
            0 if initialize => {
                let id = Uuid::now_v7();
                file.write_all(id.as_bytes()).ok()?;
                file.sync_all().ok()?;
                id
            }
            16 => {
                let mut bytes = [0; 16];
                file.read_exact(&mut bytes).ok()?;
                Uuid::from_bytes(bytes)
            }
            _ => return None,
        };
        (!guard_id.is_nil()).then_some(Self {
            backend,
            guard_id: *guard_id.as_bytes(),
        })
    }
}

#[cfg(target_os = "linux")]
fn backend(guard: &MutationGuard, _path: &Path) -> Option<Backend> {
    use std::os::fd::AsRawFd;
    let fd =
        std::fs::read_to_string(format!("/proc/self/fdinfo/{}", guard.file.as_raw_fd())).ok()?;
    let mount = fd
        .lines()
        .find_map(|line| line.strip_prefix("mnt_id:"))?
        .trim();
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    mounts.lines().find_map(|line| {
        let (mount_fields, fs_fields) = line.split_once(" - ")?;
        if mount_fields.split_whitespace().next()? != mount {
            return None;
        }
        let mut fields = fs_fields.split_whitespace();
        let fs = fields.next()?;
        let source = fields.next()?.as_bytes();
        let options = fields.next()?;
        (fs == "9p"
            && source.len() >= 2
            && source[0].is_ascii_alphabetic()
            && source[1] == b':'
            && options
                .split([',', ';'])
                .any(|option| option == "aname=drvfs"))
        .then_some(Backend::Drvfs)
    })
}

#[cfg(windows)]
fn backend(_guard: &MutationGuard, path: &Path) -> Option<Backend> {
    use std::path::{Component, Prefix};
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDriveTypeW(root: *const u16) -> u32;
    }
    let Component::Prefix(prefix) = path.components().next()? else {
        return None;
    };
    let drive = match prefix.kind() {
        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
        _ => return None,
    };
    let root = [u16::from(drive), u16::from(b':'), 92, 0];
    // SAFETY: a NUL-terminated drive root is valid for this call.
    (unsafe { GetDriveTypeW(root.as_ptr()) } == 3).then_some(Backend::Windows)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn backend(_guard: &MutationGuard, _path: &Path) -> Option<Backend> {
    None
}
