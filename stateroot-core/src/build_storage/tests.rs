use super::cargo::probe;
use super::*;
#[cfg(unix)]
use std::collections::BTreeSet;
use std::process::{Command, Stdio};

fn fixture() -> (tempfile::TempDir, Pool) {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    git2::Repository::init(&project).unwrap();
    local_store::init_skeleton(&project, "build-fixture", "Build fixture", "default").unwrap();
    let pool = Pool::new(
        &project,
        Policy {
            cache_root: Some(temp.path().join("outputs")),
            ..Policy::default()
        },
    )
    .unwrap();
    (temp, pool)
}
fn close(lease: &mut Lease) {
    lease.pool.validate_location(&lease.slot).unwrap();
    let seal = output_seal(&lease.slot.target).unwrap();
    lease.update("idle", None, Some(seal)).unwrap();
}

#[test]
fn released_lanes_reuse_but_active_and_incompatible_executions_do_not() {
    let (_temp, pool) = fixture();
    let mut first = pool.allocate(&pool.owner, "first", "qualified-a").unwrap();
    let mut second = pool.allocate(&pool.owner, "second", "qualified-a").unwrap();
    assert_ne!(first.slot.target, second.slot.target);
    fs::write(first.slot.target.join("binary"), "first").unwrap();
    fs::write(second.slot.target.join("binary"), "second").unwrap();
    assert!(
        checkout_disposition(&pool.owner, &pool.owner)
            .unwrap()
            .blocked
    );
    close(&mut first);
    let mut reused = pool.allocate(&pool.owner, "third", "qualified-a").unwrap();
    assert_eq!(reused.slot.id, first.slot.id);
    assert_eq!(
        fs::read(second.slot.target.join("binary")).unwrap(),
        b"second"
    );
    let incompatible = pool
        .allocate(&pool.owner, "changed-toolchain", "qualified-b")
        .unwrap();
    assert_ne!(incompatible.slot.id, reused.slot.id);
    close(&mut reused);
    close(&mut second);
}

#[test]
fn live_foreign_unknown_pinned_and_unowned_changes_are_preserved() {
    let (temp, mut pool) = fixture();
    pool.policy.max_bytes = 0;
    pool.policy.warm_slots = 0;
    fs::write(pool.owner.join("source.txt"), "retained source").unwrap();
    let (base, _) =
        crate::roots::create_root(&pool.owner, "test", "cache lifecycle base", None).unwrap();
    let checkout = temp.path().join("checkout");
    crate::roots::fork_root(&pool.owner, &base.id, Some("cache-lifecycle"), "test").unwrap();
    crate::roots::fork_materialize(&pool.owner, "cache-lifecycle", &checkout, None).unwrap();
    let mut active = {
        let _guard = crate::roots::fork_operation_guard(&pool.owner, "cache-lifecycle").unwrap();
        pool.allocate(&checkout, "live", "a").unwrap()
    };
    fs::write(active.slot.target.join("consumer"), "still in use").unwrap();
    assert!(
        crate::roots::park_fork(&pool.owner, "cache-lifecycle").is_err(),
        "active output consumption prevents checkout retirement"
    );
    assert!(checkout.is_dir());
    evict_idle_pool(&pool).unwrap();
    assert!(active.slot.target.join("consumer").is_file());
    active.slot.owner.as_mut().unwrap().namespace = "foreign-runtime".into();
    pool.save(&active.slot).unwrap();
    assert_eq!(observed_slot(&active.slot).state, "unknown");
    assert!(
        checkout_disposition(&pool.owner, &checkout)
            .unwrap()
            .blocked
    );
    assert!(
        crate::roots::park_fork(&pool.owner, "cache-lifecycle").is_err(),
        "unverified output ownership prevents checkout retirement"
    );
    assert!(checkout.is_dir());
    let mut pinned = pool.allocate(&pool.owner, "pinned", "a").unwrap();
    fs::write(pinned.slot.target.join("deliverable"), "retain").unwrap();
    close(&mut pinned);
    retain(&pool.owner, &pinned.slot.id, "explicit deliverable").unwrap();
    let mut changed = pool.allocate(&pool.owner, "changed", "a").unwrap();
    fs::write(changed.slot.target.join("generated"), "rebuildable").unwrap();
    close(&mut changed);
    fs::write(changed.slot.target.join("human-note"), "do not remove").unwrap();
    evict_idle_pool(&pool).unwrap();
    assert!(active.slot.target.join("consumer").is_file());
    assert!(pinned.slot.target.join("deliverable").is_file());
    assert_eq!(
        fs::read(changed.slot.target.join("human-note")).unwrap(),
        b"do not remove"
    );
    assert_eq!(
        pool.records()
            .unwrap()
            .into_iter()
            .find(|slot| slot.id == changed.slot.id)
            .unwrap()
            .state,
        "retained"
    );
}

