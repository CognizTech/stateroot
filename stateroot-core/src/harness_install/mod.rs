//! Harness install machinery — machine-level integration into agent harness
//! roots (global instruction blocks + MCP server registration + lifecycle
//! hooks).
//!
//! Shared by the `stateroot` CLI and other frontends (e.g. a GUI setup app).
//! Home resolution: `STATEROOT_TEST_HOME` wins (tests), otherwise `$HOME`.
//! Every write is either an idempotent marked block or a read-merge-write
//! JSON merge with a `.bak` backup — foreign config is never clobbered.
//!
//! The harness metadata now lives in the v2 quirk registry
//! (shared JSON contract + [`registry::ADAPTERS`]); the legacy 7-row detection table is a compat
//! projection from it so older `installed_harnesses` values keep working.

pub mod detect;
pub mod hooks;
pub mod plugins;
pub mod registry;

use std::path::{Path, PathBuf};

use serde_json::json;
use thiserror::Error;

use registry::{quirk_by_legacy_id, quirk_detected};

/// Env var overriding the home directory (tests).
pub const ENV_TEST_HOME: &str = "STATEROOT_TEST_HOME";

/// Errors from the harness-install machinery.
#[derive(Debug, Error)]
pub enum HarnessError {
    /// Filesystem failure.
    #[error("io error on {path}: {source}")]
    Io {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// JSON parse failure on a file we refuse to clobber.
    #[error("failed to parse {path}: {source}")]
    JsonParse {
        /// File that failed to parse.
        path: PathBuf,
        /// Underlying error.
        source: serde_json::Error,
    },
    /// Serialization failure.
    #[error("failed to serialize json: {0}")]
    JsonSerialize(#[from] serde_json::Error),
    /// Structural problem with an existing config file.
    #[error("{0}")]
    Invalid(String),
    /// No home directory could be determined.
    #[error("could not resolve the home directory (HOME unset)")]
    NoHome,
}

fn io_err(path: &Path) -> impl Fn(std::io::Error) -> HarnessError + '_ {
    move |source| HarnessError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Resolve the home directory (`STATEROOT_TEST_HOME` override supported).
///
/// Windows has no `HOME` env var — fall back to `USERPROFILE` before failing.
pub fn home_dir() -> Result<PathBuf, HarnessError> {
    if let Some(raw) = std::env::var_os(ENV_TEST_HOME) {
        let path = PathBuf::from(raw);
        if !path.as_os_str().is_empty() {
            return Ok(path);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(home));
    }
    if let Some(home) = std::env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(home));
    }
    Err(HarnessError::NoHome)
}

/// One row of the detection table.
pub struct HarnessSpec {
    /// Stable harness id.
    pub id: &'static str,
    /// Global instruction file receiving the one-agent block (when the
    /// harness has a reliable one).
    pub instruction_file: Option<PathBuf>,
    /// MCP config files to register into.
    pub mcp_files: Vec<PathBuf>,
    /// Claude-only: also install the skill copy + slash stub.
    pub claude_extras: bool,
    /// Optional guidance line printed after install.
    pub guidance: Option<&'static str>,
}

/// The legacy harness ids in their historic install order (tests depend on it).
const LEGACY_ORDER: &[&str] = &[
    "claude",
    "codex",
    "cursor",
    "kimi-code",
    "kimi",
    "opencode",
    "gemini",
];

/// Optional guidance lines kept from the hand-written table.
fn legacy_guidance(id: &str) -> Option<&'static str> {
    match id {
        "cursor" => Some(
            "cursor: project Rules + MCP are installed automatically; paste the persona from `stateroot resume` into Cursor Settings → Rules only if you want it in the global User Rules UI",
        ),
        "opencode" => Some(
            "opencode: registered MCP under the `mcpServers` key in opencode.json (no existing file found to verify the shape against)",
        ),
        _ => None,
    }
}

