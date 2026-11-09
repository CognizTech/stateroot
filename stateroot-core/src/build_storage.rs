//! Owned, rebuildable Cargo output lanes. This is a local cache, not project
//! state: source/evidence stay with their existing owners. Only an explicitly
//! released producer/consumer lifetime makes a lane reusable or evictable.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{local_store, safe_io};

mod cargo;
mod legacy;
mod process;
use cargo::{compatibility, reset_workspace_products};
pub use legacy::{
    adopt_legacy_cargo_output, adopt_legacy_cargo_output_at, inspect_legacy_cargo_output,
    inspect_legacy_cargo_output_at, LegacyCargoOutput,
};
use process::{runtime_domain, ConsumerScope};

const SCHEMA: &str = "stateroot.build-slot.v1";
const LAYOUT: &str = "uuid-direct.v1";
const MARKER: &str = ".stateroot-build-owner.json";
const RETAIN: &str = ".stateroot-retain";
const HINT: &str = "STATEROOT_BUILD_LEASE";
const CACHE_TAG:&str="Signature: 8a477f597d28d172789f06886806bc55\n# Rebuildable output created by StateRoot; https://bford.info/cachedir/\n";
const COVERAGE: &str = "managed Cargo delegate/helper lifetimes; explicit native overrides and arbitrary external consumers are unmanaged";

/// A soft space target: active, unknown, retained and foreign lanes are never
/// deleted to meet it. Warm reserve is a maximum count of released lanes.
#[derive(Clone, Debug)]
pub struct Policy {
    pub max_bytes: u64,
    pub warm_slots: usize,
    pub cache_root: Option<PathBuf>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_bytes: 32 * 1024 * 1024 * 1024,
            warm_slots: 2,
            cache_root: None,
        }
    }
}

impl Policy {
    pub fn from_env() -> Result<Self, String> {
        let mut policy = Self::default();
        if let Ok(value) = std::env::var("STATEROOT_BUILD_CACHE_MAX_BYTES") {
            policy.max_bytes = value
                .parse()
                .map_err(|_| "invalid STATEROOT_BUILD_CACHE_MAX_BYTES")?;
        }
        if let Ok(value) = std::env::var("STATEROOT_BUILD_CACHE_WARM_SLOTS") {
            policy.warm_slots = value
                .parse()
                .map_err(|_| "invalid STATEROOT_BUILD_CACHE_WARM_SLOTS")?;
        }
        if let Some(value) = std::env::var_os("STATEROOT_BUILD_CACHE_ROOT") {
            if value.is_empty() {
                return Err("empty STATEROOT_BUILD_CACHE_ROOT".into());
            }
            policy.cache_root = Some(PathBuf::from(value));
        }
        Ok(policy)
    }
}

/// Machine-local observation. Absolute paths never become cache identities.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputDisposition {
    pub cache_id: Option<String>,
    pub path: Option<PathBuf>,
    pub runtime: Option<String>,
    pub compatibility: Option<String>,
    pub state: String,
    /// Policy eligibility only; acquisition rechecks ownership and metadata.
    pub reuse_candidate: bool,
    /// Last qualified closed output size; None while contents are active or
    /// unverifiable. This is never a guessed current size for a live build.
    pub bytes: Option<u64>,
    pub reason: Option<String>,
}

impl OutputDisposition {
    fn unmanaged(reason: String, path: Option<PathBuf>) -> Self {
        Self {
            cache_id: None,
            path,
            runtime: None,
            compatibility: None,
            state: "unmanaged".into(),
            reuse_candidate: false,
            bytes: None,
            reason: Some(reason),
        }
    }

    /// The delegation record carries an observation/identity, not another
    /// inventory or an absolute build location valid on some other machine.
    pub fn shared_observation(&self) -> Value {
        json!({"cache_id":self.cache_id,"runtime":self.runtime,"compatibility":self.compatibility,
            "state":self.state,"reuse_candidate":self.reuse_candidate,"bytes":self.bytes,"reason":self.reason,"coverage":COVERAGE})
    }
}

#[derive(Debug, Serialize)]
pub struct CheckoutDisposition {
    pub locations: Vec<OutputDisposition>,
    pub blocked: bool,
    pub coverage: &'static str,
}

