//! Tier A lifecycle-hook installers — per-harness native hook configs that
//! call `stateroot hook <event> --harness <id>`.
//!
//! Config shapes extracted from the ai-memory source (`ai-memory/crates/
//! ai-memory-cli/src/commands/render_shared.rs` + `install_hooks.rs`):
//! - claude-code/grok: `settings.json` / `hooks/*.json`, Nested shape
//! - codex: `hooks.json`, Nested shape
//! - gemini-cli: `settings.json`, Nested shape
//! - cursor: `hooks.json`, Flat shape + `version: 1`
//! - kimi-code/kimi: `[[hooks]]` entries in `config.toml`
//! - devin: `hooks.v1.json` (the whole file IS the hooks object)
//! - zero: `hooks.json` exec-form (JSON on stdin)
//! - antigravity: `config/hooks.json` named groups
//! - hermes: `hooks:` block inside `config.yaml` (shell hooks; verified
//!   against `agent/shell_hooks.py` and the official hook docs)
//! - openclaw: generated native plugin under `~/.openclaw/extensions/stateroot`
//!   (real OpenClaw discovery root; typed hooks via `api.on("snake_case", …)`)
//!
//! Merge semantics mirror the MCP registration: read-merge-write with a
//! `.bak` backup; third-party hooks in the same file are preserved and only
//! stateroot-owned entries are replaced.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::paths;
use super::registry::{HarnessQuirk, HookFormat};
use super::{health, io_err, HarnessError};

fn bak_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.bak",
        path.extension().and_then(|e| e.to_str()).unwrap_or("json")
    ))
}

fn command_for(quirk: &HarnessQuirk, canonical: &str) -> String {
    paths::hook_command(quirk.id, canonical)
}

/// Escape a value for a TOML basic (double-quoted) string. Windows hook
/// commands carry verbatim paths like `\\?\C:\Users\…\stateroot.exe` —
/// written raw into `command = "…"` they are invalid escapes (`\C`, `\U`,
/// `\A`, `\P`, `\s`), and the harness's whole config.toml fails to load.
fn toml_basic_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// One `[[hooks]]` block, TOML-escaped. Kept separate so tests can drive a
/// Windows-shaped command through it (the real command comes from
/// `hook_command`, which is bare `stateroot` under test binaries).
fn toml_hook_block(harness_event: &str, command: &str) -> String {
    format!(
        "[[hooks]]\nevent = \"{harness_event}\"\ncommand = \"{}\"\n",
        toml_basic_escape(command)
    )
}

fn nested_entries(quirk: &HarnessQuirk) -> Map<String, Value> {
    let mut out = Map::new();
    for (harness_event, canonical) in quirk.event_map {
        out.insert(
            (*harness_event).to_string(),
            json!([{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": command_for(quirk, canonical),
                }],
            }]),
        );
    }
    out
}

fn cursor_hook_timeout(harness_event: &str) -> u32 {
    match harness_event {
        // Window close waits on sessionEnd. Keep this short so a slow
        // filesystem cannot stall Cursor's shutdown dialog for 30s.
        "sessionEnd" => 8,
        // session_start prints the digest first, then federation syncs.
        "sessionStart" => 30,
        _ => 15,
    }
}

fn flat_entries(quirk: &HarnessQuirk) -> Map<String, Value> {
    let mut out = Map::new();
    for (harness_event, canonical) in quirk.event_map {
        out.insert(
            (*harness_event).to_string(),
            json!([{
                "type": "command",
                "command": command_for(quirk, canonical),
                "matcher": "",
                "timeout": cursor_hook_timeout(harness_event),
            }]),
        );
    }
    out
}

/// Copilot timeout policy (PascalCase event keys). Same lessons as Cursor:
/// SessionStart carries the digest (30s); SessionEnd must not stall a window
/// close (8s).
fn copilot_hook_timeout(harness_event: &str) -> u32 {
    match harness_event {
        "SessionEnd" => 8,
        "SessionStart" => 30,
        _ => 15,
    }
}

/// Copilot entries: the documented shape — `type`/`command`/`timeout` only,
/// no `matcher` (the CLI honors it only on filterable events, and an empty
/// pattern there could silently drop the entry).
fn copilot_entries(quirk: &HarnessQuirk) -> Map<String, Value> {
    let mut out = Map::new();
    for (harness_event, canonical) in quirk.event_map {
        out.insert(
            (*harness_event).to_string(),
            json!([{
                "type": "command",
                "command": command_for(quirk, canonical),
                "timeout": copilot_hook_timeout(harness_event),
            }]),
        );
    }
    out
}

/// True when a hook entry (nested or flat) invokes stateroot.
fn is_stateroot_entry(entry: &Value) -> bool {
    let commands: Vec<&str> = entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|h| h.get("command").and_then(|c| c.as_str()))
                .collect()
        })
        .unwrap_or_else(|| {
            entry
                .get("command")
                .and_then(|c| c.as_str())
                .into_iter()
                .collect()
        });
    commands
        .iter()
        .any(|command| command.contains("stateroot hook") || command.contains("stateroot.exe hook"))
}

/// Remove stateroot-owned handlers from inside a nested matcher entry's
/// `hooks` array. Ownership is per handler: a foreign command sharing the
/// matcher survives. Flat entries have no inner array and stay untouched.
fn strip_owned_handlers(entry: &mut Value) {
    if let Some(handlers) = entry.get_mut("hooks").and_then(Value::as_array_mut) {
        handlers.retain(|handler| !is_stateroot_entry(handler));
    }
}

/// True when an event entry should drop after [`strip_owned_handlers`]: a
/// flat stateroot handler, or a nested matcher left with no handlers (purely
/// ours, or emptied by the strip). A matcher still holding a foreign handler
/// is kept — never a substring test against the whole serialized entry.
fn entry_fully_ours_or_empty(entry: &Value) -> bool {
    if let Some(handlers) = entry.get("hooks").and_then(Value::as_array) {
        return handlers.is_empty() || is_stateroot_entry(entry);
    }
    is_stateroot_entry(entry)
}

/// Overlay our entries into an event-keyed hooks object: replace prior
/// stateroot-owned content under each event we own, append ours, keep
/// everything else (including foreign hooks under the same event, and foreign
/// handlers sharing one of our nested tool matchers).
fn overlay(hooks_obj: &mut Map<String, Value>, ours: &Map<String, Value>) {
    for (event, value) in ours {
        let entry = hooks_obj.entry(event.clone()).or_insert_with(|| json!([]));
        if let Some(arr) = entry.as_array_mut() {
            for existing in arr.iter_mut() {
                strip_owned_handlers(existing);
            }
            arr.retain(|existing| !entry_fully_ours_or_empty(existing));
            if let Some(ours_arr) = value.as_array() {
                arr.extend(ours_arr.iter().cloned());
            }
        } else {
            *entry = value.clone();
        }
    }
}

fn read_json_file(path: &Path) -> Result<Value, HarnessError> {
    if !path.is_file() {
        return Ok(json!({}));
    }
    let text = std::fs::read_to_string(path).map_err(io_err(path))?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text).map_err(|source| HarnessError::JsonParse {
        path: path.to_path_buf(),
        source,
    })
}

fn backup_once(path: &Path) -> Result<(), HarnessError> {
    if path.is_file() {
        let bak = bak_path(path);
        if !bak.exists() {
            std::fs::copy(path, &bak).map_err(io_err(&bak))?;
        }
    }
    Ok(())
}

/// Write `text` to `path` atomically: tempfile in the same directory, fsync,
/// rename. A crash mid-write leaves the old file intact — a harness config
/// is the user's file, not ours. (Windows rename never replaces: remove the
/// destination first; `backup_once` already holds the previous content.)
fn atomic_write(path: &Path, text: &str) -> Result<(), HarnessError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    let tmp = path.with_extension(format!("stateroot-tmp-{}", std::process::id()));
    {
        use std::io::Write as _;
        let mut file = std::fs::File::create(&tmp).map_err(io_err(&tmp))?;
        file.write_all(text.as_bytes()).map_err(io_err(&tmp))?;
        file.sync_all().map_err(io_err(&tmp))?;
    }
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    std::fs::rename(&tmp, path).map_err(io_err(path))
}

fn write_json_if_changed(path: &Path, doc: &Value) -> Result<bool, HarnessError> {
    let text = format!("{}\n", serde_json::to_string_pretty(doc)?);
    let current = std::fs::read_to_string(path).unwrap_or_default();
    if current == text {
        return Ok(false);
    }
    atomic_write(path, &text).map(|_| true)
}