/// The full detection table for `home` (regardless of existence) — a compat
/// projection of the registry's legacy-id rows in the historic order.
pub fn all_specs(home: &Path) -> Vec<HarnessSpec> {
    LEGACY_ORDER
        .iter()
        .filter_map(|legacy| {
            let quirk = quirk_by_legacy_id(legacy)?;
            Some(HarnessSpec {
                id: legacy,
                instruction_file: quirk.instruction_file.map(|rel| home.join(rel)),
                mcp_files: quirk
                    .mcp
                    .map(|target| vec![home.join(target.path)])
                    .unwrap_or_default(),
                claude_extras: quirk.id == "claude-code",
                guidance: legacy_guidance(legacy),
            })
        })
        .collect()
}

/// Detection markers per harness id: does this harness exist on the machine?
pub fn spec_exists(home: &Path, id: &str) -> bool {
    match quirk_by_legacy_id(id) {
        Some(quirk) => quirk_detected(home, quirk),
        None => false,
    }
}

// ---------------------------------------------------------------------
// Marked instruction blocks
// ---------------------------------------------------------------------

/// Begin marker for the managed block.
pub const BLOCK_BEGIN: &str = "<!-- stateroot:begin -->";
/// End marker for the managed block.
pub const BLOCK_END: &str = "<!-- stateroot:end -->";

/// Insert or replace the managed block in `path`. Creates the file (and
/// parent dirs) when missing. Returns true when the file changed.
pub fn ensure_marked_block(path: &Path, body: &str) -> Result<bool, HarnessError> {
    let block = format!("{BLOCK_BEGIN}\n{}\n{BLOCK_END}", body.trim_end());
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(io_err(path)(err)),
    };

    if let (Some(begin), Some(end)) = (existing.find(BLOCK_BEGIN), existing.find(BLOCK_END)) {
        let after_end = end + BLOCK_END.len();
        let current = &existing[begin..after_end];
        if current == block {
            return Ok(false);
        }
        let updated = format!("{}{}{}", &existing[..begin], block, &existing[after_end..]);
        write_file(path, &updated)?;
        return Ok(true);
    }

    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    if !updated.is_empty() {
        updated.push('\n');
    }
    updated.push_str(&block);
    updated.push('\n');
    write_file(path, &updated)?;
    Ok(true)
}

/// Remove the managed block from `path` (uninstall). Returns true when a
/// block was removed.
pub fn remove_marked_block(path: &Path) -> Result<bool, HarnessError> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(io_err(path)(err)),
    };
    let (Some(begin), Some(end)) = (existing.find(BLOCK_BEGIN), existing.find(BLOCK_END)) else {
        return Ok(false);
    };
    let after_end = end + BLOCK_END.len();
    // Swallow one trailing newline so no empty gap remains.
    let after_end = if existing[after_end..].starts_with('\n') {
        after_end + 1
    } else {
        after_end
    };
    let mut updated = format!("{}{}", &existing[..begin], &existing[after_end..]);
    // Collapse 3+ consecutive blank lines left behind.
    while updated.contains("\n\n\n") {
        updated = updated.replace("\n\n\n", "\n\n");
    }
    write_file(path, &updated)?;
    Ok(true)
}

fn write_file(path: &Path, contents: &str) -> Result<(), HarnessError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    std::fs::write(path, contents).map_err(io_err(path))?;
    Ok(())
}

// ---------------------------------------------------------------------
// MCP registration
// ---------------------------------------------------------------------

/// The MCP registration entry written into every harness config.
///
/// Always the stdio bridge (`stateroot mcp-stdio`): the bridge injects fresh
/// auth from the local credential store on every call, refreshing when needed.
/// A `{url, headers: Bearer <token>}` HTTP entry was deliberately rejected —
/// access tokens expire in ~60 minutes, so any registration embedding one
/// silently rots.
pub fn mcp_entry() -> serde_json::Value {
    json!({
        "command": "stateroot",
        "args": ["mcp-stdio"],
    })
}

fn bak_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.bak",
        path.extension().and_then(|e| e.to_str()).unwrap_or("json")
    ))
}

/// Merge `entry` into `path` under `mcpServers.stateroot` (read-merge-write,
/// `.bak` backup on first modification). Returns true when the file changed.
pub fn merge_mcp_entry(path: &Path, entry: &serde_json::Value) -> Result<bool, HarnessError> {
    merge_named_mcp_entry_at(path, "mcpServers", "stateroot", entry)
}