#[test]
fn bounded_eviction_resumes_without_deleting_a_reused_original_path() {
    let (_temp, mut pool) = fixture();
    pool.policy.max_bytes = 0;
    pool.policy.warm_slots = 0;
    let mut lease = pool.allocate(&pool.owner, "completed", "a").unwrap();
    for n in 0..8 {
        fs::write(lease.slot.target.join(format!("generated-{n}")), b"output").unwrap();
    }
    close(&mut lease);
    let original = lease.slot.target.clone();
    evict_idle_step(&pool, 1).unwrap();
    let pending = pool
        .records()
        .unwrap()
        .into_iter()
        .find(|slot| slot.id == lease.slot.id)
        .unwrap();
    assert_eq!(pending.state, "evicting");
    assert!(pending.quarantine.is_some());
    fs::create_dir_all(&original).unwrap();
    fs::write(original.join("new-human-file"), "preserve").unwrap();
    let active = pool
        .allocate(&pool.owner, "another-operation", "a")
        .unwrap();
    assert_ne!(active.slot.id, lease.slot.id);
    for _ in 0..16 {
        evict_idle_step(&pool, 1).unwrap();
    }
    assert!(pool
        .records()
        .unwrap()
        .iter()
        .all(|slot| slot.id != lease.slot.id));
    assert_eq!(
        fs::read(original.join("new-human-file")).unwrap(),
        b"preserve"
    );
    assert!(active.slot.target.is_dir());
}

#[test]
fn tracked_outputs_and_copied_repository_ownership_are_not_evicted() {
    let (temp, mut pool) = fixture();
    pool.policy.max_bytes = 0;
    pool.policy.warm_slots = 0;
    pool.policy.cache_root = Some(pool.owner.join(".stateroot/local/generated"));
    let checkout = temp.path().join("other-checkout");
    fs::create_dir(&checkout).unwrap();
    let mut tracked = pool.allocate(&checkout, "tracked", "a").unwrap();
    let lane = tracked.slot.target.parent().unwrap();
    assert_eq!(
        lane.strip_prefix(pool.policy.cache_root.as_ref().unwrap())
            .unwrap()
            .components()
            .count(),
        1,
        "only the opaque full UUID belongs below the configured cache root"
    );
    assert_eq!(lane.file_name().unwrap(), tracked.slot.id.as_str());
    let marker: Value = serde_json::from_slice(&fs::read(lane.join(MARKER)).unwrap()).unwrap();
    assert_eq!(marker["project_id"], pool.project_id);
    assert_eq!(marker["repository_key"], pool.repository_key);
    assert_eq!(marker["runtime"], pool.runtime);
    assert_eq!(marker["layout"], LAYOUT);
    pool.validate_location(&tracked.slot).unwrap();
    let mut altered = marker.clone();
    altered["runtime"] = json!("unqualified-runtime");
    fs::write(lane.join(MARKER), serde_json::to_vec(&altered).unwrap()).unwrap();
    assert!(
        pool.validate_location(&tracked.slot).is_err(),
        "short physical paths retain full ownership validation"
    );
    fs::write(lane.join(MARKER), serde_json::to_vec(&marker).unwrap()).unwrap();
    let file = tracked.slot.target.join("retained-source");
    fs::write(&file, "tracked source").unwrap();
    let seal = output_seal(&tracked.slot.target).unwrap();
    tracked.update("idle", None, Some(seal)).unwrap();
    let repo = git2::Repository::open(&pool.owner).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_path(file.strip_prefix(&pool.owner).unwrap())
        .unwrap();
    index.write().unwrap();
    evict_idle_pool(&pool).unwrap();
    assert!(file.is_file());
    let mut legacy = pool.allocate(&checkout, "legacy-layout", "legacy").unwrap();
    fs::write(legacy.slot.target.join("old-data"), "preserve old lane").unwrap();
    close(&mut legacy);
    let old_lane = legacy.slot.target.parent().unwrap().to_owned();
    let previous_layout = pool
        .policy
        .cache_root
        .as_ref()
        .unwrap()
        .join("previous-layout")
        .join(&legacy.slot.id);
    fs::create_dir_all(previous_layout.parent().unwrap()).unwrap();
    fs::rename(old_lane, &previous_layout).unwrap();
    legacy.slot.target = previous_layout.join("target");
    legacy.slot.layout = None;
    fs::write(
        previous_layout.join(MARKER),
        serde_json::to_vec(&pool.marker(&legacy.slot)).unwrap(),
    )
    .unwrap();
    pool.save(&legacy.slot).unwrap();
    let fresh = pool.allocate(&checkout, "fresh-layout", "legacy").unwrap();
    assert_ne!(
        fresh.slot.id, legacy.slot.id,
        "old layouts must not be adopted"
    );
    evict_idle_pool(&pool).unwrap();
    assert_eq!(
        fs::read(legacy.slot.target.join("old-data")).unwrap(),
        b"preserve old lane"
    );
    let mut invalid = pool.allocate(&checkout, "foreign-repository", "b").unwrap();
    fs::write(invalid.slot.target.join("data"), "unowned").unwrap();
    close(&mut invalid);
    invalid.slot.repository_key = "another-repository".into();
    pool.save(&invalid.slot).unwrap();
    assert!(evict_idle_pool(&pool).is_err());
    assert!(invalid.slot.target.join("data").is_file());
}

