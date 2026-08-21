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
pub fn dsh_root(home: &Path) -> PathBuf {
    dsh_root_in(home, std::env::var_os(ENV_DSH_HOME))
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
pub fn resolve_registry_path_in(
    home: &Path,
    quirk_id: &str,
    rel: &str,
    codex_home: Option<OsString>,
    claude_config: Option<OsString>,
    kimi_code_home: Option<OsString>,
    grok_home: Option<OsString>,
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
        "grok" => resolve_prefixed(grok_root_in(home, grok_home), home, rel, ".grok"),
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
    )
}

/// Active install path plus the legacy default when a relocation env is in play.
pub fn registry_path_candidates_in(
    home: &Path,
    quirk_id: &str,
    rel: &str,
    codex_home: Option<OsString>,
    claude_config: Option<OsString>,
    kimi_code_home: Option<OsString>,
    grok_home: Option<OsString>,
) -> Vec<PathBuf> {
    let active = resolve_registry_path_in(
        home,
        quirk_id,
        rel,
        codex_home,
        claude_config,
        kimi_code_home,
        grok_home,
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
        );
        assert_eq!(resolved, PathBuf::from("/stores/codex/hooks.json"));
    }

    #[test]
    fn codex_home_defaults_to_dot_codex() {
        let home = Path::new("/home/alice");
        let resolved =
            resolve_registry_path_in(home, "codex", ".codex/AGENTS.md", None, None, None, None);
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
                None
            ),
            PathBuf::from("/stores/claude/CLAUDE.md")
        );
        assert_eq!(
            resolve_registry_path_in(home, "claude-code", ".claude.json", None, env, None, None),
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
