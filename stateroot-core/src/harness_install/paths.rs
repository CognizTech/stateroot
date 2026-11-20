//! Harness config path resolution with documented home env overrides.
//!
//! Honors only env vars the harnesses themselves document (e.g. `CODEX_HOME`,
//! `CLAUDE_CONFIG_DIR`). Does not invent overrides for harnesses without one.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::registry::{HarnessQuirk, HookTarget};

/// Codex config home override (`$CODEX_HOME` when set, else `~/.codex`).
pub const ENV_CODEX_HOME: &str = "CODEX_HOME";
/// Claude Code config root override.
pub const ENV_CLAUDE_CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";
/// Kimi Code data dir override.
pub const ENV_KIMI_CODE_HOME: &str = "KIMI_CODE_HOME";
/// CodeBuddy config and data dir override.
pub const ENV_CODEBUDDY_CONFIG_DIR: &str = "CODEBUDDY_CONFIG_DIR";
/// Grok Build CLI config home override.
pub const ENV_GROK_HOME: &str = "GROK_HOME";
/// Pi agent config directory override (`$PI_CODING_AGENT_DIR`; default `~/.pi/agent`).
pub const ENV_PI_CODING_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";

/// Relocated agent config root from an env var when non-empty; else `None`.
pub fn agent_config_home(env_override: Option<OsString>) -> Option<PathBuf> {
    let value = env_override?;
    if value.to_str().is_some_and(|s| s.trim().is_empty()) {
        return None;
    }
    Some(PathBuf::from(value))
}

/// Codex config root: `$CODEX_HOME` or `home/.codex`.
pub fn codex_root_in(home: &Path, env: Option<OsString>) -> PathBuf {
    agent_config_home(env).unwrap_or_else(|| home.join(".codex"))
}

/// Claude Code config root: `$CLAUDE_CONFIG_DIR` or `home/.claude`.
///
/// The installer already honored this relocation for skills and commands;
/// every reader (transcripts, plans, memory, todos) must resolve through the
/// same root or a relocated install reads nothing while reporting health.
pub fn claude_root_in(home: &Path, env: Option<OsString>) -> PathBuf {
    claude_config_root_in(env).unwrap_or_else(|| home.join(".claude"))
}

/// Claude config root using the process environment.
pub fn claude_root(home: &Path) -> PathBuf {
    claude_root_in(home, std::env::var_os(ENV_CLAUDE_CONFIG_DIR))
}

/// Claude session store (`projects/<slug>/<session>.jsonl` lives below this).
pub fn claude_projects_root(home: &Path) -> PathBuf {
    claude_root(home).join("projects")
}

/// Decode a Claude project slug back to its cwd: a leading `-` was `/`, every
/// other `-` was `/`. Inherently ambiguous for directory names containing
/// `-`, so callers must match tolerantly (walk-up/walk-down overlap), never
/// exactly.
pub fn claude_slug_to_cwd(slug: &str) -> String {
    let rest = slug.strip_prefix('-').unwrap_or(slug);
    // Windows drive form: `c--users-me` decodes to `c:/users/me`.
    if rest.len() > 2 && rest.as_bytes()[0].is_ascii_alphabetic() && rest[1..].starts_with("--") {
        return format!("{}:/{}", &rest[..1], rest[3..].replace('-', "/"));
    }
    format!("/{}", rest.replace('-', "/"))
}

/// Encode an absolute path as a Claude project slug (best-effort fallback when
/// no existing slug directory matches).
///
/// Every separator becomes `-`. A Windows drive separator (`c:/…`) keeps the
/// drive letter and folds the colon too (`c--users-me`), because a slug
/// containing `:` is not a valid directory name on Windows.
///
/// Deliberately *not* routed through `normalize_host_path`: that folds a WSL
/// path (`/mnt/d/project`) into its Windows spelling (`d:/project`), which
/// would encode a slug Claude on this side never uses.
pub fn claude_slug_encode(path: &str) -> String {
    let normalized = path.trim().replace('\\', "/");
    if normalized.len() > 1
        && normalized.as_bytes()[1] == b':'
        && normalized.as_bytes()[0].is_ascii_alphabetic()
    {
        let drive = normalized[..1].to_ascii_lowercase();
        return format!("{drive}{}", normalized[1..].replace([':', '/'], "-"));
    }
    let replaced = normalized.replace('/', "-");
    if replaced.starts_with('-') {
        replaced
    } else {
        format!("-{replaced}")
    }
}

