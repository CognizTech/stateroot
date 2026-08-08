//! Portable OpenClaw identity discovery.
//!
//! Resolves the OpenClaw **state dir** and **agent workspace(s)** the same way
//! OpenClaw itself does (docs: agent-workspace), then composes the working
//! identity from the standard bootstrap files:
//!
//! - `SOUL.md` — persona / tone / boundaries
//! - `IDENTITY.md` — agent name / vibe
//! - `USER.md` — human profile
//! - `MEMORY.md` — long-term memory (returned separately; not mixed into soul)
//!
//! Resolution order for workspaces (first existing wins for the default pack;
//! additional agent workspaces are also returned when they contain identity files):
//!
//! 1. `OPENCLAW_WORKSPACE_DIR` — a real OpenClaw override used by its Docker
//!    setup / e2e harness (openclaw `docs/help/testing.md`); honored so
//!    containerized installs resolve identically.
//! 2. `agents.defaults.workspace` + `agents.list[].workspace` from
//!    `OPENCLAW_CONFIG_PATH` or `<state>/openclaw.json` (legacy
//!    `agents.entries.*.workspace` accepted as a fallback)
//! 3. `<state>/workspace` (or `workspace-<OPENCLAW_PROFILE>`)
//! 4. Legacy `~/openclaw`
//! 5. Any `<state>/workspace-*` directory that contains `SOUL.md` / `IDENTITY.md`

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// One discovered OpenClaw workspace with composed identity material.
#[derive(Debug, Clone)]
pub struct OpenClawIdentity {
    /// Human label for setup prompts.
    pub label: String,
    /// Origin id (`openclaw`).
    pub origin: &'static str,
    /// Absolute workspace path.
    pub workspace: PathBuf,
    /// Composed SOUL + IDENTITY + USER markdown (ready for soul import).
    pub identity_markdown: String,
    /// Absolute paths that contributed to `identity_markdown`.
    pub identity_files: Vec<PathBuf>,
    /// Optional MEMORY.md body (for later memory ingest — not soul).
    pub memory_markdown: Option<String>,
    /// Absolute MEMORY.md path when present.
    pub memory_path: Option<PathBuf>,
}

/// Resolve OpenClaw state directory (`OPENCLAW_STATE_DIR` or `~/.openclaw`).
pub fn openclaw_state_dir(home: &Path) -> PathBuf {
    if let Ok(raw) = std::env::var("OPENCLAW_STATE_DIR") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return expand_user_path(trimmed, home);
        }
    }
    home.join(".openclaw")
}

/// Path to `openclaw.json` (`OPENCLAW_CONFIG_PATH` or `<state>/openclaw.json`).
pub fn openclaw_config_path(home: &Path) -> PathBuf {
    if let Ok(raw) = std::env::var("OPENCLAW_CONFIG_PATH") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return expand_user_path(trimmed, home);
        }
    }
    openclaw_state_dir(home).join("openclaw.json")
}

/// Resolve every configured OpenClaw agent workspace under `home`.
///
/// This is shared by identity import and skill federation. It deliberately
/// returns existing workspaces even when they do not contain identity files:
/// a workspace may legitimately contain only `skills/`.
pub fn discover_openclaw_workspace_dirs(home: &Path) -> Vec<PathBuf> {
    let mut workspaces = Vec::new();
    let mut seen = BTreeSet::new();

    let push = |path: PathBuf, workspaces: &mut Vec<PathBuf>, seen: &mut BTreeSet<String>| {
        let Ok(canon) = path
            .canonicalize()
            .or_else(|_| Ok::<_, std::io::Error>(path.clone()))
        else {
            return;
        };
        if !canon.is_dir() {
            return;
        }
        let key = canon.to_string_lossy().to_ascii_lowercase();
        if seen.insert(key) {
            workspaces.push(canon);
        }
    };

    if let Ok(raw) = std::env::var("OPENCLAW_WORKSPACE_DIR") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            push(expand_user_path(trimmed, home), &mut workspaces, &mut seen);
        }
    }

    let state = openclaw_state_dir(home);
    let config_path = openclaw_config_path(home);
    if let Some(cfg) = read_jsonish(&config_path) {
        for ws in workspaces_from_config(&cfg, home, &state) {
            push(ws, &mut workspaces, &mut seen);
        }
    }

    // Default workspace (+ profile variant).
    let profile = std::env::var("OPENCLAW_PROFILE")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "default");
    let default_ws = match &profile {
        Some(p) => state.join(format!("workspace-{p}")),
        None => state.join("workspace"),
    };
    push(default_ws, &mut workspaces, &mut seen);

    // Legacy location from older OpenClaw installs.
    push(home.join("openclaw"), &mut workspaces, &mut seen);

    // Any sibling workspace-* dirs that look like agent homes.
    if let Ok(entries) = std::fs::read_dir(&state) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "workspace" || name.starts_with("workspace-") {
                push(entry.path(), &mut workspaces, &mut seen);
            }
        }
    }

    workspaces
}