/// Install one Tier A harness's native hook config. Returns action lines.
pub fn install_hooks(home: &Path, quirk: &HarnessQuirk) -> Result<Vec<String>, HarnessError> {
    let Some(target) = quirk.hooks else {
        return Ok(Vec::new());
    };
    let path = paths::hook_target_path(home, quirk)
        .ok_or_else(|| HarnessError::Invalid(format!("{}: no hook target path", quirk.id)))?;
    match target.format {
        HookFormat::NestedJson => {
            // `settings.json`-style files wrap entries in a top-level `hooks`
            // object; devin's hooks.v1.json IS the object.
            let wrap = path.file_name() != Some("hooks.v1.json".as_ref());
            let mut doc = read_json_file(&path)?;
            if !doc.is_object() {
                doc = json!({});
            }
            let ours = nested_entries(quirk);
            backup_once(&path)?;
            if wrap {
                let root = doc.as_object_mut().ok_or_else(|| {
                    HarnessError::Invalid(format!("{}: not a JSON object", path.display()))
                })?;
                let hooks = root.entry("hooks".to_string()).or_insert_with(|| json!({}));
                let hooks = hooks.as_object_mut().ok_or_else(|| {
                    HarnessError::Invalid(format!("{}: `hooks` is not an object", path.display()))
                })?;
                overlay(hooks, &ours);
            } else {
                let root = doc.as_object_mut().ok_or_else(|| {
                    HarnessError::Invalid(format!("{}: not a JSON object", path.display()))
                })?;
                overlay(root, &ours);
            }
            let changed = write_json_if_changed(&path, &doc)?;
            Ok(vec![format!(
                "hooks {} → {}",
                if changed {
                    "installed"
                } else {
                    "already up to date"
                },
                path.display()
            )])
        }
        HookFormat::FlatJson => install_flat_style(&path, &flat_entries(quirk)),
        // Same `version` + `hooks` envelope as FlatJson, Copilot entry shape.
        HookFormat::CopilotJson => install_flat_style(&path, &copilot_entries(quirk)),
        HookFormat::TomlHooks => install_toml_hooks(&path, quirk),
        HookFormat::ZeroExecJson => install_zero_hooks(&path, quirk),
        HookFormat::NamedGroupsJson => install_named_groups(&path, quirk),
        HookFormat::NativePlugin => install_native_plugin(&path, quirk),
        HookFormat::YamlHooks => install_yaml_hooks(&path, quirk),
    }
}

/// True when a hook command string invokes stateroot (any quoting/platform
/// form the YAML shell-hook block may carry). Delegates to the tested health
/// matcher: bare `stateroot`, quoted or absolute paths with spaces, either
/// separator — while a command that merely MENTIONS stateroot stays foreign.
fn is_stateroot_command_str(command: &str) -> bool {
    health::is_stateroot_hook_command(command)
}

/// Quote the binary leg of a `<binary> hook …` command for `shlex.split`
/// (hermes spawns hooks with `shell=False`): a path containing whitespace
/// survives as one argv element only inside double quotes, and a bare
/// Windows backslash path must be quoted too — shlex eats the separators
/// (`C:\Users\…` parses as `C:Users…`). Inside shlex double quotes `\`
/// escapes only `"` and `\`, so backslashes are doubled (`\\?\C:\…` verbatim
/// paths round-trip) and quotes escaped. Bare `stateroot` and clean absolute
/// paths pass through unchanged.
fn yaml_shell_command(command: &str) -> String {
    let Some((binary, rest)) = command.rsplit_once(" hook ") else {
        return command.to_string();
    };
    if binary
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\\')
    {
        let escaped = binary.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\" hook {rest}")
    } else {
        command.to_string()
    }
}

/// Read a YAML config that may legitimately not exist yet. NotFound reads as
/// empty (fresh install); any other read failure is propagated — a config
/// whose bytes cannot be read must never be treated as empty and rewritten,
/// nor falsely reported unchanged.
fn read_yaml_config(path: &Path) -> Result<String, HarnessError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(io_err(path)(err)),
    }
}

/// Hermes YAML shell hooks: one entry list per hermes event under the
/// `hooks:` mapping of `config.yaml`, `{command: "…"}` entries. Foreign
/// events, foreign entries under our events, and reserved sub-sections
/// (`outbound`, `output_spill` — parsed elsewhere by hermes) are preserved
/// verbatim; only stateroot-owned entries under our events are replaced.
/// serde_yaml re-serializes (comments normalized) — the same accepted
/// tradeoff as the YAML MCP writer; the pristine original stays at `.bak`.
fn install_yaml_hooks(path: &Path, quirk: &HarnessQuirk) -> Result<Vec<String>, HarnessError> {
    use serde_yaml::{Mapping, Value as Yaml};
    let text = read_yaml_config(path)?;
    let mut doc: Yaml = if text.trim().is_empty() {
        Yaml::Mapping(Mapping::new())
    } else {
        serde_yaml::from_str(&text).map_err(|source| {
            HarnessError::Invalid(format!(
                "{} is not valid YAML — refusing to modify ({source})",
                path.display()
            ))
        })?
    };
    let root = doc.as_mapping_mut().ok_or_else(|| {
        HarnessError::Invalid(format!(
            "{}: top-level YAML is not a mapping",
            path.display()
        ))
    })?;
    let hooks = root
        .entry(Yaml::String("hooks".to_string()))
        .or_insert_with(|| Yaml::Mapping(Mapping::new()));
    let hooks = hooks.as_mapping_mut().ok_or_else(|| {
        HarnessError::Invalid(format!("{}: `hooks` is not a mapping", path.display()))
    })?;
    for (harness_event, canonical) in quirk.event_map {
        let key = Yaml::String((*harness_event).to_string());
        let entry = hooks
            .entry(key)
            .or_insert_with(|| Yaml::Sequence(Vec::new()));
        let list = entry.as_sequence_mut().ok_or_else(|| {
            HarnessError::Invalid(format!(
                "{}: `hooks.{harness_event}` is not a list",
                path.display()
            ))
        })?;
        list.retain(|item| {
            item.get("command")
                .and_then(|c| c.as_str())
                .map(|command| !is_stateroot_command_str(command))
                .unwrap_or(true)
        });
        let mut ours = Mapping::new();
        ours.insert(
            Yaml::String("command".to_string()),
            Yaml::String(yaml_shell_command(&command_for(quirk, canonical))),
        );
        list.push(Yaml::Mapping(ours));
    }
    let out = serde_yaml::to_string(&doc)
        .map_err(|e| HarnessError::Invalid(format!("YAML serialization failed: {e}")))?;
    if out == text {
        return Ok(vec![format!(
            "hooks already up to date ({})",
            path.display()
        )]);
    }
    backup_once(path)?;
    atomic_write(path, &out)?;
    Ok(vec![
        format!("hooks installed → {}", path.display()),
        // Honest consent surface: hermes itself gates every shell hook behind
        // a first-use prompt per (event, command) pair; unapproved hooks are
        // skipped. The bypasses are the operator's choice — stateroot never
        // sets `hooks_auto_accept` on their behalf.
        "note: hermes asks for one-time consent per shell hook on first use (TTY prompt; `--accept-hooks`, HERMES_ACCEPT_HOOKS=1, or `hooks_auto_accept: true` are the operator's bypasses — stateroot does not set them)".to_string(),
    ])
}

/// Remove stateroot-owned entries from the YAML `hooks:` block. Foreign
/// entries survive verbatim; event lists emptied by the strip are dropped,
/// and a `hooks:` mapping left empty is removed (reserved sub-sections like
/// `outbound` keep it alive). Returns whether the file changed.
fn remove_yaml_hooks_from(path: &Path) -> Result<bool, HarnessError> {
    use serde_yaml::{Mapping, Value as Yaml};
    let text = read_yaml_config(path)?;
    if text.trim().is_empty() {
        return Ok(false);
    }
    let mut doc: Yaml = serde_yaml::from_str(&text).map_err(|source| {
        HarnessError::Invalid(format!(
            "{} is not valid YAML — refusing to modify ({source})",
            path.display()
        ))
    })?;
    let mut changed = false;
    if let Some(hooks) = doc
        .as_mapping_mut()
        .and_then(|root| root.get_mut(Yaml::String("hooks".to_string())))
        .and_then(|hooks| hooks.as_mapping_mut())
    {
        let mut empty_keys = Vec::new();
        for (event, entries) in hooks.iter_mut() {
            let Some(list) = entries.as_sequence_mut() else {
                continue;
            };
            let before = list.len();
            list.retain(|item| {
                item.get("command")
                    .and_then(|c| c.as_str())
                    .map(|command| !is_stateroot_command_str(command))
                    .unwrap_or(true)
            });
            if list.len() != before {
                changed = true;
            }
            if list.is_empty() {
                empty_keys.push(event.clone());
            }
        }
        for key in empty_keys {
            hooks.remove(&key);
        }
    }
    // A `hooks:` mapping left empty by the strip is dropped entirely.
    if let Some(root) = doc.as_mapping_mut() {
        let empty = root
            .get(Yaml::String("hooks".to_string()))
            .and_then(|hooks| hooks.as_mapping())
            .map(Mapping::is_empty)
            .unwrap_or(false);
        if empty {
            root.remove(Yaml::String("hooks".to_string()));
            changed = true;
        }
    }
    if !changed {
        return Ok(false);
    }
    let out = serde_yaml::to_string(&doc)
        .map_err(|e| HarnessError::Invalid(format!("YAML serialization failed: {e}")))?;
    backup_once(path)?;
    atomic_write(path, &out)?;
    Ok(true)
}

