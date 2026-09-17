//! VS Code / Cursor extension discovery and reconciliation.
//!
//! CLI-owned: detect working editor launchers, compare installed extension
//! semver against the release-declared VSIX, and install only when missing
//! or stale. Exact and newer stay untouched. One editor's failure never
//! rolls back the CLI or the other editor.

use std::cmp::Ordering;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context as _};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::update::{self, ReleaseInfo};
use super::{note, Ctx};

/// Marketplace / Open VSX extension id. Comparisons are case-insensitive.
pub const EXTENSION_ID: &str = "CognizTech.stateroot";
const MANIFEST_ASSET: &str = "stateroot-extension.json";
const LOCK_NAME: &str = "editor-reconcile.lock";
const LAST_FAILURE_NAME: &str = "editor-last-failure.json";
const LOCK_STALE: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorKind {
    VsCode,
    Cursor,
}

impl EditorKind {
    fn label(self) -> &'static str {
        match self {
            Self::VsCode => "VS Code",
            Self::Cursor => "Cursor",
        }
    }

    fn bin(self) -> &'static str {
        match self {
            Self::VsCode => "code",
            Self::Cursor => "cursor",
        }
    }

    fn test_env(self) -> &'static str {
        match self {
            Self::VsCode => "STATEROOT_TEST_EDITOR_CODE",
            Self::Cursor => "STATEROOT_TEST_EDITOR_CURSOR",
        }
    }

    fn all() -> [Self; 2] {
        [Self::VsCode, Self::Cursor]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionState {
    NotDetected,
    Unavailable { installed: Option<String> },
    Missing { desired: String },
    Stale { installed: String, desired: String },
    Exact { version: String },
    Newer { installed: String, desired: String },
    ProbeError { detail: String },
}

impl ExtensionState {
    fn slug(&self) -> &'static str {
        match self {
            Self::NotDetected => "not-detected",
            Self::Unavailable { .. } => "unavailable",
            Self::Missing { .. } => "missing",
            Self::Stale { .. } => "stale",
            Self::Exact { .. } => "exact",
            Self::Newer { .. } => "newer",
            Self::ProbeError { .. } => "error",
        }
    }

    fn needs_install(&self) -> bool {
        matches!(self, Self::Missing { .. } | Self::Stale { .. })
    }

    fn is_healthy(&self) -> bool {
        matches!(
            self,
            Self::NotDetected | Self::Unavailable { .. } | Self::Exact { .. } | Self::Newer { .. }
        )
    }
}

#[derive(Debug, Clone)]
pub struct EditorReport {
    pub kind: EditorKind,
    pub launcher: Option<PathBuf>,
    pub installed: Option<String>,
    pub state: ExtensionState,
    pub mutated: bool,
}

#[derive(Debug, Clone)]
struct ExtensionManifest {
    extension_version: String,
    vsix_filename: String,
    digest: String,
}

enum Mode {
    Status,
    Explicit,
    Automatic { quiet: bool },
}