#[test]
#[ignore = "bounded installed Cargo qualification; run explicitly, offline, outside the ordinary CI gate"]
fn native_cargo_storage_qualification() {
    const MODE: &str = "STATEROOT_BUILD_NATIVE_CHILD";
    if std::env::var(MODE).as_deref() == Ok("consumer") {
        let _file =
            fs::File::open(std::env::var_os("STATEROOT_BUILD_NATIVE_FILE").unwrap()).unwrap();
        let release = PathBuf::from(std::env::var_os("STATEROOT_BUILD_NATIVE_RELEASE").unwrap());
        let deadline = Instant::now() + Duration::from_secs(20);
        while !release.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        return;
    }
    if std::env::var(MODE).as_deref() == Ok("run") {
        native_exercise();
        return;
    }
    let base = std::env::var_os("STATEROOT_BUILD_STORAGE_PROOF_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    fs::create_dir_all(&base).unwrap();
    let root = tempfile::Builder::new()
        .prefix("stateroot-n6b-native-")
        .tempdir_in(base)
        .unwrap()
        .keep();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "build_storage::tests::native_cargo_storage_qualification",
            "--nocapture",
        ])
        .env(MODE, "run")
        .env("STATEROOT_BUILD_NATIVE_ROOT", &root)
        .env("STATEROOT_BUILD_CACHE_ROOT", root.join("managed-outputs"))
        .env("STATEROOT_BUILD_CACHE_MAX_BYTES", "1073741824")
        .env("STATEROOT_BUILD_CACHE_WARM_SLOTS", "2")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove(HINT)
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .env_remove("CARGO_BUILD_BUILD_DIR")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .status()
        .unwrap();
    eprintln!("N6b native proof retained at {}", root.display());
    assert!(
        status.success(),
        "native qualification failed; retained evidence is above"
    );
}