/// Shared writer for the `{"version": 1, "hooks": {…}}` envelope (cursor
/// FlatJson, vscode-copilot CopilotJson) — read-merge-write with a `.bak`,
/// foreign entries under the same events preserved.
fn install_flat_style(path: &Path, ours: &Map<String, Value>) -> Result<Vec<String>, HarnessError> {
    let mut doc = read_json_file(path)?;
    if !doc.is_object() {
        doc = json!({});
    }
    backup_once(path)?;
    let root = doc
        .as_object_mut()
        .ok_or_else(|| HarnessError::Invalid(format!("{}: not a JSON object", path.display())))?;
    root.entry("version".to_string()).or_insert(json!(1));
    let hooks = root.entry("hooks".to_string()).or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or_else(|| {
        HarnessError::Invalid(format!("{}: `hooks` is not an object", path.display()))
    })?;
    overlay(hooks, ours);
    let changed = write_json_if_changed(path, &doc)?;
    Ok(vec![format!(
        "hooks {} → {}",
        if changed {
            "installed"
        } else {
            "already up to date"
        },
        path.display()
    )])
}

/// `[[hooks]]` TOML entries: remove prior stateroot-marked blocks, append ours.
fn install_toml_hooks(path: &Path, quirk: &HarnessQuirk) -> Result<Vec<String>, HarnessError> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    // Normalize what the strip leaves behind (stray blank lines where our
    // blocks and markers were) so reinstalls converge instead of accruing
    // one newline per run.
    let cleaned = strip_stateroot_toml_hooks(&existing);
    let mut block = String::from(
        "\n# stateroot hooks (managed by `stateroot install` — do not edit by hand)\n",
    );
    for (harness_event, canonical) in quirk.event_map {
        block.push_str(&toml_hook_block(
            harness_event,
            &command_for(quirk, canonical),
        ));
    }
    let mut updated = cleaned.trim_end().to_string();
    if !updated.is_empty() {
        updated.push('\n');
    }
    updated.push_str(&block);

    let mut actions = Vec::new();
    // A broken config breaks the harness's whole session on some harnesses
    // (kimi fails to load config.toml). Our write is textual — appending is
    // safe even into a broken file — but the user must know it's broken.
    if !existing.trim().is_empty() && toml::from_str::<toml::Value>(&existing).is_err() {
        actions.push(format!(
            "warning: {} is not valid TOML — hooks appended textually; fix the syntax error for the harness to load it",
            path.display()
        ));
    }
    if updated == existing {
        actions.push(format!("hooks already up to date ({})", path.display()));
        return Ok(actions);
    }
    backup_once(path)?;
    atomic_write(path, &updated)?;
    actions.push(format!("hooks installed → {}", path.display()));
    Ok(actions)
}

/// Remove `[[hooks]]` blocks whose body mentions `stateroot hook` (idempotent
/// reinstall; foreign hook blocks survive verbatim).
fn strip_stateroot_toml_hooks(text: &str) -> String {
    let mut out = String::new();
    let mut block: Vec<&str> = Vec::new();
    let mut in_hooks_block = false;

    let flush = |block: &[&str], out: &mut String, in_hooks: bool| {
        if in_hooks {
            let body = block.join("\n");
            // Our commands are always quoted, bare or absolute, any platform:
            //   "stateroot hook …"  "/abs/path/stateroot hook …"
            //   "C:\…\stateroot.exe hook …"
            // (The old matcher looked for a bare `command = "stateroot hook`
            // prefix and never matched the absolute-path forms, so every
            // reinstall appended another full set of blocks.)
            let is_ours = body.contains("\"stateroot hook ")
                || body.contains("/stateroot hook ")
                || body.contains("\\stateroot hook ")
                || body.contains("\"stateroot.exe hook ")
                || body.contains("/stateroot.exe hook ")
                || body.contains("\\stateroot.exe hook ");
            if is_ours {
                return; // drop the stateroot-owned block
            }
        }
        for line in block {
            out.push_str(line);
            out.push('\n');
        }
    };

    for line in text.lines() {
        // Drop our orphaned managed-block marker comments wherever they sit.
        if line.trim_start().starts_with("# stateroot hooks") {
            continue;
        }
        let is_array_header = line.trim_start().starts_with("[[");
        if is_array_header {
            flush(&block, &mut out, in_hooks_block);
            block.clear();
            in_hooks_block = line.trim() == "[[hooks]]";
        }
        block.push(line);
    }
    flush(&block, &mut out, in_hooks_block);
    out
}

fn install_zero_hooks(path: &Path, quirk: &HarnessQuirk) -> Result<Vec<String>, HarnessError> {
    let mut doc = read_json_file(path)?;
    if !doc.is_object() {
        doc = json!({});
    }
    let ours: Vec<Value> = quirk
        .event_map
        .iter()
        .map(|(zero_event, canonical)| {
            json!({
                "id": format!("stateroot-{canonical}"),
                "name": format!("stateroot {canonical}"),
                "event": zero_event,
                "command": "stateroot",
                "args": ["hook", canonical, "--harness", quirk.id],
                "enabled": true,
            })
        })
        .collect();
    backup_once(path)?;
    let root = doc
        .as_object_mut()
        .ok_or_else(|| HarnessError::Invalid(format!("{}: not a JSON object", path.display())))?;
    let hooks = root.entry("hooks".to_string()).or_insert_with(|| json!([]));
    let arr = hooks.as_array_mut().ok_or_else(|| {
        HarnessError::Invalid(format!("{}: `hooks` is not an array", path.display()))
    })?;
    arr.retain(|entry| {
        entry
            .get("id")
            .and_then(|v| v.as_str())
            .map(|id| !id.starts_with("stateroot-"))
            .unwrap_or(true)
    });
    arr.extend(ours);
    root.entry("enabled".to_string()).or_insert(json!(true));
    let changed = write_json_if_changed(path, &doc)?;
    Ok(vec![format!(
        "hooks {} → {}",
        if changed {
            "installed"
        } else {
            "already up to date"
        },
        path.display()
    )])
}

fn install_named_groups(path: &Path, quirk: &HarnessQuirk) -> Result<Vec<String>, HarnessError> {
    let mut ours = Map::new();
    for (harness_event, canonical) in quirk.event_map {
        let handler = json!({"type": "command", "command": command_for(quirk, canonical)});
        if matches!(*canonical, "pre_tool_use" | "post_tool_use") {
            // Tool events: nested shape (matcher + hooks array) — the native
            // contract keys matchers off the tool name for Pre/PostToolUse.
            ours.insert(
                (*harness_event).to_string(),
                json!([{ "matcher": "", "hooks": [handler] }]),
            );
        } else {
            // Lifecycle events (SessionStart/PreInvocation/Stop): flat
            // handler list — the native contract ignores matchers there.
            ours.insert((*harness_event).to_string(), json!([handler]));
        }
    }
    let mut doc = read_json_file(path)?;
    if !doc.is_object() {
        doc = json!({});
    }
    backup_once(path)?;
    let root = doc
        .as_object_mut()
        .ok_or_else(|| HarnessError::Invalid(format!("{}: not a JSON object", path.display())))?;
    // Merge into the existing group: other named groups and group-level
    // foreign settings (e.g. a user-toggled `"enabled": false`) survive an
    // install/update verbatim; only stateroot-owned entries under the events
    // we own are replaced.
    let mut group = root
        .get("stateroot")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    overlay(&mut group, &ours);
    root.insert("stateroot".to_string(), Value::Object(group));
    let changed = write_json_if_changed(path, &doc)?;
    Ok(vec![format!(
        "hooks {} → {}",
        if changed {
            "installed"
        } else {
            "already up to date"
        },
        path.display()
    )])
}

