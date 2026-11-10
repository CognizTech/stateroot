//! Native runtime domains and producer/consumer lifetime qualification.
#[cfg(any(target_os = "linux", windows))]
use super::hash;
#[cfg(windows)]
use crate::safe_io;
#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
#[cfg(windows)]
use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::fs;

pub(super) fn runtime_domain() -> Result<String, String> {
    #[cfg(target_os = "linux")]
    let machine = fs::read_to_string("/etc/machine-id")
        .map_err(|e| format!("stable native runtime identity unavailable: {e}"))?;
    #[cfg(windows)]
    let machine = safe_io::host_namespace()
        .ok_or("stable Windows identity unavailable")?
        .to_owned();
    #[cfg(not(any(target_os = "linux", windows)))]
    return Err("native build-consumer containment is not qualified on this platform".into());
    #[cfg(any(target_os = "linux", windows))]
    {
        if machine.trim().is_empty() {
            return Err("empty native runtime identity".into());
        }
        #[cfg(target_os = "linux")]
        let account = unsafe { libc::geteuid() }.to_string();
        #[cfg(windows)]
        let account = directories::BaseDirs::new()
            .ok_or("native account profile unavailable")?
            .data_local_dir()
            .to_string_lossy()
            .into_owned();
        Ok(hash(
            format!(
                "{}:{}:{}:{}",
                std::env::consts::OS,
                std::env::consts::ARCH,
                machine.trim(),
                account
            )
            .as_bytes(),
        ))
    }
}

pub(super) struct ConsumerScope {
    #[cfg(target_os = "linux")]
    baseline: BTreeMap<u32, u64>,
    #[cfg(windows)]
    baseline: BTreeSet<u32>,
    #[cfg(target_os = "linux")]
    _subreaper: linux::Subreaper,
}