/// Codex config root using the process environment.
pub fn codex_root(home: &Path) -> PathBuf {
    codex_root_in(home, std::env::var_os(ENV_CODEX_HOME))
}

/// Claude Code relocated config root when `CLAUDE_CONFIG_DIR` is set.
pub fn claude_config_root_in(env: Option<OsString>) -> Option<PathBuf> {
    agent_config_home(env)
}

/// Kimi Code data dir: `$KIMI_CODE_HOME` or `home/.kimi-code`.
pub fn kimi_code_root_in(home: &Path, env: Option<OsString>) -> PathBuf {
    agent_config_home(env).unwrap_or_else(|| home.join(".kimi-code"))
}

/// CodeBuddy config root: `$CODEBUDDY_CONFIG_DIR` or `home/.codebuddy`.
pub fn codebuddy_root_in(home: &Path, env: Option<OsString>) -> PathBuf {
    agent_config_home(env).unwrap_or_else(|| home.join(".codebuddy"))
}

/// CodeBuddy config root using the process environment.
pub fn codebuddy_root(home: &Path) -> PathBuf {
    codebuddy_root_in(home, std::env::var_os(ENV_CODEBUDDY_CONFIG_DIR))
}

/// Command Code home (`home/.commandcode`). Command Code documents no
/// config-dir override, so none is invented here.
pub fn commandcode_root(home: &Path) -> PathBuf {
    home.join(".commandcode")
}

/// Grok config root: `$GROK_HOME` or `home/.grok`.
pub fn grok_root_in(home: &Path, env: Option<OsString>) -> PathBuf {
    agent_config_home(env).unwrap_or_else(|| home.join(".grok"))
}

/// Pi agent config root: `$PI_CODING_AGENT_DIR` or `home/.pi/agent`.
pub fn pi_agent_root_in(home: &Path, env: Option<OsString>) -> PathBuf {
    agent_config_home(env).unwrap_or_else(|| home.join(".pi/agent"))
}

/// Pi agent config root using the process environment.
pub fn pi_agent_root(home: &Path) -> PathBuf {
    pi_agent_root_in(home, std::env::var_os(ENV_PI_CODING_AGENT_DIR))
}

/// DeepSeek Harness home override (`$DSH_HOME`; default `~/.dsh`).
pub const ENV_DSH_HOME: &str = "DSH_HOME";

/// DSH home: `$DSH_HOME` or `home/.dsh`.
pub fn dsh_root_in(home: &Path, env: Option<OsString>) -> PathBuf {
    agent_config_home(env).unwrap_or_else(|| home.join(".dsh"))
}

/// DSH home using the process environment.
///
/// `DSH_HOME` addresses a relocated store and always wins when it names one.
/// The *default* `$HOME/.dsh` value is ambient (a running DSH exports it to
/// every child), so when the caller passes a different home — a test or
/// sandbox home — that explicit home is authoritative and the ambient
/// default cannot redirect it into the operator's live store.
pub fn dsh_root(home: &Path) -> PathBuf {
    let raw_home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    if let Some(raw) = std::env::var_os(ENV_DSH_HOME) {
        let configured = PathBuf::from(&raw);
        let default = raw_home
            .as_ref()
            .map(|process_home| process_home.join(".dsh"));
        if default.as_deref() == Some(configured.as_path())
            && raw_home
                .as_deref()
                .is_some_and(|process_home| process_home != home)
        {
            return home.join(".dsh");
        }
        return configured;
    }
    home.join(".dsh")
}

/// Hermes home override (`$HERMES_HOME`; default `~/.hermes`) — the harness's
/// own documented override (`hermes_constants.get_hermes_home`), which the
/// shell-hook subprocess also inherits per profile.
pub const ENV_HERMES_HOME: &str = "HERMES_HOME";

