//! Per-user configuration: `config.toml` (service endpoints) and
//! `projects.toml` (registry mapping absolute project directories to server
//! project ids).
//!
//! The config directory defaults to the platform config dir
//! (`~/.config/stateroot` on Linux) and can be overridden with the
//! `STATEROOT_HOME` environment variable — tests always set it to a temp dir
//! so the real home is never touched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use directories::ProjectDirs;

/// Environment variable overriding the config directory.
pub const ENV_HOME: &str = "STATEROOT_HOME";
/// File name of the main config file inside the config dir.
pub const CONFIG_FILE: &str = "config.toml";
/// File name of the project registry inside the config dir.
pub const PROJECTS_FILE: &str = "projects.toml";
/// File name of the credential file fallback inside the config dir.
pub const CREDENTIALS_FILE: &str = "credentials.json";
/// Pointer file recording the account email for keyring lookups.
pub const ACCOUNT_FILE: &str = "account.json";

/// Environment variable overriding the agentdrive home directory.
pub const ENV_DRIVE_HOME: &str = "AGENTDRIVE_HOME";
/// Environment variable overriding the agentdrive cache directory.
pub const ENV_DRIVE_CACHE_HOME: &str = "AGENTDRIVE_CACHE_HOME";
/// Agentdrive config file name inside the drive home.
pub const DRIVE_CONFIG_FILE: &str = "config.toml";
/// Agentdrive pidfile name (daemon liveness).
pub const DRIVE_PIDFILE: &str = "agentdrive.pid";
/// Agentdrive log file name.
pub const DRIVE_LOGFILE: &str = "agentdrive.log";
/// Agentdrive live-state file name (read by `agentdrive status`).
pub const DRIVE_STATEFILE: &str = "state.json";
/// Agentdrive sync metadata database name.
pub const DRIVE_METADATA_DB: &str = "metadata.db";

/// Errors from config and registry IO.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Filesystem failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// TOML parse failure.
    #[error("failed to parse {path}: {source}")]
    Parse {
        /// File that failed to parse.
        path: PathBuf,
        /// Underlying TOML error.
        source: toml::de::Error,
    },
    /// TOML serialization failure.
    #[error("failed to serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// No platform config dir could be determined.
    #[error("could not determine a config directory; set STATEROOT_HOME")]
    NoConfigDir,
}

/// Result of the last interruption-recovery fire drill (W7).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FireDrillRecord {
    /// RFC3339 completion timestamp.
    pub completed_at: String,
    /// Drill path taken: `native-native`, `native-cloud`, or `cloud-only`.
    pub path: String,
    /// Seconds from drill start to the confirmed acknowledgment
    /// (0 when the live wait was skipped).
    pub time_to_continuity_seconds: u64,
}

/// Synthesis layer configuration (`[synthesis]` in config.toml).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SynthesisConfig {
    /// Master switch for the LLM synthesis pass (import + manual synthesize).
    pub enabled: bool,
    /// OpenAI-compatible API key (empty = unavailable; env
    /// `STATEROOT_SYNTHESIS_API_KEY` wins when set).
    pub api_key: String,
    /// Base URL of the OpenAI-compatible endpoint (`{base_url}/chat/completions`
    /// is called — DeepSeek/OpenAI/Ollama/litellm all work).
    pub base_url: String,
    /// Alias for `base_url` (either may be set; base_url wins).
    pub api_url: String,
    /// Model name sent in the request body.
    pub model: String,
    /// Extra request-body fields merged verbatim (non-thinking passthrough,
    /// vendor flags, temperature, …).
    pub extra_body: serde_json::Value,
    /// Minimum seconds between synthesis runs (governance).
    pub min_interval_seconds: i64,
    /// Maximum synthesis runs per day (governance).
    pub daily_cap: i64,
}

impl Default for SynthesisConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key: String::new(),
            base_url: "https://api.openai.com/v1".into(),
            api_url: String::new(),
            model: "gpt-4o-mini".into(),
            extra_body: serde_json::Value::Object(serde_json::Map::new()),
            min_interval_seconds: 600,
            daily_cap: 20,
        }
    }
}