#[derive(Debug, Serialize)]
pub struct StorageReport {
    pub locations: Vec<OutputDisposition>,
    pub last_closed_bytes: u64,
    pub max_bytes: u64,
    pub warm_slots: usize,
    /// None when active/retained/foreign outputs have no current size proof.
    pub over_budget: Option<bool>,
    pub unresolved: Vec<String>,
    pub coverage: &'static str,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Owner {
    pid: u32,
    namespace: String,
    token: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Slot {
    schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layout: Option<String>,
    id: String,
    project_id: String,
    repository_key: String,
    runtime: String,
    compatibility: String,
    target: PathBuf,
    checkout: PathBuf,
    fork: Option<String>,
    operation: String,
    state: String,
    owner: Option<Owner>,
    last_used: u128,
    bytes: u64,
    seal: Option<String>,
    retained: Option<String>,
    reason: Option<String>,
    #[serde(default)]
    reusable: bool,
    #[serde(default)]
    quarantine: Option<PathBuf>,
}

#[derive(Clone)]
struct Pool {
    project_id: String,
    repository_key: String,
    owner: PathBuf,
    root: PathBuf,
    runtime: String,
    policy: Policy,
}

impl Pool {
    fn new(owner: &Path, policy: Policy) -> Result<Self, String> {
        let manifest = local_store::read_manifest(owner)
            .map_err(|e| e.to_string())?
            .ok_or("project manifest unavailable")?;
        let project_id = manifest["project_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("project identity unavailable")?
            .to_owned();
        if manifest["schema_version"] != local_store::SCHEMA_MANIFEST_V1 {
            return Err("unsupported project identity".into());
        }
        let runtime = runtime_domain()?;
        let repository = git2::Repository::open(owner).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        let physical = {
            use std::os::unix::fs::MetadataExt;
            let meta = fs::metadata(repository.commondir()).map_err(|e| e.to_string())?;
            format!("{}:{}", meta.dev(), meta.ino())
        };
        #[cfg(not(unix))]
        let physical = crate::path_identity::equivalent_project_key(repository.commondir());
        let repository_key = hash(format!("{project_id}:{physical}").as_bytes());
        let root = local_store::root(owner)
            .join("local/build-storage")
            .join(&runtime);
        Ok(Self {
            project_id,
            repository_key,
            owner: owner.to_owned(),
            root,
            runtime,
            policy,
        })
    }

    fn record_path(&self, id: &str) -> PathBuf {
        self.root.join("slots").join(format!("{id}.json"))
    }

    fn records(&self) -> Result<Vec<Slot>, String> {
        self.read_records(true)
    }

    fn read_records(&self, verify_native_repository: bool) -> Result<Vec<Slot>, String> {
        let mut records = Vec::new();
        let dir = self.root.join("slots");
        if !dir.exists() {
            return Ok(records);
        }
        for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
            if records.len() >= 4096 {
                return Err("build record inspection budget exceeded; records preserved".into());
            }
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 65536 {
                return Err("build ownership record exceeds inspection budget".into());
            }
            let record: Slot = serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
                .map_err(|e| format!("unreadable build record {}: {e}", path.display()))?;
            if record.schema_version != SCHEMA
                || record.project_id != self.project_id
                || verify_native_repository && record.repository_key != self.repository_key
                || record.runtime != self.runtime
                || uuid::Uuid::parse_str(&record.id).is_err()
                || path != self.record_path(&record.id)
                || !matches!(
                    record.state.as_str(),
                    "active" | "idle" | "unknown" | "retained" | "evicting"
                )
                || record.state == "active" && record.owner.is_none()
                || matches!(record.state.as_str(), "idle" | "retained") && record.owner.is_some()
            {
                return Err(format!(
                    "unqualified build ownership record {}",
                    path.display()
                ));
            }
            records.push(record);
        }
        Ok(records)
    }