impl ConsumerScope {
    pub(super) fn begin() -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        {
            let guard = linux::Subreaper::begin()?;
            let baseline = linux::children()?
                .into_iter()
                .map(|(pid, (birth, _))| (pid, birth))
                .collect();
            Ok(Self {
                baseline,
                _subreaper: guard,
            })
        }
        #[cfg(windows)]
        {
            let current = win::job_processes()?;
            if !current.contains(&std::process::id()) {
                return Err("native job identity unavailable".into());
            }
            // Do not exempt unrelated prior descendants or reused child PIDs.
            let baseline = BTreeSet::from([std::process::id()]);
            Ok(Self { baseline })
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        Err("native consumer containment is not qualified on this platform".into())
    }
    pub(super) fn closed(&self) -> Result<bool, &str> {
        #[cfg(target_os = "linux")]
        {
            linux::closed(&self.baseline)
                .map_err(|_| "native descendant closure could not be established")
        }
        #[cfg(windows)]
        {
            win::job_processes()
                .map(|current| current.difference(&self.baseline).next().is_none())
                .map_err(|_| "native job closure could not be established")
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        Err("native consumer containment unavailable")
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    static SCOPE: std::sync::Mutex<(usize, i32)> = std::sync::Mutex::new((0, 0));
    pub struct Subreaper;
    impl Subreaper {
        pub fn begin() -> Result<Self, String> {
            let mut state = SCOPE.lock().map_err(|_| "consumer scope mutex poisoned")?;
            if state.0 == 0 {
                let mut previous = 0i32;
                // SAFETY: documented prctl scalar/owned output pointer arguments.
                if unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut previous, 0, 0, 0) } != 0
                    || unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0
                {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                state.1 = previous;
            }
            state.0 += 1;
            Ok(Self)
        }
    }
    impl Drop for Subreaper {
        fn drop(&mut self) {
            if let Ok(mut state) = SCOPE.lock() {
                state.0 = state.0.saturating_sub(1);
                if state.0 == 0 {
                    unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, state.1, 0, 0, 0) };
                }
            }
        }
    }
    pub fn children() -> Result<BTreeMap<u32, (u64, char)>, String> {
        let mut children = BTreeMap::new();
        for entry in fs::read_dir("/proc/self/task").map_err(|e| e.to_string())? {
            let file = entry.map_err(|e| e.to_string())?.path().join("children");
            let text = match fs::read_to_string(file) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            };
            for word in text.split_whitespace() {
                let pid = word
                    .parse::<u32>()
                    .map_err(|_| "invalid native child identity")?;
                let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
                    Ok(stat) => stat,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.to_string()),
                };
                let fields = stat
                    .rsplit_once(") ")
                    .ok_or("native process state unavailable")?
                    .1
                    .split_whitespace()
                    .collect::<Vec<_>>();
                let state = fields
                    .first()
                    .and_then(|value| value.chars().next())
                    .ok_or("native process state unavailable")?;
                let birth = fields
                    .get(19)
                    .ok_or("native process birth unavailable")?
                    .parse::<u64>()
                    .map_err(|_| "native process birth unavailable")?;
                children.insert(pid, (birth, state));
            }
        }
        Ok(children)
    }
    pub fn closed(baseline: &BTreeMap<u32, u64>) -> Result<bool, String> {
        let mut previous = None;
        for _ in 0..3 {
            let current = children()?;
            if current.iter().any(|(pid, (birth, state))| {
                baseline.get(pid) != Some(birth) && *state != 'Z' && *state != 'X'
            }) {
                return Ok(false);
            }
            let identities = current
                .iter()
                .map(|(pid, (birth, _))| (*pid, *birth))
                .collect::<BTreeMap<_, _>>();
            if previous.as_ref() == Some(&identities) {
                return Ok(true);
            }
            previous = Some(identities);
        }
        Ok(false)
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> *mut c_void;
        fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
        fn QueryInformationJobObject(
            job: *mut c_void,
            class: i32,
            data: *mut c_void,
            size: u32,
            returned: *mut u32,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    static JOB: std::sync::OnceLock<Result<usize, String>> = std::sync::OnceLock::new();
    #[repr(C)]
    struct Processes {
        assigned: u32,
        listed: u32,
        pids: [usize; 1024],
    }
    pub fn job_processes() -> Result<BTreeSet<u32>, String> {
        let handle = JOB
            .get_or_init(|| {
                // SAFETY: null security/name creates a private job; no kill-on-close
                // policy. Windows permits nested jobs where the native host does.
                let job = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
                if job.is_null() {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                if unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) } == 0 {
                    let error = std::io::Error::last_os_error().to_string();
                    unsafe { CloseHandle(job) };
                    return Err(error);
                }
                Ok(job as usize)
            })
            .as_ref()
            .map_err(Clone::clone)?;
        let mut processes = Box::new(Processes {
            assigned: 0,
            listed: 0,
            pids: [0; 1024],
        });
        // SAFETY: correctly aligned writable repr(C) buffer, bounded native API.
        if unsafe {
            QueryInformationJobObject(
                *handle as *mut c_void,
                3,
                std::ptr::from_mut(processes.as_mut()).cast(),
                std::mem::size_of::<Processes>() as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if processes.assigned != processes.listed
            || processes.listed as usize > processes.pids.len()
        {
            return Err("native job process budget exceeded".into());
        }
        processes.pids[..processes.listed as usize]
            .iter()
            .map(|pid| u32::try_from(*pid).map_err(|_| "native job PID unavailable".into()))
            .collect()
    }
}

/// One-time legacy ownership qualification. Normal managed output uses the
/// producer/consumer scope above; old outputs additionally need a native scan.
pub(super) fn ensure_legacy_idle(
    checkout: &std::path::Path,
    target: &std::path::Path,
) -> Result<(), String> {
    ensure_legacy_idle_with_exclusions(checkout, target, &[])
}

pub(super) fn ensure_legacy_idle_with_exclusions(
    checkout: &std::path::Path,
    target: &std::path::Path,
    owned_fds: &[i32],
) -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (checkout, target, owned_fds);
        Err("legacy consumer ownership is unqualified on this runtime".into())
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = unsafe { libc::geteuid() };
        let checkout_boundary = NativeBoundary::new(checkout)?;
        let target_boundary = NativeBoundary::new(target)?;
        for entry in fs::read_dir("/proc").map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let process = entry.path();
            let metadata = match fs::metadata(&process) {
                Ok(meta) => meta,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                    ) =>
                {
                    continue
                }
                Err(error) => return Err(error.to_string()),
            };
            if metadata.uid() != uid {
                continue;
            }
            for link in ["cwd", "exe"] {
                match fs::read_link(process.join(link)) {
                    Ok(path)
                        if checkout_boundary.contains(&path) || target_boundary.contains(&path) =>
                    {
                        return Err(format!(
                            "legacy output has live native consumer {pid}; retained"
                        ))
                    }
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                        ) => {}
                    Err(error) => {
                        return Err(format!(
                            "legacy native ownership unavailable for {pid}: {error}"
                        ))
                    }
                }
            }
            let descriptors = match fs::read_dir(process.join("fd")) {
                Ok(entries) => entries,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                    ) =>
                {
                    continue
                }
                Err(error) => return Err(error.to_string()),
            };
            for descriptor in descriptors {
                let descriptor = descriptor.map_err(|error| error.to_string())?;
                if pid == std::process::id()
                    && descriptor
                        .file_name()
                        .to_str()
                        .and_then(|name| name.parse::<i32>().ok())
                        .is_some_and(|fd| owned_fds.contains(&fd))
                {
                    continue;
                }
                match fs::read_link(descriptor.path()) {
                    Ok(path)
                        if target_boundary.contains(&path) || checkout_boundary.contains(&path) =>
                    {
                        return Err(format!(
                            "legacy output has open native consumer {pid}; retained"
                        ))
                    }
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                        ) => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            let maps = match fs::read_to_string(process.join("maps")) {
                Ok(maps) => maps,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                    ) =>
                {
                    continue
                }
                Err(error) => return Err(error.to_string()),
            };
            if maps
                .lines()
                .filter_map(|line| {
                    line.find('/')
                        .map(|offset| std::path::Path::new(&line[offset..]))
                })
                .any(|path| target_boundary.contains(path) || checkout_boundary.contains(path))
            {
                return Err(format!(
                    "legacy output is mapped by native consumer {pid}; retained"
                ));
            }
        }
        Ok(())
    }
}