/// GitHub integration (`[github]` in config.toml).
///
/// The OAuth App is registered by the project owner; until then the
/// client_id ships as a documented placeholder and every flow errors
/// honestly. Env overrides: `STATEROOT_GITHUB_CLIENT_ID`,
/// `STATEROOT_GITHUB_WEB_BASE` (device flow), `STATEROOT_GITHUB_API_BASE`
/// (REST), `STATEROOT_GITHUB_GIT_BASE` (clone/push URLs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct GithubConfig {
    /// OAuth App client id (placeholder until registered — see README).
    pub client_id: String,
    /// OAuth scope: `repo` (private repos, default) — `public_repo` is the
    /// variant for users who only sync public repos.
    pub scope: String,
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            client_id: "STATEROOT_GITHUB_CLIENT_ID_PLACEHOLDER".into(),
            scope: "repo".into(),
        }
    }
}

/// Cloud runs configuration (`[cloud]` in config.toml).
///
/// `base_url` is the StateSmith deployment that hosts the cloud-runs API
/// (agent-21 builds the server side); env override `STATEROOT_CLOUD_URL`.
/// Auth uses the Phase-1 login credential (GitHub token) as the bearer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CloudConfig {
    /// Cloud API base URL.
    pub base_url: String,
    /// Preview gate for cloud features (login/repo/sync/cloud runs) during
    /// the controlled launch — default OFF; `STATEROOT_CLOUD_PREVIEW=1`
    /// overrides.
    pub preview: bool,
}

impl Default for CloudConfig {
    fn default() -> Self {
        Self {
            base_url: "https://cloud.statesmith.dev".into(),
            preview: false,
        }
    }
}

/// Auto-update configuration (`[update]` in config.toml).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UpdateConfig {
    /// Release repo (owner/name). Placeholder until the public repo exists.
    pub repo: String,
    /// Auto-update enabled (env `STATEROOT_NO_AUTO_UPDATE=1` opts out).
    pub enabled: bool,
    /// Minimum hours between version checks (cached in
    /// `config_dir/update-check.json`).
    pub check_interval_hours: i64,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            repo: "OWNER/stateroot (placeholder — set to the public repo)".into(),
            enabled: true,
            check_interval_hours: 24,
        }
    }
}

/// Service endpoint configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AppConfig {
    /// SkillsAgent base URL (stateroot REST).
    pub server_url: String,
    /// Auth microservice base URL.
    pub auth_url: String,
    /// FileSystem microservice base URL.
    pub filesystem_url: String,
    /// Dashboard webapp base URL (used by the dashboard-first login flow).
    pub dashboard_url: String,
    /// Logical user id used for the `X-User-ID` dev fallback header.
    pub user_id: String,
    /// Agent whose prompt profile provides the persona (v1.1).
    pub agent_id: String,
    /// Harnesses installed at machine level by `stateroot install`
    /// (drives `init`'s one-time global install).
    pub installed_harnesses: Vec<String>,
    /// Last fire-drill record (W7; absent until the drill completes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire_drill: Option<FireDrillRecord>,
    /// Synthesis layer (`synthesis.enabled`, default true).
    #[serde(default)]
    pub synthesis: SynthesisConfig,
    /// GitHub integration (`[github]`).
    #[serde(default)]
    pub github: GithubConfig,
    /// Cloud runs (`[cloud]`).
    #[serde(default)]
    pub cloud: CloudConfig,
    /// Auto-update (`[update]`).
    #[serde(default)]
    pub update: UpdateConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server_url: "http://localhost:8080".to_string(),
            auth_url: "http://localhost:8000".to_string(),
            filesystem_url: "http://localhost:19051".to_string(),
            dashboard_url: "http://localhost:3100".to_string(),
            user_id: "default".to_string(),
            agent_id: "default".to_string(),
            installed_harnesses: Vec::new(),
            fire_drill: None,
            synthesis: SynthesisConfig::default(),
            github: GithubConfig::default(),
            cloud: CloudConfig::default(),
            update: UpdateConfig::default(),
        }
    }
}

/// One registered project directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProjectEntry {
    /// Server project id (v1.1: `prj_<24hex>`; legacy: equals workspace id).
    pub project_id: String,
    /// Root workspace id hosting the project (legacy: same as `project_id`).
    pub workspace_id: String,
    /// Project display name.
    #[serde(default)]
    pub name: String,
    /// Project directory inside the root workspace (v1.1; "/" legacy).
    #[serde(default = "default_root_path")]
    pub root_path: String,
    /// Remote path mirror of `root_path` kept for display/drive pairing.
    #[serde(default)]
    pub drive_path: String,
    /// Harnesses the skill was installed into (e.g. `claude`, `cursor`).
    #[serde(default)]
    pub harnesses_installed: Vec<String>,
    /// RFC3339 creation timestamp.
    pub created_at: String,
    /// Responses-API conversation bookkeeping for `stateroot run`.
    #[serde(default)]
    pub conversation: ConversationState,
    /// Transition hashes already acknowledged by this machine (W5; prevents
    /// repeat `POST …/acknowledge` calls on later resumes).
    #[serde(default)]
    pub acknowledged_transitions: Vec<String>,
}

