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
//! - openclaw: generated native plugin under `~/.openclaw/extensions/stateroot`
//!   (real OpenClaw discovery root; typed hooks via `api.on("snake_case", …)`)
//!
//! Merge semantics mirror the MCP registration: read-merge-write with a
//! `.bak` backup; third-party hooks in the same file are preserved and only
//! stateroot-owned entries are replaced.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::registry::{HarnessQuirk, HookFormat};
use super::{io_err, HarnessError};

fn bak_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.bak",
        path.extension().and_then(|e| e.to_str()).unwrap_or("json")
    ))
}

fn command_for(quirk: &HarnessQuirk, canonical: &str) -> String {
    format!("stateroot hook {canonical} --harness {}", quirk.id)
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

fn flat_entries(quirk: &HarnessQuirk) -> Map<String, Value> {
    let mut out = Map::new();
    for (harness_event, canonical) in quirk.event_map {
        out.insert(
            (*harness_event).to_string(),
            json!([{
                "type": "command",
                "command": command_for(quirk, canonical),
                "matcher": "",
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
    commands.iter().any(|c| c.contains("stateroot hook"))
}

/// Overlay our entries into an event-keyed hooks object: drop prior
/// stateroot-owned entries under each event we own, append ours, keep
/// everything else (including foreign hooks under the same event).
fn overlay(hooks_obj: &mut Map<String, Value>, ours: &Map<String, Value>) {
    for (event, value) in ours {
        let entry = hooks_obj.entry(event.clone()).or_insert_with(|| json!([]));
        if let Some(arr) = entry.as_array_mut() {
            arr.retain(|existing| !is_stateroot_entry(existing));
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

fn write_json_if_changed(path: &Path, doc: &Value) -> Result<bool, HarnessError> {
    let text = format!("{}\n", serde_json::to_string_pretty(doc)?);
    let current = std::fs::read_to_string(path).unwrap_or_default();
    if current == text {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    std::fs::write(path, text).map_err(io_err(path))?;
    Ok(true)
}

/// Install one Tier A harness's native hook config. Returns action lines.
pub fn install_hooks(home: &Path, quirk: &HarnessQuirk) -> Result<Vec<String>, HarnessError> {
    let Some(target) = quirk.hooks else {
        return Ok(Vec::new());
    };
    let path = home.join(target.path);
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
        HookFormat::FlatJson => {
            let mut doc = read_json_file(&path)?;
            if !doc.is_object() {
                doc = json!({});
            }
            let ours = flat_entries(quirk);
            backup_once(&path)?;
            let root = doc.as_object_mut().ok_or_else(|| {
                HarnessError::Invalid(format!("{}: not a JSON object", path.display()))
            })?;
            root.entry("version".to_string()).or_insert(json!(1));
            let hooks = root.entry("hooks".to_string()).or_insert_with(|| json!({}));
            let hooks = hooks.as_object_mut().ok_or_else(|| {
                HarnessError::Invalid(format!("{}: `hooks` is not an object", path.display()))
            })?;
            overlay(hooks, &ours);
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
        HookFormat::TomlHooks => install_toml_hooks(&path, quirk),
        HookFormat::ZeroExecJson => install_zero_hooks(&path, quirk),
        HookFormat::NamedGroupsJson => install_named_groups(&path, quirk),
        HookFormat::NativePlugin => install_native_plugin(&path, quirk),
    }
}

/// `[[hooks]]` TOML entries: remove prior stateroot-marked blocks, append ours.
fn install_toml_hooks(path: &Path, quirk: &HarnessQuirk) -> Result<Vec<String>, HarnessError> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let cleaned = strip_stateroot_toml_hooks(&existing);
    let mut block = String::from(
        "\n# stateroot hooks (managed by `stateroot install` — do not edit by hand)\n",
    );
    for (harness_event, canonical) in quirk.event_map {
        block.push_str("[[hooks]]\n");
        block.push_str(&format!("event = \"{harness_event}\"\n"));
        block.push_str(&format!(
            "command = \"{}\"\n",
            command_for(quirk, canonical)
        ));
    }
    let mut updated = cleaned;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&block);

    if updated == existing {
        return Ok(vec![format!(
            "hooks already up to date ({})",
            path.display()
        )]);
    }
    backup_once(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    std::fs::write(path, &updated).map_err(io_err(path))?;
    Ok(vec![format!("hooks installed → {}", path.display())])
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
            let is_ours = body.contains("command = \"stateroot hook")
                || body.contains("command=\"stateroot hook");
            let is_marker = block
                .first()
                .map(|l| l.trim().starts_with("# stateroot hooks"))
                .unwrap_or(false);
            if is_ours || is_marker {
                return; // drop the stateroot-owned block
            }
        }
        for line in block {
            out.push_str(line);
            out.push('\n');
        }
    };

    for line in text.lines() {
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
    let mut group = Map::new();
    for (harness_event, canonical) in quirk.event_map {
        let handler = json!({"type": "command", "command": command_for(quirk, canonical)});
        if harness_event.starts_with("Pre") && *harness_event != "PreInvocation" {
            // Tool events: nested shape (matcher + hooks array).
            group.insert(
                (*harness_event).to_string(),
                json!([{ "matcher": "", "hooks": [handler] }]),
            );
        } else {
            // Lifecycle events: flat handler list.
            group.insert((*harness_event).to_string(), json!([handler]));
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
/// file is a no-op.
pub fn remove_hooks(home: &Path, quirk: &HarnessQuirk) -> Result<Vec<String>, HarnessError> {
    let Some(target) = quirk.hooks else {
        return Ok(Vec::new());
    };
    let path = home.join(target.path);
    if target.format == HookFormat::NativePlugin {
        if path.is_dir() {
            std::fs::remove_dir_all(&path).map_err(io_err(&path))?;
            return Ok(vec![format!("plugin removed → {}", path.display())]);
        }
        return Ok(Vec::new());
    }
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let changed = match target.format {
        HookFormat::TomlHooks => {
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let stripped = strip_stateroot_toml_hooks(&existing);
            if stripped != existing {
                backup_once(&path)?;
                std::fs::write(&path, stripped).map_err(io_err(&path))?;
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
        HookFormat::NamedGroupsJson => {
            let mut doc = read_json_file(&path)?;
            let had = doc.get("stateroot").is_some();
            if had {
                if let Some(root) = doc.as_object_mut() {
                    root.remove("stateroot");
                }
                backup_once(&path)?;
                let _ = write_json_if_changed(&path, &doc)?;
            }
            had
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
                let mut empty_keys = Vec::new();
                for (key, value) in map.iter_mut() {
                    if let Some(arr) = value.as_array_mut() {
                        let before = arr.len();
                        arr.retain(|entry| {
                            !is_stateroot_entry(entry)
                                && !entry.to_string().contains("stateroot hook")
                        });
                        if arr.len() != before {
                            changed = true;
                        }
                        if arr.is_empty() {
                            empty_keys.push(key.clone());
                        }
                    }
                }
                for key in empty_keys {
                    map.remove(&key);
                }
            }
            if changed {
                backup_once(&path)?;
                let _ = write_json_if_changed(&path, &doc)?;
            }
            changed
        }
    };
    Ok(if changed {
        vec![format!("hooks removed → {}", path.display())]
    } else {
        Vec::new()
    })
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
    //   (session_start is void). Resume digest still via MCP pull.
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
// Context injection: session_start is void; resume arrives via MCP pull.
// before_prompt_build fires user_prompt_submit for capture only.
import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

function fire(event: string): void {
  execFile("stateroot", ["hook", event, "--harness", "openclaw"], { timeout: 5000 }, () => {});
}

async function fireAsync(event: string): Promise<void> {
  try {
    await execFileAsync("stateroot", ["hook", event, "--harness", "openclaw"], {
      timeout: 5000,
    });
  } catch {
    // Never block the agent loop on hook failures.
  }
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
    // Prompt-mutation hook — capture signal only; do not invent context.
    api.on("before_prompt_build", async () => {
      await fireAsync("user_prompt_submit");
      return;
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
        std::fs::write(&path, content).map_err(io_err(&path))?;
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
    std::fs::write(&path, pretty).map_err(io_err(&path))?;
    Ok(Some(format!(
        "enabled plugins.entries.stateroot in {}",
        path.display()
    )))
}