fn native_exercise() {
    let root = PathBuf::from(std::env::var_os("STATEROOT_BUILD_NATIVE_ROOT").unwrap());
    let owner = root.join("project");
    fs::create_dir_all(owner.join("src")).unwrap();
    git2::Repository::init(&owner).unwrap();
    local_store::init_skeleton(
        &owner,
        "native-build-proof",
        "Native build proof",
        "default",
    )
    .unwrap();
    let lock: toml::Value = toml::from_str(include_str!("../../../Cargo.lock")).unwrap();
    let version = lock["package"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["name"].as_str() == Some("serde_json"))
        .unwrap()["version"]
        .as_str()
        .unwrap();
    fs::write(owner.join("Cargo.toml"),format!("[package]\nname='native_lane'\nversion='0.0.0'\nedition='2021'\n[dependencies]\nserde_json='={version}'\n")).unwrap();
    write_native_source(&owner, "owner");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let lock_output = Command::new(&cargo)
        .args(["generate-lockfile", "--offline"])
        .current_dir(&owner)
        .env("RUSTUP_AUTO_INSTALL", "0")
        .output()
        .unwrap();
    assert!(
        lock_output.status.success(),
        "{}",
        String::from_utf8_lossy(&lock_output.stderr)
    );
    let (base, _) = crate::roots::create_root(&owner, "test", "native source", None).unwrap();
    let mut forks = Vec::new();
    for label in ["a", "b", "c"] {
        let name = format!("proof-{label}");
        let checkout = root.join(&name);
        crate::roots::fork_root(&owner, &base.id, Some(&name), "test").unwrap();
        crate::roots::fork_materialize(&owner, &name, &checkout, None).unwrap();
        write_native_source(&checkout, label);
        forks.push(checkout);
    }
    fs::create_dir(root.join("logs")).unwrap();
    let mut baseline = Vec::new();
    let mut baseline_targets = Vec::new();
    for (n, checkout) in forks.iter().enumerate() {
        let target = root.join(format!("baseline-{n}"));
        let mut command = Command::new(&cargo);
        command.env("CARGO_TARGET_DIR", &target);
        let started = Instant::now();
        let (mut child, log) =
            spawn_native_build(command, checkout, &root, &format!("baseline-{n}"));
        assert!(child.wait().unwrap().success());
        let binary = native_artifact(&log);
        assert_native_output(&binary, ["a", "b", "c"][n], false);
        baseline.push(json!({"elapsed_ms":started.elapsed().as_millis(),"bytes":output_seal(&target).unwrap().1}));
        baseline_targets.push(target);
    }
    let baseline_bytes = baseline
        .iter()
        .map(|value| value["bytes"].as_u64().unwrap())
        .sum::<u64>();
    let mut sequential = Vec::new();
    let mut first_id = None;
    let mut observed = None;
    for (step, n) in [0usize, 1, 2, 0].into_iter().enumerate() {
        let prepare = Instant::now();
        let storage = prepare_execution(&owner, &forks[n], &format!("sequential-{step}"));
        assert_eq!(
            storage.disposition().state,
            "active",
            "{:?}",
            storage.disposition()
        );
        let prepare_ms = prepare.elapsed().as_millis();
        if let Some(id) = &first_id {
            assert_eq!(storage.disposition().cache_id.as_ref(), Some(id));
        } else {
            first_id = storage.disposition().cache_id.clone();
        }
        let mut command = Command::new(&cargo);
        command.envs(storage.environment());
        let started = Instant::now();
        let (mut child, log) =
            spawn_native_build(command, &forks[n], &root, &format!("managed-{step}"));
        assert!(child.wait().unwrap().success());
        let binary = native_artifact(&log);
        assert_native_output(&binary, ["a", "b", "c"][n], false);
        if step == 0 {
            let plan = crate::plans::record(
                &forks[n],
                "source-bound native result",
                "test",
                None,
                "# Check native source A",
            )
            .unwrap();
            crate::roots::create_root(&forks[n], "test", "built source", None).unwrap();
            let record = crate::verification::start(
                &forks[n],
                &plan.id,
                "test",
                &[binary.to_string_lossy().into_owned()],
            )
            .unwrap();
            let output = Command::new(&binary).output().unwrap();
            assert!(output.status.success());
            let directory = crate::verification::output_directory(&forks[n]).unwrap();
            fs::create_dir_all(&directory).unwrap();
            let path = directory.join(format!("{}.stdout", record.id));
            fs::write(&path, &output.stdout).unwrap();
            let finished = crate::verification::finish(
                &forks[n],
                record,
                Some(0),
                None,
                vec![crate::verification::output_digest(&path).unwrap()],
            )
            .unwrap();
            fs::write(
                root.join("source-observation.json"),
                serde_json::to_vec_pretty(&finished).unwrap(),
            )
            .unwrap();
            fs::write(root.join("source-observation.stdout"), &output.stdout).unwrap();
            observed = Some((plan.id, finished));
        }
        let build_ms = started.elapsed().as_millis();
        let finish = Instant::now();
        let disposition = storage.finish();
        assert_eq!(disposition.state, "idle", "{disposition:?}");
        sequential.push(json!({"prepare_ms":prepare_ms,"build_and_consume_ms":build_ms,"finish_ms":finish.elapsed().as_millis(),"bytes":disposition.bytes}));
    }
    let (plan, record) = observed.unwrap();
    assert_eq!(
        crate::verification::validate(&forks[0], &plan, &record.id)
            .unwrap()
            .product_tree,
        record.product_tree
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(root.join("source-observation.stdout")).unwrap())
            .unwrap()["source"],
        "a"
    );
    for checkout in &forks[..2] {
        fs::create_dir(checkout.join(".cargo")).unwrap();
        fs::write(
            checkout.join(".cargo/config.toml"),
            "[build]\nrustflags=['--cfg','native_variant','--check-cfg','cfg(native_variant)']\n",
        )
        .unwrap();
    }
    let baseline_started = Instant::now();
    let mut baseline_parallel = Vec::new();
    for (n, checkout) in forks[..2].iter().enumerate() {
        let target = root.join(format!("baseline-parallel-{n}"));
        baseline_targets.push(target.clone());
        let mut command = Command::new(&cargo);
        command.env("CARGO_TARGET_DIR", target);
        baseline_parallel.push(spawn_native_build(
            command,
            checkout,
            &root,
            &format!("baseline-parallel-{n}"),
        ));
    }
    for (n, (child, log)) in baseline_parallel.iter_mut().enumerate() {
        assert!(child.wait().unwrap().success());
        assert_native_output(&native_artifact(log), ["a", "b"][n], true);
    }
    let baseline_parallel_ms = baseline_started.elapsed().as_millis();
    let baseline_peak_bytes = baseline_targets
        .iter()
        .map(|target| output_seal(target).unwrap().1)
        .sum::<u64>();
    let baseline_allocated = allocated_output_bytes(&baseline_targets);
    let prepare = Instant::now();
    let one = prepare_execution(&owner, &forks[0], "parallel-one");
    let two = prepare_execution(&owner, &forks[1], "parallel-two");
    assert_eq!(one.disposition().state, "active");
    assert_eq!(two.disposition().state, "active");
    assert_ne!(one.disposition().cache_id, two.disposition().cache_id);
    assert_ne!(one.disposition().cache_id, first_id);
    let parallel_prepare_ms = prepare.elapsed().as_millis();
    let parallel_started = Instant::now();
    let mut children = Vec::new();
    for (n, storage) in [&one, &two].into_iter().enumerate() {
        let mut command = Command::new(&cargo);
        command.envs(storage.environment());
        children.push(spawn_native_build(
            command,
            &forks[n],
            &root,
            &format!("managed-parallel-{n}"),
        ));
    }
    for (n, (child, log)) in children.iter_mut().enumerate() {
        assert!(child.wait().unwrap().success());
        assert_native_output(&native_artifact(log), ["a", "b"][n], true);
    }
    let managed_parallel_ms = parallel_started.elapsed().as_millis();
    let mut pool = Pool::new(&owner, Policy::from_env().unwrap()).unwrap();
    let peak_bytes = pool
        .records()
        .unwrap()
        .iter()
        .map(|slot| output_seal(&slot.target).unwrap().1)
        .sum::<u64>();
    let managed_peak_allocated = allocated_output_bytes(
        &pool
            .records()
            .unwrap()
            .into_iter()
            .map(|slot| slot.target)
            .collect::<Vec<_>>(),
    );
    assert_eq!(one.finish().state, "idle");
    assert_eq!(two.finish().state, "idle");
    for _ in 0..32 {
        if pool
            .records()
            .unwrap()
            .iter()
            .all(|slot| slot.state != "evicting")
        {
            break;
        }
        evict_idle_pool(&pool).unwrap();
    }
    let records = pool.records().unwrap();
    assert_eq!(
        records.iter().filter(|slot| slot.state == "idle").count(),
        2
    );
    let steady_bytes = records
        .iter()
        .map(|slot| output_seal(&slot.target).unwrap().1)
        .sum::<u64>();
    let managed_steady_allocated = allocated_output_bytes(
        &records
            .iter()
            .map(|slot| slot.target.clone())
            .collect::<Vec<_>>(),
    );
    for checkout in &forks {
        assert!(!checkout.join("target").exists());
    }
    fs::create_dir(forks[2].join(".cargo")).unwrap();
    let config = toml::to_string(&toml::toml! { [build] target-dir = "human-output" }).unwrap();
    fs::write(forks[2].join(".cargo/config.toml"), &config).unwrap();
    let override_storage = prepare_execution(&owner, &forks[2], "native-output-override");
    assert_eq!(override_storage.disposition().state, "unmanaged");
    assert!(override_storage.environment().is_empty());
    assert_eq!(
        fs::read_to_string(forks[2].join(".cargo/config.toml")).unwrap(),
        config
    );
    fs::write(
        forks[2].join(".cargo/config.toml"),
        "[build]\nrustflags=['-C','opt-level=1']\n",
    )
    .unwrap();
    let consumer_storage = prepare_execution(&owner, &forks[2], "live-binary-consumer");
    assert_eq!(consumer_storage.disposition().state, "active");
    let consumer_path = consumer_storage
        .disposition()
        .path
        .clone()
        .unwrap()
        .join("consumer.data");
    fs::write(&consumer_path, b"x").unwrap();
    let release = root.join("consumer-release");
    let mut consumer = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "build_storage::tests::native_cargo_storage_qualification",
        ])
        .env("STATEROOT_BUILD_NATIVE_CHILD", "consumer")
        .env("STATEROOT_BUILD_NATIVE_FILE", &consumer_path)
        .env("STATEROOT_BUILD_NATIVE_RELEASE", &release)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let live = consumer_storage.finish();
    assert_eq!(live.state, "unknown");
    pool.policy.max_bytes = 0;
    pool.policy.warm_slots = 0;
    for _ in 0..64 {
        evict_idle_pool(&pool).unwrap();
        if pool
            .records()
            .unwrap()
            .iter()
            .all(|slot| !matches!(slot.state.as_str(), "idle" | "evicting"))
        {
            break;
        }
    }
    assert!(consumer_path.is_file());
    fs::write(&release, b"go").unwrap();
    assert!(consumer.wait().unwrap().success());
    for target in baseline_targets {
        fs::remove_dir_all(target).unwrap();
    }
    let summary = json!({"scope":"offline Rust/serde fixture; actual StateRoot forks, no model calls; not a whole-project performance claim",
            "baseline_sequential":baseline,"baseline_historical_apparent_bytes":baseline_bytes,"baseline_peak_apparent_bytes":baseline_peak_bytes,"baseline_peak_allocated_bytes":baseline_allocated,"managed_sequential":sequential,
            "baseline_cold_parallel_ms":baseline_parallel_ms,"managed_parallel_prepare_ms":parallel_prepare_ms,"managed_cold_parallel_ms":managed_parallel_ms,
            "managed_peak_apparent_bytes":peak_bytes,"managed_steady_apparent_bytes":steady_bytes,"managed_peak_allocated_bytes":managed_peak_allocated,"managed_steady_allocated_bytes":managed_steady_allocated,"warm_lanes":2,"max_active_lanes":2,
            "source_observation":record,"consumer_during_budget_pressure":live,"retained_unknown_bytes":1,
            "toolchain":String::from_utf8(probe(std::env::var_os("RUSTC").unwrap_or_else(||"rustc".into()),&["-vV"],&owner).unwrap()).unwrap()});
    fs::write(
        root.join("measurements.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&summary).unwrap());
}

fn allocated_output_bytes(targets: &[PathBuf]) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mut seen = BTreeSet::new();
        let mut bytes = 0u64;
        for target in targets {
            let tree = output_tree(target).ok()?;
            for relative in tree.entries.keys() {
                let meta = fs::symlink_metadata(target.join(relative)).ok()?;
                if seen.insert((meta.dev(), meta.ino())) {
                    bytes = bytes.saturating_add(meta.blocks().saturating_mul(512));
                }
            }
        }
        Some(bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = targets;
        None
    }
}