impl Default for ProjectEntry {
    fn default() -> Self {
        Self {
            project_id: String::new(),
            workspace_id: String::new(),
            name: String::new(),
            root_path: default_root_path(),
            drive_path: String::new(),
            harnesses_installed: Vec::new(),
            created_at: String::new(),
            conversation: ConversationState::default(),
            acknowledged_transitions: Vec::new(),
        }
    }
}

fn default_root_path() -> String {
    "/".to_string()
}

/// `projects.toml` content: absolute project dir → entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectsRegistry {
    /// Map keyed by canonical absolute project directory.
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectEntry>,
}

/// Resolve the config directory: `STATEROOT_HOME` wins, otherwise the
/// platform default (`~/.config/stateroot` on Linux).
pub fn config_dir() -> Result<PathBuf, ConfigError> {
    if let Some(raw) = std::env::var_os(ENV_HOME) {
        let path = PathBuf::from(raw);
        if !path.as_os_str().is_empty() {
            return Ok(path);
        }
    }
    let dirs = ProjectDirs::from("", "", "stateroot").ok_or(ConfigError::NoConfigDir)?;
    Ok(dirs.config_dir().to_path_buf())
}

/// Load `config.toml`, returning defaults when the file does not exist.
pub fn load_config(dir: &Path) -> Result<AppConfig, ConfigError> {
    let path = dir.join(CONFIG_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.clone(),
            source,
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(AppConfig::default()),
        Err(err) => Err(ConfigError::Io(err)),
    }
}

/// Persist `config.toml`, creating the config dir when needed.
pub fn save_config(dir: &Path, config: &AppConfig) -> Result<(), ConfigError> {
    std::fs::create_dir_all(dir)?;
    let text = toml::to_string_pretty(config)?;
    std::fs::write(dir.join(CONFIG_FILE), text)?;
    Ok(())
}

// ---------------------------------------------------------------------
// agentdrive daemon configuration
// ---------------------------------------------------------------------

/// Daemon configuration written by `agentdrive pair`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct DriveConfig {
    /// FileSystem base URL.
    pub filesystem_url: String,
    /// Auth base URL (token refresh).
    pub auth_url: String,
    /// Paired workspace id.
    pub workspace_id: String,
    /// Paired workspace display name.
    pub workspace_name: String,
    /// Logical user id (legacy fallback when no JWT is stored).
    pub user_id: String,
    /// Local sync root folder.
    pub root: PathBuf,
    /// Sync mode: `mirror` (default) or `fuse`.
    pub mode: String,
    /// Remote-change detection: `snapshot` (default, full poll) or `changes`
    /// (incremental `/changes` feed with snapshot fallback).
    pub sync_mode: String,
}

impl Default for DriveConfig {
    fn default() -> Self {
        Self {
            filesystem_url: "http://localhost:19051".to_string(),
            auth_url: "http://localhost:8000".to_string(),
            workspace_id: String::new(),
            workspace_name: String::new(),
            user_id: "default".to_string(),
            root: PathBuf::from("~/AgentDrive"),
            mode: "mirror".to_string(),
            sync_mode: "snapshot".to_string(),
        }
    }
}

/// Resolve the agentdrive home: `AGENTDRIVE_HOME` wins, otherwise
/// `~/.config/agentdrive`.
pub fn drive_home() -> Result<PathBuf, ConfigError> {
    if let Some(raw) = std::env::var_os(ENV_DRIVE_HOME) {
        let path = PathBuf::from(raw);
        if !path.as_os_str().is_empty() {
            return Ok(path);
        }
    }
    let dirs = ProjectDirs::from("", "", "agentdrive").ok_or(ConfigError::NoConfigDir)?;
    Ok(dirs.config_dir().to_path_buf())
}