    fn lock(&self) -> Result<safe_io::ResourceMutationGuard, String> {
        fs::create_dir_all(self.root.join("slots")).map_err(|e| e.to_string())?;
        // The shared primitive appends .guard: keep the same physical mutex
        // pathname as prior output-pool writers, never a second lock domain.
        for _ in 0..40 {
            if let Some(guard) = safe_io::ResourceMutationGuard::acquire(self.root.join("pool"))
                .map_err(|e| e.to_string())?
            {
                return Ok(guard);
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        Err("build-pool mutation is busy; no unlocked mutation was attempted".into())
    }

    fn save(&self, slot: &Slot) -> Result<(), String> {
        safe_io::atomic_replace_json(
            &self.record_path(&slot.id),
            &serde_json::to_value(slot).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }

    fn marker(&self, slot: &Slot) -> Value {
        let mut marker = json!({"schema_version":SCHEMA,"id":slot.id,"project_id":slot.project_id,"repository_key":slot.repository_key,"runtime":slot.runtime});
        if let Some(layout) = &slot.layout {
            marker["layout"] = json!(layout);
        }
        marker
    }

    fn validate_location(&self, slot: &Slot) -> Result<(), String> {
        let lane = slot
            .target
            .parent()
            .ok_or("build lane parent unavailable")?;
        let matching_lane = slot.quarantine.as_ref().map_or_else(
            || {
                lane.file_name()
                    .is_some_and(|name| name == slot.id.as_str())
            },
            |quarantine| quarantine == lane,
        );
        if slot.target.file_name().is_none_or(|name| name != "target")
            || !matching_lane
            || slot.layout.as_deref() != Some(LAYOUT)
            || fs::symlink_metadata(lane)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
        {
            return Err("build location no longer matches its owned lane".into());
        }
        let marker: Value =
            serde_json::from_slice(&fs::read(lane.join(MARKER)).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if marker != self.marker(slot) {
            return Err("build ownership marker changed".into());
        }
        for entry in fs::read_dir(lane).map_err(|e| e.to_string())? {
            let name = entry.map_err(|e| e.to_string())?.file_name();
            if name == RETAIN {
                return Err("explicitly retained build lane".into());
            }
            if name != MARKER && name != "target" {
                return Err("unrecognized bytes beside declared build output; retained".into());
            }
        }
        if slot.retained.is_some() {
            return Err(slot.retained.clone().unwrap_or_default());
        }
        match git2::Repository::discover(lane) {
            Ok(repository) => {
                let workdir = repository
                    .workdir()
                    .ok_or("build output overlaps a bare Git repository; retained")?;
                let canonical_root = fs::canonicalize(workdir).map_err(|e| e.to_string())?;
                let canonical_target = fs::canonicalize(lane)
                    .map_err(|e| e.to_string())?
                    .join("target");
                let relative = canonical_target
                    .strip_prefix(&canonical_root)
                    .map_err(|_| "build repository location is unqualified; retained")?;
                let index = repository.index().map_err(|e| e.to_string())?;
                for entry in index.iter() {
                    let path = std::str::from_utf8(&entry.path)
                        .map_err(|_| "tracked path encoding unavailable")?;
                    if Path::new(path).starts_with(relative) {
                        return Err("tracked files inside declared build output; retained".into());
                    }
                }
            }
            Err(error) if error.code() == git2::ErrorCode::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "build output repository ownership unavailable: {error}"
                ))
            }
        }
        Ok(())
    }

    fn allocate(
        &self,
        checkout: &Path,
        operation: &str,
        compatibility: &str,
    ) -> Result<Lease, String> {
        let _guard = self.lock()?;
        let mut slots = self.records()?;
        slots.sort_by_key(|slot| std::cmp::Reverse(slot.last_used));
        let owner = Owner {
            pid: std::process::id(),
            namespace: safe_io::host_namespace()
                .ok_or("process namespace unavailable")?
                .into(),
            token: uuid::Uuid::new_v4().to_string(),
        };
        let existing = slots.into_iter().find(|slot| {
            slot.state == "idle"
                && slot.reusable
                && slot.owner.is_none()
                && slot.retained.is_none()
                && slot.compatibility == compatibility
                && slot.layout.as_deref() == Some(LAYOUT)
        });
        let mut slot = if let Some(slot) = existing {
            slot
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            let base = self
                .policy
                .cache_root
                .clone()
                .or_else(|| {
                    directories::BaseDirs::new()
                        .map(|dirs| dirs.cache_dir().join("stateroot/build"))
                })
                .ok_or("native cache directory unavailable")?;
            let base = if base.is_absolute() {
                base
            } else {
                self.owner.join(base)
            };
            // Opaque slot identity is enough in the physical path. Complete
            // project/repository/runtime identities stay checked in metadata.
            let lane = base.join(&id);
            if lane.starts_with(checkout) {
                return Err("cache root inside execution checkout is unmanaged".into());
            }
            fs::create_dir_all(lane.parent().ok_or("cache parent unavailable")?)
                .map_err(|e| e.to_string())?;
            fs::create_dir(&lane).map_err(|e| e.to_string())?;
            fs::create_dir(lane.join("target")).map_err(|e| e.to_string())?;
            fs::write(lane.join("target/CACHEDIR.TAG"), CACHE_TAG).map_err(|e| e.to_string())?;
            let slot = Slot {
                schema_version: SCHEMA.into(),
                layout: Some(LAYOUT.into()),
                id,
                project_id: self.project_id.clone(),
                repository_key: self.repository_key.clone(),
                runtime: self.runtime.clone(),
                compatibility: compatibility.into(),
                target: lane.join("target"),
                checkout: checkout.into(),
                fork: None,
                operation: operation.into(),
                state: "active".into(),
                owner: None,
                last_used: now(),
                bytes: 0,
                seal: None,
                retained: None,
                reason: None,
                reusable: true,
                quarantine: None,
            };
            safe_io::atomic_replace_json(&lane.join(MARKER), &self.marker(&slot))
                .map_err(|e| e.to_string())?;
            slot
        };
        slot.checkout = checkout.into();
        slot.fork = local_store::fork_context(checkout).map(|context| context.fork);
        slot.operation = operation.into();
        slot.state = "active".into();
        slot.owner = Some(owner.clone());
        slot.reason = None;
        slot.last_used = now();
        self.save(&slot)?;
        Ok(Lease {
            pool: self.clone(),
            slot,
            token: owner.token,
        })
    }
}

struct Lease {
    pool: Pool,
    slot: Slot,
    token: String,
}

impl Lease {
    fn update(
        &mut self,
        state: &str,
        reason: Option<String>,
        seal: Option<(String, u64)>,
    ) -> Result<(), String> {
        let _guard = self.pool.lock()?;
        let mut slot = self
            .pool
            .records()?
            .into_iter()
            .find(|slot| slot.id == self.slot.id)
            .ok_or("build ownership record disappeared")?;
        if slot.state != "active"
            || slot
                .owner
                .as_ref()
                .is_none_or(|owner| owner.token != self.token)
        {
            return Err("build execution ownership changed".into());
        }
        slot.state = state.into();
        slot.reusable = self.slot.reusable;
        slot.reason = reason;
        slot.last_used = now();
        if state != "unknown" {
            slot.owner = None;
        }
        if let Some((digest, bytes)) = seal {
            slot.seal = Some(digest);
            slot.bytes = bytes;
        }
        self.pool.save(&slot)?;
        self.slot = slot;
        Ok(())
    }