/// Merge into a specific root key — vscode-copilot uses `servers`, not
/// `mcpServers` (verified in the registry).
pub fn merge_mcp_entry_at(
    path: &Path,
    root_key: &str,
    entry: &serde_json::Value,
) -> Result<bool, HarnessError> {
    merge_named_mcp_entry_at(path, root_key, "stateroot", entry)
}

/// Merge an arbitrary named MCP server entry under `root_key` in a JSON config.
pub fn merge_named_mcp_entry_at(
    path: &Path,
    root_key: &str,
    server_name: &str,
    entry: &serde_json::Value,
) -> Result<bool, HarnessError> {
    let (mut doc, existed) = if path.is_file() {
        let text = std::fs::read_to_string(path).map_err(io_err(path))?;
        if text.trim().is_empty() {
            (json!({}), true)
        } else {
            let parsed: serde_json::Value = match serde_json::from_str(&text) {
                Ok(parsed) => parsed,
                Err(source) => {
                    return Err(HarnessError::Invalid(format!(
                        "{} is not valid JSON — refusing to modify ({source})",
                        path.display()
                    )))
                }
            };
            (parsed, true)
        }
    } else {
        (json!({}), false)
    };

    if existed {
        let bak = bak_path(path);
        // First modification only — the .bak must stay pristine for uninstall.
        if !bak.exists() {
            std::fs::copy(path, &bak).map_err(io_err(&bak))?;
        }
    }

    let root = doc.as_object_mut().ok_or_else(|| {
        HarnessError::Invalid(format!(
            "{}: top-level JSON is not an object",
            path.display()
        ))
    })?;
    let servers = root
        .entry(root_key.to_string())
        .or_insert_with(|| json!({}));
    let servers = servers.as_object_mut().ok_or_else(|| {
        HarnessError::Invalid(format!("{}: `{root_key}` is not an object", path.display()))
    })?;
    if servers.get(server_name) == Some(entry) {
        return Ok(false);
    }
    servers.insert(server_name.to_string(), entry.clone());

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    let text = serde_json::to_string_pretty(&doc)?;
    std::fs::write(path, format!("{text}\n")).map_err(io_err(path))?;
    Ok(true)
}

/// Read MCP server map from a JSON config under `root_key`.
pub fn read_named_mcp_servers_json(
    path: &Path,
    root_key: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, HarnessError> {
    if !path.is_file() {
        return Ok(serde_json::Map::new());
    }
    let text = std::fs::read_to_string(path).map_err(io_err(path))?;
    if text.trim().is_empty() {
        return Ok(serde_json::Map::new());
    }
    let doc: serde_json::Value = serde_json::from_str(&text).map_err(|source| {
        HarnessError::Invalid(format!(
            "{} is not valid JSON — refusing to read ({source})",
            path.display()
        ))
    })?;
    Ok(doc
        .get(root_key)
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default())
}

/// Merge `entry` into a YAML config under the `mcp_servers` mapping
/// (hermes `~/.hermes/config.yaml`). Read-modify-write with a `.bak` backup
/// on first modification; foreign keys are preserved; the file is created
/// when absent.
///
/// Caveat: serde_yaml has no comment preservation, so a pre-existing file
/// is re-serialized (comments/formatting normalized). That is acceptable
/// here ONLY because the pristine original is kept at `<file>.bak` and the
/// caller surfaces that fact; a line-based inserter was rejected — it
/// cannot safely handle quoted `#`, flow maps, or anchors in a hand-edited
/// YAML file.
pub fn merge_yaml_mcp_entry(path: &Path, entry: &serde_json::Value) -> Result<bool, HarnessError> {
    merge_named_yaml_mcp_entry(path, "stateroot", entry)
}

/// Merge an arbitrary named MCP server into Hermes-style YAML `mcp_servers`.
pub fn merge_named_yaml_mcp_entry(
    path: &Path,
    server_name: &str,
    entry: &serde_json::Value,
) -> Result<bool, HarnessError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(io_err(path)(err)),
    };
    let mut doc: serde_yaml::Value = if text.trim().is_empty() {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    } else {
        match serde_yaml::from_str(&text) {
            Ok(doc) => doc,
            Err(source) => {
                return Err(HarnessError::Invalid(format!(
                    "{} is not valid YAML — refusing to modify ({source})",
                    path.display()
                )))
            }
        }
    };
    let desired = serde_yaml::to_value(entry)
        .map_err(|e| HarnessError::Invalid(format!("mcp entry not YAML-representable: {e}")))?;

    let root = doc.as_mapping_mut().ok_or_else(|| {
        HarnessError::Invalid(format!(
            "{}: top-level YAML is not a mapping",
            path.display()
        ))
    })?;
    let key_servers = serde_yaml::Value::String("mcp_servers".to_string());
    let key_ours = serde_yaml::Value::String(server_name.to_string());
    let servers = root
        .entry(key_servers)
        .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    let servers = servers.as_mapping_mut().ok_or_else(|| {
        HarnessError::Invalid(format!(
            "{}: `mcp_servers` is not a mapping",
            path.display()
        ))
    })?;
    if servers.get(&key_ours) == Some(&desired) {
        return Ok(false);
    }

    let existed = path.is_file();
    if existed {
        let bak = bak_path(path);
        // First modification only — the .bak must stay pristine for uninstall.
        if !bak.exists() {
            std::fs::copy(path, &bak).map_err(io_err(&bak))?;
        }
    }
    servers.insert(key_ours, desired);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    let out = serde_yaml::to_string(&doc)
        .map_err(|e| HarnessError::Invalid(format!("YAML serialization failed: {e}")))?;
    std::fs::write(path, out).map_err(io_err(path))?;
    Ok(true)
}