/// Resolve the agentdrive cache home: `AGENTDRIVE_CACHE_HOME` wins, otherwise
/// `~/.cache/agentdrive`.
pub fn drive_cache_home() -> Result<PathBuf, ConfigError> {
    if let Some(raw) = std::env::var_os(ENV_DRIVE_CACHE_HOME) {
        let path = PathBuf::from(raw);
        if !path.as_os_str().is_empty() {
            return Ok(path);
        }
    }
    let dirs = ProjectDirs::from("", "", "agentdrive").ok_or(ConfigError::NoConfigDir)?;
    Ok(dirs.cache_dir().to_path_buf())
}

/// Blob cache directory (`<cache>/blobs`) used for hydrated content.
pub fn blob_cache_dir() -> Result<PathBuf, ConfigError> {
    Ok(drive_cache_home()?.join("blobs"))
}

/// Load the drive config, or `None` when the daemon was never paired.
pub fn load_drive_config(home: &Path) -> Result<Option<DriveConfig>, ConfigError> {
    let path = home.join(DRIVE_CONFIG_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let config = toml::from_str(&text).map_err(|source| ConfigError::Parse {
                path: path.clone(),
                source,
            })?;
            Ok(Some(config))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(ConfigError::Io(err)),
    }
}

/// Persist the drive config.
pub fn save_drive_config(home: &Path, config: &DriveConfig) -> Result<(), ConfigError> {
    std::fs::create_dir_all(home)?;
    let text = toml::to_string_pretty(config)?;
    std::fs::write(home.join(DRIVE_CONFIG_FILE), text)?;
    Ok(())
}

/// Persistent device id for this machine (`<home>/device_id`, generated once).
pub fn device_id(home: &Path) -> Result<String, ConfigError> {
    let path = home.join("device_id");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let id = text.trim().to_string();
            if !id.is_empty() {
                return Ok(id);
            }
            Err(ConfigError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "empty device_id file",
            )))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(home)?;
            let id = format!("dev_{}", uuid::Uuid::new_v4().simple());
            std::fs::write(&path, format!("{id}\n"))?;
            Ok(id)
        }
        Err(err) => Err(ConfigError::Io(err)),
    }
}

/// Load `projects.toml`, returning an empty registry when absent.
pub fn load_registry(dir: &Path) -> Result<ProjectsRegistry, ConfigError> {
    let path = dir.join(PROJECTS_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.clone(),
            source,
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(ProjectsRegistry::default()),
        Err(err) => Err(ConfigError::Io(err)),
    }
}

/// Persist `projects.toml`, creating the config dir when needed.
pub fn save_registry(dir: &Path, registry: &ProjectsRegistry) -> Result<(), ConfigError> {
    std::fs::create_dir_all(dir)?;
    let text = toml::to_string_pretty(registry)?;
    std::fs::write(dir.join(PROJECTS_FILE), text)?;
    Ok(())
}

/// Canonical key for a project directory in the registry.
fn registry_key(project_dir: &Path) -> String {
    let canonical =
        std::fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    canonical.to_string_lossy().to_string()
}

/// Insert or update the entry for `project_dir`.
pub fn register_project(
    dir: &Path,
    project_dir: &Path,
    entry: ProjectEntry,
) -> Result<(), ConfigError> {
    let mut registry = load_registry(dir)?;
    registry.projects.insert(registry_key(project_dir), entry);
    save_registry(dir, &registry)
}

/// Look up the entry for `project_dir`, if any.
pub fn lookup_project(dir: &Path, project_dir: &Path) -> Result<Option<ProjectEntry>, ConfigError> {
    let registry = load_registry(dir)?;
    Ok(registry.projects.get(&registry_key(project_dir)).cloned())
}

/// Remove the entry for `project_dir` (no-op when absent). Returns true when
/// an entry was actually removed.
pub fn unregister_project(dir: &Path, project_dir: &Path) -> Result<bool, ConfigError> {
    let mut registry = load_registry(dir)?;
    let removed = registry
        .projects
        .remove(&registry_key(project_dir))
        .is_some();
    if removed {
        save_registry(dir, &registry)?;
    }
    Ok(removed)
}

/// Load the conversation state for a project dir (default when unregistered).
pub fn load_conversation(dir: &Path, project_dir: &Path) -> Result<ConversationState, ConfigError> {
    Ok(lookup_project(dir, project_dir)?
        .map(|entry| entry.conversation)
        .unwrap_or_default())
}