    fn report(&self) -> OutputDisposition {
        OutputDisposition {
            cache_id: Some(self.slot.id.clone()),
            path: Some(self.slot.target.clone()),
            runtime: Some(self.slot.runtime.clone()),
            compatibility: Some(self.slot.compatibility.clone()),
            state: self.slot.state.clone(),
            reuse_candidate: self.slot.state == "idle" && self.slot.reusable,
            bytes: (self.slot.state == "idle").then_some(self.slot.bytes),
            reason: self.slot.reason.clone(),
        }
    }
}

/// Keep this object through the entire command, including binary consumers.
/// Dropping an unfinished execution records Unknown; it never implies idle.
pub struct ExecutionStorage {
    lease: Option<Lease>,
    scope: Option<ConsumerScope>,
    report: OutputDisposition,
}

impl ExecutionStorage {
    pub fn environment(&self) -> Vec<(String, String)> {
        self.lease.as_ref().map(|lease|vec![
            ("CARGO_TARGET_DIR".into(),lease.slot.target.to_string_lossy().into_owned()),
            (HINT.into(),json!({"record":lease.pool.record_path(&lease.slot.id),"target":lease.slot.target,"token":lease.token}).to_string())
        ]).unwrap_or_default()
    }
    pub fn disposition(&self) -> &OutputDisposition {
        &self.report
    }

