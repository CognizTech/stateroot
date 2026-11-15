//! DeepSeek Harness (DSH) native integration.
//!
//! DSH has no lifecycle-hook configuration file. Its extension channel is a
//! cordis plugin loaded from the profile tree, and its user layer is a
//! home-level patch (`$DSH_HOME/cordis.patch.yml`) applied over every profile.
//! This module owns both halves of the integration:
//!
//! - a generated, dependency-free `hooks.mjs` next to the patch, which
//!   forwards `agent/pre-step` (digest injection), `tools/result`,
//!   `agent/turn-stopping`, `compaction/end` and `session/disposed` to
//!   `stateroot hook … --harness dsh`;
//! - two patch entries: the plugin insert and an
//!   `@deepseek-ai/dsh-mcp-client` instance that serves `stateroot mcp-stdio`.
//!
//! The patch is edited as a marked block, never re-serialized: operator
//! comments and foreign entries survive verbatim. The block is idempotent and
//! a reinstall with unchanged inputs writes nothing.

use std::path::{Path, PathBuf};

use super::{io_err, HarnessError};

/// Begin marker of the managed block in `$DSH_HOME/cordis.patch.yml`.
pub const MANAGED_BEGIN: &str = "# >>> stateroot managed (do not edit)";
/// End marker of the managed block.
pub const MANAGED_END: &str = "# <<< stateroot managed";

/// Embedded plugin source (regenerated on every install).
const PLUGIN_SOURCE: &str = include_str!("dsh/hooks.mjs");

/// `$DSH_HOME/stateroot` — the managed plugin directory.
pub fn plugin_dir(home: &Path) -> PathBuf {
    super::paths::dsh_root(home).join("stateroot")
}

/// `$DSH_HOME/stateroot/hooks.mjs`.
pub fn plugin_path(home: &Path) -> PathBuf {
    plugin_dir(home).join("hooks.mjs")
}

/// `$DSH_HOME/cordis.patch.yml` — the home-level user patch layer.
pub fn patch_path(home: &Path) -> PathBuf {
    super::paths::dsh_root(home).join("cordis.patch.yml")
}

/// The binary to embed in the plugin and MCP config.
///
/// `STATEROOT_BIN` wins so a relocated install (or a dev build) can name the
/// canonical executable; otherwise the running binary is authoritative.
pub fn install_command() -> PathBuf {
    if let Some(raw) = std::env::var_os("STATEROOT_BIN") {
        if !raw.is_empty() {
            return PathBuf::from(raw);
        }
    }
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("stateroot"))
}

fn yaml_string(value: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(value.to_string())
}

fn yaml_key(key: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(key.to_string())
}

fn insert_entry(id: &str, name: &str, config: Option<serde_yaml::Value>) -> serde_yaml::Value {
    let mut entry = serde_yaml::Mapping::new();
    entry.insert(yaml_key("id"), yaml_string(id));
    entry.insert(yaml_key("name"), yaml_string(name));
    if let Some(config) = config {
        entry.insert(yaml_key("config"), config);
    }
    let mut insert = serde_yaml::Mapping::new();
    insert.insert(
        yaml_key("insert"),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(entry)]),
    );
    serde_yaml::Value::Mapping(insert)
}

fn mcp_config(command: &Path) -> serde_yaml::Value {
    let mut config = serde_yaml::Mapping::new();
    config.insert(yaml_key("transport"), yaml_string("stdio"));
    config.insert(yaml_key("serverName"), yaml_string("stateroot"));
    config.insert(yaml_key("command"), yaml_string(&command.to_string_lossy()));
    config.insert(
        yaml_key("args"),
        serde_yaml::Value::Sequence(vec![yaml_string("mcp-stdio")]),
    );
    // A moved or deleted binary must not stop DSH itself from booting; doctor
    // and the integration-health document report the missing server instead.
    config.insert(
        yaml_key("failOnStartupError"),
        serde_yaml::Value::Bool(false),
    );
    serde_yaml::Value::Mapping(config)
}

fn hook_config(command: &Path) -> serde_yaml::Value {
    let mut config = serde_yaml::Mapping::new();
    config.insert(yaml_key("command"), yaml_string(&command.to_string_lossy()));
    serde_yaml::Value::Mapping(config)
}