/// Discover OpenClaw identity packs under `home` (OS-portable).
pub fn discover_openclaw_identities(home: &Path) -> Vec<OpenClawIdentity> {
    discover_openclaw_workspace_dirs(home)
        .into_iter()
        .filter_map(compose_identity)
        .collect()
}

fn compose_identity(workspace: PathBuf) -> Option<OpenClawIdentity> {
    let soul = read_workspace_file(&workspace, &["SOUL.md", "soul.md"]);
    let identity = read_workspace_file(&workspace, &["IDENTITY.md", "identity.md"]);
    let user = read_workspace_file(&workspace, &["USER.md", "user.md"]);
    let memory = read_workspace_file(&workspace, &["MEMORY.md", "memory.md"]);

    if soul.is_none() && identity.is_none() && user.is_none() {
        return None;
    }

    let mut files = Vec::new();
    let mut sections = Vec::new();
    sections.push("# Soul".to_string());
    sections.push(String::new());
    sections.push(format!(
        "<!-- composed from openclaw workspace {} -->",
        workspace.display()
    ));
    sections.push(String::new());

    // Identity + User first: they are short and carry the names. The compact
    // per-harness projection is line-capped, so long persona prose must not
    // push "who am I / who are you" out of the window.
    if let Some((path, text)) = &identity {
        files.push(path.clone());
        sections.push("## Identity (IDENTITY.md)".to_string());
        sections.push(String::new());
        sections.push(strip_outer_h1(text));
        sections.push(String::new());
    }
    if let Some((path, text)) = &user {
        files.push(path.clone());
        sections.push("## User (USER.md)".to_string());
        sections.push(String::new());
        sections.push(strip_outer_h1(text));
        sections.push(String::new());
    }
    if let Some((path, text)) = &soul {
        files.push(path.clone());
        sections.push("## Persona (SOUL.md)".to_string());
        sections.push(String::new());
        sections.push(strip_outer_h1(text));
        sections.push(String::new());
    }

    let identity_markdown = sections.join("\n").trim().to_string() + "\n";
    if identity_markdown.trim().is_empty() {
        return None;
    }

    let (memory_path, memory_markdown) = match memory {
        Some((path, text)) => (Some(path), Some(text)),
        None => (None, None),
    };

    let short = workspace
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("workspace");
    Some(OpenClawIdentity {
        label: format!("openclaw workspace ({short}) — {} file(s)", files.len()),
        origin: "openclaw",
        workspace,
        identity_markdown,
        identity_files: files,
        memory_markdown,
        memory_path,
    })
}

fn read_workspace_file(workspace: &Path, names: &[&str]) -> Option<(PathBuf, String)> {
    for name in names {
        let path = workspace.join(name);
        if let Ok(text) = std::fs::read_to_string(&path) {
            let trimmed = text.trim().to_string();
            if !trimmed.is_empty() {
                return Some((path, trimmed));
            }
        }
    }
    None
}

fn strip_outer_h1(text: &str) -> String {
    let mut lines = text.lines().peekable();
    if let Some(first) = lines.peek() {
        let t = first.trim();
        if t.starts_with("# ") && !t.starts_with("## ") {
            lines.next();
            while matches!(lines.peek().map(|l| l.trim()), Some("")) {
                lines.next();
            }
        }
    }
    lines.collect::<Vec<_>>().join("\n").trim().to_string()
}