/// Read Hermes-style YAML `mcp_servers` map as JSON values.
pub fn read_named_yaml_mcp_servers(
    path: &Path,
) -> Result<serde_json::Map<String, serde_json::Value>, HarnessError> {
    if !path.is_file() {
        return Ok(serde_json::Map::new());
    }
    let text = std::fs::read_to_string(path).map_err(io_err(path))?;
    if text.trim().is_empty() {
        return Ok(serde_json::Map::new());
    }
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).map_err(|source| {
        HarnessError::Invalid(format!(
            "{} is not valid YAML — refusing to read ({source})",
            path.display()
        ))
    })?;
    let Some(servers) = doc.get("mcp_servers").and_then(|v| v.as_mapping()) else {
        return Ok(serde_json::Map::new());
    };
    let mut out = serde_json::Map::new();
    for (k, v) in servers {
        let Some(name) = k.as_str() else {
            continue;
        };
        let json_v: serde_json::Value = serde_json::to_value(v).unwrap_or(serde_json::Value::Null);
        out.insert(name.to_string(), json_v);
    }
    Ok(out)
}

/// Register MCP for one registry quirk (Tier A/B rows with an `mcp` target,
/// Tier C vscode-copilot whose root key is `servers`, and hermes whose
/// config is YAML with an `mcp_servers` mapping).
pub fn install_quirk_mcp(
    home: &Path,
    quirk: &registry::HarnessQuirk,
) -> Result<Vec<String>, HarnessError> {
    let Some(target) = quirk.mcp else {
        return Ok(Vec::new());
    };
    let path = home.join(target.path);
    match target.shape {
        registry::McpShape::YamlMcpServers => {
            let pre_existed = path.is_file();
            let changed = merge_yaml_mcp_entry(&path, &mcp_entry())?;
            let mut line = format!(
                "MCP {} → {} (`mcp_servers`)",
                if changed {
                    "registered"
                } else {
                    "already registered"
                },
                path.display()
            );
            if changed && pre_existed {
                line.push_str(&format!(
                    " (re-serialized; pristine original at {})",
                    bak_path(&path).display()
                ));
            }
            Ok(vec![line])
        }
        registry::McpShape::McpServersJson | registry::McpShape::ServersJson => {
            let root_key = match target.shape {
                registry::McpShape::McpServersJson => "mcpServers",
                registry::McpShape::ServersJson => "servers",
                registry::McpShape::YamlMcpServers => unreachable!(),
            };
            let changed = merge_mcp_entry_at(&path, root_key, &mcp_entry())?;
            Ok(vec![format!(
                "MCP {} → {} (`{root_key}`)",
                if changed {
                    "registered"
                } else {
                    "already registered"
                },
                path.display()
            )])
        }
    }
}