struct FileLock {
    path: PathBuf,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// `stateroot editor status` — read-only.
pub async fn status(ctx: &Ctx) -> anyhow::Result<()> {
    let reports = inspect(ctx, Mode::Status).await?;
    print_matrix(&reports, ctx);
    Ok(())
}

/// `stateroot editor reconcile` — intentional mutation; nonzero when a
/// detected target fails to converge.
pub async fn reconcile(ctx: &Ctx) -> anyhow::Result<i32> {
    let reports = inspect(ctx, Mode::Explicit).await?;
    print_matrix(&reports, ctx);
    if reports
        .iter()
        .any(|r| !matches!(r.state, ExtensionState::NotDetected) && !r.state.is_healthy())
    {
        Ok(1)
    } else {
        Ok(0)
    }
}

/// After `stateroot install`: best-effort, never fails the CLI install.
pub async fn reconcile_after_install(ctx: &Ctx) {
    if !should_auto_reconcile() {
        return;
    }
    match inspect(ctx, Mode::Automatic { quiet: false }).await {
        Ok(reports) => {
            if reports
                .iter()
                .any(|r| !matches!(r.state, ExtensionState::NotDetected))
            {
                print_matrix(&reports, ctx);
            }
        }
        Err(err) => note!(
            "warning: editor reconciliation failed ({err:#}) — retry with `stateroot editor reconcile`"
        ),
    }
}

/// After an already-current scheduled CLI check. Honors the auto-update gate.
pub async fn reconcile_after_current_cli(ctx: &Ctx) {
    if update::disabled(ctx) || !should_auto_reconcile() {
        return;
    }
    let _ = inspect(ctx, Mode::Automatic { quiet: true }).await;
}

/// Soft doctor rows (never hard-fail the CLI).
pub async fn doctor_checks(ctx: &Ctx) -> Vec<(String, bool, String)> {
    match inspect(ctx, Mode::Status).await {
        Ok(reports) => reports
            .into_iter()
            .map(|r| {
                (
                    format!("editor ({})", r.kind.label()),
                    r.state.is_healthy(),
                    status_detail(&r),
                )
            })
            .collect(),
        Err(err) => vec![(
            "editor".into(),
            false,
            format!("{err:#}; retry with `stateroot editor reconcile`"),
        )],
    }
}

async fn inspect(ctx: &Ctx, mode: Mode) -> anyhow::Result<Vec<EditorReport>> {
    let mutate = !matches!(mode, Mode::Status);
    let quiet = matches!(mode, Mode::Automatic { quiet: true });
    let _lock = if mutate {
        Some(acquire_lock(&ctx.config_dir.join(LOCK_NAME))?)
    } else {
        None
    };

    let release = if allow_release_lookup() {
        update::selected_channel_release(ctx, matches!(mode, Mode::Explicit)).await
    } else {
        None
    };
    let desired_pkg = match &release {
        Some(info) => match resolve_manifest(ctx, info).await {
            Ok(Some(manifest)) => Some(manifest),
            Ok(None) => None,
            Err(err) if mutate && !quiet => {
                note!("warning: extension release metadata: {err:#}");
                None
            }
            Err(_) => None,
        },
        None => None,
    };
    let desired = desired_pkg.as_ref().map(|m| m.extension_version.clone());

    let mut reports = Vec::new();
    for kind in EditorKind::all() {
        reports.push(probe_one(kind, desired.as_deref()));
    }

    if mutate {
        if let Some(manifest) = desired_pkg.as_ref() {
            if reports.iter().any(|r| r.state.needs_install()) {
                match ensure_vsix(ctx, release.as_ref(), manifest).await {
                    Ok(vsix) => {
                        for report in &mut reports {
                            if report.state.needs_install() {
                                apply_install(report, &vsix, &manifest.extension_version);
                            }
                        }
                    }
                    Err(err) => {
                        for report in &mut reports {
                            if report.state.needs_install() {
                                report.state = ExtensionState::ProbeError {
                                    detail: format!("VSIX unavailable ({err:#})"),
                                };
                            }
                        }
                    }
                }
            }
        } else {
            for report in &mut reports {
                if !matches!(
                    report.state,
                    ExtensionState::NotDetected | ExtensionState::ProbeError { .. }
                ) {
                    report.state = ExtensionState::Unavailable {
                        installed: report.installed.clone(),
                    };
                }
            }
        }
    }

    persist_failures(ctx, &reports);
    Ok(reports)
}

fn probe_one(kind: EditorKind, desired: Option<&str>) -> EditorReport {
    match find_launcher(kind) {
        None => EditorReport {
            kind,
            launcher: None,
            installed: None,
            state: ExtensionState::NotDetected,
            mutated: false,
        },
        Some(Err(detail)) => EditorReport {
            kind,
            launcher: None,
            installed: None,
            state: ExtensionState::ProbeError { detail },
            mutated: false,
        },
        Some(Ok(launcher)) => match list_installed(&launcher) {
            Err(detail) => EditorReport {
                kind,
                launcher: Some(launcher),
                installed: None,
                state: ExtensionState::ProbeError { detail },
                mutated: false,
            },
            Ok(installed) => {
                let state = classify(installed.as_deref(), desired);
                EditorReport {
                    kind,
                    launcher: Some(launcher),
                    installed,
                    state,
                    mutated: false,
                }
            }
        },
    }
}

fn apply_install(report: &mut EditorReport, vsix: &Path, desired: &str) {
    let Some(launcher) = report.launcher.as_ref() else {
        return;
    };
    match install_vsix(launcher, vsix) {
        Err(detail) => {
            report.state = ExtensionState::ProbeError { detail };
        }
        Ok(()) => match list_installed(launcher) {
            Err(detail) => {
                report.state = ExtensionState::ProbeError { detail };
            }
            Ok(installed) => {
                report.installed = installed.clone();
                report.state = classify(installed.as_deref(), Some(desired));
                report.mutated = report.state.slug() == "exact";
                if !matches!(report.state, ExtensionState::Exact { .. }) {
                    report.state = ExtensionState::ProbeError {
                        detail: format!(
                            "install completed but version is still {}",
                            installed.unwrap_or_else(|| "missing".into())
                        ),
                    };
                    report.mutated = false;
                }
            }
        },
    }
}

fn classify(installed: Option<&str>, desired: Option<&str>) -> ExtensionState {
    let Some(desired) = desired else {
        return ExtensionState::Unavailable {
            installed: installed.map(str::to_string),
        };
    };
    let Some(installed) = installed else {
        return ExtensionState::Missing {
            desired: desired.to_string(),
        };
    };
    match version_cmp(installed, desired) {
        None => ExtensionState::ProbeError {
            detail: format!("unparseable version {installed} vs {desired}"),
        },
        Some(Ordering::Less) => ExtensionState::Stale {
            installed: installed.to_string(),
            desired: desired.to_string(),
        },
        Some(Ordering::Equal) => ExtensionState::Exact {
            version: installed.to_string(),
        },
        Some(Ordering::Greater) => ExtensionState::Newer {
            installed: installed.to_string(),
            desired: desired.to_string(),
        },
    }
}

fn version_cmp(left: &str, right: &str) -> Option<Ordering> {
    Some(update::parse_semver(left)?.cmp(&update::parse_semver(right)?))
}

fn parse_installed_version(list_output: &str) -> Option<String> {
    let want = EXTENSION_ID.to_ascii_lowercase();
    for line in list_output.lines() {
        let line = line.trim();
        let Some((id, ver)) = line.rsplit_once('@') else {
            continue;
        };
        if id.to_ascii_lowercase() == want {
            let ver = ver.trim();
            if !ver.is_empty() {
                return Some(ver.to_string());
            }
        }
    }
    None
}

fn find_launcher(kind: EditorKind) -> Option<Result<PathBuf, String>> {
    if let Some(override_path) = test_editor_override(kind) {
        return validate_candidate(&override_path);
    }
    if skip_host_editor_scan() {
        return None;
    }
    let mut first_error: Option<String> = None;
    for candidate in discover_candidates(kind) {
        match validate_candidate(&candidate) {
            Some(Ok(path)) => return Some(Ok(path)),
            Some(Err(err)) => {
                first_error.get_or_insert(err);
            }
            None => {}
        }
    }
    first_error.map(Err)
}

fn validate_candidate(path: &Path) -> Option<Result<PathBuf, String>> {
    if !path.is_file() {
        return None;
    }
    match list_installed(path) {
        Ok(_) => Some(Ok(path.to_path_buf())),
        Err(err) => Some(Err(format!("{}: {err}", path.display()))),
    }
}

fn discover_candidates(kind: EditorKind) -> Vec<PathBuf> {
    let mut out = Vec::new();
    out.extend(path_candidates(kind));
    out.extend(known_locations(kind));
    out
}

fn path_candidates(kind: EditorKind) -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for dir in std::env::split_paths(&path) {
        for name in binary_names(kind) {
            out.push(dir.join(name));
        }
    }
    out
}