/// Persist the conversation state for a project dir (no-op when unregistered).
pub fn save_conversation(
    dir: &Path,
    project_dir: &Path,
    conversation: &ConversationState,
) -> Result<(), ConfigError> {
    let mut registry = load_registry(dir)?;
    if let Some(entry) = registry.projects.get_mut(&registry_key(project_dir)) {
        entry.conversation = conversation.clone();
        save_registry(dir, &registry)?;
    }
    Ok(())
}

/// Record an acknowledged transition hash for a project dir (deduped; no-op
/// when the project has no registry entry).
pub fn add_acknowledged_transition(
    dir: &Path,
    project_dir: &Path,
    transition_id: &str,
) -> Result<(), ConfigError> {
    let mut registry = load_registry(dir)?;
    if let Some(entry) = registry.projects.get_mut(&registry_key(project_dir)) {
        if !entry
            .acknowledged_transitions
            .iter()
            .any(|t| t == transition_id)
        {
            entry
                .acknowledged_transitions
                .push(transition_id.to_string());
            save_registry(dir, &registry)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = AppConfig {
            server_url: "http://example.test:9999".to_string(),
            ..Default::default()
        };
        save_config(tmp.path(), &cfg).expect("save");
        let loaded = load_config(tmp.path()).expect("load");
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn missing_config_yields_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let loaded = load_config(tmp.path()).expect("load");
        assert_eq!(loaded.server_url, "http://localhost:8080");
        assert_eq!(loaded.auth_url, "http://localhost:8000");
        assert_eq!(loaded.filesystem_url, "http://localhost:19051");
        assert_eq!(loaded.user_id, "default");
    }

    #[test]
    fn registry_roundtrip_and_lookup() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_dir = tmp.path().join("proj");
        std::fs::create_dir_all(&project_dir).expect("mkdir");
        let entry = ProjectEntry {
            project_id: "p-1".to_string(),
            workspace_id: "p-1".to_string(),
            name: "demo".to_string(),
            harnesses_installed: vec!["claude".to_string()],
            created_at: "2026-07-18T00:00:00Z".to_string(),
            ..Default::default()
        };
        register_project(tmp.path(), &project_dir, entry.clone()).expect("register");
        let found = lookup_project(tmp.path(), &project_dir).expect("lookup");
        assert_eq!(found, Some(entry));
        let missing = lookup_project(tmp.path(), tmp.path()).expect("lookup missing");
        assert!(missing.is_none());
    }

    #[test]
    fn registry_migrates_v1_entries() {
        // Pre-v1.1 entries had no root_path/drive_path fields.
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join(PROJECTS_FILE),
            r#"
[projects."/old/proj"]
project_id = "ws-legacy"
workspace_id = "ws-legacy"
name = "legacy"
harnesses_installed = ["claude"]
created_at = "2026-07-01T00:00:00Z"
"#,
        )
        .expect("write old registry");
        let registry = load_registry(tmp.path()).expect("load");
        let entry = registry.projects.get("/old/proj").expect("entry");
        assert_eq!(entry.project_id, "ws-legacy");
        assert_eq!(entry.root_path, "/");
        assert!(entry.drive_path.is_empty());
    }

    #[test]
    fn config_dir_prefers_env_override() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Safety: tests in this crate mutate process env; run single-threaded
        // semantics are guaranteed per-test by using a unique var value and
        // restoring afterwards.
        let prev = std::env::var_os(ENV_HOME);
        std::env::set_var(ENV_HOME, tmp.path());
        let resolved = config_dir().expect("config_dir");
        assert_eq!(resolved, tmp.path());
        match prev {
            Some(v) => std::env::set_var(ENV_HOME, v),
            None => std::env::remove_var(ENV_HOME),
        }
    }
}

/// Persisted conversation bookkeeping (stored in the projects.toml entry).
/// Lifted verbatim from the monorepo's `agent` module (the Responses-API
/// client itself is server-coupled and not lifted).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConversationState {
    /// Server conversation id (uuid generated client-side).
    #[serde(default)]
    pub conversation_id: String,
    /// Last response id (the chain link).
    #[serde(default)]
    pub previous_response_id: Option<String>,
    /// Last run id seen.
    #[serde(default)]
    pub active_run_id: Option<String>,
}

impl ConversationState {
    /// A fresh conversation with a new id.
    pub fn new_fresh() -> Self {
        Self {
            conversation_id: uuid::Uuid::new_v4().to_string(),
            previous_response_id: None,
            active_run_id: None,
        }
    }

    /// True when no conversation id has been assigned yet.
    pub fn is_empty(&self) -> bool {
        self.conversation_id.is_empty()
    }
}