/// Remove stateroot-managed hook entries for one harness (full uninstall).
/// Foreign entries survive verbatim in every format. Idempotent; a missing
/// file is a no-op. Sweeps relocated and legacy default hook paths.
pub fn remove_hooks(home: &Path, quirk: &HarnessQuirk) -> Result<Vec<String>, HarnessError> {
    let Some(target) = quirk.hooks else {
        return Ok(Vec::new());
    };
    if target.format == HookFormat::NativePlugin {
        let path = home.join(target.path);
        if path.is_dir() {
            std::fs::remove_dir_all(&path).map_err(io_err(&path))?;
            return Ok(vec![format!("plugin removed → {}", path.display())]);
        }
        return Ok(Vec::new());
    }
    let mut actions = Vec::new();
    for path in paths::hook_target_candidates(home, quirk) {
        if !path.is_file() {
            continue;
        }
        let changed = match target.format {
            HookFormat::TomlHooks => {
                let existing = std::fs::read_to_string(&path).unwrap_or_default();
                let stripped = strip_stateroot_toml_hooks(&existing);
                if stripped != existing {
                    backup_once(&path)?;
                    atomic_write(&path, &stripped)?;
                    true
                } else {
                    false
                }
            }
            HookFormat::ZeroExecJson => {
                let mut doc = read_json_file(&path)?;
                let mut changed = false;
                if let Some(arr) = doc.get_mut("hooks").and_then(Value::as_array_mut) {
                    let before = arr.len();
                    arr.retain(|entry| {
                        entry
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(|id| !id.starts_with("stateroot-"))
                            .unwrap_or(true)
                    });
                    changed = arr.len() != before;
                }
                if changed {
                    backup_once(&path)?;
                    let _ = write_json_if_changed(&path, &doc)?;
                }
                changed
            }
            HookFormat::YamlHooks => remove_yaml_hooks_from(&path)?,
            HookFormat::NamedGroupsJson => {
                let mut doc = read_json_file(&path)?;
                let mut changed = false;
                // Strip stateroot-owned handlers inside our named group only;
                // foreign groups, foreign events under our group, and
                // group-level settings survive an uninstall verbatim.
                // Ownership is per handler: a nested tool matcher loses only
                // our handlers and survives while a foreign command shares
                // it. The group key itself is dropped once no handler arrays
                // remain (a bare `enabled` toggle with zero hooks is our
                // debris).
                let remove_group =
                    if let Some(group) = doc.get_mut("stateroot").and_then(Value::as_object_mut) {
                        let before = group.clone();
                        let mut empty_events = Vec::new();
                        for (event, value) in group.iter_mut() {
                            if let Some(arr) = value.as_array_mut() {
                                for entry in arr.iter_mut() {
                                    strip_owned_handlers(entry);
                                }
                                arr.retain(|entry| !entry_fully_ours_or_empty(entry));
                                if arr.is_empty() {
                                    empty_events.push(event.clone());
                                }
                            }
                        }
                        for event in empty_events {
                            group.remove(&event);
                        }
                        if *group != before {
                            changed = true;
                        }
                        !group
                            .values()
                            .any(|value| value.as_array().is_some_and(|arr| !arr.is_empty()))
                    } else {
                        false
                    };
                if remove_group {
                    if let Some(root) = doc.as_object_mut() {
                        root.remove("stateroot");
                        changed = true;
                    }
                }
                if changed {
                    backup_once(&path)?;
                    let _ = write_json_if_changed(&path, &doc)?;
                }
                changed
            }
            // NestedJson + FlatJson share the "hooks object of arrays" shape.
            _ => {
                let mut doc = read_json_file(&path)?;
                let wrap = path.file_name() != Some("hooks.v1.json".as_ref());
                let mut changed = false;
                let target_obj = if matches!(target.format, HookFormat::FlatJson) || wrap {
                    doc.get_mut("hooks")
                } else {
                    Some(&mut doc)
                };
                if let Some(Value::Object(map)) = target_obj {
                    let before = map.clone();
                    let mut empty_keys = Vec::new();
                    for (key, value) in map.iter_mut() {
                        if let Some(arr) = value.as_array_mut() {
                            for entry in arr.iter_mut() {
                                strip_owned_handlers(entry);
                            }
                            arr.retain(|entry| !entry_fully_ours_or_empty(entry));
                            if arr.is_empty() {
                                empty_keys.push(key.clone());
                            }
                        }
                    }
                    for key in empty_keys {
                        map.remove(&key);
                    }
                    if *map != before {
                        changed = true;
                    }
                }
                if changed {
                    backup_once(&path)?;
                    let _ = write_json_if_changed(&path, &doc)?;
                }
                changed
            }
        };
        if changed {
            actions.push(format!("hooks removed → {}", path.display()));
        }
    }
    Ok(actions)
}

fn install_native_plugin(dir: &Path, _quirk: &HarnessQuirk) -> Result<Vec<String>, HarnessError> {
    // Real OpenClaw plugin contract (verified against openclaw/src/plugins/
    // and docs/plugins/manifest.md):
    // - Discovery root: ~/.openclaw/extensions/<id>/ (NOT plugins/)
    // - openclaw.plugin.json: required `id` + `configSchema` (no `entry` field)
    // - package.json: `openclaw.extensions: ["./index.ts"]`
    // - Module must export `register(api)` (or default with register)
    // - Hooks: api.on("snake_case_name", handler) — camelCase invented
    //   names (sessionStart, postToolUse, …) are NOT valid
    // - Context injection: only before_prompt_build / before_agent_start
    //   (session_start is void). Identity rides user_prompt_submit stdout.
    let package_json = json!({
        "name": "@stateroot/openclaw-plugin",
        "version": "0.1.0",
        "private": true,
        "type": "module",
        "openclaw": {
            "extensions": ["./index.ts"]
        }
    });
    let manifest_json = json!({
        "id": "stateroot",
        "name": "StateRoot",
        "description": "StateRoot lifecycle hooks (resume, capture, checkpoint)",
        "version": "0.1.0",
        "configSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {}
        }
    });
    // Plain register export — avoids depending on openclaw/plugin-sdk at
    // load time (gateway resolves the module; side-effect execFile hooks
    // need no SDK types). Managed marker kept for idempotent reinstall.
    let index_ts = r#"// Auto-generated by `stateroot install` — edit by re-running, not by hand.
// stateroot managed — OpenClaw typed lifecycle hooks (api.on).
// Discovery: ~/.openclaw/extensions/stateroot (NOT plugins/).
// Context injection: before_prompt_build pulls `stateroot hook user_prompt_submit`
// stdout (digest) into prependContext when the API accepts it. session_start is
// fire-and-forget (federation/sync only) — its stdout is discarded.
import { execFile } from "node:child_process";

function fire(event: string): void {
  void fireAsync(event);
}

function fireAsync(event: string, payload: unknown = {}): Promise<string> {
  return new Promise((resolve) => {
    try {
      const child = execFile(
        "stateroot",
        ["hook", event, "--harness", "openclaw"],
        { timeout: 8000, maxBuffer: 8 * 1024 * 1024, encoding: "utf8" },
        (_err, stdout) => {
          resolve(typeof stdout === "string" ? stdout : String(stdout ?? ""));
        },
      );
      child.stdin?.write(JSON.stringify(payload ?? {}));
      child.stdin?.end();
    } catch {
      resolve("");
    }
  });
}

const plugin = {
  id: "stateroot",
  name: "StateRoot",
  description: "StateRoot lifecycle hooks (resume, capture, checkpoint)",
  register(api: {
    on: (hookName: string, handler: (...args: unknown[]) => unknown) => void;
  }) {
    api.on("session_start", () => {
      fire("session_start");
    });
    api.on("session_end", () => {
      fire("session_end");
    });
    api.on("after_tool_call", (event: unknown) => {
      const err =
        event && typeof event === "object" && "error" in event
          ? (event as { error?: unknown }).error
          : undefined;
      fire(err ? "tool_failure" : "post_tool_use");
    });
    api.on("before_compaction", () => {
      fire("pre_compact");
    });
    api.on("agent_end", () => {
      fire("stop");
    });
    // Pull digest into the prompt when OpenClaw accepts prependContext.
    api.on("before_prompt_build", async (event?: unknown) => {
      const stdout = await fireAsync("user_prompt_submit", event ?? {});
      const digest = (stdout || "").trim();
      if (!digest) {
        return;
      }
      return { prependContext: digest };
    });
  },
};

export default plugin;
"#;
    let files = [
        (
            "package.json",
            format!("{}\n", serde_json::to_string_pretty(&package_json)?),
        ),
        (
            "openclaw.plugin.json",
            format!("{}\n", serde_json::to_string_pretty(&manifest_json)?),
        ),
        ("index.ts", index_ts.to_string()),
    ];
    let mut actions = Vec::new();
    for (name, content) in &files {
        let path = dir.join(name);
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if &current == content {
            continue;
        }
        std::fs::create_dir_all(dir).map_err(io_err(dir))?;
        atomic_write(&path, content)?;
        actions.push(format!("plugin file → {}", path.display()));
    }
    if actions.is_empty() {
        actions.push(format!("plugin already up to date ({})", dir.display()));
    }

    // Enable in ~/.openclaw/openclaw.json → plugins.entries.stateroot.
    // Global extensions are enabled-by-default unless deny/allowlist, but
    // explicit enable matches `openclaw plugins install` and survives
    // allowlists. Parent of extensions/stateroot is ~/.openclaw.
    if let Some(openclaw_home) = dir.parent().and_then(|p| p.parent()) {
        match enable_openclaw_plugin_entry(openclaw_home) {
            Ok(Some(msg)) => actions.push(msg),
            Ok(None) => {}
            Err(err) => actions.push(format!(
                "note: could not enable plugins.entries.stateroot in openclaw.json ({err}) — enable manually or run `openclaw plugins install --link {}`",
                dir.display()
            )),
        }
    }

    // Migrate note if legacy debris exists at plugins/stateroot.
    if let Some(openclaw_home) = dir.parent().and_then(|p| p.parent()) {
        let legacy = openclaw_home.join("plugins/stateroot");
        if legacy.is_dir() && legacy != dir {
            actions.push(format!(
                "note: legacy debris at {} is invisible to OpenClaw — safe to delete after verifying the extensions/ install",
                legacy.display()
            ));
        }
    }

    actions.push(
        "note: restart the OpenClaw gateway after install (`openclaw gateway restart`)".to_string(),
    );
    Ok(actions)
}