/// Install every component a registry row supports: the one-agent
/// instruction block, MCP registration, and the tier installer (native
/// hooks for Tier A, generated plugin for Tier B). Rows without an
/// installer path for a component get an honest note, never a failure.
pub fn install_quirk_full(home: &Path, quirk: &registry::HarnessQuirk, block: &str) -> Vec<String> {
    let mut actions: Vec<String> = Vec::new();
    if let Some(rel) = quirk.instruction_file {
        let file = home.join(rel);
        match ensure_marked_block(&file, block) {
            Ok(true) => actions.push(format!("block → {}", file.display())),
            Ok(false) => actions.push(format!("block already up to date ({})", file.display())),
            Err(err) => tracing::warn!("  ! {} block failed: {err}", quirk.id),
        }
    }
    match install_quirk_mcp(home, quirk) {
        Ok(lines) => actions.extend(lines),
        Err(err) => tracing::warn!("  ! {} MCP registration failed: {err}", quirk.id),
    }
    let tier_actions = if quirk.id == "crush" {
        Ok(vec![
            "crush: managed harness — hook support planned; no files written (placeholder)"
                .to_string(),
        ])
    } else if quirk.hooks.is_some() {
        hooks::install_hooks(home, quirk)
    } else if quirk.tier == registry::Tier::B {
        plugins::install_ts_plugin(home, quirk)
    } else if quirk.id == "hermes" {
        Ok(vec![
            "note: hermes hooks/resume plugin planned — resume works today via the MCP bridge"
                .to_string(),
        ])
    } else {
        Ok(Vec::new())
    };
    match tier_actions {
        Ok(lines) => actions.extend(lines),
        Err(err) => tracing::warn!("  ! {} tier install failed: {err}", quirk.id),
    }
    if actions.is_empty() {
        actions.push("managed — no files".to_string());
    }
    actions
}

/// Remove the stateroot MCP registration: restore `.bak` when present, else
/// delete only the `mcpServers.stateroot` key.
pub fn uninstall_mcp_entry(path: &Path) -> Result<bool, HarnessError> {
    let bak = bak_path(path);
    if bak.exists() {
        std::fs::rename(&bak, path).map_err(io_err(path))?;
        return Ok(true);
    }
    if !path.is_file() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(path).map_err(io_err(path))?;
    if text.trim().is_empty() {
        return Ok(false);
    }
    let mut doc: serde_json::Value = match serde_json::from_str(&text) {
        Ok(doc) => doc,
        Err(source) => {
            return Err(HarnessError::Invalid(format!(
                "{} is not valid JSON — leaving it alone ({source})",
                path.display()
            )))
        }
    };
    let removed = doc
        .get_mut("mcpServers")
        .and_then(|s| s.as_object_mut())
        .map(|servers| servers.remove("stateroot").is_some())
        .unwrap_or(false);
    if removed {
        let text = serde_json::to_string_pretty(&doc)?;
        std::fs::write(path, format!("{text}\n")).map_err(io_err(path))?;
    }
    Ok(removed)
}

// ---------------------------------------------------------------------
// Per-harness install
// ---------------------------------------------------------------------

/// Which parts of one harness integration to install (wizard toggles).
#[derive(Debug, Clone, Copy)]
pub struct InstallToggles {
    /// Global one-agent instruction block.
    pub block: bool,
    /// MCP server registration.
    pub mcp: bool,
    /// Claude skill copy + slash stub (claude only).
    pub extras: bool,
    /// Native lifecycle hooks (when the harness has a hook target).
    pub hooks: bool,
}

impl Default for InstallToggles {
    fn default() -> Self {
        Self {
            block: true,
            mcp: true,
            extras: true,
            hooks: true,
        }
    }
}