/// Hermes home: `$HERMES_HOME` or `home/.hermes`.
pub fn hermes_root_in(home: &Path, env: Option<OsString>) -> PathBuf {
    agent_config_home(env).unwrap_or_else(|| home.join(".hermes"))
}

/// Hermes home using the process environment.
pub fn hermes_root(home: &Path) -> PathBuf {
    hermes_root_in(home, std::env::var_os(ENV_HERMES_HOME))
}

fn resolve_prefixed(root: PathBuf, home: &Path, rel: &str, prefix: &str) -> PathBuf {
    if rel == prefix {
        return root;
    }
    if let Some(suffix) = rel.strip_prefix(&format!("{prefix}/")) {
        return root.join(suffix);
    }
    home.join(rel)
}

fn resolve_claude_path_in(home: &Path, rel: &str, env: Option<OsString>) -> PathBuf {
    if let Some(root) = claude_config_root_in(env) {
        return match rel {
            ".claude.json" => root.join(".claude.json"),
            ".claude/settings.json" => root.join("settings.json"),
            ".claude/CLAUDE.md" => root.join("CLAUDE.md"),
            path if path.starts_with(".claude/") => {
                root.join(path.strip_prefix(".claude/").unwrap())
            }
            _ => home.join(rel),
        };
    }
    home.join(rel)
}

/// Resolve a registry home-relative path for `quirk_id`, honoring env overrides.
#[allow(clippy::too_many_arguments)]
pub fn resolve_registry_path_in(
    home: &Path,
    quirk_id: &str,
    rel: &str,
    codex_home: Option<OsString>,
    claude_config: Option<OsString>,
    kimi_code_home: Option<OsString>,
    grok_home: Option<OsString>,
    hermes_home: Option<OsString>,
    codebuddy_home: Option<OsString>,
) -> PathBuf {
    match quirk_id {
        "codex" => resolve_prefixed(codex_root_in(home, codex_home), home, rel, ".codex"),
        "claude-code" => resolve_claude_path_in(home, rel, claude_config),
        "kimi-code" => resolve_prefixed(
            kimi_code_root_in(home, kimi_code_home),
            home,
            rel,
            ".kimi-code",
        ),
        "codebuddy" => resolve_prefixed(
            codebuddy_root_in(home, codebuddy_home),
            home,
            rel,
            ".codebuddy",
        ),
        "grok" => resolve_prefixed(grok_root_in(home, grok_home), home, rel, ".grok"),
        "hermes" => resolve_prefixed(hermes_root_in(home, hermes_home), home, rel, ".hermes"),
        _ => home.join(rel),
    }
}

/// Resolve a registry home-relative path using the process environment.
pub fn resolve_registry_path(home: &Path, quirk_id: &str, rel: &str) -> PathBuf {
    resolve_registry_path_in(
        home,
        quirk_id,
        rel,
        std::env::var_os(ENV_CODEX_HOME),
        std::env::var_os(ENV_CLAUDE_CONFIG_DIR),
        std::env::var_os(ENV_KIMI_CODE_HOME),
        std::env::var_os(ENV_GROK_HOME),
        std::env::var_os(ENV_HERMES_HOME),
        std::env::var_os(ENV_CODEBUDDY_CONFIG_DIR),
    )
}

/// Active install path plus the legacy default when a relocation env is in play.
#[allow(clippy::too_many_arguments)]
pub fn registry_path_candidates_in(
    home: &Path,
    quirk_id: &str,
    rel: &str,
    codex_home: Option<OsString>,
    claude_config: Option<OsString>,
    kimi_code_home: Option<OsString>,
    grok_home: Option<OsString>,
    hermes_home: Option<OsString>,
    codebuddy_home: Option<OsString>,
) -> Vec<PathBuf> {
    let active = resolve_registry_path_in(
        home,
        quirk_id,
        rel,
        codex_home,
        claude_config,
        kimi_code_home,
        grok_home,
        hermes_home,
        codebuddy_home,
    );
    let default = home.join(rel);
    if default == active {
        vec![active]
    } else {
        vec![active, default]
    }
}