fn workspaces_from_config(cfg: &Value, home: &Path, state: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let agents = cfg.get("agents").unwrap_or(&Value::Null);

    if let Some(ws) = agents
        .pointer("/defaults/workspace")
        .and_then(|v| v.as_str())
    {
        out.push(expand_user_path(ws, home));
    }

    // Real schema (current OpenClaw): `agents.list[]` — an array of agent
    // objects with `id`, optional `default`, and optional `workspace`.
    if let Some(list) = agents.get("list").and_then(|v| v.as_array()) {
        if !list.is_empty() {
            for entry in list {
                let id = entry.get("id").and_then(Value::as_str).unwrap_or("");
                let is_default = entry
                    .get("default")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || matches!(id, "" | "main" | "default");
                if let Some(ws) = entry.get("workspace").and_then(|v| v.as_str()) {
                    out.push(expand_user_path(ws, home));
                } else if !is_default {
                    // Non-default agents without explicit workspace → state/workspace-<id>
                    out.push(state.join(format!("workspace-{id}")));
                }
            }
            return out;
        }
    }

    // Legacy fallback: `agents.entries` object map keyed by agent id.
    if let Some(entries) = agents.get("entries").and_then(|v| v.as_object()) {
        for (agent_id, entry) in entries {
            if let Some(ws) = entry.get("workspace").and_then(|v| v.as_str()) {
                out.push(expand_user_path(ws, home));
            } else {
                // Non-default agents without explicit workspace → state/workspace-<id>
                if agent_id != "main" && agent_id != "default" {
                    out.push(state.join(format!("workspace-{agent_id}")));
                }
            }
        }
    }
    out
}

/// Expand `~` / `%USERPROFILE%`-style prefixes against `home`.
pub fn expand_user_path(raw: &str, home: &Path) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return PathBuf::new();
    }
    if trimmed == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return home.join(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("~\\") {
        return home.join(rest);
    }
    // Windows: %USERPROFILE%\...
    if let Some(rest) = trimmed
        .strip_prefix("%USERPROFILE%\\")
        .or_else(|| trimmed.strip_prefix("%USERPROFILE%/"))
        .or_else(|| trimmed.strip_prefix("%userprofile%\\"))
        .or_else(|| trimmed.strip_prefix("%userprofile%/"))
    {
        return home.join(rest);
    }
    PathBuf::from(trimmed)
}

fn read_jsonish(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        return Some(v);
    }
    // Tolerant JSON5-ish: drop // line comments and trailing commas.
    let cleaned = strip_json_comments(&text);
    serde_json::from_str(&cleaned).ok()
}

fn strip_json_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for line in input.lines() {
        let mut truncated = line;
        if let Some(idx) = find_unquoted(line, "//") {
            truncated = &line[..idx];
        }
        out.push_str(truncated);
        out.push('\n');
    }
    // Remove trailing commas before } or ]
    let mut cleaned = String::with_capacity(out.len());
    let chars: Vec<char> = out.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                i += 1;
                continue;
            }
        }
        cleaned.push(chars[i]);
        i += 1;
    }
    cleaned
}