/// The skill bundle to materialize for claude extras (built by the frontend
/// from its own assets — the CLI passes its embedded copy).
pub struct SkillBundle {
    /// Bundle files as (path relative to the bundle root, bytes).
    pub files: Vec<(PathBuf, Vec<u8>)>,
    /// Claude slash-command markdown (written to `.claude/commands/stateroot.md`).
    pub claude_command_md: Option<Vec<u8>>,
}

/// Write a [`SkillBundle`] into `dest`, making `scripts/*.sh` executable.
/// Returns the number of files written.
pub fn extract_skill_bundle(dest: &Path, bundle: &SkillBundle) -> Result<usize, HarnessError> {
    let mut count = 0usize;
    for (rel, bytes) in &bundle.files {
        let target = dest.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        std::fs::write(&target, bytes).map_err(io_err(&target))?;
        let rel_str = rel.to_string_lossy();
        if rel_str.starts_with("scripts/") && rel_str.ends_with(".sh") {
            make_executable(&target)?;
        }
        count += 1;
    }
    Ok(count)
}

/// Mark a Claude (or other) product skill install as StateRoot-managed so
/// federation reclaim treats it as ours rather than user content.
fn write_product_install_marker(dest: &Path, bundle: &SkillBundle) -> Result<(), HarnessError> {
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    let mut files = BTreeMap::new();
    for (rel, bytes) in &bundle.files {
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.starts_with("assets/") {
            continue;
        }
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        files.insert(rel, format!("{:x}", hasher.finalize()));
    }
    let mut canonical = String::new();
    for (path, digest) in &files {
        canonical.push_str(path);
        canonical.push('\0');
        canonical.push_str(digest);
        canonical.push('\n');
    }
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let package_digest = format!("{:x}", hasher.finalize());
    let mut id_hasher = Sha256::new();
    id_hasher.update(b"product\0stateroot");
    let identity_key = format!("psi_{}", &format!("{:x}", id_hasher.finalize())[..32]);
    let marker = dest.join(".stateroot-projection.json");
    let body = json!({
        "schema_version": "stateroot.skill_projection.v1",
        "managed_by": "stateroot",
        "projection_kind": "product_install",
        "identity_key": identity_key,
        "slug": "stateroot",
        "package_digest": package_digest,
        "source_harness": "skillsagent",
        "native_harness": "skillsagent",
        "ownership_class": "statesmith_authored",
    });
    std::fs::write(
        &marker,
        serde_json::to_vec_pretty(&body).map_err(HarnessError::JsonSerialize)?,
    )
    .map_err(io_err(&marker))?;
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), HarnessError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).map_err(io_err(path))?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).map_err(io_err(path))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), HarnessError> {
    Ok(())
}