    pub fn finish(mut self) -> OutputDisposition {
        if let Some(mut lease) = self.lease.take() {
            let closure = self
                .scope
                .as_ref()
                .ok_or("consumer scope unavailable")
                .and_then(|scope| scope.closed());
            let result = match closure {
                Ok(true) => match lease.pool.validate_location(&lease.slot).and_then(|()| {
                    ensure_output(&lease.slot.target)?;
                    output_seal(&lease.slot.target)
                }) {
                    Ok(seal) => lease.update("idle", None, Some(seal)),
                    Err(reason) => lease.update("retained", Some(reason), None),
                },
                Ok(false) => lease.update(
                    "unknown",
                    Some("a consuming descendant is still active".into()),
                    None,
                ),
                Err(reason) => lease.update("unknown", Some(reason.into()), None),
            };
            self.report = lease.report();
            if let Err(error) = result {
                self.report.state = "unknown".into();
                self.report.reason = Some(error);
            }
            if self.report.state == "idle" {
                if let Err(error) = evict_idle_pool(&lease.pool) {
                    self.report.reason = Some(format!("idle cleanup incomplete: {error}"));
                } else {
                    match lease
                        .pool
                        .records()
                        .map(|slots| slots.into_iter().find(|slot| slot.id == lease.slot.id))
                    {
                        Ok(Some(slot)) => self.report = observed_slot(&slot),
                        Ok(None) => {
                            self.report.state = "evicted".into();
                            self.report.bytes = Some(0);
                            self.report.path = None;
                        }
                        Err(error) => {
                            self.report.state = "unknown".into();
                            self.report.reason = Some(error);
                        }
                    }
                }
            }
        }
        self.report.clone()
    }
}

impl Drop for ExecutionStorage {
    fn drop(&mut self) {
        if let Some(mut lease) = self.lease.take() {
            if let Err(error) = lease.update(
                "unknown",
                Some("execution ended without confirmed consumer release".into()),
                None,
            ) {
                tracing::warn!("build ownership retained: {error}");
            }
        }
    }
}

/// Prepare a qualified Rust output path in a short-lived owned command runner.
/// Windows Job membership lasts until that runner exits and does not permit
/// breakaway; never attach it implicitly to a reusable IDE/MCP/agent host.
/// Native overrides and unavailable qualification remain unmanaged observations.
/// Callers must separately hold the fork lifecycle admission while validating
/// and publishing execution/output ownership; an Unmanaged result grants no
/// permission to execute in a closing checkout.
pub fn prepare_execution(
    owner_project: &Path,
    checkout: &Path,
    operation: &str,
) -> ExecutionStorage {
    let result = (|| {
        if !checkout.join("Cargo.toml").is_file() {
            return Err("no root Cargo manifest; native output is unmanaged".into());
        }
        let expected =
            crate::roots::retained_storage_owner(checkout).map_err(|error| error.to_string())?;
        if crate::path_identity::equivalent_project_key(&expected)
            != crate::path_identity::equivalent_project_key(owner_project)
        {
            return Err("verified storage owner does not match execution checkout".into());
        }
        let pool = Pool::new(owner_project, Policy::from_env()?)?;
        let (compatibility, packages) = compatibility(checkout)?;
        let scope = ConsumerScope::begin()?;
        for _ in 0..8 {
            let mut lease = pool.allocate(checkout, operation, &compatibility)?;
            let checked = pool
                .validate_location(&lease.slot)
                .and_then(|()| {
                    let reset = ensure_output(&lease.slot.target)?;
                    if reset {
                        lease.slot.seal = None;
                    }
                    output_seal(&lease.slot.target)
                })
                .and_then(|seal| {
                    if lease.slot.seal.as_ref().is_some_and(|old| old != &seal.0) {
                        Err(
                            "idle output bytes changed outside their execution owner; retained"
                                .into(),
                        )
                    } else {
                        Ok(())
                    }
                });
            match checked {
                Ok(()) => {
                    let has_products =
                        output_tree(&lease.slot.target)?.entries.keys().any(|path| {
                            !path.as_os_str().is_empty() && path != Path::new("CACHEDIR.TAG")
                        });
                    if has_products {
                        if let Err(reason) =
                            reset_workspace_products(checkout, &lease.slot.target, &packages)
                        {
                            if scope.closed() == Ok(true) {
                                lease.slot.reusable = false;
                                let seal = output_seal(&lease.slot.target)?;
                                lease.update("idle",Some(format!("workspace invalidation unqualified; fresh lane required: {reason}")),Some(seal))?;
                                evict_idle_pool(&pool)?;
                            } else {
                                lease.update(
                                    "unknown",
                                    Some(format!(
                                        "workspace invalidation consumer closure unknown: {reason}"
                                    )),
                                    None,
                                )?;
                            }
                            continue;
                        }
                    }
                    let report = lease.report();
                    return Ok(ExecutionStorage {
                        lease: Some(lease),
                        scope: Some(scope),
                        report,
                    });
                }
                Err(reason) => lease.update("retained", Some(reason), None)?,
            }
        }
        Err("too many unqualified idle output lanes; inspect local storage records".into())
    })();
    result.unwrap_or_else(|reason| ExecutionStorage {
        lease: None,
        scope: None,
        report: OutputDisposition::unmanaged(reason, explicit_target(checkout)),
    })
}

/// General Windows harnesses preserve existing breakaway behavior. Job-style
/// containment is offered only to a qualified short-lived native command runner.
pub fn prepare_delegate_execution(
    owner_project: &Path,
    checkout: &Path,
    operation: &str,
) -> ExecutionStorage {
    #[cfg(windows)]
    {
        let _ = (owner_project, operation);
        ExecutionStorage{lease:None,scope:None,report:OutputDisposition::unmanaged("general Windows harness consumer containment is unqualified; native environment and breakaway behavior preserved".into(),explicit_target(checkout))}
    }
    #[cfg(not(windows))]
    prepare_execution(owner_project, checkout, operation)
}

/// Reuse the qualified native reader at a checkout boundary. Foreground
/// command leases remain the primary cross-platform owner; Linux additionally
/// observes surviving unmanaged native consumers before and after staging.
pub(crate) fn ensure_native_checkout_idle(checkout: &Path, original: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        process::ensure_legacy_idle(checkout, original)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (checkout, original);
        Ok(())
    }
}

/// One canonical reader for lifecycle callers. Unknown ownership blocks a
/// checkout's retirement even when the output directory lives elsewhere.
pub fn checkout_disposition(
    owner_project: &Path,
    checkout: &Path,
) -> Result<CheckoutDisposition, String> {
    if storage_absent(owner_project)? {
        return Ok(CheckoutDisposition {
            locations: Vec::new(),
            blocked: false,
            coverage: COVERAGE,
        });
    }
    let pool = Pool::new(owner_project, Policy::from_env()?)?;
    let fork = local_store::fork_context(checkout).map(|context| context.fork);
    let locations = all_runtime_records(&pool)?
        .into_iter()
        .filter(|slot| {
            fork.as_ref()
                .is_some_and(|fork| slot.fork.as_ref() == Some(fork))
                || crate::path_identity::equivalent_project_key(&slot.checkout)
                    == crate::path_identity::equivalent_project_key(checkout)
        })
        .map(|slot| observed_slot(&slot))
        .collect::<Vec<_>>();
    let blocked = locations
        .iter()
        .any(|location| matches!(location.state.as_str(), "active" | "unknown"));
    Ok(CheckoutDisposition {
        locations,
        blocked,
        coverage: COVERAGE,
    })
}

pub fn inspect(owner_project: &Path) -> Result<StorageReport, String> {
    if storage_absent(owner_project)? {
        return Ok(StorageReport {
            locations: Vec::new(),
            last_closed_bytes: 0,
            max_bytes: Policy::from_env()?.max_bytes,
            warm_slots: Policy::from_env()?.warm_slots,
            over_budget: Some(false),
            unresolved: Vec::new(),
            coverage: COVERAGE,
        });
    }
    let pool = Pool::new(owner_project, Policy::from_env()?)?;
    let slots = all_runtime_records(&pool)?;
    let last_closed_bytes = slots
        .iter()
        .fold(0u64, |total, slot| total.saturating_add(slot.bytes));
    let locations = slots.iter().map(observed_slot).collect::<Vec<_>>();
    let qualified = locations.iter().all(|location| {
        location.bytes.is_some() && location.runtime.as_deref() == Some(pool.runtime.as_str())
    });
    Ok(StorageReport {
        locations,
        last_closed_bytes,
        max_bytes: pool.policy.max_bytes,
        warm_slots: pool.policy.warm_slots,
        over_budget: qualified.then_some(last_closed_bytes > pool.policy.max_bytes),
        unresolved: slots
            .iter()
            .filter(|slot| slot.state != "idle" || slot.runtime != pool.runtime)
            .map(|slot| {
                format!(
                    "{}: output size/consumer closure is not current-qualified",
                    slot.id
                )
            })
            .collect(),
        coverage: COVERAGE,
    })
}

pub fn evict_idle(owner_project: &Path) -> Result<StorageReport, String> {
    if storage_absent(owner_project)? {
        return inspect(owner_project);
    }
    let pool = Pool::new(owner_project, Policy::from_env()?)?;
    evict_idle_pool(&pool)?;
    inspect(owner_project)
}

/// Pin the whole lane when a retained deliverable must remain near outputs.
pub fn retain(owner_project: &Path, cache_id: &str, reason: &str) -> Result<(), String> {
    let pool = Pool::new(owner_project, Policy::from_env()?)?;
    let _guard = pool.lock()?;
    let mut slot = pool
        .records()?
        .into_iter()
        .find(|slot| slot.id == cache_id)
        .ok_or("build cache not found")?;
    if slot.state == "evicting" {
        return Err("eviction is already reserved; retain a deliverable before releasing producer/consumer ownership".into());
    }
    slot.retained = Some(reason.into());
    if slot.state == "idle" {
        slot.state = "retained".into();
    }
    pool.save(&slot)
}

fn observed_slot(slot: &Slot) -> OutputDisposition {
    let mut state = slot.state.clone();
    let mut reason = slot.reason.clone();
    if state == "active" {
        let known = slot.owner.as_ref().is_some_and(|owner| {
            safe_io::host_namespace() == Some(owner.namespace.as_str())
                && safe_io::pid_alive(owner.pid)
        });
        if !known {
            state = "unknown".into();
            reason = Some("producer/consumer closure not established in this runtime".into());
        }
    }
    let bytes = (state == "idle").then_some(slot.bytes);
    OutputDisposition {
        cache_id: Some(slot.id.clone()),
        path: Some(slot.target.clone()),
        runtime: Some(slot.runtime.clone()),
        compatibility: Some(slot.compatibility.clone()),
        state,
        reuse_candidate: slot.state == "idle"
            && slot.reusable
            && slot.layout.as_deref() == Some(LAYOUT)
            && runtime_domain().as_deref() == Ok(slot.runtime.as_str()),
        bytes,
        reason,
    }
}

fn all_runtime_records(pool: &Pool) -> Result<Vec<Slot>, String> {
    let parent = pool
        .root
        .parent()
        .ok_or("build runtime parent unavailable")?;
    if !parent.exists() {
        return Ok(Vec::new());
    }
    let mut slots = Vec::new();
    for entry in fs::read_dir(parent).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        let runtime = entry
            .file_name()
            .to_str()
            .ok_or("build runtime identity unavailable")?
            .to_owned();
        if runtime.len() != 64 || !runtime.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("unknown build runtime directory; preserved".into());
        }
        let mut reader = pool.clone();
        reader.root = entry.path();
        reader.runtime = runtime;
        slots.extend(reader.read_records(reader.runtime == pool.runtime)?);
    }
    Ok(slots)
}

