//! Cross-harness MCP federation — discover, canonical store, project.
//!
//! User-installed MCP servers are pooled under `.stateroot/tools/mcp.json`
//! and projected into every harness MCP config. Product bridge keys
//! (`stateroot`, `statesmith-stateroot`) are reserved and never federated.
//!
//! StateSmith (canonical id `statesmith`) cloud mode only receives SSE / streamable-HTTP /
//! URL-based remote servers via `.stateroot/tools/mcp.cloud.json`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::harness_install::{
    self, merge_named_mcp_entry_at, merge_named_yaml_mcp_entry, read_named_mcp_servers_json,
    read_named_yaml_mcp_servers,
};
use crate::skill_federation::{load_registry, normalize_harness, McpConfigTarget};

const SCHEMA_VERSION: &str = "stateroot.mcp_federation.v1";
const PROJECTIONS_SCHEMA: &str = "stateroot.mcp_projections.v1";
// Product bridge keys (canonical only — no legacy variants per owner
// directive).
const RESERVED_KEYS: &[&str] = &["stateroot", "statesmith-stateroot"];
const CLOUD_HARNESS_ID: &str = "statesmith";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportHint {
    Stdio,
    Sse,
    StreamableHttp,
    HttpUrl,
    Unknown,
}

impl TransportHint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Sse => "sse",
            Self::StreamableHttp => "streamable_http",
            Self::HttpUrl => "http_url",
            Self::Unknown => "unknown",
        }
    }
}

/// Classify an MCP server entry for federation / StateSmith cloud filtering.
pub fn classify_mcp_transport(entry: &Value) -> TransportHint {
    let Some(obj) = entry.as_object() else {
        return TransportHint::Unknown;
    };
    let type_str = obj
        .get("type")
        .or_else(|| obj.get("transport"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .replace('-', "_");
    let has_command = obj
        .get("command")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty());
    let has_url = obj
        .get("url")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty());

    match type_str.as_str() {
        "stdio" => return TransportHint::Stdio,
        "sse" => return TransportHint::Sse,
        "http" | "streamable_http" | "streamablehttp" => {
            return TransportHint::StreamableHttp;
        }
        _ => {}
    }

    if has_command && !has_url {
        return TransportHint::Stdio;
    }
    if has_url && !has_command {
        if type_str.contains("sse") {
            return TransportHint::Sse;
        }
        return TransportHint::HttpUrl;
    }
    if has_command {
        return TransportHint::Stdio;
    }
    TransportHint::Unknown
}

/// Cloud StateSmith can only reach remote MCP transports (not local stdio).
pub fn is_cloud_eligible(hint: TransportHint) -> bool {
    matches!(
        hint,
        TransportHint::Sse | TransportHint::StreamableHttp | TransportHint::HttpUrl
    )
}

fn is_cloud_projection_harness(harness: &str) -> bool {
    normalize_harness(harness) == CLOUD_HARNESS_ID
}

