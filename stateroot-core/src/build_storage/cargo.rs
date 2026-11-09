//! Native Cargo qualification and workspace-artifact invalidation.
use super::{Slot, HINT, SCHEMA};
use crate::safe_io;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Seek},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn inherited_managed_target() -> bool {
    let Some(target) = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from) else {
        return false;
    };
    let Ok(hint) = std::env::var(HINT).and_then(|raw| {
        serde_json::from_str::<Value>(&raw).map_err(|_| std::env::VarError::NotPresent)
    }) else {
        return false;
    };
    let Some(record) = hint["record"].as_str() else {
        return false;
    };
    let Ok(slot) = fs::read(record)
        .map_err(|_| ())
        .and_then(|bytes| serde_json::from_slice::<Slot>(&bytes).map_err(|_| ()))
    else {
        return false;
    };
    slot.schema_version == SCHEMA
        && matches!(slot.state.as_str(), "active" | "unknown")
        && slot.target == target
        && slot.owner.as_ref().is_some_and(|owner| {
            hint["token"].as_str() == Some(owner.token.as_str())
                && safe_io::host_namespace() == Some(owner.namespace.as_str())
        })
}

pub(super) fn compatibility(checkout: &Path) -> Result<(String, Vec<String>), String> {
    if std::env::var_os("CARGO_TARGET_DIR").is_some() && !inherited_managed_target() {
        return Err("explicit CARGO_TARGET_DIR preserved; output is unmanaged".into());
    }
    for key in [
        "CARGO_BUILD_TARGET_DIR",
        "CARGO_BUILD_BUILD_DIR",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        if std::env::var_os(key).is_some_and(|value| !value.is_empty()) {
            return Err(format!(
                "native {key} override preserved; managed output is not qualified"
            ));
        }
    }
    let mut digest = Sha256::new();
    digest.update(probe(
        std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()),
        &["-vV"],
        checkout,
    )?);
    digest.update(probe(
        std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()),
        &["-vV"],
        checkout,
    )?);
    let home = crate::harness_install::home_dir().map_err(|e| e.to_string())?;
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cargo"));
    let mut configs = BTreeSet::new();
    for ancestor in checkout.ancestors() {
        configs.insert(ancestor.join(".cargo/config"));
        configs.insert(ancestor.join(".cargo/config.toml"));
    }
    configs.insert(cargo_home.join("config"));
    configs.insert(cargo_home.join("config.toml"));
    for config in configs {
        let bytes = match fs::read(&config) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("native configuration unavailable: {error}")),
        };
        let value: toml::Value = toml::from_str(
            std::str::from_utf8(&bytes)
                .map_err(|_| "native Cargo configuration encoding unavailable")?,
        )
        .map_err(|e| e.to_string())?;
        if value.get("build").is_some_and(|build| {
            build.get("target-dir").is_some() || build.get("build-dir").is_some()
        }) || value
            .get("env")
            .is_some_and(|env| env.get("CARGO_TARGET_DIR").is_some())
        {
            return Err(format!(
                "native output configuration in {} preserved; output is unmanaged",
                config.display()
            ));
        }
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    for name in ["rust-toolchain", "rust-toolchain.toml"] {
        for ancestor in checkout.ancestors() {
            if let Ok(bytes) = fs::read(ancestor.join(name)) {
                digest.update(bytes);
                break;
            }
        }
    }
    let mut environment = std::env::vars_os()
        .filter(|(key, _)| {
            let key = key.to_string_lossy().to_ascii_uppercase();
            (key.starts_with("CARGO_BUILD_")
                || key.starts_with("CARGO_TARGET_")
                || key.starts_with("CARGO_PROFILE_")
                || key.starts_with("RUST")
                || [
                    "CC_",
                    "CXX_",
                    "AR_",
                    "CFLAGS_",
                    "CXXFLAGS_",
                    "LDFLAGS_",
                    "BINDGEN_EXTRA_CLANG_ARGS",
                ]
                .iter()
                .any(|prefix| key.starts_with(prefix))
                || matches!(
                    key.as_ref(),
                    "PATH"
                        | "CC"
                        | "CXX"
                        | "AR"
                        | "CFLAGS"
                        | "CXXFLAGS"
                        | "LDFLAGS"
                        | "SDKROOT"
                        | "LIBRARY_PATH"
                        | "INCLUDE"
                        | "LIB"
                        | "LIBPATH"
                        | "VCINSTALLDIR"
                        | "VCTOOLSINSTALLDIR"
                        | "WINDOWSSDKDIR"
                        | "VSINSTALLDIR"
                        | "VSCMD_ARG_TGT_ARCH"
                        | "CL"
                        | "_CL_"
                        | "LINK"
                        | "_LINK_"
                ))
                && key != "CARGO_TARGET_DIR"
        })
        .collect::<Vec<_>>();
    environment.sort();
    for (key, value) in environment {
        digest.update(key.as_encoded_bytes());
        digest.update([0]);
        digest.update(value.as_encoded_bytes());
        digest.update([0]);
    }
    let packages = workspace_identity(checkout, &mut digest)?;
    // Cargo retains authority for per-invocation features/profiles and source
    // fingerprints. A lane is exclusive; this envelope is not a new build system.
    Ok((format!("{:x}", digest.finalize()), packages))
}