/// Install one harness spec honoring `toggles`. `bundle` is required only for
/// claude extras (ignored otherwise). Returns action lines; failures are
/// logged via `tracing::warn`, not fatal.
pub fn install_spec(
    home: &Path,
    spec: &HarnessSpec,
    block: &str,
    toggles: InstallToggles,
    bundle: Option<&SkillBundle>,
) -> Vec<String> {
    let mut actions: Vec<String> = Vec::new();
    if toggles.block {
        if let Some(file) = &spec.instruction_file {
            match ensure_marked_block(file, block) {
                Ok(true) => actions.push(format!("block → {}", file.display())),
                Ok(false) => actions.push(format!("block already up to date ({})", file.display())),
                Err(err) => tracing::warn!("  ! {} block failed: {err}", spec.id),
            }
        }
    }
    if toggles.mcp {
        for mcp_file in &spec.mcp_files {
            match merge_mcp_entry(mcp_file, &mcp_entry()) {
                Ok(true) => actions.push(format!("MCP registered → {}", mcp_file.display())),
                Ok(false) => {
                    actions.push(format!("MCP already registered ({})", mcp_file.display()))
                }
                Err(err) => tracing::warn!("  ! {} MCP registration failed: {err}", spec.id),
            }
        }
    }
    if toggles.extras && spec.claude_extras {
        if let Some(bundle) = bundle {
            let skill_dest = home.join(".claude/skills/stateroot");
            match extract_skill_bundle(&skill_dest, bundle) {
                Ok(count) => {
                    if let Err(err) = write_product_install_marker(&skill_dest, bundle) {
                        tracing::warn!("  ! claude product marker failed: {err}");
                    }
                    actions.push(format!("skill ({count} files) → {}", skill_dest.display()))
                }
                Err(err) => tracing::warn!("  ! claude skill copy failed: {err}"),
            }
            if let Some(bytes) = &bundle.claude_command_md {
                let command_path = home.join(".claude/commands/stateroot.md");
                if let Some(parent) = command_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::write(&command_path, bytes) {
                    Ok(()) => actions.push(format!("slash stub → {}", command_path.display())),
                    Err(err) => tracing::warn!("  ! claude command stub failed: {err}"),
                }
            }
        } else {
            tracing::warn!(
                "  ! {} extras requested but no skill bundle provided",
                spec.id
            );
        }
    }
    if toggles.hooks {
        if let Some(quirk) = registry::quirk_by_legacy_id(spec.id) {
            if quirk.hooks.is_some() {
                match hooks::install_hooks(home, quirk) {
                    Ok(lines) => actions.extend(lines),
                    Err(err) => tracing::warn!("  ! {} hooks install failed: {err}", spec.id),
                }
            }
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_insert_replace_remove() {
        let tmp = tempfile::tempdir().expect("tmp");
        let file = tmp.path().join("AGENTS.md");
        std::fs::write(&file, "# Title\n").expect("seed");

        assert!(ensure_marked_block(&file, "body v1").expect("insert"));
        assert!(!ensure_marked_block(&file, "body v1").expect("noop"));
        assert!(ensure_marked_block(&file, "body v2").expect("replace"));
        let text = std::fs::read_to_string(&file).expect("read");
        assert_eq!(text.matches(BLOCK_BEGIN).count(), 1);
        assert!(text.contains("body v2"));
        assert!(text.starts_with("# Title"));

        assert!(remove_marked_block(&file).expect("remove"));
        let text = std::fs::read_to_string(&file).expect("read");
        assert!(!text.contains(BLOCK_BEGIN));
        assert!(text.contains("# Title"));
        assert!(!remove_marked_block(&file).expect("noop remove"));
    }

    #[test]
    fn block_creates_missing_file() {
        let tmp = tempfile::tempdir().expect("tmp");
        let file = tmp.path().join("nested/GEMINI.md");
        assert!(ensure_marked_block(&file, "hello").expect("create"));
        let text = std::fs::read_to_string(&file).expect("read");
        assert!(text.contains(BLOCK_BEGIN));
        assert!(text.contains("hello"));
    }

    #[test]
    fn mcp_merge_preserves_foreign_keys_and_uninstall_restores() {
        let tmp = tempfile::tempdir().expect("tmp");
        let file = tmp.path().join("mcp.json");
        std::fs::write(
            &file,
            r#"{"mcpServers": {"foreign": {"url": "https://x.example"}}}"#,
        )
        .expect("seed");

        let entry = mcp_entry();
        assert!(merge_mcp_entry(&file, &entry).expect("merge"));
        // Idempotent on rerun.
        assert!(!merge_mcp_entry(&file, &entry).expect("merge noop"));

        let merged: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).expect("read")).expect("parse");
        assert_eq!(merged["mcpServers"]["foreign"]["url"], "https://x.example");
        assert_eq!(merged["mcpServers"]["stateroot"]["command"], "stateroot");

        // Backup is pristine; uninstall restores it.
        let bak: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(bak_path(&file)).expect("read bak"))
                .expect("parse bak");
        assert!(bak["mcpServers"].get("stateroot").is_none());
        assert!(uninstall_mcp_entry(&file).expect("uninstall"));
        let restored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).expect("read")).expect("parse");
        assert!(restored["mcpServers"].get("stateroot").is_none());
        assert!(!bak_path(&file).exists());
    }

    // ------------------------------------------------------------------
    // YAML MCP merge (hermes ~/.hermes/config.yaml)
    // ------------------------------------------------------------------

    fn yaml_doc(path: &Path) -> serde_yaml::Value {
        serde_yaml::from_str(&std::fs::read_to_string(path).expect("read")).expect("parse yaml")
    }

    #[test]
    fn yaml_mcp_creates_file_when_absent() {
        let tmp = tempfile::tempdir().expect("tmp");
        let file = tmp.path().join(".hermes/config.yaml");
        assert!(merge_yaml_mcp_entry(&file, &mcp_entry()).expect("merge"));
        let doc = yaml_doc(&file);
        let servers = doc["mcp_servers"]
            .as_mapping()
            .expect("mcp_servers mapping");
        let ours = &servers[serde_yaml::Value::String("stateroot".to_string())];
        assert_eq!(ours["command"], "stateroot");
        assert_eq!(ours["args"][0], "mcp-stdio");
        // No .bak for a freshly created file.
        assert!(!bak_path(&file).exists());
    }

    #[test]
    fn yaml_mcp_preserves_foreign_keys_and_backs_up() {
        let tmp = tempfile::tempdir().expect("tmp");
        let file = tmp.path().join("config.yaml");
        std::fs::write(
            &file,
            "model: gpt-4\nmcp_servers:\n  foreign:\n    command: other\n    args: [\"serve\"]\n",
        )
        .expect("seed");

        assert!(merge_yaml_mcp_entry(&file, &mcp_entry()).expect("merge"));
        // Idempotent on rerun.
        assert!(!merge_yaml_mcp_entry(&file, &mcp_entry()).expect("noop"));

        let doc = yaml_doc(&file);
        assert_eq!(doc["model"], "gpt-4", "foreign root key preserved");
        let servers = doc["mcp_servers"].as_mapping().expect("mapping");
        let foreign = &servers[serde_yaml::Value::String("foreign".to_string())];
        assert_eq!(foreign["command"], "other", "foreign server preserved");
        let ours = &servers[serde_yaml::Value::String("stateroot".to_string())];
        assert_eq!(ours["command"], "stateroot");

        // .bak holds the pristine original (no stateroot key yet).
        let bak = yaml_doc(&bak_path(&file));
        let bak_servers = bak["mcp_servers"].as_mapping().expect("bak mapping");
        assert!(!bak_servers.contains_key(serde_yaml::Value::String("stateroot".to_string())));
    }

    #[test]
    fn yaml_mcp_refuses_invalid_yaml_and_non_mapping_servers() {
        let tmp = tempfile::tempdir().expect("tmp");
        let bad = tmp.path().join("bad.yaml");
        std::fs::write(&bad, "mcp_servers: [unclosed\n").expect("seed");
        assert!(merge_yaml_mcp_entry(&bad, &mcp_entry()).is_err());

        let wrong = tmp.path().join("wrong.yaml");
        std::fs::write(&wrong, "mcp_servers: 42\n").expect("seed");
        assert!(merge_yaml_mcp_entry(&wrong, &mcp_entry()).is_err());
        // Untouched on refusal.
        assert_eq!(
            std::fs::read_to_string(&wrong).expect("read"),
            "mcp_servers: 42\n"
        );
    }

    #[test]
    fn install_quirk_full_hermes_writes_block_and_mcp() {
        let tmp = tempfile::tempdir().expect("tmp");
        let hermes = registry::quirk("hermes").expect("hermes");
        let actions = install_quirk_full(tmp.path(), hermes, "BLOCK BODY");
        let joined = actions.join("\n");
        assert!(joined.contains("SOUL.md"), "actions: {joined}");
        assert!(joined.contains("config.yaml"), "actions: {joined}");
        assert!(joined.contains("plugin planned"), "actions: {joined}");

        let soul = std::fs::read_to_string(tmp.path().join(".hermes/SOUL.md")).expect("soul");
        assert!(soul.contains(BLOCK_BEGIN));
        assert!(soul.contains("BLOCK BODY"));
        // Idempotent block on rerun.
        let rerun = install_quirk_full(tmp.path(), hermes, "BLOCK BODY").join("\n");
        assert!(rerun.contains("block already up to date"), "rerun: {rerun}");
        assert!(rerun.contains("MCP already registered"), "rerun: {rerun}");
    }
}