/// Merge `plugins.entries.stateroot.enabled = true` into `openclaw.json`.
/// Preserves foreign entries; writes `.bak` on first modification.
fn enable_openclaw_plugin_entry(openclaw_home: &Path) -> Result<Option<String>, HarnessError> {
    let path = openclaw_home.join("openclaw.json");
    if !path.is_file() {
        return Ok(Some(format!(
            "note: {} missing — plugin files written; run openclaw onboard / enable plugins.entries.stateroot after config exists",
            path.display()
        )));
    }
    let raw = std::fs::read_to_string(&path).map_err(io_err(&path))?;
    let mut root: Value = serde_json::from_str(&raw)
        .map_err(|e| HarnessError::Invalid(format!("{}: {e}", path.display())))?;
    let obj = root.as_object_mut().ok_or_else(|| {
        HarnessError::Invalid(format!("{}: root must be an object", path.display()))
    })?;
    let plugins = obj.entry("plugins").or_insert_with(|| json!({}));
    let plugins_obj = plugins.as_object_mut().ok_or_else(|| {
        HarnessError::Invalid(format!("{}: plugins must be an object", path.display()))
    })?;
    let entries = plugins_obj.entry("entries").or_insert_with(|| json!({}));
    let entries_obj = entries.as_object_mut().ok_or_else(|| {
        HarnessError::Invalid(format!(
            "{}: plugins.entries must be an object",
            path.display()
        ))
    })?;
    let already = entries_obj
        .get("stateroot")
        .and_then(|v| v.get("enabled"))
        .and_then(|v| v.as_bool())
        == Some(true);
    if already {
        return Ok(None);
    }
    entries_obj.insert("stateroot".into(), json!({ "enabled": true }));

    let bak = bak_path(&path);
    if !bak.exists() {
        std::fs::copy(&path, &bak).map_err(io_err(&bak))?;
    }
    let pretty = format!("{}\n", serde_json::to_string_pretty(&root)?);
    atomic_write(&path, &pretty)?;
    Ok(Some(format!(
        "enabled plugins.entries.stateroot in {}",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness_install::registry::quirk;

    #[test]
    fn cursor_session_end_timeout_is_shorter_than_session_start() {
        assert_eq!(cursor_hook_timeout("sessionEnd"), 8);
        assert_eq!(cursor_hook_timeout("sessionStart"), 30);
        let q = quirk("cursor").expect("cursor");
        let entries = flat_entries(q);
        assert_eq!(entries["sessionEnd"][0]["timeout"], 8);
        assert_eq!(entries["sessionStart"][0]["timeout"], 30);
    }

    #[test]
    fn flat_overlay_replaces_windows_exe_entries_without_duplicates() {
        let command = r"\\?\C:\Users\alice\AppData\Local\Programs\StateRoot\stateroot.exe hook session_end --harness cursor";
        let mut hooks = Map::from_iter([(
            "sessionEnd".to_string(),
            json!([
                {"type": "command", "command": command, "matcher": "", "timeout": 30},
                {"type": "command", "command": command, "matcher": "", "timeout": 30},
                {"type": "command", "command": "foreign-hook", "matcher": ""}
            ]),
        )]);
        let ours = Map::from_iter([(
            "sessionEnd".to_string(),
            json!([{
                "type": "command",
                "command": command,
                "matcher": "",
                "timeout": 30
            }]),
        )]);

        overlay(&mut hooks, &ours);

        let entries = hooks["sessionEnd"].as_array().expect("entries");
        assert_eq!(entries.len(), 2, "one foreign and one StateRoot entry");
        assert_eq!(
            entries
                .iter()
                .filter(|entry| is_stateroot_entry(entry))
                .count(),
            1
        );
    }

    #[test]
    fn openclaw_plugin_injects_on_prompt_build_not_session_start() {
        let home = tempfile::tempdir().expect("home");
        let q = quirk("openclaw").expect("openclaw");
        install_hooks(home.path(), q).expect("install");
        let src =
            std::fs::read_to_string(home.path().join(".openclaw/extensions/stateroot/index.ts"))
                .expect("index.ts");
        assert!(src.contains("await fireAsync(\"user_prompt_submit\""));
        assert!(src.contains("prependContext"));
        assert!(src.contains("before_prompt_build"));
        assert!(src.contains("fire(\"session_start\")"));
    }

    #[test]
    fn toml_strip_removes_absolute_path_blocks_markers_and_keeps_foreign() {
        // The live bug: installs wrote absolute-path commands the strip
        // matcher never recognized, so every re-arm appended a full set
        // (152 `[[hooks]]` blocks on the dogfood box).
        let mut text = String::from("[model]\nname = \"k2\"\n\n[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"node /home/u/foreign.mjs\"\n");
        for _ in 0..3 {
            text.push_str(
                "\n# stateroot hooks (managed by `stateroot install` — do not edit by hand)\n",
            );
            for event in ["SessionStart", "UserPromptSubmit"] {
                text.push_str(&format!(
                    "[[hooks]]\nevent = \"{event}\"\ncommand = \"/home/ubuntu/.local/bin/stateroot hook {} --harness kimi-code\"\n",
                    event.to_lowercase()
                ));
            }
        }
        // Also a bare-form block and a windows-form block.
        text.push_str(
            "[[hooks]]\nevent = \"Stop\"\ncommand = \"stateroot hook stop --harness kimi-code\"\n",
        );
        text.push_str("[[hooks]]\nevent = \"Stop\"\ncommand = \"C:\\\\bin\\\\stateroot.exe hook stop --harness kimi-code\"\n");

        let cleaned = strip_stateroot_toml_hooks(&text);
        assert!(
            cleaned.contains("node /home/u/foreign.mjs"),
            "foreign hook survives: {cleaned}"
        );
        assert!(cleaned.contains("[model]"), "config survives: {cleaned}");
        assert!(
            !cleaned.contains("stateroot hook"),
            "all absolute/bare stateroot blocks dropped: {cleaned}"
        );
        assert!(
            !cleaned.contains("stateroot.exe hook"),
            "windows-form block dropped: {cleaned}"
        );
        assert!(
            !cleaned.contains("# stateroot hooks"),
            "orphan markers dropped: {cleaned}"
        );
        assert_eq!(
            cleaned.matches("[[hooks]]").count(),
            1,
            "only the foreign block remains: {cleaned}"
        );
    }

    #[test]
    fn toml_install_is_idempotent() {
        let home = tempfile::tempdir().expect("home");
        let q = quirk("kimi-code").expect("kimi-code");
        let first = install_hooks(home.path(), q).expect("first install");
        assert!(first.iter().any(|m| m.contains("hooks installed")));
        let second = install_hooks(home.path(), q).expect("second install");
        assert!(
            second.iter().any(|m| m.contains("already up to date")),
            "reinstall must be a no-op: {second:?}"
        );
        let text =
            std::fs::read_to_string(home.path().join(".kimi-code/config.toml")).expect("cfg");
        assert_eq!(
            text.matches("[[hooks]]").count(),
            q.event_map.len(),
            "exactly one set of hook blocks: {text}"
        );
    }

    #[test]
    fn toml_hooks_escape_windows_verbatim_paths() {
        let command = "\\\\?\\C:\\Users\\usama\\AppData\\Local\\Programs\\StateRoot\\stateroot.exe hook session_start --harness kimi";
        let block = toml_hook_block("session_start", command);
        let parsed: toml::Value = toml::from_str(&block).expect("hook block parses as TOML");
        let written = parsed["hooks"][0]["command"]
            .as_str()
            .expect("command is a string");
        assert_eq!(written, command, "the path round-trips exactly");
    }

    #[test]
    fn copilot_hooks_file_shape_idempotency_and_backup() {
        let home = tempfile::tempdir().expect("home");
        let q = quirk("vscode-copilot").expect("vscode-copilot");
        let first = install_hooks(home.path(), q).expect("first install");
        assert!(
            first.iter().any(|m| m.contains("hooks installed")),
            "{first:?}"
        );

        let path = home.path().join(".copilot/hooks/stateroot.json");
        let text = std::fs::read_to_string(&path).expect("hooks file");
        let doc: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert_eq!(doc["version"], json!(1));
        let hooks = doc["hooks"].as_object().expect("hooks object");
        let expected: [(&str, &str); 6] = [
            ("SessionStart", "session_start"),
            ("UserPromptSubmit", "user_prompt_submit"),
            ("PostToolUse", "post_tool_use"),
            ("PreCompact", "pre_compact"),
            ("Stop", "stop"),
            ("SessionEnd", "session_end"),
        ];
        assert_eq!(hooks.len(), expected.len(), "exactly our events: {text}");
        for (event, canonical) in expected {
            let arr = hooks
                .get(event)
                .and_then(|v| v.as_array())
                .unwrap_or_else(|| panic!("missing {event}"));
            assert_eq!(arr.len(), 1, "{event}");
            let entry = &arr[0];
            assert_eq!(entry["type"], json!("command"), "{event}");
            let cmd = entry["command"].as_str().unwrap_or("");
            assert!(
                cmd.contains(&format!("stateroot hook {canonical} ")),
                "{event}: {cmd}"
            );
            assert!(cmd.ends_with("--harness vscode-copilot"), "{event}: {cmd}");
            assert!(
                entry.get("matcher").is_none(),
                "{event} must not carry matcher"
            );
        }
        // SessionStart carries the digest; SessionEnd never stalls a close.
        assert_eq!(hooks["SessionStart"][0]["timeout"], json!(30));
        assert_eq!(hooks["SessionEnd"][0]["timeout"], json!(8));

        // Idempotent.
        let second = install_hooks(home.path(), q).expect("second install");
        assert!(
            second.iter().any(|m| m.contains("already up to date")),
            "{second:?}"
        );

        // A hand edit is backed up before being replaced.
        std::fs::write(&path, "{\"version\":1,\"hooks\":{}}\n").expect("hand edit");
        install_hooks(home.path(), q).expect("third install");
        assert!(
            home.path()
                .join(".copilot/hooks/stateroot.json.bak")
                .is_file(),
            "hand-edited file backed up"
        );

        // A foreign neighbor hook file is never touched.
        let foreign = home.path().join(".copilot/hooks/foreign.json");
        let foreign_body = "{\"version\":1,\"hooks\":{\"SessionStart\":[]}}\n";
        std::fs::write(&foreign, foreign_body).expect("foreign");
        install_hooks(home.path(), q).expect("fourth install");
        assert_eq!(
            std::fs::read_to_string(&foreign).expect("foreign read"),
            foreign_body
        );
    }

    fn hermes_yaml(home: &Path) -> String {
        std::fs::read_to_string(home.join(".hermes/config.yaml")).expect("config.yaml")
    }

    #[test]
    fn yaml_hooks_install_merges_and_preserves_foreign_config() {
        let home = tempfile::tempdir().expect("home");
        let dir = home.path().join(".hermes");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("config.yaml"),
            concat!(
                "model: anthropic/claude-sonnet-4.6\n",
                "mcp_servers:\n",
                "  stateroot:\n",
                "    command: stateroot\n",
                "    args: [mcp-stdio]\n",
                "hooks:\n",
                "  pre_llm_call:\n",
                "    - command: ~/.hermes/agent-hooks/foreign.sh\n",
                "  outbound:\n",
                "    - name: ci\n",
                "      url: https://ci.example/hook\n",
                "      events: [on_session_end]\n",
            ),
        )
        .expect("seed config");
        let q = quirk("hermes").expect("hermes");

        let first = install_hooks(home.path(), q).expect("first install");
        assert!(
            first.iter().any(|m| m.contains("hooks installed")),
            "{first:?}"
        );
        // Consent guidance is honest: stateroot never sets hooks_auto_accept.
        assert!(first.iter().any(|m| m.contains("consent")), "{first:?}");

        let text = hermes_yaml(home.path());
        let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("valid yaml");
        // Foreign settings survive.
        assert_eq!(
            doc.get("model").and_then(|m| m.as_str()),
            Some("anthropic/claude-sonnet-4.6")
        );
        assert!(doc["mcp_servers"]["stateroot"]["command"]
            .as_str()
            .is_some());
        // Reserved sub-section survives untouched.
        assert_eq!(
            doc["hooks"]["outbound"][0]["name"].as_str(),
            Some("ci"),
            "outbound preserved: {text}"
        );
        // Every mapped event carries exactly one stateroot entry, and the
        // foreign entry under the same event survives.
        for (event, canonical) in q.event_map {
            let entries = doc["hooks"][event]
                .as_sequence()
                .unwrap_or_else(|| panic!("missing hooks.{event}: {text}"));
            let stateroot_entries: Vec<_> = entries
                .iter()
                .filter(|entry| {
                    entry
                        .get("command")
                        .and_then(|c| c.as_str())
                        .map(is_stateroot_command_str)
                        .unwrap_or(false)
                })
                .collect();
            assert_eq!(stateroot_entries.len(), 1, "{event}: {text}");
            let command = stateroot_entries[0]["command"].as_str().expect("command");
            assert!(
                command.contains(&format!("hook {canonical} --harness hermes")),
                "{event}: {command}"
            );
            // Observer entries carry no matcher/timeout/fail_closed keys.
            assert_eq!(stateroot_entries[0].as_mapping().expect("map").len(), 1);
        }
        let pre_llm = doc["hooks"]["pre_llm_call"].as_sequence().expect("pre_llm");
        assert!(
            pre_llm
                .iter()
                .any(|entry| entry["command"].as_str() == Some("~/.hermes/agent-hooks/foreign.sh")),
            "foreign entry under our event preserved: {text}"
        );
        // on_session_reset is never installed (its session_id is the new session).
        assert!(doc["hooks"].get("on_session_reset").is_none(), "{text}");

        // Replay is a no-op with no duplicate entries.
        let second = install_hooks(home.path(), q).expect("second install");
        assert!(
            second.iter().any(|m| m.contains("already up to date")),
            "{second:?}"
        );
        let replayed = hermes_yaml(home.path());
        let count = replayed.matches("--harness hermes").count();
        assert_eq!(count, q.event_map.len(), "no duplicates: {replayed}");

        // First modification produced a pristine backup of the seeded file.
        let bak = std::fs::read_to_string(dir.join("config.yaml.bak")).expect("bak");
        assert!(bak.contains("foreign.sh"), "pristine backup: {bak}");
        assert!(!bak.contains("--harness hermes"), "pristine backup: {bak}");
    }

    #[test]
    fn yaml_hooks_uninstall_strips_only_stateroot_entries() {
        let home = tempfile::tempdir().expect("home");
        let q = quirk("hermes").expect("hermes");
        install_hooks(home.path(), q).expect("install");
        // Add foreign entries after install: one under OUR event (same-list
        // neighbor) and one under an event we never install.
        let mut doc: serde_yaml::Value =
            serde_yaml::from_str(&hermes_yaml(home.path())).expect("valid yaml");
        let mut foreign = serde_yaml::Mapping::new();
        foreign.insert(
            serde_yaml::Value::String("command".to_string()),
            serde_yaml::Value::String("/opt/foreign/hook.sh".to_string()),
        );
        doc["hooks"]["on_session_start"]
            .as_sequence_mut()
            .expect("list")
            .insert(0, serde_yaml::Value::Mapping(foreign.clone()));
        doc["hooks"]["pre_verify"] =
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(foreign)]);
        std::fs::write(
            home.path().join(".hermes/config.yaml"),
            serde_yaml::to_string(&doc).expect("serialize"),
        )
        .expect("foreign edit");

        let actions = remove_hooks(home.path(), q).expect("uninstall");
        assert!(
            actions.iter().any(|m| m.contains("hooks removed")),
            "{actions:?}"
        );
        let doc: serde_yaml::Value =
            serde_yaml::from_str(&hermes_yaml(home.path())).expect("valid yaml");
        let hooks = doc["hooks"].as_mapping().expect("hooks mapping");
        assert!(
            !hooks
                .values()
                .filter_map(|entries| entries.as_sequence())
                .flatten()
                .any(|entry| {
                    entry
                        .get("command")
                        .and_then(|c| c.as_str())
                        .map(is_stateroot_command_str)
                        .unwrap_or(false)
                }),
            "no stateroot entries remain"
        );
        // The foreign neighbor under our event survives; emptied event lists
        // were dropped; the event we never installed survives whole.
        assert_eq!(
            hooks["on_session_start"][0]["command"].as_str(),
            Some("/opt/foreign/hook.sh")
        );
        assert_eq!(
            hooks["pre_verify"][0]["command"].as_str(),
            Some("/opt/foreign/hook.sh")
        );
        assert!(
            hooks
                .keys()
                .all(|k| k.as_str() == Some("on_session_start") || k.as_str() == Some("pre_verify")),
            "emptied lists dropped: {hooks:?}"
        );

        // Second uninstall is a no-op; missing file is a no-op.
        assert!(remove_hooks(home.path(), q)
            .expect("second uninstall")
            .is_empty());
        std::fs::remove_file(home.path().join(".hermes/config.yaml")).expect("rm");
        assert!(remove_hooks(home.path(), q)
            .expect("missing file")
            .is_empty());
    }

    #[test]
    fn yaml_hooks_uninstall_drops_emptied_hooks_key() {
        let home = tempfile::tempdir().expect("home");
        let q = quirk("hermes").expect("hermes");
        install_hooks(home.path(), q).expect("install");
        remove_hooks(home.path(), q).expect("uninstall");
        let text = hermes_yaml(home.path());
        let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("valid yaml");
        assert!(
            doc.get("hooks").is_none(),
            "empty hooks map removed: {text}"
        );
    }

    #[test]
    fn yaml_hooks_refuse_to_clobber_invalid_config() {
        let home = tempfile::tempdir().expect("home");
        let dir = home.path().join(".hermes");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let broken = "hooks: [not, a, mapping\n  unbalanced";
        std::fs::write(dir.join("config.yaml"), broken).expect("broken");
        let q = quirk("hermes").expect("hermes");
        let err = install_hooks(home.path(), q).expect_err("must fail");
        assert!(err.to_string().contains("not valid YAML"), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.join("config.yaml")).expect("read"),
            broken,
            "broken file untouched"
        );
        // A mapping-shaped but non-list event is diagnosed, not clobbered.
        std::fs::write(dir.join("config.yaml"), "hooks:\n  pre_llm_call: nope\n").expect("seed");
        let err = install_hooks(home.path(), q).expect_err("must fail");
        assert!(err.to_string().contains("is not a list"), "{err}");
    }

    #[test]
    fn yaml_shell_command_quotes_whitespace_paths_only() {
        assert_eq!(
            yaml_shell_command("stateroot hook stop --harness hermes"),
            "stateroot hook stop --harness hermes"
        );
        assert_eq!(
            yaml_shell_command("/home/alice/.local/bin/stateroot hook stop --harness hermes"),
            "/home/alice/.local/bin/stateroot hook stop --harness hermes"
        );
        let spaced =
            yaml_shell_command("/home/alice/my bin/stateroot hook session_start --harness hermes");
        assert_eq!(
            spaced,
            "\"/home/alice/my bin/stateroot\" hook session_start --harness hermes"
        );
        // The health matcher reads the quoted form back as a stateroot binary.
        assert!(crate::harness_install::health::is_stateroot_hook_command(
            &spaced
        ));
        // Windows verbatim paths quote and round-trip through the matcher too
        // (backslashes doubled so shlex hands the original path back).
        let windows = yaml_shell_command(
            "C:\\Users\\alice\\StateRoot App\\stateroot.exe hook stop --harness hermes",
        );
        assert_eq!(
            windows,
            "\"C:\\\\Users\\\\alice\\\\StateRoot App\\\\stateroot.exe\" hook stop --harness hermes"
        );
        assert!(crate::harness_install::health::is_stateroot_hook_command(
            &windows
        ));
    }

    #[test]
    fn yaml_shell_command_quotes_bare_backslash_paths_for_shlex() {
        // Parser fixture only: proves the emitted string survives hermes'
        // `shlex.split` (shell=False) grammar — NOT proof of native Windows
        // execution. Bare Windows paths carry no whitespace, but shlex would
        // eat the separators, so the binary leg must be quoted too. Inside
        // shlex double quotes `\` escapes only `"` and `\`, so the emitter
        // doubles backslashes and the original path reads back exactly.
        let win =
            yaml_shell_command("C:\\Users\\alice\\bin\\stateroot.exe hook stop --harness hermes");
        assert_eq!(
            win,
            "\"C:\\\\Users\\\\alice\\\\bin\\\\stateroot.exe\" hook stop --harness hermes"
        );
        assert!(crate::harness_install::health::is_stateroot_hook_command(
            &win
        ));
        // Verbatim canonical paths (`\\?\C:\…`) round-trip: the doubled
        // leading pair is what shlex hands back as `\\?\`.
        let verbatim =
            yaml_shell_command("\\\\?\\C:\\Users\\alice\\stateroot.exe hook stop --harness hermes");
        assert_eq!(
            verbatim,
            "\"\\\\\\\\?\\\\C:\\\\Users\\\\alice\\\\stateroot.exe\" hook stop --harness hermes"
        );
        assert!(crate::harness_install::health::is_stateroot_hook_command(
            &verbatim
        ));
        // Bare invocation and clean absolute paths stay unquoted.
        assert_eq!(
            yaml_shell_command("stateroot hook stop --harness hermes"),
            "stateroot hook stop --harness hermes"
        );
        assert_eq!(
            yaml_shell_command("/opt/stateroot/bin/stateroot hook stop --harness hermes"),
            "/opt/stateroot/bin/stateroot hook stop --harness hermes"
        );
    }

    #[test]
    fn yaml_hooks_replay_and_uninstall_recognize_quoted_and_keep_mention() {
        let home = tempfile::tempdir().expect("home");
        let dir = home.path().join(".hermes");
        std::fs::create_dir_all(&dir).expect("mkdir");
        // Quoted executable paths exactly as the installer emits them (spaced
        // Unix path, backslash Windows path), plus a FOREIGN command that
        // merely mentions stateroot — it must never be treated as ours.
        std::fs::write(
            dir.join("config.yaml"),
            concat!(
                "model: gpt-4\n",
                "hooks:\n",
                "  on_session_start:\n",
                "    - command: '\"/opt/my bin/stateroot\" hook session_start --harness hermes'\n",
                "  on_session_end:\n",
                "    - command: '\"C:\\Users\\alice\\bin\\stateroot.exe\" hook session_end --harness hermes'\n",
                "  pre_llm_call:\n",
                "    - command: echo stateroot hook stop --harness hermes >> /tmp/stateroot-hooks.log\n",
            ),
        )
        .expect("seed");
        let q = quirk("hermes").expect("hermes");

        install_hooks(home.path(), q).expect("install");
        let text = hermes_yaml(home.path());
        let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("valid yaml");
        // Quoted predecessors are REPLACED, not duplicated: exactly one
        // stateroot entry per mapped event.
        for (event, _) in q.event_map {
            let entries = doc["hooks"][event]
                .as_sequence()
                .unwrap_or_else(|| panic!("missing hooks.{event}: {text}"));
            let ours = entries
                .iter()
                .filter(|entry| {
                    entry
                        .get("command")
                        .and_then(|c| c.as_str())
                        .map(crate::harness_install::health::is_stateroot_hook_command)
                        .unwrap_or(false)
                })
                .count();
            assert_eq!(
                ours, 1,
                "{event}: quoted entry replaced, not duplicated: {text}"
            );
        }
        // The foreign mention survives install.
        let mention = "echo stateroot hook stop --harness hermes >> /tmp/stateroot-hooks.log";
        let pre_llm = doc["hooks"]["pre_llm_call"]
            .as_sequence()
            .expect("pre_llm_call");
        assert!(
            pre_llm
                .iter()
                .any(|e| e["command"].as_str() == Some(mention)),
            "foreign command that mentions stateroot survives install: {text}"
        );

        // Replay converges: still one entry per event.
        let second = install_hooks(home.path(), q).expect("replay");
        assert!(
            second.iter().any(|m| m.contains("already up to date")),
            "{second:?}"
        );

        // Uninstall strips ours INCLUDING the quoted forms and keeps the
        // foreign mention.
        remove_hooks(home.path(), q).expect("uninstall");
        let text = hermes_yaml(home.path());
        let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("valid yaml");
        let hooks = doc["hooks"].as_mapping().expect("hooks mapping");
        assert!(
            !hooks
                .values()
                .filter_map(|entries| entries.as_sequence())
                .flatten()
                .any(|entry| {
                    entry
                        .get("command")
                        .and_then(|c| c.as_str())
                        .map(crate::harness_install::health::is_stateroot_hook_command)
                        .unwrap_or(false)
                }),
            "quoted stateroot entries stripped: {text}"
        );
        let pre_llm = hooks["pre_llm_call"].as_sequence().expect("pre_llm_call");
        assert_eq!(pre_llm.len(), 1, "{text}");
        assert_eq!(pre_llm[0]["command"].as_str(), Some(mention), "{text}");
    }

    #[test]
    fn yaml_hooks_propagate_read_errors_instead_of_clobbering() {
        // A config whose bytes cannot be read (here: invalid UTF-8) is NOT an
        // empty config. Install must fail and leave the bytes alone; remove
        // must fail instead of reporting "unchanged".
        let home = tempfile::tempdir().expect("home");
        let dir = home.path().join(".hermes");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let config = dir.join("config.yaml");
        let bytes = b"model: \xFF\xFE not utf8\n";
        std::fs::write(&config, bytes).expect("seed");
        let q = quirk("hermes").expect("hermes");

        let err = install_hooks(home.path(), q).expect_err("read error must propagate");
        assert!(
            matches!(err, HarnessError::Io { .. }),
            "io error, not silent clobber: {err}"
        );
        assert_eq!(
            std::fs::read(&config).expect("read"),
            bytes,
            "unreadable config bytes untouched"
        );

        let err = remove_yaml_hooks_from(&config).expect_err("read error must propagate");
        assert!(
            matches!(err, HarnessError::Io { .. }),
            "io error, not falsely unchanged: {err}"
        );
        assert_eq!(
            std::fs::read(&config).expect("read"),
            bytes,
            "unreadable config bytes untouched"
        );
    }

    #[test]
    fn named_groups_install_native_shapes_and_preserve_foreign_content() {
        let home = tempfile::tempdir().expect("home");
        let q = quirk("antigravity").expect("antigravity");
        let dir = home.path().join(".gemini/config");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("hooks.json");
        let seed = json!({
            "my-linter-hook": {
                "PostToolUse": [{
                    "matcher": "run_command",
                    "hooks": [{"type": "command", "command": "./lint.sh"}]
                }]
            },
            "stateroot": {
                "enabled": false,
                "CustomEvent": [{"type": "command", "command": "./mine.sh"}],
                "Stop": [{"type": "command", "command": "stateroot hook stop --harness antigravity"}]
            }
        });
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&seed).unwrap()),
        )
        .expect("seed");

        let first = install_hooks(home.path(), q).expect("install");
        assert!(
            first.iter().any(|m| m.contains("hooks installed")),
            "{first:?}"
        );
        let text = std::fs::read_to_string(&path).expect("read");
        let doc: serde_json::Value = serde_json::from_str(&text).expect("json");

        // The foreign group is preserved verbatim.
        assert_eq!(doc["my-linter-hook"], seed["my-linter-hook"]);
        let group = doc["stateroot"].as_object().expect("group object");
        // Group-level foreign settings (a user toggle) and foreign events
        // inside our group survive an install/update.
        assert_eq!(group["enabled"], json!(false));
        assert_eq!(group["CustomEvent"], seed["stateroot"]["CustomEvent"]);

        // Native shapes: tool events nested (matcher + hooks array),
        // lifecycle events flat handler lists (matcher ignored there).
        for event in ["PreToolUse", "PostToolUse"] {
            let arr = group[event].as_array().unwrap_or_else(|| panic!("{event}"));
            assert_eq!(arr.len(), 1, "{event}");
            assert_eq!(arr[0]["matcher"], json!(""), "{event}");
            let handlers = arr[0]["hooks"].as_array().expect("handlers");
            assert_eq!(handlers.len(), 1, "{event}");
            assert_eq!(handlers[0]["type"], json!("command"), "{event}");
        }
        for event in ["SessionStart", "PreInvocation", "Stop"] {
            let arr = group[event].as_array().unwrap_or_else(|| panic!("{event}"));
            assert_eq!(arr.len(), 1, "{event}");
            assert_eq!(arr[0]["type"], json!("command"), "{event}");
            assert!(arr[0].get("matcher").is_none(), "{event} flat handler");
            assert!(arr[0].get("hooks").is_none(), "{event} flat handler");
        }
        // Both lifecycle registrations invoke the canonical session_start;
        // hook.rs tells them apart by the payload's invocationNum.
        for event in ["SessionStart", "PreInvocation"] {
            let command = group[event][0]["command"].as_str().expect("command");
            assert!(
                command.contains("stateroot hook session_start "),
                "{event}: {command}"
            );
            assert!(
                command.ends_with("--harness antigravity"),
                "{event}: {command}"
            );
        }
        // Health extraction reads the same commands through both shapes.
        let commands = crate::harness_install::health::extract_hook_commands(
            &path,
            HookFormat::NamedGroupsJson,
        );
        assert_eq!(commands.len(), 5, "{commands:?}");

        // Reinstall is a no-op (foreign content included in the merge).
        let second = install_hooks(home.path(), q).expect("reinstall");
        assert!(
            second.iter().any(|m| m.contains("already up to date")),
            "{second:?}"
        );
        let again: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(again["stateroot"]["enabled"], json!(false));
    }

    #[test]
    fn named_groups_remove_strips_only_stateroot_entries() {
        let home = tempfile::tempdir().expect("home");
        let q = quirk("antigravity").expect("antigravity");
        install_hooks(home.path(), q).expect("install");
        let path = home.path().join(".gemini/config/hooks.json");
        let mut doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        // Foreign content: a neighbor group, a foreign event under our group,
        // and a group-level toggle.
        doc["foreign-hooks"] = json!({
            "SessionStart": [{"type": "command", "command": "./init.sh"}]
        });
        doc["stateroot"]["CustomEvent"] = json!([{"type": "command", "command": "./mine.sh"}]);
        doc["stateroot"]["enabled"] = json!(false);
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).expect("write");

        let actions = remove_hooks(home.path(), q).expect("remove");
        assert!(
            actions.iter().any(|m| m.contains("hooks removed")),
            "{actions:?}"
        );
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        // Foreign group untouched.
        assert_eq!(
            after["foreign-hooks"],
            json!({"SessionStart": [{"type": "command", "command": "./init.sh"}]})
        );
        // Our group survives because a foreign event remains — with the
        // group-level toggle preserved; every stateroot event is gone.
        let group = after["stateroot"].as_object().expect("group kept");
        assert_eq!(group["enabled"], json!(false));
        assert_eq!(
            group["CustomEvent"],
            json!([{"type": "command", "command": "./mine.sh"}])
        );
        for event in [
            "SessionStart",
            "PreInvocation",
            "PreToolUse",
            "PostToolUse",
            "Stop",
        ] {
            assert!(group.get(event).is_none(), "{event} stripped: {group:?}");
        }

        // When nothing foreign remains, the group itself is dropped.
        let actions = remove_hooks(home.path(), q).expect("second remove is a no-op");
        assert!(actions.is_empty(), "{actions:?}");
        let ours_only = json!({
            "foreign-hooks": after["foreign-hooks"].clone(),
            "stateroot": {
                "enabled": false,
                "Stop": [{"type": "command", "command": "stateroot hook stop --harness antigravity"}]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&ours_only).unwrap()).expect("write");
        remove_hooks(home.path(), q).expect("remove");
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert!(
            after.get("stateroot").is_none(),
            "bare toggle with zero hooks is our debris: {after:?}"
        );
        assert!(after.get("foreign-hooks").is_some());
    }

    #[test]
    fn named_groups_install_preserves_foreign_handlers_inside_owned_events() {
        // Same-event foreign content: a foreign flat handler inside the
        // stateroot group's Stop array, and a tool matcher whose hooks array
        // mixes our command with a foreign command. Install/update must
        // replace only the owned handlers — never drop the foreign ones.
        let home = tempfile::tempdir().expect("home");
        let q = quirk("antigravity").expect("antigravity");
        let dir = home.path().join(".gemini/config");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("hooks.json");
        let foreign_stop = json!({"type": "command", "command": "./user-stop.sh", "timeout": 5});
        let foreign_lint = json!({"type": "command", "command": "./user-lint.sh"});
        let seed = json!({
            "stateroot": {
                "enabled": false,
                "Stop": [
                    foreign_stop.clone(),
                    {"type": "command", "command": "stateroot hook stop --harness antigravity"}
                ],
                "PostToolUse": [{
                    "matcher": "run_command",
                    "hooks": [
                        {"type": "command", "command": "stateroot hook post_tool_use --harness antigravity"},
                        foreign_lint.clone()
                    ]
                }]
            }
        });
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&seed).unwrap()),
        )
        .expect("seed");

        install_hooks(home.path(), q).expect("install");
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        let group = doc["stateroot"].as_object().expect("group object");
        assert_eq!(group["enabled"], json!(false), "group toggle preserved");

        // The foreign Stop handler survives verbatim next to our fresh one.
        let stop = group["Stop"].as_array().expect("Stop array");
        assert!(
            stop.contains(&foreign_stop),
            "foreign Stop handler preserved byte-for-byte: {stop:?}"
        );
        assert!(
            stop.iter().any(is_stateroot_entry),
            "our Stop handler installed: {stop:?}"
        );

        // The mixed matcher survives with its foreign handler; our handler
        // lives in our own matcher entry, not duplicated inside the user's.
        let post = group["PostToolUse"].as_array().expect("PostToolUse array");
        let user_matcher = json!({
            "matcher": "run_command",
            "hooks": [foreign_lint.clone()]
        });
        assert!(
            post.contains(&user_matcher),
            "user matcher keeps its foreign handler: {post:?}"
        );
        let ours: Vec<&Value> = post
            .iter()
            .filter(|entry| is_stateroot_entry(entry))
            .collect();
        assert_eq!(ours.len(), 1, "exactly one owned matcher: {post:?}");
        let our_handlers = ours[0]["hooks"].as_array().expect("our handlers");
        assert_eq!(our_handlers.len(), 1, "no duplication: {our_handlers:?}");

        // Repeated install is a byte-stable no-op: foreign content included.
        let second = install_hooks(home.path(), q).expect("reinstall");
        assert!(
            second.iter().any(|m| m.contains("already up to date")),
            "{second:?}"
        );
        let again: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(again, doc, "reinstall is idempotent");
    }

    #[test]
    fn named_groups_remove_preserves_foreign_handlers_inside_owned_events() {
        // Uninstall strips only the owned handlers: a foreign handler sharing
        // our event survives, and a tool matcher mixing our command with a
        // foreign command loses only ours — the matcher itself stays.
        let home = tempfile::tempdir().expect("home");
        let q = quirk("antigravity").expect("antigravity");
        install_hooks(home.path(), q).expect("install");
        let path = home.path().join(".gemini/config/hooks.json");
        let mut doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        let foreign_stop = json!({"type": "command", "command": "./user-stop.sh", "timeout": 5});
        let foreign_lint = json!({"type": "command", "command": "./user-lint.sh"});
        doc["stateroot"]["Stop"]
            .as_array_mut()
            .expect("Stop array")
            .push(foreign_stop.clone());
        doc["stateroot"]["PostToolUse"] = json!([{
            "matcher": "run_command",
            "hooks": [
                {"type": "command", "command": "stateroot hook post_tool_use --harness antigravity"},
                foreign_lint.clone()
            ]
        }]);
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).expect("write");

        remove_hooks(home.path(), q).expect("remove");
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        let group = after["stateroot"]
            .as_object()
            .expect("group kept: foreign handlers remain");
        // Our Stop handler is gone; the foreign one is byte-identical.
        let stop = group["Stop"].as_array().expect("Stop survives");
        assert_eq!(stop.len(), 1, "{stop:?}");
        assert_eq!(stop[0], foreign_stop);
        // The mixed matcher kept its foreign handler and lost only ours.
        let post = group["PostToolUse"]
            .as_array()
            .expect("PostToolUse survives");
        assert_eq!(
            post.as_slice(),
            &[json!({
                "matcher": "run_command",
                "hooks": [foreign_lint]
            })],
            "{post:?}"
        );
        // Fully-owned events are gone entirely.
        for event in ["SessionStart", "PreInvocation", "PreToolUse"] {
            assert!(group.get(event).is_none(), "{event} stripped: {group:?}");
        }

        // Second remove is a no-op — nothing owned remains.
        let actions = remove_hooks(home.path(), q).expect("second remove");
        assert!(actions.is_empty(), "{actions:?}");
    }
}
