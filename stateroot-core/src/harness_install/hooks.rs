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

use super::paths;
use super::registry::{HarnessQuirk, HookFormat};
use super::{io_err, HarnessError};

fn bak_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.bak",
        path.extension().and_then(|e| e.to_str()).unwrap_or("json")
    ))
}

fn command_for(quirk: &HarnessQuirk, canonical: &str) -> String {
    paths::hook_command(quirk.id, canonical)
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
                // Cursor kills hooks at a short default timeout; session_start
                // also runs federation syncs, which can take ~10s on slow
                // filesystems. The digest prints first, then the syncs.
                "timeout": 30,
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
    // Normalize what the strip leaves behind (stray blank lines where our
    // blocks and markers were) so reinstalls converge instead of accruing
    // one newline per run.
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
}