fn find_unquoted(line: &str, needle: &str) -> Option<usize> {
    let mut in_string = false;
    let mut escape = false;
    let bytes = line.as_bytes();
    let n = needle.as_bytes();
    let mut i = 0;
    while i + n.len() <= bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(n) {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn expands_tilde_and_userprofile() {
        let home = PathBuf::from("/Users/ada");
        assert_eq!(
            expand_user_path("~/.openclaw/workspace", &home),
            PathBuf::from("/Users/ada/.openclaw/workspace")
        );
        assert_eq!(
            expand_user_path("%USERPROFILE%\\.openclaw\\workspace", &home),
            home.join(".openclaw\\workspace")
        );
    }

    #[test]
    fn discovers_composed_pack_from_default_workspace() {
        let home = tempfile::tempdir().expect("home");
        let ws = home.path().join(".openclaw/workspace");
        fs::create_dir_all(&ws).expect("mkdir");
        fs::write(ws.join("SOUL.md"), "# Soul\n\nBe direct.\n").expect("soul");
        fs::write(ws.join("IDENTITY.md"), "# Identity\n\nName: Yinyue\n").expect("id");
        fs::write(ws.join("USER.md"), "# User\n\nName: Han Li\n").expect("user");
        fs::write(ws.join("MEMORY.md"), "# Memory\n\nWe shipped the demo.\n").expect("mem");

        let packs = discover_openclaw_identities(home.path());
        assert_eq!(packs.len(), 1);
        let pack = &packs[0];
        assert!(pack.identity_markdown.contains("Be direct."));
        assert!(pack.identity_markdown.contains("Yinyue"));
        assert!(pack.identity_markdown.contains("Han Li"));
        assert!(!pack.identity_markdown.contains("We shipped the demo."));
        assert_eq!(
            pack.memory_markdown.as_deref(),
            Some("# Memory\n\nWe shipped the demo.")
        );
        assert_eq!(pack.identity_files.len(), 3);
    }

    #[test]
    fn reads_workspace_from_openclaw_json() {
        let home = tempfile::tempdir().expect("home");
        let state = home.path().join(".openclaw");
        let custom = home.path().join("Agents/Yinyue");
        fs::create_dir_all(&state).expect("state");
        fs::create_dir_all(&custom).expect("custom");
        fs::write(
            state.join("openclaw.json"),
            r#"{
  // comment ok
  "agents": {
    "defaults": {
      "workspace": "~/Agents/Yinyue",
    },
  },
}"#,
        )
        .expect("cfg");
        fs::write(custom.join("SOUL.md"), "Custom persona\n").expect("soul");

        let packs = discover_openclaw_identities(home.path());
        assert_eq!(packs.len(), 1);
        assert!(packs[0].workspace.ends_with("Yinyue"));
        assert!(packs[0].identity_markdown.contains("Custom persona"));
    }

    #[test]
    fn empty_workspace_yields_nothing() {
        let home = tempfile::tempdir().expect("home");
        fs::create_dir_all(home.path().join(".openclaw/workspace")).expect("mkdir");
        assert!(discover_openclaw_identities(home.path()).is_empty());
    }

    #[test]
    fn reads_agents_list_schema_and_ignores_entries() {
        let home = tempfile::tempdir().expect("home");
        let state = home.path().join(".openclaw");
        let ws_a = home.path().join("agents/alpha");
        let ws_b = home.path().join("agents/beta");
        fs::create_dir_all(&state).expect("state");
        fs::create_dir_all(&ws_a).expect("a");
        fs::create_dir_all(&ws_b).expect("b");
        fs::write(ws_a.join("SOUL.md"), "Alpha persona\n").expect("soul a");
        fs::write(ws_b.join("SOUL.md"), "Beta persona\n").expect("soul b");
        fs::write(
            state.join("openclaw.json"),
            r#"{
  "agents": {
    "list": [
      { "id": "main", "default": true, "workspace": "~/agents/alpha" },
      { "id": "research", "workspace": "~/agents/beta" }
    ],
    "entries": { "stale": { "workspace": "~/nope" } }
  }
}"#,
        )
        .expect("cfg");

        let packs = discover_openclaw_identities(home.path());
        assert!(
            packs
                .iter()
                .any(|p| p.identity_markdown.contains("Alpha persona")),
            "agents.list[0] workspace must resolve: {packs:#?}"
        );
        assert!(
            packs
                .iter()
                .any(|p| p.identity_markdown.contains("Beta persona")),
            "agents.list[1] workspace must resolve: {packs:#?}"
        );
        assert!(
            !packs.iter().any(|p| p.workspace.ends_with("nope")),
            "agents.entries must be ignored when agents.list is present"
        );
    }

    #[test]
    fn falls_back_to_agents_entries_when_list_absent() {
        let home = tempfile::tempdir().expect("home");
        let state = home.path().join(".openclaw");
        let ws = home.path().join("agents/legacy");
        fs::create_dir_all(&state).expect("state");
        fs::create_dir_all(&ws).expect("ws");
        fs::write(ws.join("SOUL.md"), "Legacy persona\n").expect("soul");
        fs::write(
            state.join("openclaw.json"),
            r#"{ "agents": { "entries": { "research": { "workspace": "~/agents/legacy" } } } }"#,
        )
        .expect("cfg");

        let packs = discover_openclaw_identities(home.path());
        assert_eq!(packs.len(), 1);
        assert!(packs[0].identity_markdown.contains("Legacy persona"));
    }
}