/// One scan-local physical boundary. Cache its identity once and reuse common
/// candidate ancestors across /proc maps/FD rows. Follow filesystem identity,
/// not drive spelling: symlink and bind aliases can have different depths.
#[cfg(target_os = "linux")]
struct NativeBoundary {
    path: std::path::PathBuf,
    identity: Option<(u64, u64)>,
    observed: std::cell::RefCell<BTreeMap<std::path::PathBuf, bool>>,
}
#[cfg(target_os = "linux")]
impl NativeBoundary {
    fn new(path: &std::path::Path) -> Result<Self, String> {
        use std::os::unix::fs::MetadataExt;
        let identity = match fs::metadata(path) {
            Ok(metadata) => Some((metadata.dev(), metadata.ino())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("native boundary identity unavailable: {error}")),
        };
        Ok(Self {
            path: path.to_path_buf(),
            identity,
            observed: std::cell::RefCell::new(BTreeMap::new()),
        })
    }
    fn contains(&self, path: &std::path::Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        if path.starts_with(&self.path) {
            return true;
        }
        let Some(expected) = self.identity else {
            return false;
        };
        let mut visited = Vec::new();
        let mut contained = false;
        for ancestor in path.ancestors() {
            if let Some(known) = self.observed.borrow().get(ancestor).copied() {
                contained = known;
                break;
            }
            visited.push(ancestor.to_path_buf());
            if fs::metadata(ancestor).is_ok_and(|metadata| {
                metadata.is_dir() && (metadata.dev(), metadata.ino()) == expected
            }) {
                contained = true;
                break;
            }
        }
        let mut observed = self.observed.borrow_mut();
        for path in visited {
            // Bound only this derived cache; exhausting it never suppresses a
            // physical ownership check or changes the containment decision.
            if observed.len() < 16_384 {
                observed.insert(path, contained);
            }
        }
        contained
    }
}
