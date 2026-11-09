//! Bounded one-time migration into the existing output owner. An authorized
//! operator supplies the exact inspected tip/inventory and disposal judgment.
//! Cache provenance alone never authorizes unknown data deletion.
use super::*;

#[cfg(test)]
type LegacyTransferObserver = Box<dyn FnOnce(&Path)>;
#[cfg(test)]
thread_local! {
    static TEST_BEFORE_LEGACY_TRANSFER: std::cell::RefCell<Option<LegacyTransferObserver>> = const { std::cell::RefCell::new(None) };
    static TEST_AFTER_LEGACY_LOCK_RELEASE: std::cell::RefCell<Option<LegacyTransferObserver>> = const { std::cell::RefCell::new(None) };
    static TEST_REFUSE_LEGACY_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Debug, Serialize)]
pub struct LegacyCargoOutput {
    pub fork: String,
    pub tip: String,
    pub checkout: PathBuf,
    pub target: PathBuf,
    pub inventory: String,
    pub bytes: u64,
    pub entries: usize,
    pub compiler_host: String,
    pub coverage: &'static str,
}

#[cfg(target_os = "linux")]
fn checkout(owner: &Path, fork: &str) -> Result<PathBuf, String> {
    let detail = crate::roots::fork_lifecycle(owner, fork).map_err(|e| e.to_string())?;
    let path = PathBuf::from(
        detail["worktree"]
            .as_str()
            .ok_or("fork has no registered checkout")?,
    );
    crate::roots::validate_fork_execution(owner, fork, &path).map_err(|e| e.to_string())?;
    let path = crate::roots::retirement_checkout(owner, fork)
        .map_err(|e| e.to_string())?
        .ok_or("fork has no verified resident checkout")?;
    for entry in crate::delegation_status::read_entries(owner) {
        match entry {
            crate::delegation_status::DelegationEntry::Degraded { .. } => {
                return Err("worker ownership unavailable; legacy output retained".into())
            }
            crate::delegation_status::DelegationEntry::Record(_, worker)
                if worker["fork_id"] == fork =>
            {
                let pid = worker["pid"]
                    .as_u64()
                    .filter(|pid| *pid > 0 && *pid <= i32::MAX as u64);
                if !crate::delegation_status::owns_namespace(&worker)
                    || pid.is_none()
                    || pid.is_some_and(|pid| safe_io::pid_alive(pid as u32))
                {
                    return Err(
                        "worker process release is active/unverifiable; legacy output retained"
                            .into(),
                    );
                }
            }
            _ => {}
        }
    }
    if checkout_disposition(owner, &path)?.blocked {
        return Err("output ownership is active/unverifiable; legacy output retained".into());
    }
    Ok(path)
}

/// The standard top-level Cargo target is the only accepted legacy locator.
/// Other ignored paths/native configuration never become disposable implicitly.
pub fn inspect_legacy_cargo_output(owner: &Path, fork: &str) -> Result<LegacyCargoOutput, String> {
    inspect_legacy_cargo_output_at(owner, fork, Path::new("target"))
}

pub fn inspect_legacy_cargo_output_at(
    owner: &Path,
    fork: &str,
    relative: &Path,
) -> Result<LegacyCargoOutput, String> {
    let _guard = crate::roots::fork_operation_guard(owner, fork).map_err(|e| e.to_string())?;
    inspect_locked(owner, fork, relative)
}