fn evict_idle_pool(pool: &Pool) -> Result<(), String> {
    evict_idle_step(pool, 10_000)
}

fn evict_idle_step(pool: &Pool, entry_budget: usize) -> Result<(), String> {
    let mut selected = {
        let _guard = pool.lock()?;
        let records = pool.records()?;
        let mut pending = records
            .iter()
            .filter(|slot| {
                slot.state == "evicting"
                    && slot.retained.is_none()
                    && slot.owner.as_ref().is_none_or(|owner| {
                        safe_io::host_namespace() == Some(owner.namespace.as_str())
                            && !safe_io::pid_alive(owner.pid)
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        pending.sort_by_key(|slot| slot.last_used);
        let mut slot = if let Some(slot) = pending.into_iter().next() {
            slot
        } else {
            let total = records
                .iter()
                .fold(0u64, |total, slot| total.saturating_add(slot.bytes));
            let mut idle = records
                .into_iter()
                .filter(|slot| {
                    slot.state == "idle" && slot.owner.is_none() && slot.retained.is_none()
                })
                .collect::<Vec<_>>();
            if idle.iter().all(|slot| slot.reusable)
                && idle.len() <= pool.policy.warm_slots
                && total <= pool.policy.max_bytes
            {
                return Ok(());
            }
            idle.sort_by_key(|slot| (slot.reusable, slot.last_used));
            let Some(slot) = idle.into_iter().next() else {
                return Ok(());
            };
            slot
        };
        slot.state = "evicting".into();
        slot.owner = Some(Owner {
            pid: std::process::id(),
            namespace: safe_io::host_namespace()
                .ok_or("eviction namespace unavailable")?
                .into(),
            token: uuid::Uuid::new_v4().to_string(),
        });
        pool.save(&slot)?;
        slot
    };
    let result = (|| {
        pool.validate_location(&selected)?;
        let before = closed_output_tree(&selected)?;
        if selected.seal.as_ref() != Some(&before.digest) {
            return Err("output changed outside its execution; retained".into());
        }
        if selected.quarantine.is_none() {
            let _guard = pool.lock()?;
            let current = pool
                .records()?
                .into_iter()
                .find(|slot| slot.id == selected.id)
                .ok_or("eviction record disappeared")?;
            if current.owner.as_ref().map(|owner| &owner.token)
                != selected.owner.as_ref().map(|owner| &owner.token)
                || current.state != "evicting"
                || current.retained.is_some()
            {
                return Err("eviction ownership changed".into());
            }
            let lane = selected
                .target
                .parent()
                .ok_or("build lane parent unavailable")?;
            let quarantine =
                lane.with_file_name(format!(".evict-{}-{}", selected.id, uuid::Uuid::new_v4()));
            fs::rename(lane, &quarantine).map_err(|e| e.to_string())?;
            selected.target = quarantine.join("target");
            selected.quarantine = Some(quarantine);
            pool.save(&selected)?;
        }
        // A step has a bounded entry/time budget. The verified remaining tree
        // becomes its next seal; no second inventory or background remover.
        let remaining = delete_output_step(
            &selected.target,
            before,
            entry_budget,
            selected.operation.starts_with("legacy:"),
        )?;
        if remaining.is_none() {
            let lane = selected
                .target
                .parent()
                .ok_or("quarantined lane unavailable")?;
            for entry in fs::read_dir(lane).map_err(|e| e.to_string())? {
                if entry.map_err(|e| e.to_string())?.file_name() != MARKER {
                    return Err("new bytes beside evicted output; lane retained".into());
                }
            }
            fs::remove_file(lane.join(MARKER)).map_err(|e| e.to_string())?;
            fs::remove_dir(lane).map_err(|e| e.to_string())?;
        }
        Ok(remaining)
    })();
    let _guard = pool.lock()?;
    let current = pool
        .records()?
        .into_iter()
        .find(|slot| slot.id == selected.id)
        .ok_or("eviction record disappeared")?;
    if current.state != "evicting"
        || current.owner.as_ref().map(|owner| &owner.token)
            != selected.owner.as_ref().map(|owner| &owner.token)
    {
        return Err("eviction ownership changed".into());
    }
    match result {
        Ok(None) => fs::remove_file(pool.record_path(&selected.id)).map_err(|e| e.to_string()),
        Ok(Some(tree)) => {
            selected.owner = None;
            selected.seal = Some(tree.digest);
            selected.bytes = tree.bytes;
            selected.reason =
                Some("bounded idle eviction pending; producer and consumers were released".into());
            pool.save(&selected)
        }
        Err(reason) => {
            selected.state = "retained".into();
            selected.owner = None;
            selected.reason = Some(reason);
            pool.save(&selected)
        }
    }
}

struct OutputTree {
    digest: String,
    bytes: u64,
    entries: BTreeMap<PathBuf, (bool, u64, u128)>,
}

fn delete_output_step(
    target: &Path,
    before: OutputTree,
    entry_budget: usize,
    legacy: bool,
) -> Result<Option<OutputTree>, String> {
    let deadline = Instant::now()
        + if legacy {
            Duration::from_secs(30)
        } else {
            Duration::from_millis(250)
        };
    let mut remaining = before.entries;
    let mut removed = Vec::new();
    let paths = remaining
        .keys()
        .rev()
        .take(entry_budget)
        .cloned()
        .collect::<Vec<_>>();
    for relative in paths {
        if !removed.is_empty() && Instant::now() >= deadline {
            break;
        }
        let path = target.join(&relative);
        let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        let expected = remaining
            .get(&relative)
            .ok_or("eviction input disappeared")?;
        if meta.file_type().is_symlink() || meta.is_dir() != expected.0 {
            return Err("eviction output kind changed; retained".into());
        }
        if !meta.is_dir() {
            let modified = meta
                .modified()
                .map_err(|e| e.to_string())?
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos();
            if (meta.len(), modified) != (expected.1, expected.2) {
                return Err("eviction output bytes changed; retained".into());
            }
            fs::remove_file(&path).map_err(|e| e.to_string())?;
        } else {
            fs::remove_dir(&path).map_err(|e| e.to_string())?;
        }
        remaining.remove(&relative);
        removed.push(relative);
    }
    if remaining.is_empty() {
        return Ok(None);
    }
    let after = if legacy {
        output_tree_bounded(target, 500_000, Duration::from_secs(120))?
    } else {
        output_tree(target)?
    };
    if after.entries.len() != remaining.len() {
        return Err("new bytes during eviction; remaining output retained".into());
    }
    for (path, stamp) in remaining {
        let actual = after
            .entries
            .get(&path)
            .ok_or("eviction inputs changed; remaining output retained")?;
        let known_directory_change =
            stamp.0 && removed.iter().any(|removed| removed.starts_with(&path));
        if actual.0 != stamp.0 || !known_directory_change && *actual != stamp {
            return Err("unowned changes during eviction; remaining output retained".into());
        }
    }
    Ok(Some(after))
}

fn storage_absent(owner: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(local_store::root(owner).join("local/build-storage")) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.to_string()),
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn now() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|time| time.as_millis())
        .unwrap_or(0)
}

fn explicit_target(checkout: &Path) -> Option<PathBuf> {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                checkout.join(path)
            }
        })
}