fn binary_names(kind: EditorKind) -> Vec<String> {
    let bin = kind.bin();
    if cfg!(windows) {
        vec![
            format!("{bin}.cmd"),
            format!("{bin}.exe"),
            format!("{bin}.bat"),
            bin.to_string(),
        ]
    } else {
        vec![bin.to_string()]
    }
}

fn known_locations(kind: EditorKind) -> Vec<PathBuf> {
    let home = dirs_home();
    if cfg!(windows) {
        windows_known(
            kind,
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .as_deref(),
            std::env::var_os("ProgramFiles")
                .map(PathBuf::from)
                .as_deref(),
        )
    } else if cfg!(target_os = "macos") {
        macos_known(kind, home.as_deref().unwrap_or(Path::new("/")))
    } else {
        linux_known(kind, home.as_deref().unwrap_or(Path::new("/")))
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn linux_known(kind: EditorKind, home: &Path) -> Vec<PathBuf> {
    let bin = kind.bin();
    let mut out = vec![
        PathBuf::from("/usr/bin").join(bin),
        PathBuf::from("/usr/local/bin").join(bin),
        PathBuf::from("/snap/bin").join(bin),
    ];
    if kind == EditorKind::Cursor {
        out.push(PathBuf::from("/opt").join("Cursor").join(bin));
        out.push(PathBuf::from("/opt").join("cursor").join(bin));
        out.push(home.join(".local").join("bin").join(bin));
        out.push(home.join(".cursor").join("bin").join(bin));
    }
    out
}

fn macos_known(kind: EditorKind, home: &Path) -> Vec<PathBuf> {
    let (app, bin) = match kind {
        EditorKind::VsCode => ("Visual Studio Code.app", "code"),
        EditorKind::Cursor => ("Cursor.app", "cursor"),
    };
    let rel = Path::new("Contents")
        .join("Resources")
        .join("app")
        .join("bin")
        .join(bin);
    vec![
        PathBuf::from("/Applications").join(app).join(&rel),
        home.join("Applications").join(app).join(&rel),
    ]
}

fn windows_known(
    kind: EditorKind,
    local_app_data: Option<&Path>,
    program_files: Option<&Path>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match kind {
        EditorKind::VsCode => {
            if let Some(local) = local_app_data {
                out.push(
                    local
                        .join("Programs")
                        .join("Microsoft VS Code")
                        .join("bin")
                        .join("code.cmd"),
                );
            }
            if let Some(pf) = program_files {
                out.push(pf.join("Microsoft VS Code").join("bin").join("code.cmd"));
            }
        }
        EditorKind::Cursor => {
            if let Some(local) = local_app_data {
                out.push(
                    local
                        .join("Programs")
                        .join("cursor")
                        .join("resources")
                        .join("app")
                        .join("bin")
                        .join("cursor.cmd"),
                );
                out.push(
                    local
                        .join("Programs")
                        .join("Cursor")
                        .join("resources")
                        .join("app")
                        .join("bin")
                        .join("cursor.cmd"),
                );
            }
        }
    }
    out
}

fn list_installed(launcher: &Path) -> Result<Option<String>, String> {
    let output = run_editor(launcher, &["--list-extensions", "--show-versions"])?;
    Ok(parse_installed_version(&output))
}

fn install_vsix(launcher: &Path, vsix: &Path) -> Result<(), String> {
    let vsix = vsix
        .to_str()
        .ok_or_else(|| "VSIX path is not UTF-8".to_string())?;
    let _ = run_editor(launcher, &["--install-extension", vsix, "--force"])?;
    Ok(())
}

fn run_editor(program: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = spawn_editor(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = cmd
        .output()
        .map_err(|err| format!("spawn {}: {err}", program.display()))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{} exited {}: {}",
            program.display(),
            output.status,
            err.trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|err| format!("stdout utf8: {err}"))
}

fn spawn_editor(program: &Path) -> Command {
    if cfg!(windows) {
        let ext = program.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat") {
            let mut cmd = Command::new("cmd.exe");
            cmd.arg("/C").arg(program);
            return cmd;
        }
    }
    Command::new(program)
}

fn in_test_harness() -> bool {
    std::env::var_os("STATEROOT_TEST_CMD_PROBES").is_some()
}

fn skip_host_editor_scan() -> bool {
    in_test_harness() || std::env::var_os("STATEROOT_TEST_HOME").is_some()
}

fn any_test_editor_override() -> bool {
    EditorKind::all()
        .iter()
        .any(|kind| test_editor_override(*kind).is_some())
}

fn should_auto_reconcile() -> bool {
    !skip_host_editor_scan() || any_test_editor_override()
}

fn allow_release_lookup() -> bool {
    std::env::var_os("STATEROOT_GITHUB_API_BASE").is_some() || !skip_host_editor_scan()
}

fn test_editor_override(kind: EditorKind) -> Option<PathBuf> {
    std::env::var_os(kind.test_env())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

async fn resolve_manifest(
    ctx: &Ctx,
    info: &ReleaseInfo,
) -> anyhow::Result<Option<ExtensionManifest>> {
    let Some(url) = info.extension_manifest_url.as_deref() else {
        return Ok(None);
    };
    let dest = cache_dir(ctx, info).join(MANIFEST_ASSET);
    update::download_verified_asset(ctx, url, &info.checksums_url, MANIFEST_ASSET, &dest).await?;
    let text =
        std::fs::read_to_string(&dest).with_context(|| format!("reading {}", dest.display()))?;
    let manifest = parse_manifest(&text)?;
    if let Some(name) = info.extension_vsix_name.as_deref() {
        if name != manifest.vsix_filename {
            anyhow::bail!(
                "release asset {name} does not match manifest {}",
                manifest.vsix_filename
            );
        }
    }
    Ok(Some(manifest))
}

async fn ensure_vsix(
    ctx: &Ctx,
    release: Option<&ReleaseInfo>,
    manifest: &ExtensionManifest,
) -> anyhow::Result<PathBuf> {
    let info = release.ok_or_else(|| anyhow!("no release metadata for VSIX download"))?;
    let url = info
        .extension_vsix_url
        .as_deref()
        .ok_or_else(|| anyhow!("release has no VSIX asset"))?;
    let dest = cache_dir(ctx, info).join(&manifest.vsix_filename);
    if dest.is_file() {
        let bytes = std::fs::read(&dest)?;
        if hex_sha256(&bytes) == manifest.digest {
            return Ok(dest);
        }
    }
    update::download_verified_asset(
        ctx,
        url,
        &info.checksums_url,
        &manifest.vsix_filename,
        &dest,
    )
    .await?;
    let bytes = std::fs::read(&dest)?;
    let actual = hex_sha256(&bytes);
    anyhow::ensure!(
        actual == manifest.digest,
        "VSIX digest mismatch (manifest {}, file {})",
        manifest.digest,
        actual
    );
    Ok(dest)
}

fn cache_dir(ctx: &Ctx, info: &ReleaseInfo) -> PathBuf {
    ctx.config_dir
        .join("cache")
        .join("extensions")
        .join(&info.tag)
}

fn parse_manifest(text: &str) -> anyhow::Result<ExtensionManifest> {
    let value: Value = serde_json::from_str(text).context("parsing stateroot-extension.json")?;
    let schema = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    anyhow::ensure!(
        schema == 1,
        "unsupported extension manifest schema {schema}"
    );
    let id = value
        .get("extension_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    anyhow::ensure!(
        id.eq_ignore_ascii_case(EXTENSION_ID),
        "extension_id {id} is not {EXTENSION_ID}"
    );
    let version = value
        .get("extension_version")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("manifest missing extension_version"))?
        .trim()
        .to_string();
    update::parse_semver(&version).ok_or_else(|| anyhow!("manifest version is not semver"))?;
    let vsix_filename = value
        .get("vsix_filename")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("manifest missing vsix_filename"))?
        .trim()
        .to_string();
    anyhow::ensure!(
        vsix_filename.starts_with("stateroot-vscode-") && vsix_filename.ends_with(".vsix"),
        "unexpected vsix_filename {vsix_filename}"
    );
    let digest = value
        .get("digest")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("manifest missing digest"))?
        .trim()
        .trim_start_matches("sha256:")
        .to_ascii_lowercase();
    anyhow::ensure!(digest.len() == 64, "manifest digest is not sha256 hex");
    Ok(ExtensionManifest {
        extension_version: version,
        vsix_filename,
        digest,
    })
}

fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn acquire_lock(path: &Path) -> anyhow::Result<FileLock> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    for _ in 0..1200 {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut file) => {
                let _ = writeln!(file, "{}", std::process::id());
                return Ok(FileLock {
                    path: path.to_path_buf(),
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if lock_stale(path) {
                    let _ = std::fs::remove_file(path);
                    continue;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => return Err(err.into()),
        }
    }
    anyhow::bail!("timed out waiting for editor reconciliation lock")
}

fn lock_stale(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return true;
    };
    if let Ok(modified) = meta.modified() {
        if SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default()
            > LOCK_STALE
        {
            return true;
        }
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(pid) = text.trim().parse::<u32>() else {
        return false;
    };
    !pid_alive(pid)
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: kill(pid, 0) is an existence probe; pid is read from our lock file.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

fn persist_failures(ctx: &Ctx, reports: &[EditorReport]) {
    let failed: Vec<_> = reports
        .iter()
        .filter(|r| matches!(r.state, ExtensionState::ProbeError { .. }))
        .collect();
    let path = ctx.config_dir.join(LAST_FAILURE_NAME);
    if failed.is_empty() {
        return;
    }
    let body = json!({
        "at": stateroot_core::local_store::now_rfc3339(),
        "editors": failed.iter().map(|r| json!({
            "editor": r.kind.label(),
            "state": r.state.slug(),
            "detail": status_detail(r),
        })).collect::<Vec<_>>(),
        "retry": "stateroot editor reconcile",
    });
    let _ = std::fs::create_dir_all(&ctx.config_dir);
    let _ = std::fs::write(path, format!("{}\n", body));
}

fn last_failure_line(ctx: &Ctx) -> Option<String> {
    let text = std::fs::read_to_string(ctx.config_dir.join(LAST_FAILURE_NAME)).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let at = value.get("at").and_then(Value::as_str)?;
    Some(format!("last automatic failure at {at}"))
}

fn status_detail(report: &EditorReport) -> String {
    let launcher = report
        .launcher
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "not detected".into());
    match &report.state {
        ExtensionState::NotDetected => "not detected".into(),
        ExtensionState::Unavailable { installed } => format!(
            "{launcher}; installed {}; desired unavailable (release has no extension manifest) — `stateroot editor reconcile` is a no-op",
            installed.as_deref().unwrap_or("none")
        ),
        ExtensionState::Missing { desired } => {
            format!("{launcher}; missing; desired {desired}")
        }
        ExtensionState::Stale { installed, desired } => {
            format!("{launcher}; installed {installed}; desired {desired} (stale)")
        }
        ExtensionState::Exact { version } => {
            format!("{launcher}; installed {version}; exact")
        }
        ExtensionState::Newer {
            installed,
            desired,
        } => format!("{launcher}; installed {installed} (newer than {desired}); left untouched"),
        ExtensionState::ProbeError { detail } => {
            format!("{launcher}; error: {detail} — retry with `stateroot editor reconcile`")
        }
    }
}

fn print_matrix(reports: &[EditorReport], ctx: &Ctx) {
    println!("editor reconciliation:");
    for report in reports {
        let mark = if report.state.is_healthy() {
            "ok"
        } else {
            "!!"
        };
        let extra = if report.mutated { " (updated)" } else { "" };
        println!(
            "  [{mark}] {} — {}{extra}",
            report.kind.label(),
            status_detail(report)
        );
    }
    if let Some(line) = last_failure_line(ctx) {
        println!("  {line}; retry: `stateroot editor reconcile`");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn has_component(path: &Path, name: &str) -> bool {
        path.components().any(|c| c.as_os_str() == OsStr::new(name))
    }

    #[test]
    fn parse_extension_id_is_case_insensitive() {
        assert_eq!(
            parse_installed_version("cogniztech.STATEROOT@0.2.19\n"),
            Some("0.2.19".into())
        );
        assert_eq!(
            parse_installed_version("other.ext@1.0.0\nCognizTech.stateroot@0.2.18\n"),
            Some("0.2.18".into())
        );
        assert_eq!(parse_installed_version("nothing here\n"), None);
    }

    #[test]
    fn classify_covers_missing_stale_exact_newer_unavailable() {
        assert!(matches!(
            classify(None, Some("0.2.19")),
            ExtensionState::Missing { .. }
        ));
        assert!(matches!(
            classify(Some("0.2.18"), Some("0.2.19")),
            ExtensionState::Stale { .. }
        ));
        assert!(matches!(
            classify(Some("0.2.19"), Some("0.2.19")),
            ExtensionState::Exact { .. }
        ));
        assert!(matches!(
            classify(Some("0.2.20"), Some("0.2.19")),
            ExtensionState::Newer { .. }
        ));
        assert!(matches!(
            classify(Some("0.2.19"), None),
            ExtensionState::Unavailable { .. }
        ));
        assert!(!classify(Some("0.2.19"), Some("0.2.19")).needs_install());
        assert!(!classify(Some("0.2.20"), Some("0.2.19")).needs_install());
        assert!(classify(Some("0.2.18"), Some("0.2.19")).needs_install());
    }

    #[test]
    fn linux_known_uses_path_components() {
        let home = Path::new("home-user");
        let code = linux_known(EditorKind::VsCode, home);
        assert!(code
            .iter()
            .any(|p| has_component(p, "snap") && has_component(p, "bin")));
        assert!(code
            .iter()
            .any(|p| has_component(p, "usr") && has_component(p, "local")));
        let cursor = linux_known(EditorKind::Cursor, home);
        assert!(cursor.iter().any(|p| has_component(p, "opt")));
        assert!(cursor
            .iter()
            .any(|p| has_component(p, ".local") || has_component(p, "local")));
    }

    #[test]
    fn macos_known_uses_app_bundle_components() {
        let home = Path::new("Users").join("ada");
        let code = macos_known(EditorKind::VsCode, &home);
        assert!(code.iter().any(|p| {
            has_component(p, "Contents")
                && has_component(p, "Resources")
                && has_component(p, "code")
        }));
        let cursor = macos_known(EditorKind::Cursor, &home);
        assert!(cursor
            .iter()
            .any(|p| has_component(p, "Cursor.app") && has_component(p, "cursor")));
    }

    #[test]
    fn windows_known_uses_native_components() {
        let local = Path::new("LocalAppData");
        let pf = Path::new("Program Files");
        let code = windows_known(EditorKind::VsCode, Some(local), Some(pf));
        assert!(code
            .iter()
            .any(|p| { has_component(p, "Microsoft VS Code") && has_component(p, "code.cmd") }));
        let cursor = windows_known(EditorKind::Cursor, Some(local), None);
        assert!(cursor
            .iter()
            .any(|p| has_component(p, "cursor.cmd") && has_component(p, "resources")));
    }

    #[test]
    fn manifest_parses_sha256_prefix_and_rejects_wrong_id() {
        let body = r#"{
            "schema_version": 1,
            "extension_id": "cogniztech.stateroot",
            "extension_version": "0.2.19",
            "vsix_filename": "stateroot-vscode-0.2.19.vsix",
            "digest": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        }"#;
        let parsed = parse_manifest(body).expect("manifest");
        assert_eq!(parsed.extension_version, "0.2.19");
        assert_eq!(parsed.digest.len(), 64);
        let bad = body.replace("cogniztech.stateroot", "other.ext");
        assert!(parse_manifest(&bad).is_err());
    }
}