fn inspect_locked(owner: &Path, fork: &str, relative: &Path) -> Result<LegacyCargoOutput, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (owner, fork, relative);
        Err("legacy output migration is qualified only for native Linux/WSL Cargo output; other runtime bytes retained".into())
    }
    #[cfg(target_os = "linux")]
    {
        let path = checkout(owner, fork)?;
        if !relative.starts_with("target")
            || relative.components().count() > 3
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(
                "legacy target must be an explicit bounded relative path under target/".into(),
            );
        }
        let target = path.join(relative);
        let mut ancestor = path.clone();
        for component in relative.components() {
            ancestor.push(component.as_os_str());
            if fs::symlink_metadata(&ancestor)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("legacy locator contains a symlink; retained".into());
            }
        }
        let metadata = fs::symlink_metadata(&target).map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("legacy target is not a plain directory; retained".into());
        }
        let tag = fs::read_to_string(target.join("CACHEDIR.TAG")).map_err(|e| e.to_string())?;
        if !tag.starts_with("Signature: 8a477f597d28d172789f06886806bc55\n") {
            return Err("legacy output has no Cargo cache provenance; retained".into());
        }
        let compiler: Value = serde_json::from_slice(
            &fs::read(target.join(".rustc_info.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let native = String::from_utf8(cargo::probe(
            std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()),
            &["-vV"],
            &path,
        )?)
        .map_err(|e| e.to_string())?;
        let host = native
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .ok_or("native compiler host unavailable")?;
        let hosts = compiler["outputs"]
            .as_object()
            .ok_or("legacy compiler provenance unavailable")?
            .values()
            .filter_map(|value| value["stdout"].as_str())
            .flat_map(str::lines)
            .filter_map(|line| line.strip_prefix("host: "))
            .collect::<std::collections::BTreeSet<_>>();
        if hosts.len() != 1 || !hosts.contains(host) || !host.contains("linux") {
            return Err("legacy output belongs to a foreign/unverifiable runtime; retained".into());
        }
        let repo = git2::Repository::open(&path).map_err(|e| e.to_string())?;
        for entry in repo.index().map_err(|e| e.to_string())?.iter() {
            let entry = std::str::from_utf8(&entry.path).map_err(|_| "index path unavailable")?;
            if Path::new(entry).starts_with(relative) {
                return Err("tracked/indexed bytes inside legacy output; retained".into());
            }
        }
        if !crate::sync_engine::ignore::IgnoreRules::load(&path)
            .is_ignored(&relative.to_string_lossy(), true)
        {
            return Err("legacy output is not excluded from source capture; retained".into());
        }
        process::ensure_legacy_idle(&path, &target)?;
        let tree = output_tree_bounded(&target, 500_000, Duration::from_secs(120))?;
        let tip = crate::roots::fork_lifecycle(owner, fork).map_err(|e| e.to_string())?["root"]
            .as_str()
            .ok_or("fork tip unavailable")?
            .to_owned();
        Ok(LegacyCargoOutput{fork:fork.into(),tip,checkout:path,target,inventory:tree.digest,bytes:tree.bytes,entries:tree.entries.len(),compiler_host:host.into(),coverage:"metadata inventory and native Cargo provenance; operator must inspect unknown deliverables before declaring this exact inventory disposable"})
    }
}

fn try_lock_cargo(file: &fs::File) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        // Cargo uses this native flock domain. Keep the file open until the
        // migration finishes; closing releases only our acquired lock.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(format!(
                "legacy Cargo lock is active/unavailable: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Err("legacy Cargo ownership is unqualified on this runtime".into())
    }
}

fn cargo_lock_identity(file: &fs::File) -> Result<(u64, u64), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        Ok((metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Err("legacy Cargo lock identity is unqualified on this runtime".into())
    }
}