/// Active install path plus the legacy default when a relocation env is in play.
pub fn registry_path_candidates(home: &Path, quirk_id: &str, rel: &str) -> Vec<PathBuf> {
    registry_path_candidates_in(
        home,
        quirk_id,
        rel,
        std::env::var_os(ENV_CODEX_HOME),
        std::env::var_os(ENV_CLAUDE_CONFIG_DIR),
        std::env::var_os(ENV_KIMI_CODE_HOME),
        std::env::var_os(ENV_GROK_HOME),
        std::env::var_os(ENV_HERMES_HOME),
        std::env::var_os(ENV_CODEBUDDY_CONFIG_DIR),
    )
}

/// Hook config path for install (honors env overrides).
pub fn hook_target_path(home: &Path, quirk: &HarnessQuirk) -> Option<PathBuf> {
    quirk
        .hooks
        .as_ref()
        .map(|target: &HookTarget| resolve_registry_path(home, quirk.id, target.path))
}

/// Hook config paths to sweep on uninstall (active + legacy default).
pub fn hook_target_candidates(home: &Path, quirk: &HarnessQuirk) -> Vec<PathBuf> {
    quirk
        .hooks
        .as_ref()
        .map(|target| registry_path_candidates(home, quirk.id, target.path))
        .unwrap_or_default()
}

/// Instruction file path for install.
pub fn instruction_file_path(home: &Path, quirk: &HarnessQuirk) -> Option<PathBuf> {
    quirk
        .instruction_file
        .map(|rel| resolve_registry_path(home, quirk.id, rel))
}

/// Instruction file paths to sweep on uninstall.
pub fn instruction_file_candidates(home: &Path, quirk: &HarnessQuirk) -> Vec<PathBuf> {
    quirk
        .instruction_file
        .map(|rel| registry_path_candidates(home, quirk.id, rel))
        .unwrap_or_default()
}

/// MCP config path for install.
pub fn mcp_target_path(home: &Path, quirk: &HarnessQuirk) -> Option<PathBuf> {
    quirk
        .mcp
        .as_ref()
        .map(|target| resolve_registry_path(home, quirk.id, target.path))
}

/// MCP config paths to sweep on uninstall.
pub fn mcp_target_candidates(home: &Path, quirk: &HarnessQuirk) -> Vec<PathBuf> {
    quirk
        .mcp
        .as_ref()
        .map(|target| registry_path_candidates(home, quirk.id, target.path))
        .unwrap_or_default()
}

/// True when any detection marker exists (relocated or legacy default).
pub fn quirk_detected(home: &Path, quirk: &HarnessQuirk) -> bool {
    if quirk.id == "pi" {
        let root = pi_agent_root(home);
        if root.is_dir() || root.is_file() {
            return true;
        }
    }
    quirk.detect.iter().any(|marker| {
        registry_path_candidates(home, quirk.id, marker)
            .iter()
            .any(|path| path.is_dir() || path.is_file())
    })
}

/// Codex transcript store roots (`sessions`, `archived_sessions`).
pub fn codex_transcript_roots(home: &Path) -> (PathBuf, PathBuf) {
    let root = codex_root(home);
    (root.join("sessions"), root.join("archived_sessions"))
}

/// Claude skill install destination (honors `CLAUDE_CONFIG_DIR`).
pub fn claude_skill_dest(home: &Path) -> PathBuf {
    if let Some(root) = claude_config_root_in(std::env::var_os(ENV_CLAUDE_CONFIG_DIR)) {
        root.join("skills/stateroot")
    } else {
        home.join(".claude/skills/stateroot")
    }
}

/// Claude slash-command stub destination (honors `CLAUDE_CONFIG_DIR`).
pub fn claude_command_dest(home: &Path) -> PathBuf {
    if let Some(root) = claude_config_root_in(std::env::var_os(ENV_CLAUDE_CONFIG_DIR)) {
        root.join("commands/stateroot.md")
    } else {
        home.join(".claude/commands/stateroot.md")
    }
}