/// Render the complete managed block for one binary.
fn render_block(command: &Path) -> Result<String, HarnessError> {
    let entries = serde_yaml::Value::Sequence(vec![
        insert_entry(
            "stateroot-hooks",
            "./stateroot/hooks.mjs",
            Some(hook_config(command)),
        ),
        insert_entry(
            "stateroot-mcp",
            "@deepseek-ai/dsh-mcp-client",
            Some(mcp_config(command)),
        ),
    ]);
    let body = serde_yaml::to_string(&entries)
        .map_err(|err| HarnessError::Invalid(format!("DSH patch serialization failed: {err}")))?;
    Ok(format!(
        "{MANAGED_BEGIN}\n{}\n{MANAGED_END}",
        body.trim_end()
    ))
}

/// Byte range of the managed block, markers included.
fn block_range(text: &str) -> Option<(usize, usize)> {
    block_range_for(text, MANAGED_BEGIN, MANAGED_END)
}

fn block_range_for(text: &str, begin: &str, end: &str) -> Option<(usize, usize)> {
    fn marker_offset(text: &str, marker: &str) -> Option<usize> {
        let mut offset = 0;
        for line in text.split_inclusive('\n') {
            if line.trim_end_matches(['\r', '\n']) == marker {
                return Some(offset);
            }
            offset += line.len();
        }
        None
    }
    let start = marker_offset(text, begin)?;
    let after_start = start + begin.len();
    let relative_end = marker_offset(&text[after_start..], end)?;
    let end = after_start + relative_end + end.len();
    Some((start, end))
}

/// Remove the managed block, preserving everything the operator wrote.
pub fn strip_managed_block(text: &str) -> String {
    let Some((start, end)) = block_range(text) else {
        return text.to_string();
    };
    let before = text[..start].trim_end_matches(['\r', '\n']);
    let after = text[end..].trim_start_matches(['\r', '\n']);
    match (before.is_empty(), after.is_empty()) {
        (true, true) => String::new(),
        (true, false) => format!("{after}\n"),
        (false, true) => format!("{before}\n"),
        (false, false) => format!("{before}\n\n{after}\n"),
    }
}

/// Replace or append the managed block in a patch document.
fn upsert_block(existing: &str, block: &str) -> String {
    upsert_named_block(existing, block, MANAGED_BEGIN, MANAGED_END)
}

fn upsert_named_block(existing: &str, block: &str, begin: &str, end: &str) -> String {
    if let Some((start, end)) = block_range_for(existing, begin, end) {
        return format!("{}{block}{}", &existing[..start], &existing[end..]);
    }
    let trimmed = existing.trim();
    if trimmed.is_empty() || trimmed == "[]" || trimmed == "---\n[]" {
        return format!("{block}\n");
    }
    format!("{}\n\n{block}\n", existing.trim_end_matches(['\r', '\n']))
}

fn pooled_markers(name: &str) -> (String, String) {
    (
        format!("# >>> stateroot pooled MCP {name}"),
        format!("# <<< stateroot pooled MCP {name}"),
    )
}

pub(crate) fn pooled_mcp_owned(path: &Path, name: &str) -> bool {
    let (begin, end) = pooled_markers(name);
    read_optional(path).is_ok_and(|text| block_range_for(&text, &begin, &end).is_some())
}

/// Project one pooled server without reserializing operator entries or the
/// separate lifecycle-plugin block. Ownership is checked by MCP federation.
pub(crate) fn merge_mcp_server(
    path: &Path,
    name: &str,
    config: &serde_json::Value,
) -> Result<bool, HarnessError> {
    let existing = read_optional(path)?;
    let (begin, end) = pooled_markers(name);
    let entries = vec![insert_entry(
        &format!("stateroot-pooled-{name}"),
        "@deepseek-ai/dsh-mcp-client",
        Some(
            serde_yaml::to_value(config)
                .map_err(|error| HarnessError::Invalid(error.to_string()))?,
        ),
    )];
    let body = serde_yaml::to_string(&entries)
        .map_err(|error| HarnessError::Invalid(error.to_string()))?;
    let block = format!("{begin}\n{}{end}", body);
    let next = upsert_named_block(&existing, &block, &begin, &end);
    validate_patch(path, &next)?;
    if next == existing {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    crate::safe_io::atomic_replace(path, next.as_bytes()).map_err(io_err(path))?;
    Ok(true)
}

fn validate_patch(path: &Path, text: &str) -> Result<(), HarnessError> {
    let parsed: serde_yaml::Value = serde_yaml::from_str(text).map_err(|source| {
        HarnessError::Invalid(format!(
            "{} would not be a valid cordis patch after the stateroot edit ({source}); refusing to write",
            path.display()
        ))
    })?;
    if parsed.is_sequence() || parsed.is_null() {
        Ok(())
    } else {
        Err(HarnessError::Invalid(format!(
            "{} is not a cordis patch list (top level is not a sequence); refusing to write",
            path.display()
        )))
    }
}

fn read_optional(path: &Path) -> Result<String, HarnessError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(io_err(path)(error)),
    }
}