/// Only a fully validated, empty lane from the known pre-transfer refusal can
/// be reused. Moved, unknown, pinned, changed and extra bytes remain retained.
fn empty_refused_transfer(pool: &Pool, slot: &Slot) -> Result<bool, String> {
    if slot.state != "retained"
        || slot.owner.is_some()
        || slot.retained.is_some()
        || slot.quarantine.is_some()
        || slot.reusable
        || slot.seal.is_some()
        || slot.bytes != 0
        || !slot.reason.as_deref().is_some_and(|reason| {
            reason.starts_with("legacy output move refused without deletion: ")
        })
    {
        return Ok(false);
    }
    pool.validate_location(slot)?;
    match fs::symlink_metadata(&slot.target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Ok(_) => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

fn resume_empty_transfer(
    pool: &Pool,
    previous: &Slot,
    checkout: &Path,
    compatibility: &str,
) -> Result<Lease, String> {
    let _guard = pool.lock()?;
    let mut slot = pool
        .records()?
        .into_iter()
        .find(|slot| slot.id == previous.id)
        .ok_or("migration slot disappeared")?;
    if serde_json::to_value(&slot).map_err(|error| error.to_string())?
        != serde_json::to_value(previous).map_err(|error| error.to_string())?
        || !empty_refused_transfer(pool, &slot)?
        || !crate::path_identity::same_directory(&slot.checkout, checkout)
            .map_err(|error| error.to_string())?
        || slot.fork != local_store::fork_context(checkout).map(|context| context.fork)
    {
        return Err("refused migration ownership changed before retry; retained".into());
    }
    let owner = Owner {
        pid: std::process::id(),
        namespace: safe_io::host_namespace()
            .ok_or("process namespace unavailable")?
            .into(),
        token: uuid::Uuid::new_v4().to_string(),
    };
    slot.state = "active".into();
    slot.owner = Some(owner.clone());
    slot.reason = None;
    slot.last_used = now();
    slot.compatibility = compatibility.into();
    pool.save(&slot)?;
    Ok(Lease {
        pool: pool.clone(),
        slot,
        token: owner.token,
    })
}

/// Adopt only the exact explicitly inspected rebuildable inventory. Source,
/// intelligence, original cwd and evidence remain with their lifecycle owner.
/// Crash/late writes leave an owned unknown/retained lane, never deletion.
pub fn adopt_legacy_cargo_output(
    owner: &Path,
    fork: &str,
    expected_tip: &str,
    expected_inventory: &str,
    reason: &str,
) -> Result<OutputDisposition, String> {
    adopt_legacy_cargo_output_at(
        owner,
        fork,
        Path::new("target"),
        expected_tip,
        expected_inventory,
        reason,
    )
}

pub fn adopt_legacy_cargo_output_at(
    owner: &Path,
    fork: &str,
    relative: &Path,
    expected_tip: &str,
    expected_inventory: &str,
    reason: &str,
) -> Result<OutputDisposition, String> {
    if reason.trim().is_empty() {
        return Err(
            "migration requires the operator's evidenced disposable-output judgment".into(),
        );
    }
    let _guard = crate::roots::fork_operation_guard(owner, fork).map_err(|e| e.to_string())?;
    let pool = Pool::new(owner, Policy::from_env()?)?;
    let operation = format!(
        "legacy:{fork}:{}:{expected_tip}:{expected_inventory}",
        relative.display()
    );
    let mut retry_slot = None;
    if let Some(slot) = pool
        .records()?
        .into_iter()
        .find(|slot| slot.operation == operation)
    {
        // Retry the same owned disposal without allocating a second lane.
        // Retained/unknown ownership still requires explicit resolution.
        if matches!(slot.state.as_str(), "idle" | "evicting") {
            process::ensure_legacy_idle(&slot.checkout, &slot.target)?;
            evict_idle_pool(&pool)?;
            return migration_disposition(&pool, &slot.id, &slot.compatibility);
        }
        if empty_refused_transfer(&pool, &slot)? {
            retry_slot = Some(slot);
        } else {
            return Ok(observed_slot(&slot));
        }
    }
    let inspected = inspect_locked(owner, fork, relative)?;
    if inspected.tip != expected_tip || inspected.inventory != expected_inventory {
        return Err("legacy fork tip/inventory changed since inspection; retained".into());
    }
    let (compatibility, _) = cargo::compatibility(&inspected.checkout)?;
    // Lock Cargo's existing build resources, without creating or rewriting
    // any unknown file. An active legacy compiler blocks even before publish.
    let mut locks = Vec::new();
    let mut lock_identities = Vec::new();
    #[allow(unused_mut)]
    let mut owned_fds: Vec<i32> = Vec::new();
    for profile in ["debug", "release"] {
        for name in [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"] {
            let path = inspected.target.join(profile).join(name);
            match fs::OpenOptions::new().read(true).write(true).open(&path) {
                Ok(file) => {
                    try_lock_cargo(&file)?;
                    #[cfg(target_os = "linux")]
                    {
                        use std::os::fd::AsRawFd;
                        owned_fds.push(file.as_raw_fd());
                    }
                    lock_identities.push((
                        path.strip_prefix(&inspected.target)
                            .map_err(|error| error.to_string())?
                            .to_path_buf(),
                        cargo_lock_identity(&file)?,
                    ));
                    locks.push(file);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    }
    process::ensure_legacy_idle_with_exclusions(
        &inspected.checkout,
        &inspected.target,
        &owned_fds,
    )?;
    let sealed = output_tree_bounded(&inspected.target, 500_000, Duration::from_secs(120))?;
    if sealed.digest != expected_inventory {
        return Err("legacy output changed while acquiring ownership; retained".into());
    }
    #[cfg(test)]
    if let Some(observer) = TEST_BEFORE_LEGACY_TRANSFER.with(|slot| slot.borrow_mut().take()) {
        observer(&inspected.target);
    }
    // Unique migration compatibility prevents selecting an existing warm lane.
    let retry = retry_slot.is_some();
    let mut lease = match retry_slot {
        Some(previous) => {
            resume_empty_transfer(&pool, &previous, &inspected.checkout, &compatibility)?
        }
        None => pool.allocate(&inspected.checkout, &operation, &operation)?,
    };
    lease.slot.compatibility = compatibility;
    lease.slot.reusable = false;
    pool.save(&lease.slot)?;
    pool.validate_location(&lease.slot)?;
    if !retry {
        fs::remove_file(lease.slot.target.join("CACHEDIR.TAG")).map_err(|e| e.to_string())?;
        fs::remove_dir(&lease.slot.target).map_err(|e| e.to_string())?;
    }
    // DrvFs refuses directory rename while these Cargo files are open. Release
    // only our exact descriptors, then reacquire the same inodes at the new
    // locator before accepting any disposal ownership.
    drop(locks);
    owned_fds.clear();
    #[cfg(test)]
    if let Some(observer) = TEST_AFTER_LEGACY_LOCK_RELEASE.with(|slot| slot.borrow_mut().take()) {
        observer(&inspected.target);
    }
    #[cfg(test)]
    let injected_refusal = TEST_REFUSE_LEGACY_RENAME.with(|slot| slot.replace(false));
    #[cfg(not(test))]
    let injected_refusal = false;
    let moved = if injected_refusal {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    } else {
        fs::rename(&inspected.target, &lease.slot.target)
    };
    if let Err(error) = moved {
        lease.update(
            "retained",
            Some(format!(
                "legacy output move refused without deletion: {error}"
            )),
            None,
        )?;
        return Err(error.to_string());
    }
    let mut locks = Vec::new();
    let reacquired = (|| -> Result<(), String> {
        for (relative, expected) in &lock_identities {
            let path = lease.slot.target.join(relative);
            if !fs::symlink_metadata(&path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_file()
            {
                return Err("moved Cargo lock is not a plain file".into());
            }
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|error| error.to_string())?;
            if cargo_lock_identity(&file)? != *expected {
                return Err("moved Cargo lock inode changed".into());
            }
            try_lock_cargo(&file)?;
            #[cfg(target_os = "linux")]
            {
                use std::os::fd::AsRawFd;
                owned_fds.push(file.as_raw_fd());
            }
            locks.push(file);
        }
        Ok(())
    })();
    if let Err(reason) = reacquired {
        lease.update(
            "retained",
            Some(format!(
                "legacy Cargo ownership changed during transfer: {reason}"
            )),
            None,
        )?;
        return Ok(lease.report());
    }
    let after = output_tree_bounded(&lease.slot.target, 500_000, Duration::from_secs(120))?;
    if after.digest != expected_inventory || inspected.target.exists() {
        lease.update(
            "retained",
            Some("legacy output changed during transfer; both locators preserved".into()),
            None,
        )?;
        return Ok(lease.report());
    }
    // A reader can open an existing artifact while metadata is being sealed
    // without changing that seal. Recheck native ownership after the atomic
    // move against both the original source locator and the moved output.
    if let Err(reason) = process::ensure_legacy_idle_with_exclusions(
        &inspected.checkout,
        &lease.slot.target,
        &owned_fds,
    ) {
        lease.update(
            "retained",
            Some(format!(
                "legacy consumer ownership changed during transfer: {reason}"
            )),
            None,
        )?;
        return Ok(lease.report());
    }
    drop(locks);
    owned_fds.clear();
    // Adoption establishes disposal ownership, never cross-source Cargo reuse.
    lease.slot.reusable = false;
    lease.update(
        "idle",
        Some(format!(
            "legacy inventory explicitly declared rebuildable: {reason}"
        )),
        Some((after.digest, after.bytes)),
    )?;
    // Policy owner applies its normal bounded reclaim, rather than rm/prune.
    evict_idle_pool(&pool)?;
    migration_disposition(&pool, &lease.slot.id, &lease.slot.compatibility)
}

fn migration_disposition(
    pool: &Pool,
    id: &str,
    compatibility: &str,
) -> Result<OutputDisposition, String> {
    Ok(pool
        .records()?
        .iter()
        .find(|slot| slot.id == id)
        .map(observed_slot)
        .unwrap_or(OutputDisposition {
            cache_id: Some(id.into()),
            path: None,
            runtime: Some(pool.runtime.clone()),
            compatibility: Some(compatibility.into()),
            state: "evicted".into(),
            reuse_candidate: false,
            bytes: Some(0),
            reason: Some("declared legacy output reclaimed by existing pool policy".into()),
        }))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn legacy_cargo_migration_qualifies_identity_and_preserves_unowned_bytes() {
        const HELPER: &str = "STATEROOT_LEGACY_MIGRATION_TEST_HELPER";
        if std::env::var_os(HELPER).is_none() {
            let output=std::process::Command::new(std::env::current_exe().unwrap())
    .args(["--exact","build_storage::legacy::tests::legacy_cargo_migration_qualifies_identity_and_preserves_unowned_bytes","--nocapture"])
    .env(HELPER,"1").env_remove("CARGO_TARGET_DIR").env_remove(HINT)
    .env_remove("CARGO_BUILD_TARGET_DIR").env_remove("CARGO_BUILD_BUILD_DIR")
    .output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let owner = tmp.path().join("owner");
        fs::create_dir_all(owner.join("src")).unwrap();
        local_store::init_skeleton(&owner, "legacy-test", "Legacy test", "default").unwrap();
        fs::write(owner.join(".gitignore"), "target/\n.stateroot/\n").unwrap();
        fs::write(
            owner.join("Cargo.toml"),
            "[package]\nname='legacy_fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        fs::write(
            owner.join("src/lib.rs"),
            "pub fn retained_source()->u32{7}\n",
        )
        .unwrap();
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        assert!(std::process::Command::new(&cargo)
            .args(["generate-lockfile", "--offline"])
            .current_dir(&owner)
            .status()
            .unwrap()
            .success());
        let (base, _) = crate::roots::create_root(&owner, "test", "base", None).unwrap();
        crate::roots::fork_root(&owner, &base.id, Some("legacy"), "test").unwrap();
        let path = tmp.path().join("checkout");
        crate::roots::fork_materialize(&owner, "legacy", &path, None).unwrap();
        assert!(std::process::Command::new(&cargo)
            .args(["build", "--locked", "--offline"])
            .current_dir(&path)
            .status()
            .unwrap()
            .success());
        std::env::set_var("STATEROOT_BUILD_CACHE_ROOT", tmp.path().join("outputs"));
        std::env::set_var("STATEROOT_BUILD_CACHE_MAX_BYTES", "0");
        std::env::set_var("STATEROOT_BUILD_CACHE_WARM_SLOTS", "0");
        let observed = inspect_legacy_cargo_output(&owner, "legacy").unwrap();
        let alias = tmp.path().join("different/depth/checkout-alias");
        fs::create_dir_all(alias.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        {
            let reader = fs::File::open(path.join("target/debug/liblegacy_fixture.rlib")).unwrap();
            assert!(
                process::ensure_legacy_idle(&alias, &alias.join("target")).is_err(),
                "physical consumer must be detected through a different-depth symlink boundary"
            );
            drop(reader);
        }
        fs::write(
            path.join("target/.stateroot-retain"),
            "required deliverable",
        )
        .unwrap();
        assert!(adopt_legacy_cargo_output(
            &owner,
            "legacy",
            &observed.tip,
            &observed.inventory,
            "fixture reviewed Cargo outputs"
        )
        .is_err());
        assert!(path.join("target/.stateroot-retain").is_file());
        fs::remove_file(path.join("target/.stateroot-retain")).unwrap();
        let observed = inspect_legacy_cargo_output(&owner, "legacy").unwrap();
        let mut consumer = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(path.join("target"))
            .spawn()
            .unwrap();
        assert!(adopt_legacy_cargo_output(
            &owner,
            "legacy",
            &observed.tip,
            &observed.inventory,
            "fixture reviewed Cargo outputs"
        )
        .is_err());
        consumer.kill().unwrap();
        consumer.wait().unwrap();
        let (_, command) = crate::verification::reserve_execution(&path).unwrap();
        assert!(inspect_legacy_cargo_output(&owner, "legacy").is_err());
        drop(command);
        let observed = inspect_legacy_cargo_output(&owner, "legacy").unwrap();
        assert!(adopt_legacy_cargo_output(
            &owner,
            "legacy",
            "wrong-tip",
            &observed.inventory,
            "fixture reviewed Cargo outputs"
        )
        .is_err());
        TEST_REFUSE_LEGACY_RENAME.with(|slot| slot.set(true));
        assert!(adopt_legacy_cargo_output(
            &owner,
            "legacy",
            &observed.tip,
            &observed.inventory,
            "injected pre-transfer refusal"
        )
        .is_err());
        let pool = Pool::new(&owner, Policy::from_env().unwrap()).unwrap();
        let refused = pool
            .records()
            .unwrap()
            .into_iter()
            .find(|slot| slot.fork.as_deref() == Some("legacy"))
            .unwrap();
        assert_eq!(refused.state, "retained");
        assert!(!refused.target.exists());
        assert!(path.join("target").is_dir());
        let original_operation = refused.operation.clone();
        let extra = refused.target.parent().unwrap().join("unknown-deliverable");
        fs::write(&extra, "valuable bytes outside output").unwrap();
        assert!(adopt_legacy_cargo_output(
            &owner,
            "legacy",
            &observed.tip,
            &observed.inventory,
            "retry must preserve extra bytes"
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(&extra).unwrap(),
            "valuable bytes outside output"
        );
        fs::remove_file(extra).unwrap();
        retain(&owner, &refused.id, "explicitly pinned refusal").unwrap();
        let pinned = adopt_legacy_cargo_output(
            &owner,
            "legacy",
            &observed.tip,
            &observed.inventory,
            "pinned retry",
        )
        .unwrap();
        assert_eq!(pinned.state, "retained");
        assert!(path.join("target").is_dir());
        {
            let _guard = pool.lock().unwrap();
            let mut slot = pool
                .records()
                .unwrap()
                .into_iter()
                .find(|slot| slot.id == refused.id)
                .unwrap();
            assert_eq!(slot.operation, original_operation);
            slot.retained = None;
            pool.save(&slot).unwrap();
        }
        let result = adopt_legacy_cargo_output(
            &owner,
            "legacy",
            &observed.tip,
            &observed.inventory,
            "inspected real Cargo build; all target bytes rebuildable",
        )
        .unwrap();
        assert_eq!(
            result.cache_id.as_deref(),
            Some(refused.id.as_str()),
            "retry reuses the same allocated UUID"
        );
        eprintln!(
            "legacy migration fixture filesystem {}: refused slot {} reused successfully",
            tmp.path().display(),
            refused.id
        );
        assert!(
            !result.reuse_candidate,
            "legacy migration never asserts compatible package reuse"
        );
        assert!(!path.join("target").exists());
        for _ in 0..4 {
            if inspect(&owner).unwrap().locations.is_empty() {
                break;
            }
            evict_idle(&owner).unwrap();
        }
        assert!(inspect(&owner).unwrap().locations.is_empty());
        assert_eq!(
            fs::read_to_string(path.join("src/lib.rs")).unwrap(),
            "pub fn retained_source()->u32{7}\n"
        );
        assert!(crate::roots::get_root(&owner, &observed.tip).is_ok());

        // Real late consumer: open an existing artifact after the last native
        // pre-scan and metadata seal, without changing its bytes or timestamps.
        assert!(std::process::Command::new(&cargo)
            .args(["build", "--locked", "--offline"])
            .current_dir(&path)
            .status()
            .unwrap()
            .success());
        let observed = inspect_legacy_cargo_output(&owner, "legacy").unwrap();
        let consumer = std::sync::Arc::new(std::sync::Mutex::new(None));
        let started = consumer.clone();
        let outside = tmp.path().to_path_buf();
        TEST_BEFORE_LEGACY_TRANSFER.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |target| {
                let artifact = fs::File::open(target.join("debug/liblegacy_fixture.rlib")).unwrap();
                let child = std::process::Command::new("sleep")
                    .arg("30")
                    .current_dir(outside)
                    .stdin(std::process::Stdio::from(artifact))
                    .spawn()
                    .unwrap();
                *started.lock().unwrap() = Some(child);
            }));
        });
        let result = adopt_legacy_cargo_output(
            &owner,
            "legacy",
            &observed.tip,
            &observed.inventory,
            "reviewed Cargo outputs; late consumer must override disposal eligibility",
        );
        let mut child = consumer.lock().unwrap().take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        match result {
            Ok(result) => {
                assert_eq!(result.state, "retained", "{result:?}");
                assert!(result
                    .reason
                    .as_deref()
                    .unwrap()
                    .contains("consumer ownership changed during transfer"));
                let moved = result.path.unwrap();
                assert!(moved.join("debug/liblegacy_fixture.rlib").is_file());
                assert_eq!(
                    output_tree_bounded(&moved, 500_000, Duration::from_secs(120))
                        .unwrap()
                        .digest,
                    observed.inventory
                );
                evict_idle(&owner).unwrap();
                assert!(moved.join("debug/liblegacy_fixture.rlib").is_file());
                eprintln!("late native consumer: moved lane retained");
            }
            Err(error) => {
                assert!(
                    path.join("target/debug/liblegacy_fixture.rlib").is_file(),
                    "{error}"
                );
                assert!(pool
                    .records()
                    .unwrap()
                    .iter()
                    .any(|slot| slot.state == "retained"
                        && slot.reason.as_deref().is_some_and(|reason| reason
                            .starts_with("legacy output move refused without deletion:"))));
                eprintln!("late native consumer: directory move refused with original output intact: {error}");
            }
        }
        // A competitor may claim the same Cargo inode during the required
        // descriptor-release gap. Refuse rename or retain the moved lane;
        // neither outcome can proceed to disposal.
        assert!(std::process::Command::new(&cargo)
            .args(["build", "--locked", "--offline"])
            .current_dir(&path)
            .status()
            .unwrap()
            .success());
        let observed = inspect_legacy_cargo_output(&owner, "legacy").unwrap();
        let competitor = std::sync::Arc::new(std::sync::Mutex::new(None));
        let claimed = competitor.clone();
        TEST_AFTER_LEGACY_LOCK_RELEASE.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |target| {
                let file = fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(target.join("debug/.cargo-lock"))
                    .unwrap();
                try_lock_cargo(&file).unwrap();
                *claimed.lock().unwrap() = Some(file);
            }));
        });
        let raced = adopt_legacy_cargo_output(
            &owner,
            "legacy",
            &observed.tip,
            &observed.inventory,
            "Cargo competitor must prevent disposal",
        );
        drop(competitor.lock().unwrap().take());
        match raced {
            Ok(result) => {
                assert_eq!(result.state, "retained", "{result:?}");
                assert!(result
                    .reason
                    .as_deref()
                    .unwrap()
                    .contains("Cargo ownership changed during transfer"));
                assert!(result
                    .path
                    .unwrap()
                    .join("debug/liblegacy_fixture.rlib")
                    .is_file());
                let repeated = adopt_legacy_cargo_output(
                    &owner,
                    "legacy",
                    &observed.tip,
                    &observed.inventory,
                    "post-move retained retry",
                )
                .unwrap();
                assert_eq!(repeated.state, "retained");
                assert_eq!(repeated.cache_id, result.cache_id);
                eprintln!("Cargo lock gap competitor: moved lane retained, retry stays retained");
            }
            Err(error) => {
                assert!(
                    path.join("target/debug/liblegacy_fixture.rlib").is_file(),
                    "{error}"
                );
                eprintln!("Cargo lock gap competitor: original move refused: {error}");
            }
        }
    }
}