fn server_transport(server: &CanonicalMcpServer) -> TransportHint {
    server
        .transport_hint
        .unwrap_or_else(|| classify_mcp_transport(&server.entry))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiscoveredMcpServer {
    pub name: String,
    pub entry: Value,
    pub entry_digest: String,
    pub origin_harness: String,
    pub origin_path: String,
    pub scope: String,
    pub shape: String,
    #[serde(default = "default_transport_hint")]
    pub transport_hint: TransportHint,
}

fn default_transport_hint() -> TransportHint {
    TransportHint::Unknown
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalMcpServer {
    pub identity_key: String,
    pub entry: Value,
    pub entry_digest: String,
    pub origins: Vec<McpOrigin>,
    pub ownership_class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_hint: Option<TransportHint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpOrigin {
    pub harness: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CanonicalMcpStore {
    pub schema_version: String,
    pub servers: BTreeMap<String, CanonicalMcpServer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ProjectionLedger {
    schema_version: String,
    /// key: `{scope}|{harness}|{abs_path}|{server_name}` → digest
    entries: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct SyncOptions {
    pub dry_run: bool,
    pub pull: bool,
    pub push: bool,
    /// Test seam: when `Some`, binary detection (`detect_cmds`) is answered
    /// from this allowlist instead of probing the host PATH.
    #[doc(hidden)]
    pub cmd_probe: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncAction {
    pub action: String,
    pub name: String,
    pub detail: String,
}

fn home_dir() -> Result<PathBuf, String> {
    harness_install::home_dir()
        .map_err(|err| format!("could not resolve user home for MCP federation: {err}"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn entry_digest(entry: &Value) -> String {
    let canonical = serde_json::to_vec(entry).unwrap_or_else(|_| b"{}".to_vec());
    sha256_hex(&canonical)
}

fn identity_key(name: &str, digest: &str) -> String {
    let seed = format!("mcp\0{name}\0{digest}");
    format!("pmi_{}", &sha256_hex(seed.as_bytes())[..32])
}

pub fn is_reserved_mcp_key(name: &str) -> bool {
    RESERVED_KEYS.iter().any(|k| k.eq_ignore_ascii_case(name))
}

fn expand_home_relative(rel: &str, home: &Path) -> PathBuf {
    if rel.starts_with('/') {
        PathBuf::from(rel)
    } else {
        home.join(rel)
    }
}

fn root_key_for_shape(shape: &str) -> Option<&'static str> {
    match shape {
        "mcpServers" => Some("mcpServers"),
        "servers" => Some("servers"),
        "mcp_servers" => None, // YAML
        _ => Some("mcpServers"),
    }
}

fn read_servers(path: &Path, shape: &str) -> Result<BTreeMap<String, Value>, String> {
    if shape == "mcp_servers" {
        let map = read_named_yaml_mcp_servers(path).map_err(|e| e.to_string())?;
        return Ok(map.into_iter().collect());
    }
    let root_key = root_key_for_shape(shape).unwrap_or("mcpServers");
    let map = read_named_mcp_servers_json(path, root_key).map_err(|e| e.to_string())?;
    Ok(map.into_iter().collect())
}

fn write_server(path: &Path, shape: &str, name: &str, entry: &Value) -> Result<bool, String> {
    if shape == "mcp_servers" {
        return merge_named_yaml_mcp_entry(path, name, entry).map_err(|e| e.to_string());
    }
    let root_key = root_key_for_shape(shape).unwrap_or("mcpServers");
    merge_named_mcp_entry_at(path, root_key, name, entry).map_err(|e| e.to_string())
}

fn canonical_path(home: &Path, project_dir: Option<&Path>, scope: &str) -> PathBuf {
    if scope == "project" {
        project_dir
            .map(|p| p.join(".stateroot/tools/mcp.json"))
            .unwrap_or_else(|| home.join(".stateroot/tools/mcp.json"))
    } else {
        home.join(".stateroot/tools/mcp.json")
    }
}

fn projections_path(home: &Path, project_dir: Option<&Path>, scope: &str) -> PathBuf {
    if scope == "project" {
        project_dir
            .map(|p| p.join(".stateroot/tools/mcp.projections.json"))
            .unwrap_or_else(|| home.join(".stateroot/tools/mcp.projections.json"))
    } else {
        home.join(".stateroot/tools/mcp.projections.json")
    }
}

fn load_store(path: &Path) -> CanonicalMcpStore {
    let Ok(text) = fs::read_to_string(path) else {
        return CanonicalMcpStore {
            schema_version: SCHEMA_VERSION.into(),
            servers: BTreeMap::new(),
        };
    };
    let mut store: CanonicalMcpStore =
        serde_json::from_str(&text).unwrap_or_else(|_| CanonicalMcpStore {
            schema_version: SCHEMA_VERSION.into(),
            servers: BTreeMap::new(),
        });
    // Backfill transport_hint for stores written before classification existed.
    for server in store.servers.values_mut() {
        if server.transport_hint.is_none() {
            server.transport_hint = Some(classify_mcp_transport(&server.entry));
        }
    }
    store
}

fn save_store(path: &Path, store: &CanonicalMcpStore) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    fs::write(path, format!("{text}\n")).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

fn load_ledger(path: &Path) -> ProjectionLedger {
    let Ok(text) = fs::read_to_string(path) else {
        return ProjectionLedger {
            schema_version: PROJECTIONS_SCHEMA.into(),
            entries: BTreeMap::new(),
        };
    };
    serde_json::from_str(&text).unwrap_or_else(|_| ProjectionLedger {
        schema_version: PROJECTIONS_SCHEMA.into(),
        entries: BTreeMap::new(),
    })
}

fn save_ledger(path: &Path, ledger: &ProjectionLedger) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(ledger).map_err(|e| e.to_string())?;
    fs::write(path, format!("{text}\n")).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

fn ledger_key(scope: &str, harness: &str, path: &Path, name: &str) -> String {
    format!("{scope}|{harness}|{}|{name}", path.display())
}

fn collect_targets(
    home: &Path,
    project_dir: Option<&Path>,
) -> Result<Vec<(String, String, PathBuf, McpConfigTarget)>, String> {
    collect_targets_with(
        home,
        project_dir,
        &crate::skill_federation::binary_probe(None),
    )
}

fn collect_targets_with(
    home: &Path,
    project_dir: Option<&Path>,
    probe: &dyn Fn(&str) -> bool,
) -> Result<Vec<(String, String, PathBuf, McpConfigTarget)>, String> {
    let reg = load_registry()?;
    let mut out = Vec::new();
    for entry in &reg.harnesses {
        for target in &entry.mcp_config.global {
            let path = expand_home_relative(&target.path, home);
            // Detection-gating (R2.1): targets for harnesses absent from the
            // machine are never materialized. An existing config file counts
            // as detected; the product harnesses are always writable.
            let product = matches!(entry.id.as_str(), "statesmith" | "planner");
            if !product
                && !crate::skill_federation::harness_detected_with(entry, home, Some(&path), probe)
            {
                continue;
            }
            out.push((
                normalize_harness(&entry.id),
                "global".into(),
                path,
                target.clone(),
            ));
        }
        if let Some(project_dir) = project_dir {
            for target in &entry.mcp_config.project {
                let path = project_dir.join(&target.path);
                let product = matches!(entry.id.as_str(), "statesmith" | "planner");
                if !product
                    && !crate::skill_federation::harness_detected_with(
                        entry,
                        home,
                        Some(&path),
                        probe,
                    )
                {
                    continue;
                }
                out.push((
                    normalize_harness(&entry.id),
                    "project".into(),
                    path,
                    target.clone(),
                ));
            }
        }
    }
    Ok(out)
}

/// Discover non-reserved MCP servers across registered harness configs.
///
/// Skips StateSmith cloud projection targets (`.stateroot/tools/mcp.cloud.json`)
/// so federated output is never re-ingested as an origin.
pub fn discover_all(
    home: Option<&Path>,
    project_dir: Option<&Path>,
) -> Result<Vec<DiscoveredMcpServer>, String> {
    let home = match home {
        Some(path) => path.to_path_buf(),
        None => home_dir()?,
    };
    let mut found = Vec::new();
    for (harness, scope, path, target) in collect_targets(&home, project_dir)? {
        if is_cloud_projection_harness(&harness) {
            continue;
        }
        if !path.exists() {
            continue;
        }
        let servers = match read_servers(&path, &target.shape) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!("mcp federation: skip {}: {err}", path.display());
                continue;
            }
        };
        for (name, entry) in servers {
            if is_reserved_mcp_key(&name) {
                continue;
            }
            let digest = entry_digest(&entry);
            let transport_hint = classify_mcp_transport(&entry);
            found.push(DiscoveredMcpServer {
                name,
                entry,
                entry_digest: digest,
                origin_harness: harness.clone(),
                origin_path: path.display().to_string(),
                scope: scope.clone(),
                shape: target.shape.clone(),
                transport_hint,
            });
        }
    }
    found.sort_by(|a, b| {
        (
            a.scope.as_str(),
            a.name.as_str(),
            a.origin_harness.as_str(),
            a.origin_path.as_str(),
        )
            .cmp(&(
                b.scope.as_str(),
                b.name.as_str(),
                b.origin_harness.as_str(),
                b.origin_path.as_str(),
            ))
    });
    Ok(found)
}

fn pull_into_store(
    store: &mut CanonicalMcpStore,
    discovered: &[DiscoveredMcpServer],
    scope: &str,
    dry_run: bool,
) -> Vec<SyncAction> {
    let mut actions = Vec::new();
    for item in discovered.iter().filter(|d| d.scope == scope) {
        match store.servers.get_mut(&item.name) {
            Some(existing) if existing.entry_digest == item.entry_digest => {
                let origin = McpOrigin {
                    harness: item.origin_harness.clone(),
                    path: item.origin_path.clone(),
                };
                if !existing
                    .origins
                    .iter()
                    .any(|o| o.harness == origin.harness && o.path == origin.path)
                {
                    if !dry_run {
                        existing.origins.push(origin);
                    }
                    actions.push(SyncAction {
                        action: "origin_added".into(),
                        name: item.name.clone(),
                        detail: format!("{} @ {}", item.origin_harness, item.origin_path),
                    });
                }
            }
            Some(existing) => {
                actions.push(SyncAction {
                    action: "collision".into(),
                    name: item.name.clone(),
                    detail: format!(
                        "kept digest {}…; ignored {} @ {} (digest {}…)",
                        &existing.entry_digest[..8.min(existing.entry_digest.len())],
                        item.origin_harness,
                        item.origin_path,
                        &item.entry_digest[..8.min(item.entry_digest.len())],
                    ),
                });
            }
            None => {
                if !dry_run {
                    store.servers.insert(
                        item.name.clone(),
                        CanonicalMcpServer {
                            identity_key: identity_key(&item.name, &item.entry_digest),
                            entry: item.entry.clone(),
                            entry_digest: item.entry_digest.clone(),
                            origins: vec![McpOrigin {
                                harness: item.origin_harness.clone(),
                                path: item.origin_path.clone(),
                            }],
                            ownership_class: "user_installed".into(),
                            transport_hint: Some(item.transport_hint),
                        },
                    );
                }
                actions.push(SyncAction {
                    action: if dry_run {
                        "would_pull".into()
                    } else {
                        "pulled".into()
                    },
                    name: item.name.clone(),
                    detail: format!(
                        "{} @ {} ({})",
                        item.origin_harness,
                        item.origin_path,
                        item.transport_hint.as_str()
                    ),
                });
            }
        }
    }
    actions
}

fn project_store(
    store: &CanonicalMcpStore,
    ledger: &mut ProjectionLedger,
    home: &Path,
    project_dir: Option<&Path>,
    scope: &str,
    dry_run: bool,
    probe: &dyn Fn(&str) -> bool,
) -> Result<Vec<SyncAction>, String> {
    let mut actions = Vec::new();
    let targets = collect_targets_with(home, project_dir, probe)?
        .into_iter()
        .filter(|(_, s, _, _)| s == scope)
        .collect::<Vec<_>>();
    // Parse each target once. An unparseable harness config is skipped with a
    // warning (R2.5) instead of poisoning the whole batch via a write error.
    let mut parsed_targets = Vec::new();
    for (harness, _, path, target) in targets {
        let existing = if path.exists() {
            match read_servers(&path, &target.shape) {
                Ok(servers) => Some(servers),
                Err(err) => {
                    actions.push(SyncAction {
                        action: "warn_unparseable".into(),
                        name: "-".into(),
                        detail: format!(
                            "{harness} → {} skipped (unparseable config): {err}",
                            path.display()
                        ),
                    });
                    None
                }
            }
        } else {
            Some(BTreeMap::new())
        };
        parsed_targets.push((harness, path, target, existing));
    }
    for (name, server) in &store.servers {
        let hint = server_transport(server);
        for (harness, path, target, existing) in &parsed_targets {
            // Cloud StateSmith: only SSE / streamable HTTP / URL remotes.
            if is_cloud_projection_harness(harness) && !is_cloud_eligible(hint) {
                actions.push(SyncAction {
                    action: "skipped_local".into(),
                    name: name.clone(),
                    detail: format!(
                        "{harness} → {} (transport={}, cloud requires sse|streamable_http)",
                        path.display(),
                        hint.as_str()
                    ),
                });
                continue;
            }
            let Some(existing) = existing else {
                continue;
            };
            let key = ledger_key(scope, harness, path, name);
            let managed_digest = ledger.entries.get(&key).cloned();
            match existing.get(name) {
                Some(current) if entry_digest(current) == server.entry_digest => {
                    if !dry_run {
                        ledger.entries.insert(key, server.entry_digest.clone());
                    }
                    actions.push(SyncAction {
                        action: "unchanged".into(),
                        name: name.clone(),
                        detail: format!("{harness} → {}", path.display()),
                    });
                }
                Some(current) => {
                    let current_digest = entry_digest(current);
                    if managed_digest.as_deref() == Some(current_digest.as_str()) {
                        if dry_run {
                            actions.push(SyncAction {
                                action: "would_update".into(),
                                name: name.clone(),
                                detail: format!("{harness} → {}", path.display()),
                            });
                        } else {
                            write_server(path, &target.shape, name, &server.entry)?;
                            ledger.entries.insert(key, server.entry_digest.clone());
                            actions.push(SyncAction {
                                action: "updated".into(),
                                name: name.clone(),
                                detail: format!("{harness} → {}", path.display()),
                            });
                        }
                    } else {
                        actions.push(SyncAction {
                            action: "conflict".into(),
                            name: name.clone(),
                            detail: format!(
                                "{harness} → {} already has a different entry",
                                path.display()
                            ),
                        });
                    }
                }
                None => {
                    if dry_run {
                        actions.push(SyncAction {
                            action: "would_project".into(),
                            name: name.clone(),
                            detail: format!("{harness} → {}", path.display()),
                        });
                    } else {
                        write_server(path, &target.shape, name, &server.entry)?;
                        ledger.entries.insert(key, server.entry_digest.clone());
                        actions.push(SyncAction {
                            action: "projected".into(),
                            name: name.clone(),
                            detail: format!("{harness} → {}", path.display()),
                        });
                    }
                }
            }
        }
    }
    Ok(actions)
}

fn sync_scope(
    home: &Path,
    project_dir: Option<&Path>,
    scope: &str,
    options: &SyncOptions,
    discovered: &[DiscoveredMcpServer],
) -> Result<Vec<SyncAction>, String> {
    let store_path = canonical_path(home, project_dir, scope);
    let ledger_path = projections_path(home, project_dir, scope);
    let mut store = load_store(&store_path);
    store.schema_version = SCHEMA_VERSION.into();
    let mut ledger = load_ledger(&ledger_path);
    ledger.schema_version = PROJECTIONS_SCHEMA.into();
    let mut actions = Vec::new();

    let do_pull = options.pull || !options.push;
    let do_push = options.push || !options.pull;
    if do_pull {
        actions.extend(pull_into_store(
            &mut store,
            discovered,
            scope,
            options.dry_run,
        ));
        if !options.dry_run {
            save_store(&store_path, &store)?;
        }
    }
    // Reload after pull so push sees latest.
    if !options.dry_run && do_pull {
        store = load_store(&store_path);
    }
    if do_push {
        actions.extend(project_store(
            &store,
            &mut ledger,
            home,
            project_dir,
            scope,
            options.dry_run,
            &crate::skill_federation::binary_probe(options.cmd_probe.as_deref()),
        )?);
        if !options.dry_run {
            save_ledger(&ledger_path, &ledger)?;
        }
    }
    Ok(actions)
}

/// Federate global MCP servers (and project servers when `project_dir` is set).
pub fn sync(
    home: Option<&Path>,
    project_dir: Option<&Path>,
    options: &SyncOptions,
) -> Result<Vec<SyncAction>, String> {
    let home = match home {
        Some(path) => path.to_path_buf(),
        None => home_dir()?,
    };
    let discovered = discover_all(Some(&home), project_dir)?;
    let mut actions = sync_scope(&home, project_dir, "global", options, &discovered)?;
    if project_dir.is_some() {
        actions.extend(sync_scope(
            &home,
            project_dir,
            "project",
            options,
            &discovered,
        )?);
    }
    Ok(actions)
}

/// Scopes whose canonical stores participate for this invocation.
fn scopes_in_play(project_dir: Option<&Path>) -> Vec<&'static str> {
    let mut scopes = vec!["global"];
    if project_dir.is_some() {
        scopes.push("project");
    }
    scopes
}

/// `stateroot mcp remove` (R2.5): drop `name` from the canonical store(s) and
/// projection ledger. Harness-side copies are left untouched — the next
/// `sync` pull re-adopts any still-discovered copy as a fresh entry, so a
/// legitimate foreign edit is no longer a permanent collision.
pub fn remove_server(
    home: Option<&Path>,
    project_dir: Option<&Path>,
    name: &str,
) -> Result<Vec<SyncAction>, String> {
    let home = match home {
        Some(path) => path.to_path_buf(),
        None => home_dir()?,
    };
    let mut actions = Vec::new();
    for scope in scopes_in_play(project_dir) {
        let store_path = canonical_path(&home, project_dir, scope);
        let mut store = load_store(&store_path);
        if store.servers.remove(name).is_some() {
            save_store(&store_path, &store)?;
            actions.push(SyncAction {
                action: "removed".into(),
                name: name.into(),
                detail: format!("{scope} canonical store {}", store_path.display()),
            });
        }
        let ledger_path = projections_path(&home, project_dir, scope);
        let mut ledger = load_ledger(&ledger_path);
        let suffix = format!("|{name}");
        let before = ledger.entries.len();
        ledger.entries.retain(|key, _| !key.ends_with(&suffix));
        if ledger.entries.len() != before {
            save_ledger(&ledger_path, &ledger)?;
            actions.push(SyncAction {
                action: "ledger_pruned".into(),
                name: name.into(),
                detail: format!("{scope} projection ledger"),
            });
        }
    }
    if actions.is_empty() {
        actions.push(SyncAction {
            action: "absent".into(),
            name: name.into(),
            detail: "not present in any canonical store".into(),
        });
    }
    Ok(actions)
}

/// `stateroot mcp accept-theirs` (R2.5): adopt a harness-side copy of `name`
/// into the canonical store, resolving a collision/conflict in favor of the
/// foreign edit. With several differing copies, `--from <harness>` picks one;
/// otherwise exactly one differing copy is adopted.
pub fn accept_theirs(
    home: Option<&Path>,
    project_dir: Option<&Path>,
    name: &str,
    from: Option<&str>,
) -> Result<Vec<SyncAction>, String> {
    let home = match home {
        Some(path) => path.to_path_buf(),
        None => home_dir()?,
    };
    let discovered = discover_all(Some(&home), project_dir)?;
    let from_norm = from.map(normalize_harness);
    let mut actions = Vec::new();
    for scope in scopes_in_play(project_dir) {
        let store_path = canonical_path(&home, project_dir, scope);
        let mut store = load_store(&store_path);
        let Some(existing_digest) = store.servers.get(name).map(|s| s.entry_digest.clone()) else {
            continue;
        };
        let candidates: Vec<&DiscoveredMcpServer> = discovered
            .iter()
            .filter(|d| d.scope == scope && d.name == name)
            .filter(|d| {
                from_norm
                    .as_ref()
                    .map(|f| &d.origin_harness == f)
                    .unwrap_or(true)
            })
            .collect();
        if candidates.is_empty() {
            if let Some(from) = &from_norm {
                return Err(format!(
                    "no MCP server '{name}' discovered from harness '{from}'"
                ));
            }
            continue;
        }
        let chosen: DiscoveredMcpServer = if from_norm.is_some() {
            // discover_all sorts deterministically — first match is stable.
            candidates[0].clone()
        } else {
            let differing: Vec<&DiscoveredMcpServer> = candidates
                .iter()
                .copied()
                .filter(|d| d.entry_digest != existing_digest)
                .collect();
            match differing.len() {
                0 => {
                    actions.push(SyncAction {
                        action: "unchanged".into(),
                        name: name.into(),
                        detail: format!("{scope}: harness copies already match the canonical store"),
                    });
                    continue;
                }
                1 => differing[0].clone(),
                _ => {
                    return Err(format!(
                        "multiple differing harness copies of '{name}' in {scope} scope; re-run with --from <harness>"
                    ))
                }
            }
        };
        let entry = store.servers.get_mut(name).expect("presence checked above");
        entry.entry = chosen.entry.clone();
        entry.entry_digest = chosen.entry_digest.clone();
        entry.identity_key = identity_key(name, &chosen.entry_digest);
        entry.transport_hint = Some(chosen.transport_hint);
        let origin = McpOrigin {
            harness: chosen.origin_harness.clone(),
            path: chosen.origin_path.clone(),
        };
        if !entry
            .origins
            .iter()
            .any(|o| o.harness == origin.harness && o.path == origin.path)
        {
            entry.origins.push(origin);
        }
        save_store(&store_path, &store)?;
        actions.push(SyncAction {
            action: "accepted_theirs".into(),
            name: name.into(),
            detail: format!(
                "{scope}: adopted {} @ {}",
                chosen.origin_harness, chosen.origin_path
            ),
        });
    }
    if actions.is_empty() {
        actions.push(SyncAction {
            action: "absent".into(),
            name: name.into(),
            detail: "not present in any canonical store".into(),
        });
    }
    Ok(actions)
}

pub fn status_report(home: Option<&Path>, project_dir: Option<&Path>) -> Result<Value, String> {
    let home = match home {
        Some(path) => path.to_path_buf(),
        None => home_dir()?,
    };
    let discovered = discover_all(Some(&home), project_dir)?;
    let global = load_store(&canonical_path(&home, project_dir, "global"));
    let project = project_dir
        .map(|p| load_store(&canonical_path(&home, Some(p), "project")))
        .unwrap_or_default();

    fn server_rows(store: &CanonicalMcpStore) -> Vec<Value> {
        store
            .servers
            .iter()
            .map(|(name, server)| {
                let hint = server_transport(server);
                json!({
                    "name": name,
                    "transport_hint": hint.as_str(),
                    "cloud_eligible": is_cloud_eligible(hint),
                    "entry_digest": server.entry_digest,
                    "origins": server.origins,
                })
            })
            .collect()
    }

    let global_rows = server_rows(&global);
    let project_rows = server_rows(&project);
    let cloud_eligible_count = global_rows
        .iter()
        .chain(project_rows.iter())
        .filter(|row| row.get("cloud_eligible").and_then(|v| v.as_bool()) == Some(true))
        .count();

    Ok(json!({
        "discovered": discovered.len(),
        "global_canonical": global.servers.len(),
        "project_canonical": project.servers.len(),
        "cloud_eligible": cloud_eligible_count,
        "reserved_keys": RESERVED_KEYS,
        "servers": {
            "global": global_rows,
            "project": project_rows,
        }
    }))
}

pub fn doctor_report(home: Option<&Path>, project_dir: Option<&Path>) -> Result<Value, String> {
    let home = match home {
        Some(path) => path.to_path_buf(),
        None => home_dir()?,
    };
    let discovered = discover_all(Some(&home), project_dir)?;
    let dry = SyncOptions {
        dry_run: true,
        pull: true,
        push: true,
        cmd_probe: None,
    };
    let actions = sync(Some(&home), project_dir, &dry)?;
    let collisions = actions
        .iter()
        .filter(|a| a.action == "collision" || a.action == "conflict")
        .cloned()
        .collect::<Vec<_>>();
    let skipped_local = actions
        .iter()
        .filter(|a| a.action == "skipped_local")
        .cloned()
        .collect::<Vec<_>>();
    let targets = collect_targets(&home, project_dir)?;
    let store = load_store(&canonical_path(&home, project_dir, "global"));
    let cloud_eligible = store
        .servers
        .values()
        .filter(|s| is_cloud_eligible(server_transport(s)))
        .count();
    let mut warnings = Vec::new();
    if !store.servers.is_empty() && cloud_eligible == 0 {
        warnings.push(
            "canonical pool has MCP servers but none are cloud-eligible (SSE/streamable HTTP); StateSmith cloud will have an empty mcp.cloud.json".to_string(),
        );
    }
    Ok(json!({
        "discovered": discovered.len(),
        "mcp_targets": targets.len(),
        "cloud_eligible": cloud_eligible,
        "warnings": warnings,
        "issues": collisions,
        "skipped_local": skipped_local,
        "actions_preview": actions,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_keys_are_skipped() {
        assert!(is_reserved_mcp_key("stateroot"));
        assert!(is_reserved_mcp_key("statesmith-stateroot"));
        assert!(!is_reserved_mcp_key("github"));
        assert!(!is_reserved_mcp_key("foreign-bridge"));
    }

    #[test]
    fn classify_stdio_sse_and_http() {
        assert_eq!(
            classify_mcp_transport(&json!({"command": "npx", "args": ["-y", "x"]})),
            TransportHint::Stdio
        );
        assert_eq!(
            classify_mcp_transport(&json!({"type": "sse", "url": "https://example.com/sse"})),
            TransportHint::Sse
        );
        assert_eq!(
            classify_mcp_transport(
                &json!({"type": "streamable-http", "url": "https://example.com/mcp"})
            ),
            TransportHint::StreamableHttp
        );
        assert_eq!(
            classify_mcp_transport(&json!({"url": "https://example.com/mcp"})),
            TransportHint::HttpUrl
        );
        assert!(is_cloud_eligible(TransportHint::Sse));
        assert!(is_cloud_eligible(TransportHint::StreamableHttp));
        assert!(is_cloud_eligible(TransportHint::HttpUrl));
        assert!(!is_cloud_eligible(TransportHint::Stdio));
        assert!(!is_cloud_eligible(TransportHint::Unknown));
    }

    #[test]
    fn sync_projects_cursor_server_to_claude() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".cursor")).unwrap();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".cursor/mcp.json"),
            r#"{"mcpServers":{"github":{"command":"npx","args":["-y","@modelcontextprotocol/server-github"]},"stateroot":{"command":"stateroot","args":["mcp-stdio"]}}}"#,
        )
        .unwrap();
        fs::write(
            home.join(".claude.json"),
            r#"{"mcpServers":{"stateroot":{"command":"stateroot","args":["mcp-stdio"]}}}"#,
        )
        .unwrap();

        let actions = sync(
            Some(&home),
            None,
            &SyncOptions {
                dry_run: false,
                pull: true,
                push: true,
                cmd_probe: None,
            },
        )
        .unwrap();
        assert!(
            actions
                .iter()
                .any(|a| a.name == "github" && a.action == "pulled"),
            "{actions:#?}"
        );
        assert!(
            actions.iter().any(|a| {
                a.name == "github" && a.action == "projected" && a.detail.contains("claude")
            }),
            "{actions:#?}"
        );
        assert!(
            actions.iter().any(|a| {
                a.name == "github" && a.action == "skipped_local" && a.detail.contains("statesmith")
            }),
            "stdio must not project to StateSmith cloud: {actions:#?}"
        );

        let claude = read_servers(&home.join(".claude.json"), "mcpServers").unwrap();
        assert!(claude.contains_key("github"));
        assert!(claude.contains_key("stateroot"));
        assert_eq!(
            claude["stateroot"]["command"],
            json!("stateroot"),
            "product bridge must stay"
        );

        // Reserved key must never appear in canonical store.
        let store = load_store(&home.join(".stateroot/tools/mcp.json"));
        assert!(!store.servers.contains_key("stateroot"));
        assert!(store.servers.contains_key("github"));
        assert_eq!(
            store.servers["github"].transport_hint,
            Some(TransportHint::Stdio)
        );

        // Cloud file must not contain stdio github.
        assert!(
            !home.join(".stateroot/tools/mcp.cloud.json").exists()
                || !read_servers(&home.join(".stateroot/tools/mcp.cloud.json"), "mcpServers")
                    .unwrap()
                    .contains_key("github")
        );

        let second = sync(
            Some(&home),
            None,
            &SyncOptions {
                dry_run: false,
                pull: true,
                push: true,
                cmd_probe: None,
            },
        )
        .unwrap();
        assert!(
            second.iter().filter(|a| a.name == "github").all(|a| {
                matches!(
                    a.action.as_str(),
                    "unchanged" | "origin_added" | "skipped_local"
                )
            }),
            "second sync must be idempotent: {second:#?}"
        );
    }

    #[test]
    fn cloud_projects_remote_not_stdio() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".cursor")).unwrap();
        fs::write(
            home.join(".cursor/mcp.json"),
            r#"{"mcpServers":{"local":{"command":"npx","args":["x"]},"remote":{"type":"sse","url":"https://example.com/sse"},"httpish":{"url":"https://example.com/mcp"}}}"#,
        )
        .unwrap();
        let actions = sync(
            Some(&home),
            None,
            &SyncOptions {
                dry_run: false,
                pull: true,
                push: true,
                // Hermetic (CI has no harness binaries on PATH): pretend
                // claude exists so the desktop projection is deterministic —
                // the assertions below pin cloud-vs-desktop routing, not the
                // host's PATH.
                cmd_probe: Some(vec!["claude".into()]),
            },
        )
        .unwrap();
        assert!(
            actions.iter().any(|a| {
                a.name == "local" && a.action == "skipped_local" && a.detail.contains("statesmith")
            }),
            "{actions:#?}"
        );
        assert!(
            actions.iter().any(|a| {
                a.name == "remote" && a.action == "projected" && a.detail.contains("statesmith")
            }),
            "{actions:#?}"
        );
        assert!(
            actions.iter().any(|a| {
                a.name == "httpish" && a.action == "projected" && a.detail.contains("statesmith")
            }),
            "{actions:#?}"
        );
        // Desktop still gets stdio.
        assert!(
            actions.iter().any(|a| {
                a.name == "local" && a.action == "projected" && a.detail.contains("claude")
            }),
            "{actions:#?}"
        );

        let cloud =
            read_servers(&home.join(".stateroot/tools/mcp.cloud.json"), "mcpServers").unwrap();
        assert!(!cloud.contains_key("local"));
        assert!(cloud.contains_key("remote"));
        assert!(cloud.contains_key("httpish"));
        assert_eq!(cloud["remote"]["type"], json!("sse"));
    }

    #[test]
    fn conflict_when_destination_differs() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".cursor")).unwrap();
        fs::write(
            home.join(".cursor/mcp.json"),
            r#"{"mcpServers":{"shared":{"command":"a"}}}"#,
        )
        .unwrap();
        fs::write(
            home.join(".claude.json"),
            r#"{"mcpServers":{"shared":{"command":"b"}}}"#,
        )
        .unwrap();
        let actions = sync(
            Some(&home),
            None,
            &SyncOptions {
                dry_run: false,
                pull: true,
                push: true,
                cmd_probe: None,
            },
        )
        .unwrap();
        assert!(
            actions
                .iter()
                .any(|a| a.name == "shared" && a.action == "conflict"),
            "{actions:#?}"
        );
        let claude = read_servers(&home.join(".claude.json"), "mcpServers").unwrap();
        assert_eq!(claude["shared"]["command"], json!("b"));
    }

    #[test]
    fn hermes_yaml_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".cursor")).unwrap();
        fs::create_dir_all(home.join(".hermes")).unwrap();
        fs::write(
            home.join(".cursor/mcp.json"),
            r#"{"mcpServers":{"browser":{"command":"browser-mcp"}}}"#,
        )
        .unwrap();
        fs::write(home.join(".hermes/config.yaml"), "mcp_servers: {}\n").unwrap();
        let actions = sync(
            Some(&home),
            None,
            &SyncOptions {
                dry_run: false,
                pull: true,
                push: true,
                cmd_probe: None,
            },
        )
        .unwrap();
        assert!(
            actions.iter().any(|a| {
                a.name == "browser" && a.action == "projected" && a.detail.contains("hermes")
            }),
            "{actions:#?}"
        );
        let hermes = read_servers(&home.join(".hermes/config.yaml"), "mcp_servers").unwrap();
        assert_eq!(hermes["browser"]["command"], json!("browser-mcp"));
    }

    /// R2.5: `remove` drops a server from the canonical store + ledger so a
    /// legitimate foreign edit stops being a permanent collision; the next
    /// sync re-adopts the edited copy as a fresh entry.
    #[test]
    fn remove_clears_collision_and_readopts_on_next_sync() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".cursor")).unwrap();
        fs::write(
            home.join(".cursor/mcp.json"),
            r#"{"mcpServers":{"github":{"command":"npx","args":["srv"]}}}"#,
        )
        .unwrap();
        let options = || SyncOptions {
            dry_run: false,
            pull: true,
            push: true,
            cmd_probe: Some(vec![]),
        };
        sync(Some(&home), None, &options()).unwrap();

        // User edits the harness copy directly → next sync reports a collision.
        fs::write(
            home.join(".cursor/mcp.json"),
            r#"{"mcpServers":{"github":{"command":"npx","args":["srv-edited"]}}}"#,
        )
        .unwrap();
        let actions = sync(Some(&home), None, &options()).unwrap();
        assert!(
            actions
                .iter()
                .any(|a| a.name == "github" && a.action == "collision"),
            "{actions:#?}"
        );

        // Remove from the canonical store; the edited copy is re-adopted.
        let removed = remove_server(Some(&home), None, "github").unwrap();
        assert!(removed.iter().any(|a| a.action == "removed"));
        let store = load_store(&home.join(".stateroot/tools/mcp.json"));
        assert!(!store.servers.contains_key("github"));

        let actions = sync(Some(&home), None, &options()).unwrap();
        assert!(
            actions
                .iter()
                .any(|a| a.name == "github" && a.action == "pulled"),
            "{actions:#?}"
        );
        let store = load_store(&home.join(".stateroot/tools/mcp.json"));
        assert_eq!(store.servers["github"].entry["args"], json!(["srv-edited"]));
    }

    /// R2.5: `accept-theirs` adopts the harness-side edit into the canonical
    /// store, resolving the collision in favor of the foreign copy.
    #[test]
    fn accept_theirs_adopts_foreign_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".cursor")).unwrap();
        fs::write(
            home.join(".cursor/mcp.json"),
            r#"{"mcpServers":{"github":{"command":"npx","args":["srv"]}}}"#,
        )
        .unwrap();
        let options = || SyncOptions {
            dry_run: false,
            pull: true,
            push: true,
            cmd_probe: Some(vec![]),
        };
        sync(Some(&home), None, &options()).unwrap();

        fs::write(
            home.join(".cursor/mcp.json"),
            r#"{"mcpServers":{"github":{"command":"npx","args":["srv-edited"]}}}"#,
        )
        .unwrap();
        let adopted = accept_theirs(Some(&home), None, "github", None).unwrap();
        assert!(adopted.iter().any(|a| a.action == "accepted_theirs"));
        let store = load_store(&home.join(".stateroot/tools/mcp.json"));
        assert_eq!(store.servers["github"].entry["args"], json!(["srv-edited"]));

        // Next sync is conflict-free: cursor already matches the adopted entry.
        let actions = sync(Some(&home), None, &options()).unwrap();
        assert!(
            !actions
                .iter()
                .any(|a| a.name == "github" && (a.action == "collision" || a.action == "conflict")),
            "{actions:#?}"
        );
    }

    /// R2.5: an unparseable harness config is skipped with a warning instead
    /// of aborting the whole projection batch.
    #[test]
    fn unparseable_harness_config_warns_and_skips() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".cursor")).unwrap();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".cursor/mcp.json"),
            r#"{"mcpServers":{"github":{"command":"npx","args":["srv"]}}}"#,
        )
        .unwrap();
        // Claude marker present but its config is corrupt.
        fs::write(home.join(".claude.json"), "{ not json").unwrap();

        let actions = sync(
            Some(&home),
            None,
            &SyncOptions {
                dry_run: false,
                pull: true,
                push: true,
                cmd_probe: Some(vec![]),
            },
        )
        .unwrap();
        assert!(
            actions
                .iter()
                .any(|a| a.action == "warn_unparseable" && a.detail.contains("claude")),
            "{actions:#?}"
        );
        // The corrupt file is left untouched; sync did not fail.
        assert_eq!(
            fs::read_to_string(home.join(".claude.json")).unwrap(),
            "{ not json"
        );
    }
}