/// Install/refresh the plugin file and both patch entries. Returns action lines.
pub fn install(home: &Path, command: &Path) -> Result<Vec<String>, HarnessError> {
    let mut actions = Vec::new();

    let plugin = plugin_path(home);
    let current_plugin = read_optional(&plugin)?;
    let path = patch_path(home);
    let existing = read_optional(&path)?;
    let block = render_block(command)?;
    let next = upsert_block(&existing, &block);
    validate_patch(&path, &next)?;
    if current_plugin == PLUGIN_SOURCE {
        actions.push(format!(
            "hook plugin already up to date ({})",
            plugin.display()
        ));
    } else {
        if let Some(parent) = plugin.parent() {
            std::fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        crate::safe_io::atomic_replace(&plugin, PLUGIN_SOURCE.as_bytes())
            .map_err(io_err(&plugin))?;
        actions.push(format!("hook plugin → {}", plugin.display()));
    }

    if next == existing {
        actions.push(format!(
            "hooks + MCP already configured ({})",
            path.display()
        ));
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        crate::safe_io::atomic_replace(&path, next.as_bytes()).map_err(io_err(&path))?;
        actions.push(format!("hooks + MCP → {}", path.display()));
    }
    Ok(actions)
}

/// Remove the managed plugin and patch block. User entries and comments survive.
pub fn uninstall(home: &Path) -> Result<Vec<String>, HarnessError> {
    let mut actions = Vec::new();

    let path = patch_path(home);
    let existing = read_optional(&path)?;
    if !existing.is_empty() {
        let stripped = strip_managed_block(&existing);
        if stripped != existing {
            let next = if stripped.trim().is_empty() {
                "[]\n".to_string()
            } else {
                stripped
            };
            validate_patch(&path, &next)?;
            crate::safe_io::atomic_replace(&path, next.as_bytes()).map_err(io_err(&path))?;
            actions.push(format!("hooks + MCP removed → {}", path.display()));
        }
    }

    let plugin = plugin_path(home);
    if plugin.is_file() {
        std::fs::remove_file(&plugin).map_err(io_err(&plugin))?;
        actions.push(format!("hook plugin removed → {}", plugin.display()));
    }
    if let Some(dir) = plugin.parent() {
        if dir.is_dir()
            && std::fs::read_dir(dir)
                .map(|mut it| it.next().is_none())
                .unwrap_or(false)
        {
            let _ = std::fs::remove_dir(dir);
        }
    }
    Ok(actions)
}

fn patch_contains(home: &Path, needle: &str) -> bool {
    std::fs::read_to_string(patch_path(home))
        .map(|text| {
            block_range(&text)
                .map(|(start, end)| text[start..end].contains(needle))
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// True when the generated hook plugin is present.
pub fn plugin_installed(home: &Path) -> bool {
    std::fs::read_to_string(plugin_path(home))
        .map(|text| text.contains("export const name = 'stateroot'"))
        .unwrap_or(false)
}

/// True when the hook plugin entry is present in the managed block.
pub fn hooks_configured(home: &Path) -> bool {
    plugin_installed(home) && patch_contains(home, "stateroot-hooks")
}

/// True when the MCP client entry is present in the managed block.
pub fn mcp_configured(home: &Path) -> bool {
    patch_contains(home, "stateroot-mcp")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> tempfile::TempDir {
        tempfile::tempdir().expect("home")
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn install_writes_plugin_and_both_patch_entries() {
        let home = temp_home();
        let command = Path::new("/opt/stateroot/bin/stateroot");
        let actions = install(home.path(), command).expect("install");
        assert!(actions.iter().any(|line| line.contains("hook plugin")));

        let plugin = read(&plugin_path(home.path()));
        assert!(plugin.contains("export const name = 'stateroot'"));
        assert!(plugin.contains("agent/pre-step"));
        assert!(plugin.contains("'hook', event, '--harness', harness"));
        assert!(!plugin.contains("shell: true"));

        let patch = read(&patch_path(home.path()));
        assert!(patch.contains(MANAGED_BEGIN));
        assert!(patch.contains("id: stateroot-hooks"));
        assert!(patch.contains("id: stateroot-mcp"));
        assert!(patch.contains("@deepseek-ai/dsh-mcp-client"));
        assert!(patch.contains("serverName: stateroot"));
        assert!(patch.contains("mcp-stdio"));
        assert!(patch.contains(command.to_string_lossy().as_ref()));

        let parsed: serde_yaml::Value = serde_yaml::from_str(&patch).expect("valid yaml");
        assert!(parsed.is_sequence());
        assert!(hooks_configured(home.path()));
        assert!(mcp_configured(home.path()));
    }

    #[test]
    fn reinstall_is_byte_identical_and_preserves_operator_patch() {
        let home = temp_home();
        let dir = home.path().join(".dsh");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("cordis.patch.yml");
        std::fs::write(
            &path,
            "# operator notes\n- id: custom\n  config:\n    keep: true\n",
        )
        .expect("seed");

        let command = Path::new("/usr/local/bin/stateroot");
        install(home.path(), command).expect("first install");
        let first = read(&path);
        assert!(first.contains("# operator notes"));
        assert!(first.contains("- id: custom"));
        assert!(first.contains("keep: true"));

        let actions = install(home.path(), command).expect("second install");
        assert!(actions.iter().any(|line| line.contains("already")));
        assert_eq!(read(&path), first, "reinstall must not rewrite the patch");
    }

    #[test]
    fn uninstall_removes_only_the_managed_block_and_plugin() {
        let home = temp_home();
        let dir = home.path().join(".dsh");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("cordis.patch.yml");
        std::fs::write(&path, "- id: custom\n  config:\n    keep: true\n").expect("seed");

        install(home.path(), Path::new("/usr/bin/stateroot")).expect("install");
        assert!(plugin_path(home.path()).is_file());

        let actions = uninstall(home.path()).expect("uninstall");
        assert!(actions.iter().any(|line| line.contains("removed")));
        let after = read(&path);
        assert!(!after.contains(MANAGED_BEGIN));
        assert!(!after.contains("stateroot-hooks"));
        assert!(after.contains("- id: custom"));
        assert!(!plugin_path(home.path()).exists());

        // Idempotent second pass.
        let actions = uninstall(home.path()).expect("second uninstall");
        assert!(actions.is_empty());
    }

    #[test]
    fn empty_array_patch_is_replaced_with_a_valid_mixed_document() {
        let home = temp_home();
        let dir = home.path().join(".dsh");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("cordis.patch.yml");
        std::fs::write(&path, "[]\n").expect("seed");

        install(home.path(), Path::new("/usr/bin/stateroot")).expect("install");
        let patch = read(&path);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&patch).expect("valid yaml");
        assert!(parsed.is_sequence());
        assert!(patch.contains("stateroot-hooks"));

        uninstall(home.path()).expect("uninstall");
        let after = read(&path);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&after).expect("valid yaml");
        assert!(parsed.is_sequence() || parsed.is_null());
    }

    #[test]
    fn strip_preserves_text_on_both_sides() {
        let text =
            format!("# before\n- id: a\n{MANAGED_BEGIN}\n- insert: []\n{MANAGED_END}\n- id: b\n");
        let stripped = strip_managed_block(&text);
        assert!(stripped.contains("# before"));
        assert!(stripped.contains("- id: a"));
        assert!(stripped.contains("- id: b"));
        assert!(!stripped.contains(MANAGED_BEGIN));
    }

    #[test]
    fn full_quirk_install_wires_instruction_block_plugin_and_patch() {
        let home = temp_home();
        std::fs::create_dir_all(home.path().join(".dsh")).expect("mkdir");
        let quirk = super::super::registry::quirk("dsh").expect("dsh");
        let actions = super::super::install_quirk_full(home.path(), quirk, "## DSH protocol\n");
        assert!(
            !actions.iter().any(|line| line.starts_with("ERROR:")),
            "install actions: {actions:?}"
        );

        let instructions = read(&home.path().join(".dsh/AGENTS.md"));
        assert!(instructions.contains("stateroot:begin"));
        assert!(instructions.contains("DSH protocol"));

        assert!(plugin_path(home.path()).is_file());
        let patch = read(&patch_path(home.path()));
        assert!(patch.contains("stateroot-hooks"));
        assert!(patch.contains("stateroot-mcp"));
        assert!(hooks_configured(home.path()));
        assert!(mcp_configured(home.path()));
    }

    #[test]
    fn failed_patch_reads_and_validation_preserve_existing_files() {
        for original in [b"# operator\n\xff".as_slice(), b"not-a-patch: true\n"] {
            let home = temp_home();
            let path = patch_path(home.path());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, original).unwrap();
            assert!(install(home.path(), Path::new("stateroot")).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert!(
                !plugin_path(home.path()).exists(),
                "validate before editing"
            );
        }
        let home = temp_home();
        let path = patch_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"\xff").unwrap();
        assert!(uninstall(home.path()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"\xff");
    }
}