pub(super) fn probe(
    program: std::ffi::OsString,
    args: &[&str],
    cwd: &Path,
) -> Result<Vec<u8>, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env("RUSTUP_AUTO_INSTALL", "0");
    let bytes = capture_native(command, 65536, Duration::from_secs(5))?;
    if bytes.is_empty() {
        return Err("installed toolchain identity output unavailable".into());
    }
    Ok(bytes)
}

fn capture_native(mut command: Command, cap: u64, budget: Duration) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut stdout = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut stderr = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone().map_err(|e| e.to_string())?)
        .stderr(stderr.try_clone().map_err(|e| e.to_string())?)
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + budget;
    let result = (|| {
        loop {
            if stdout.metadata().map_err(|e| e.to_string())?.len() > cap
                || stderr.metadata().map_err(|e| e.to_string())?.len() > cap
            {
                return Err("native qualification output exceeded bound".into());
            }
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(status) if status.success() => break,
                Some(_) => {
                    stderr.rewind().map_err(|e| e.to_string())?;
                    let mut bytes = Vec::new();
                    stderr
                        .by_ref()
                        .take(cap + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|e| e.to_string())?;
                    return Err(format!(
                        "native Cargo/tool qualification failed: {}",
                        String::from_utf8_lossy(&bytes)
                            .chars()
                            .take(2000)
                            .collect::<String>()
                    ));
                }
                None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(15)),
                None => return Err("native qualification timed out".into()),
            }
        }
        stdout.rewind().map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        stdout
            .by_ref()
            .take(cap + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > cap {
            return Err("native qualification output exceeded bound".into());
        }
        Ok(bytes)
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn workspace_identity(checkout: &Path, digest: &mut Sha256) -> Result<Vec<String>, String> {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .args([
            "metadata",
            "--no-deps",
            "--locked",
            "--offline",
            "--format-version",
            "1",
        ])
        .current_dir(checkout)
        .env("RUSTUP_AUTO_INSTALL", "0")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove(HINT);
    let metadata: Value = serde_json::from_slice(&capture_native(
        command,
        4 * 1024 * 1024,
        Duration::from_secs(5),
    )?)
    .map_err(|e| e.to_string())?;
    let workspace = PathBuf::from(
        metadata["workspace_root"]
            .as_str()
            .ok_or("Cargo workspace identity unavailable")?,
    );
    if crate::path_identity::equivalent_project_key(&workspace)
        != crate::path_identity::equivalent_project_key(checkout)
    {
        return Err(
            "Cargo workspace extends beyond the execution project; output is unmanaged".into(),
        );
    }
    let packages = metadata["packages"]
        .as_array()
        .ok_or("Cargo workspace members unavailable")?;
    let mut names = BTreeSet::new();
    let mut manifests = BTreeSet::from([workspace.join("Cargo.toml")]);
    let mut directories = BTreeSet::new();
    for package in packages {
        names.insert(
            package["name"]
                .as_str()
                .ok_or("Cargo package name unavailable")?
                .to_owned(),
        );
        let manifest = PathBuf::from(
            package["manifest_path"]
                .as_str()
                .ok_or("Cargo member manifest unavailable")?,
        );
        directories.insert(
            fs::canonicalize(
                manifest
                    .parent()
                    .ok_or("Cargo member directory unavailable")?,
            )
            .map_err(|e| e.to_string())?,
        );
        manifests.insert(manifest);
    }
    for package in packages {
        if let Some(dependencies) = package["dependencies"].as_array() {
            for dependency in dependencies {
                if let Some(path) = dependency["path"].as_str() {
                    let canonical = fs::canonicalize(path).map_err(|e| e.to_string())?;
                    if !directories.contains(&canonical) {
                        return Err("mutable path dependency outside the root Cargo workspace is unqualified; native output is unmanaged".into());
                    }
                }
            }
        }
    }
    manifests.insert(workspace.join("Cargo.lock"));
    for manifest in manifests {
        let relative = manifest
            .strip_prefix(&workspace)
            .map_err(|_| "Cargo member lies outside the project workspace")?;
        digest.update(relative.as_os_str().as_encoded_bytes());
        digest.update([0]);
        match fs::File::open(&manifest) {
            Ok(mut file) => {
                digest.update([1]);
                let deadline = Instant::now() + Duration::from_secs(1);
                let mut buffer = [0; 65536];
                loop {
                    if Instant::now() > deadline {
                        return Err("Cargo manifest fingerprint budget exceeded".into());
                    }
                    let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
                    if read == 0 {
                        break;
                    }
                    digest.update(&buffer[..read]);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => digest.update([0]),
            Err(error) => return Err(error.to_string()),
        }
        digest.update([0]);
    }
    Ok(names.into_iter().collect())
}

pub(super) fn reset_workspace_products(
    checkout: &Path,
    target: &Path,
    packages: &[String],
) -> Result<(), String> {
    // Cargo can report a different worktree fresh against timestamp-based local
    // outputs. Clean only mutable workspace products; registry dependencies stay
    // warm. --locked/--offline forbid source-lock changes and network activity.
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .args(["clean", "--locked", "--offline", "--target-dir"])
        .arg(target)
        .current_dir(checkout)
        .env("CARGO_TARGET_DIR", target)
        .env("RUSTUP_AUTO_INSTALL", "0");
    if packages.is_empty() {
        return Err("native workspace package selection unavailable".into());
    }
    for package in packages {
        command.args(["--package", package]);
    }
    capture_native(command, 65536, Duration::from_secs(30)).map(|_| ())
}