// Call only after validating the owned lane. `cargo clean` legitimately removes
// an entirely rebuildable target; absent contents are a cache miss, not user data.
fn ensure_output(target: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(target) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(target).map_err(|e| e.to_string())?;
            fs::write(target.join("CACHEDIR.TAG"), CACHE_TAG).map_err(|e| e.to_string())?;
            Ok(true)
        }
        Err(error) => Err(error.to_string()),
    }
}

fn output_seal(target: &Path) -> Result<(String, u64), String> {
    output_tree(target).map(|tree| (tree.digest, tree.bytes))
}

// Explicit operator migration may inspect a historical multi-GiB output in a
// longer bounded pass; ordinary command completion keeps its existing budget.
fn closed_output_tree(slot: &Slot) -> Result<OutputTree, String> {
    if slot.operation.starts_with("legacy:") {
        output_tree_bounded(&slot.target, 500_000, Duration::from_secs(120))
    } else {
        output_tree(&slot.target)
    }
}

fn output_tree(target: &Path) -> Result<OutputTree, String> {
    output_tree_bounded(target, 200_000, Duration::from_secs(3))
}

fn output_tree_bounded(
    target: &Path,
    entry_budget: usize,
    budget: Duration,
) -> Result<OutputTree, String> {
    let mut pending = vec![target.to_owned()];
    let mut entries = BTreeMap::new();
    let mut bytes = 0u64;
    let mut visited = 0;
    let deadline = Instant::now() + budget;
    while let Some(path) = pending.pop() {
        visited += 1;
        if visited > entry_budget || Instant::now() > deadline {
            return Err("output metadata budget exceeded; lane retained".into());
        }
        let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            return Err("unqualified output symlink; lane retained".into());
        }
        if path
            .file_name()
            .is_some_and(|name| name == RETAIN || name == ".git")
        {
            return Err(
                "explicit retention or source repository inside output; lane retained".into(),
            );
        }
        let relative = path
            .strip_prefix(target)
            .map_err(|e| e.to_string())?
            .to_path_buf();
        let modified = meta
            .modified()
            .map_err(|e| e.to_string())?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        entries.insert(relative, (meta.is_dir(), meta.len(), modified));
        if meta.is_dir() {
            for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
                pending.push(entry.map_err(|e| e.to_string())?.path());
            }
        } else if meta.is_file() {
            bytes = bytes.saturating_add(meta.len());
        } else {
            return Err("unqualified native output type; lane retained".into());
        }
    }
    let mut digest = Sha256::new();
    for (path, (directory, len, modified)) in &entries {
        let encoded = path.as_os_str().as_encoded_bytes();
        digest.update((encoded.len() as u64).to_le_bytes());
        digest.update(encoded);
        digest.update([u8::from(*directory)]);
        digest.update(len.to_le_bytes());
        digest.update(modified.to_le_bytes());
    }
    Ok(OutputTree {
        digest: format!("{:x}", digest.finalize()),
        bytes,
        entries,
    })
}

#[cfg(all(test, any(target_os = "linux", windows)))]
mod tests;