fn write_native_source(project: &Path, label: &str) {
    fs::write(project.join("src/main.rs"),format!("#![allow(unexpected_cfgs)]\nfn main() {{println!(\"{{}}\",serde_json::json!({{\"source\":\"{label}\",\"configured\":cfg!(native_variant)}}));}}\n")).unwrap();
}
fn spawn_native_build(
    mut command: Command,
    checkout: &Path,
    root: &Path,
    label: &str,
) -> (std::process::Child, PathBuf) {
    let log = root.join("logs").join(format!("{label}.log"));
    let file = fs::OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(&log)
        .unwrap();
    command
        .args(["build", "--offline", "--message-format=json"])
        .current_dir(checkout)
        .env("RUSTUP_AUTO_INSTALL", "0")
        .stdout(file.try_clone().unwrap())
        .stderr(file);
    (command.spawn().unwrap(), log)
}
fn native_artifact(log: &Path) -> PathBuf {
    fs::read_to_string(log)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|value| {
            (value["reason"] == "compiler-artifact" && value["target"]["name"] == "native_lane")
                .then(|| value["executable"].as_str().map(PathBuf::from))
                .flatten()
        })
        .expect("Cargo-reported native executable")
}
fn assert_native_output(binary: &Path, source: &str, configured: bool) {
    let output = Command::new(binary).output().unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["source"], source);
    assert_eq!(value["configured"], configured);
}