#[cfg(test)]
mod detection_gating_tests {
    use super::*;

    /// R2.1: a Cursor-only machine must never gain foreign harness configs.
    #[test]
    fn cursor_only_machine_creates_zero_foreign_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        // Only Cursor present (marker + config). `cmd_probe` below pins the
        // PATH probe to empty so host binaries cannot leak into the test.
        fs::create_dir_all(home.join(".cursor")).unwrap();
        fs::write(
            home.join(".cursor/mcp.json"),
            r#"{"mcpServers":{"github":{"command":"npx","args":["-y","srv"]},"stateroot":{"command":"stateroot","args":["mcp-stdio"]}}}"#,
        )
        .unwrap();

        let actions = sync(
            Some(&home),
            None,
            &SyncOptions {
                dry_run: false,
                pull: true,
                push: true,
                // Hermetic: pretend no harness binaries exist on PATH; only the
                // `.cursor` marker counts.
                cmd_probe: Some(vec![]),
            },
        )
        .unwrap();

        // No foreign harness config/directory was created.
        for rel in [
            ".claude.json",
            ".claude",
            ".codex",
            ".gemini",
            ".kimi",
            ".kimi-code",
            ".openclaw",
            ".config/opencode",
            ".vscode/mcp.json",
        ] {
            assert!(
                !home.join(rel).exists(),
                "foreign artifact must not be created: {rel}"
            );
        }
        // The own config is intact and the product target may be written.
        let cursor = read_servers(&home.join(".cursor/mcp.json"), "mcpServers").unwrap();
        assert!(cursor.contains_key("github"));
        assert!(cursor.contains_key("stateroot"));
        // Nothing projected INTO foreign harnesses.
        assert!(
            !actions
                .iter()
                .any(|a| a.action == "projected" && !a.detail.contains("statesmith")),
            "no foreign projections expected: {actions:#?}"
        );
    }
}