/// Claude extras paths to remove on uninstall (relocated + legacy default).
pub fn claude_extras_candidates(home: &Path) -> Vec<PathBuf> {
    let mut paths = vec![claude_skill_dest(home), claude_command_dest(home)];
    for rel in [".claude/skills/stateroot", ".claude/commands/stateroot.md"] {
        let legacy = home.join(rel);
        if !paths.contains(&legacy) {
            paths.push(legacy);
        }
    }
    paths
}

/// Hook command string — absolute `stateroot` path when `current_exe` is the real CLI.
pub fn hook_command(quirk_id: &str, canonical: &str) -> String {
    let suffix = format!("hook {canonical} --harness {quirk_id}");
    if let Ok(exe) = std::env::current_exe() {
        if exe.file_stem().and_then(|s| s.to_str()) == Some("stateroot") {
            if let Ok(abs) = std::fs::canonicalize(&exe) {
                return format!("{} {suffix}", abs.display());
            }
        }
    }
    format!("stateroot {suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn claude_root_honors_config_dir_for_readers() {
        let home = Path::new("/home/alice");
        // Default: the documented `~/.claude` layout.
        assert_eq!(
            claude_root_in(home, None),
            PathBuf::from("/home/alice/.claude")
        );
        // Relocated install: readers must follow the installer.
        assert_eq!(
            claude_root_in(home, Some(OsString::from("/stores/claude"))),
            PathBuf::from("/stores/claude")
        );
        // An empty override is not a relocation (falls back to the default).
        assert_eq!(
            claude_root_in(home, Some(OsString::from("  "))),
            PathBuf::from("/home/alice/.claude")
        );
        // The session store hangs off the same root (only asserted when the
        // ambient environment does not relocate it — tests run in parallel).
        if std::env::var_os(ENV_CLAUDE_CONFIG_DIR).is_none() {
            assert_eq!(
                claude_projects_root(home),
                PathBuf::from("/home/alice/.claude/projects")
            );
        }
    }

    #[test]
    fn claude_slug_folds_windows_separators_into_a_valid_directory_name() {
        // POSIX shape: a leading `/` becomes a leading `-`.
        assert_eq!(claude_slug_encode("/home/alice"), "-home-alice");
        assert_eq!(claude_slug_to_cwd("-home-alice"), "/home/alice");
        // WSL keeps its /mnt spelling: Claude on this side never writes the
        // Windows drive form, so the fallback must not invent it.
        assert_eq!(
            claude_slug_encode("/mnt/d/siderai/project"),
            "-mnt-d-siderai-project"
        );
        assert_eq!(
            claude_slug_to_cwd("-mnt-d-siderai-project"),
            "/mnt/d/siderai/project"
        );
        // Windows shape: the drive colon must not survive — `:` is not a
        // valid directory character there.
        let encoded = claude_slug_encode("C:\\Users\\Me\\work");
        assert_eq!(encoded, "c--Users-Me-work");
        assert!(
            !encoded.contains(':'),
            "a slug with a colon cannot be created on Windows"
        );
        assert_eq!(claude_slug_to_cwd(&encoded), "c:/Users/Me/work");
        // Both shapes decode back to something the project matcher can compare.
        assert_eq!(
            crate::path_identity::normalize_host_path(&claude_slug_to_cwd(&encoded)),
            crate::path_identity::normalize_host_path("C:\\Users\\Me\\work")
        );
    }

    #[test]
    fn codex_home_honors_env_override() {
        let home = Path::new("/home/alice");
        let resolved = resolve_registry_path_in(
            home,
            "codex",
            ".codex/hooks.json",
            Some(OsString::from("/stores/codex")),
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(resolved, PathBuf::from("/stores/codex/hooks.json"));
    }

    #[test]
    fn codex_home_defaults_to_dot_codex() {
        let home = Path::new("/home/alice");
        let resolved = resolve_registry_path_in(
            home,
            "codex",
            ".codex/AGENTS.md",
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(resolved, PathBuf::from("/home/alice/.codex/AGENTS.md"));
    }

    #[test]
    fn claude_config_dir_rewrites_settings_and_agents() {
        let home = Path::new("/home/alice");
        let env = Some(OsString::from("/stores/claude"));
        assert_eq!(
            resolve_registry_path_in(
                home,
                "claude-code",
                ".claude/settings.json",
                None,
                env.clone(),
                None,
                None,
                None,
                None
            ),
            PathBuf::from("/stores/claude/settings.json")
        );
        assert_eq!(
            resolve_registry_path_in(
                home,
                "claude-code",
                ".claude/CLAUDE.md",
                None,
                env.clone(),
                None,
                None,
                None,
                None
            ),
            PathBuf::from("/stores/claude/CLAUDE.md")
        );
        assert_eq!(
            resolve_registry_path_in(
                home,
                "claude-code",
                ".claude.json",
                None,
                env,
                None,
                None,
                None,
                None
            ),
            PathBuf::from("/stores/claude/.claude.json")
        );
    }

    #[test]
    fn blank_env_override_is_treated_as_unset() {
        for env in [Some(OsString::new()), Some(OsString::from("   "))] {
            assert_eq!(agent_config_home(env.clone()), None);
            assert_eq!(
                codex_root_in(Path::new("/home/alice"), env),
                PathBuf::from("/home/alice/.codex")
            );
        }
    }

    #[test]
    fn registry_path_candidates_include_legacy_default() {
        let home = Path::new("/home/alice");
        let paths = registry_path_candidates_in(
            home,
            "codex",
            ".codex/hooks.json",
            Some(OsString::from("/stores/codex")),
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/stores/codex/hooks.json"),
                PathBuf::from("/home/alice/.codex/hooks.json"),
            ]
        );
    }

    #[test]
    fn hermes_home_honors_env_override() {
        let home = Path::new("/home/alice");
        let resolved = resolve_registry_path_in(
            home,
            "hermes",
            ".hermes/config.yaml",
            None,
            None,
            None,
            None,
            Some(OsString::from("/stores/hermes")),
            None,
        );
        assert_eq!(resolved, PathBuf::from("/stores/hermes/config.yaml"));
        // Default keeps ~/.hermes; candidates sweep both when relocated.
        let candidates = registry_path_candidates_in(
            home,
            "hermes",
            ".hermes/config.yaml",
            None,
            None,
            None,
            None,
            Some(OsString::from("/stores/hermes")),
            None,
        );
        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/stores/hermes/config.yaml"),
                PathBuf::from("/home/alice/.hermes/config.yaml"),
            ]
        );
        assert_eq!(
            hermes_root_in(home, None),
            PathBuf::from("/home/alice/.hermes")
        );
    }

    #[test]
    fn codebuddy_config_dir_rewrites_native_paths() {
        let home = Path::new("/home/alice");
        let resolved = resolve_registry_path_in(
            home,
            "codebuddy",
            ".codebuddy/settings.json",
            None,
            None,
            None,
            None,
            None,
            Some(OsString::from("/stores/codebuddy")),
        );
        assert_eq!(resolved, PathBuf::from("/stores/codebuddy/settings.json"));
        assert_eq!(
            codebuddy_root_in(home, None),
            PathBuf::from("/home/alice/.codebuddy")
        );
    }

    #[test]
    fn hook_command_stays_bare_when_not_stateroot_exe() {
        // Unit tests run under the test harness binary, not `stateroot`.
        let cmd = hook_command("codex", "session_start");
        assert!(cmd.starts_with("stateroot hook session_start"));
        assert!(!cmd.starts_with('/'));
    }

    #[test]
    fn pi_agent_root_honors_env_override() {
        let home = Path::new("/home/alice");
        assert_eq!(
            pi_agent_root_in(home, None),
            PathBuf::from("/home/alice/.pi/agent")
        );
        assert_eq!(
            pi_agent_root_in(home, Some(OsString::from("/stores/pi"))),
            PathBuf::from("/stores/pi")
        );
    }
}
