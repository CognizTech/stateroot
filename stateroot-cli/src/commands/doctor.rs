//! `stateroot doctor` — local diagnostics only (config, store layout,
//! registry, hooks, federation). No server health checks exist in this
//! variant.

use std::path::Path;

use serde_json::Value;
use stateroot_core::harness_install::paths;
use stateroot_core::harness_install::registry::{self, HookFormat};
use stateroot_core::local_store;

use super::Ctx;

#[derive(Debug)]
struct Check {
    label: String,
    ok: bool,
    detail: String,
    hard: bool,
}

/// Run `stateroot doctor`. Returns a process exit code (0 ok, 1 hard failure).
pub async fn run(ctx: &Ctx) -> anyhow::Result<i32> {
    let mut checks: Vec<Check> = Vec::new();

    // Config dir + file.
    checks.push(Check {
        label: "config dir".into(),
        ok: true,
        detail: ctx.config_dir.display().to_string(),
        hard: false,
    });

    // Harness registry contract parses.
    match stateroot_core::skill_federation::load_registry() {
        Ok(reg) => checks.push(Check {
            label: "harness registry".into(),
            ok: true,
            detail: format!("{} harnesses", reg.harnesses.len()),
            hard: false,
        }),
        Err(err) => checks.push(Check {
            label: "harness registry".into(),
            ok: false,
            detail: err,
            hard: true,
        }),
    }

    // Project store layout (when in a project).
    if local_store::is_stateroot_dir(&ctx.cwd) {
        let root = local_store::root(&ctx.cwd);
        let manifest = root.join(local_store::MANIFEST_PATH).is_file();
        checks.push(Check {
            label: "project manifest".into(),
            ok: manifest,
            detail: root.display().to_string(),
            hard: true,
        });
        let handoff = root.join(local_store::HANDOFF_CURRENT_PATH).is_file();
        checks.push(Check {
            label: "current handoff".into(),
            ok: true,
            detail: if handoff {
                "present".into()
            } else {
                "none yet".into()
            },
            hard: false,
        });
    } else {
        checks.push(Check {
            label: "project".into(),
            ok: true,
            detail: "not in a stateroot project (init to create one)".into(),
            hard: false,
        });
    }

    // Persona cache.
    let persona = super::persona::read_cache(&ctx.config_dir).is_some();
    checks.push(Check {
        label: "persona cache".into(),
        ok: true,
        detail: if persona {
            "present".into()
        } else {
            "none (M3 soul service)".into()
        },
        hard: false,
    });

    // Honest identity-delivery tier for detected harnesses (soft).
    if let Ok(home) = super::install::home_dir() {
        let detections = stateroot_core::harness_install::detect::detect_harnesses(
            &home,
            &stateroot_core::harness_install::detect::SystemProber,
        );
        let mut any = false;
        for detection in detections {
            if !detection.installed() {
                continue;
            }
            let Some(quirk) = stateroot_core::harness_install::registry::quirk_any(&detection.id)
            else {
                continue;
            };
            any = true;
            let policy = quirk.delivery();
            let tier = match policy.tier {
                stateroot_core::harness_install::registry::DeliveryTier::Automatic => "automatic",
                stateroot_core::harness_install::registry::DeliveryTier::Degraded => "degraded",
            };
            checks.push(Check {
                label: format!("identity delivery ({})", quirk.id),
                ok: true,
                detail: format!("{tier} — {}", policy.note),
                hard: false,
            });
            if quirk.id == "pi" {
                checks.push(Check {
                    label: "Pi skill isolation".into(),
                    ok: true,
                    detail: "StateRoot launches use `stateroot harness run pi` with ambient .agents skill discovery disabled; pass --ambient-skills to opt in".into(),
                    hard: false,
                });
            }
        }
        if !any {
            checks.push(Check {
                label: "identity delivery".into(),
                ok: true,
                detail: "no harnesses detected on this machine".into(),
                hard: false,
            });
        }
        // Hook-binary health: the binary each installed hook config points
        // at must exist and match THIS cli's version (fail-open hooks never
        // complain otherwise — the Cursor-on-Windows incident: hooks.json
        // resolved to stateroot 0.1.1 while the box ran 0.1.5 and no digest
        // was ever injected).
        checks.extend(hook_binary_checks(&home));
    }

    // Federation doctors (local engines).
    if local_store::is_stateroot_dir(&ctx.cwd) {
        match stateroot_core::skill_federation::doctor(&ctx.cwd, None) {
            Ok(notes) => {
                // The engine's doctor returns informational notes; only
                // warning-prefixed lines are issues.
                let issues: Vec<&String> =
                    notes.iter().filter(|n| n.starts_with("warning:")).collect();
                checks.push(Check {
                    label: "skill federation".into(),
                    ok: issues.is_empty(),
                    detail: if issues.is_empty() {
                        "ok".into()
                    } else {
                        format!("{} issue(s)", issues.len())
                    },
                    hard: false,
                });
                for issue in issues {
                    println!("  {issue}");
                }
            }
            Err(err) => checks.push(Check {
                label: "skill federation".into(),
                ok: false,
                detail: err,
                hard: false,
            }),
        }
        let home = super::install::home_dir()?;
        let report = stateroot_core::mcp_federation::doctor_report(Some(&home), Some(&ctx.cwd))
            .map_err(|e| anyhow::anyhow!(e))?;
        let issues = report
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        checks.push(Check {
            label: "mcp federation".into(),
            ok: issues == 0,
            detail: if issues == 0 {
                "ok".into()
            } else {
                format!("{issues} issue(s) — `stateroot mcp doctor`")
            },
            hard: false,
        });
        match stateroot_core::rules::ensure_product_intent(&home) {
            Ok(_) => {
                let n = stateroot_core::rules::list_all(&ctx.cwd, &home).len();
                checks.push(Check {
                    label: "shared rules".into(),
                    ok: true,
                    detail: format!("{n} rule(s); product-intent always on"),
                    hard: false,
                });
            }
            Err(err) => checks.push(Check {
                label: "shared rules".into(),
                ok: false,
                detail: err.to_string(),
                hard: false,
            }),
        }
        // Continuity chain: not "is it installed" but "is anything flowing"
        // — duplicate managed blocks, last captured checkpoint per harness,
        // and the legacy outbox pile.
        checks.extend(continuity_chain_checks(&home, &ctx.cwd));
    }

    let mut hard_failures = 0;
    for check in &checks {
        let mark = if check.ok { "ok" } else { "!!" };
        println!("  [{mark}] {} — {}", check.label, check.detail);
        if check.hard && !check.ok {
            hard_failures += 1;
        }
    }
    if hard_failures > 0 {
        println!("{hard_failures} hard failure(s)");
        Ok(1)
    } else {
        println!("doctor: all local checks pass");
        Ok(0)
    }
}

/// Hidden test seam (mirrors `STATEROOT_TEST_HOME`): when
/// `STATEROOT_TEST_CMD_PROBES` is set, bare-binary detection answers from
/// this comma-separated allowlist instead of probing the host PATH.
fn test_cmd_probes() -> Option<Vec<String>> {
    std::env::var("STATEROOT_TEST_CMD_PROBES").ok().map(|raw| {
        raw.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// Every stateroot hook command found in `path` (the installer's
/// `hook_target_candidates` output for one harness).
fn extract_hook_commands(path: &Path, format: HookFormat) -> Vec<String> {
    match format {
        HookFormat::TomlHooks => {
            let Ok(text) = std::fs::read_to_string(path) else {
                return Vec::new();
            };
            text.lines()
                .filter_map(|line| {
                    let line = line.trim();
                    let rest = line.strip_prefix("command")?;
                    let command = rest.trim().trim_start_matches('=').trim().trim_matches('"');
                    command
                        .contains("stateroot hook")
                        .then(|| command.to_string())
                })
                .collect()
        }
        HookFormat::ZeroExecJson => {
            let Ok(text) = std::fs::read_to_string(path) else {
                return Vec::new();
            };
            let Ok(doc) = serde_json::from_str::<Value>(&text) else {
                return Vec::new();
            };
            doc.get("hooks")
                .and_then(Value::as_array)
                .map(|hooks| {
                    hooks
                        .iter()
                        .filter(|entry| {
                            entry.get("command").and_then(Value::as_str) == Some("stateroot")
                                && entry
                                    .get("args")
                                    .and_then(Value::as_array)
                                    .and_then(|args| args.first())
                                    .and_then(Value::as_str)
                                    == Some("hook")
                        })
                        .map(|_| "stateroot".to_string())
                        .collect()
                })
                .unwrap_or_default()
        }
        HookFormat::NativePlugin => {
            // The generated extension invokes bare `stateroot` via execFile.
            let Ok(text) = std::fs::read_to_string(path.join("index.ts")) else {
                return Vec::new();
            };
            if text.contains("\"stateroot\"") {
                vec!["stateroot".to_string()]
            } else {
                Vec::new()
            }
        }
        _ => {
            // NestedJson / FlatJson / NamedGroupsJson (and devin's
            // whole-object file): collect every string containing a
            // stateroot hook invocation.
            let Ok(text) = std::fs::read_to_string(path) else {
                return Vec::new();
            };
            let Ok(doc) = serde_json::from_str::<Value>(&text) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            collect_hook_commands(&doc, &mut out);
            out
        }
    }
}

fn collect_hook_commands(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) if s.contains("stateroot hook") || s.contains("stateroot.exe hook") => {
            out.push(s.clone())
        }
        Value::Array(items) => items
            .iter()
            .for_each(|item| collect_hook_commands(item, out)),
        Value::Object(map) => map
            .values()
            .for_each(|item| collect_hook_commands(item, out)),
        _ => {}
    }
}

/// The binary a stateroot hook command invokes: bare `stateroot`, or the
/// (possibly quoted) path before the ` hook <event> --harness <id>` suffix
/// the installer writes.
fn binary_of_command(command: &str) -> Option<String> {
    let command = command.trim().trim_matches('"');
    if command == "stateroot" {
        return Some("stateroot".to_string());
    }
    let (binary, _) = command.split_once(" hook ")?;
    let binary = binary.trim().trim_matches('"');
    if binary == "stateroot" || binary.ends_with("/stateroot") || binary.ends_with("stateroot.exe")
    {
        Some(binary.to_string())
    } else {
        None
    }
}

/// Run one hook binary's `--version` and grade it against this cli.
fn check_one_binary(harness_id: &str, binary: &str, probe: &dyn Fn(&str) -> bool) -> Check {
    let label = format!("hook binary ({harness_id})");
    if binary == "stateroot" && !probe("stateroot") {
        return Check {
            label,
            ok: false,
            detail: "hook command `stateroot` not found on PATH".into(),
            hard: false,
        };
    }
    if binary != "stateroot" && !Path::new(binary).is_file() {
        return Check {
            label,
            ok: false,
            detail: format!("hook command not runnable: {binary}"),
            hard: false,
        };
    }
    match std::process::Command::new(binary).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let version = stdout
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().last())
                .unwrap_or("")
                .to_string();
            if version == crate::cli::BUILD_VERSION {
                Check {
                    label,
                    ok: true,
                    detail: format!("{binary} · {version}"),
                    hard: false,
                }
            } else {
                Check {
                    label,
                    ok: false,
                    detail: format!(
                        "{harness_id} hook binary is stateroot {version} — run `stateroot self-update` on this machine"
                    ),
                    hard: false,
                }
            }
        }
        _ => Check {
            label,
            ok: false,
            detail: format!("hook command not runnable: {binary}"),
            hard: false,
        },
    }
}

/// One check per DISTINCT hook binary found in installed hook configs (a
/// full install wires ~7 events at the same binary — one line, not seven).
fn hook_binary_checks(home: &Path) -> Vec<Check> {
    let probes = test_cmd_probes();
    let probe = stateroot_core::skill_federation::binary_probe(probes.as_deref());
    let mut checks = Vec::new();
    for quirk in registry::ADAPTERS {
        let Some(target) = quirk.hooks else {
            continue;
        };
        let mut binaries = std::collections::BTreeSet::new();
        for path in paths::hook_target_candidates(home, quirk) {
            let exists = if target.format == HookFormat::NativePlugin {
                path.is_dir()
            } else {
                path.is_file()
            };
            if !exists {
                continue;
            }
            for command in extract_hook_commands(&path, target.format) {
                if let Some(binary) = binary_of_command(&command) {
                    binaries.insert(binary);
                }
            }
        }
        for binary in binaries {
            checks.push(check_one_binary(quirk.id, &binary, &probe));
        }
    }
    checks
}

/// Continuity chain: not "is it installed" but "is anything flowing".
/// Per hooked harness — a duplicate-block lint on the managed hook config
/// (the 152-block kimi pile that silenced a session) and the last captured
/// checkpoint attributed to it. Plus the legacy outbox pile (queued for the
/// removed server sync, never delivered, previously invisible).
fn continuity_chain_checks(home: &Path, project_dir: &Path) -> Vec<Check> {
    let mut checks = Vec::new();

    // Legacy outbox: ops queued for the removed server sync, never drained.
    let outbox = stateroot_core::local_store::root(project_dir)
        .join(stateroot_core::local_store::OUTBOX_PATH);
    if let Ok(text) = std::fs::read_to_string(&outbox) {
        let pending = text.lines().filter(|l| !l.trim().is_empty()).count();
        if pending > 0 {
            checks.push(Check {
                label: "legacy outbox".into(),
                ok: false,
                detail: format!(
                    "{pending} op(s) queued for the removed server-sync — never delivered; safe to delete {}",
                    outbox.display()
                ),
                hard: false,
            });
        }
    }

    // Last captured checkpoint per harness (episodic carries a harness field).
    let mut last_by_harness: std::collections::BTreeMap<String, String> = Default::default();
    for rec in stateroot_core::local_store::recent_episodic(project_dir, 100) {
        let harness = rec
            .get("harness")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let ts = rec
            .get("ts")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !harness.is_empty() && !ts.is_empty() {
            last_by_harness.entry(harness).or_insert(ts);
        }
    }

    for quirk in registry::ADAPTERS {
        let Some(target) = quirk.hooks else {
            continue;
        };
        if !registry::quirk_detected(home, quirk) {
            continue;
        }
        let config = home.join(target.path);
        if !config.exists() {
            continue;
        }
        let mut ok = true;
        let mut detail: Vec<String> = Vec::new();
        if let Ok(text) = std::fs::read_to_string(&config) {
            let blocks = text.matches("stateroot hook ").count()
                + text.matches("stateroot.exe hook ").count();
            if blocks > quirk.event_map.len() {
                ok = false;
                detail.push(format!(
                    "{blocks} stateroot hook entries (> {} events — duplicates; run `stateroot install`)",
                    quirk.event_map.len()
                ));
            } else if blocks == 0 && target.format == HookFormat::TomlHooks {
                ok = false;
                detail.push("no stateroot hook blocks found".to_string());
            }
        }
        match last_by_harness.get(quirk.id) {
            Some(ts) => detail.push(format!("last captured {ts}")),
            None => detail.push("no checkpoints captured yet".into()),
        }
        checks.push(Check {
            label: format!("chain ({})", quirk.id),
            ok,
            detail: detail.join(" · "),
            hard: false,
        });
    }
    checks
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, body).expect("write");
    }

    #[test]
    fn extracts_hook_commands_from_every_config_shape() {
        let dir = tempfile::tempdir().expect("dir");

        // cursor FlatJson.
        let flat = dir.path().join(".cursor/hooks.json");
        write(
            &flat,
            &serde_json::to_string_pretty(&json!({
                "version": 1,
                "hooks": {
                    "sessionStart": [{"type": "command", "command": "/opt/tools/stateroot hook session_start --harness cursor", "matcher": ""}],
                    "stop": [{"type": "command", "command": "stateroot hook stop --harness cursor", "matcher": ""}]
                }
            }))
            .unwrap(),
        );
        let commands = extract_hook_commands(&flat, HookFormat::FlatJson);
        assert_eq!(commands.len(), 2);
        assert_eq!(
            binary_of_command(&commands[0]).as_deref(),
            Some("/opt/tools/stateroot")
        );
        assert_eq!(
            binary_of_command(&commands[1]).as_deref(),
            Some("stateroot")
        );

        // claude NestedJson (wrapped in `hooks`).
        let nested = dir.path().join(".claude/settings.json");
        write(
            &nested,
            &serde_json::to_string_pretty(&json!({
                "hooks": {
                    "SessionStart": [{"matcher": "", "hooks": [{"type": "command", "command": "stateroot hook session_start --harness claude-code"}]}]
                }
            }))
            .unwrap(),
        );
        let commands = extract_hook_commands(&nested, HookFormat::NestedJson);
        assert_eq!(commands.len(), 1);
        assert_eq!(
            binary_of_command(&commands[0]).as_deref(),
            Some("stateroot")
        );

        // kimi TomlHooks.
        let toml = dir.path().join(".kimi-code/config.toml");
        write(
            &toml,
            "[hooks]\ncommand = \"stateroot hook session_start --harness kimi-code\"\nevent = \"SessionStart\"\n",
        );
        let commands = extract_hook_commands(&toml, HookFormat::TomlHooks);
        assert_eq!(commands.len(), 1);
        assert_eq!(
            binary_of_command(&commands[0]).as_deref(),
            Some("stateroot")
        );

        // zero ZeroExecJson (command + args form).
        let zero = dir.path().join(".zero/hooks.json");
        write(
            &zero,
            &serde_json::to_string_pretty(&json!({
                "enabled": true,
                "hooks": [{"id": "stateroot-session_start", "command": "stateroot", "args": ["hook", "session_start", "--harness", "zero"], "enabled": true}]
            }))
            .unwrap(),
        );
        let commands = extract_hook_commands(&zero, HookFormat::ZeroExecJson);
        assert_eq!(commands, vec!["stateroot".to_string()]);

        // Windows-style absolute path (backslashes, .exe suffix).
        assert_eq!(
            binary_of_command(
                "C:\\Users\\u\\bin\\stateroot.exe hook session_start --harness cursor"
            )
            .as_deref(),
            Some("C:\\Users\\u\\bin\\stateroot.exe")
        );
        // Foreign commands never extract.
        assert_eq!(binary_of_command("eslint --fix ."), None);
    }

    #[cfg(unix)]
    fn stub_binary(dir: &Path, version: &str) -> std::path::PathBuf {
        let path = dir.join("stateroot");
        write(&path, &format!("#!/bin/sh\necho 'stateroot {version}'\n"));
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        path
    }

    #[cfg(unix)]
    #[test]
    fn hook_binary_grades_ok_stale_and_missing() {
        let dir = tempfile::tempdir().expect("dir");
        let probe = |_cmd: &str| true;

        // (a) current-version stub → ok.
        let current = stub_binary(dir.path(), crate::cli::BUILD_VERSION);
        let check = check_one_binary("cursor", &current.display().to_string(), &probe);
        assert!(check.ok, "{}", check.detail);
        assert!(
            check.detail.contains(crate::cli::BUILD_VERSION),
            "{}",
            check.detail
        );

        // (b) older-version stub → warning naming the version.
        let stale = stub_binary(dir.path(), "0.1.1");
        let check = check_one_binary("cursor", &stale.display().to_string(), &probe);
        assert!(!check.ok);
        assert!(
            check
                .detail
                .contains("cursor hook binary is stateroot 0.1.1"),
            "{}",
            check.detail
        );
        assert!(check.detail.contains("self-update"), "{}", check.detail);
        assert!(!check.hard, "stale hooks warn, they never hard-fail");

        // (c) missing binary → warning.
        let missing = dir.path().join("gone").display().to_string();
        let check = check_one_binary("cursor", &missing, &probe);
        assert!(!check.ok);
        assert!(
            check.detail.contains("hook command not runnable"),
            "{}",
            check.detail
        );

        // Bare `stateroot` with a negative probe → not-found warning.
        let check = check_one_binary("cursor", "stateroot", &|_cmd: &str| false);
        assert!(!check.ok);
        assert!(
            check.detail.contains("not found on PATH"),
            "{}",
            check.detail
        );
    }

    #[cfg(unix)]
    #[test]
    fn hook_binary_checks_walk_installed_configs() {
        let home = tempfile::tempdir().expect("home");
        let stale = stub_binary(home.path(), "0.1.1");
        let config = home.path().join(".cursor/hooks.json");
        write(
            &config,
            &serde_json::to_string_pretty(&json!({
                "version": 1,
                "hooks": {
                    "sessionStart": [{
                        "type": "command",
                        "command": format!("{} hook session_start --harness cursor", stale.display()),
                        "matcher": ""
                    }],
                    // Windows incident shape: absolute stateroot.exe path.
                    "stop": [{
                        "type": "command",
                        "command": "C:\\Tools\\stateroot.exe hook stop --harness cursor",
                        "matcher": ""
                    }]
                }
            }))
            .unwrap(),
        );
        let checks = hook_binary_checks(home.path());
        assert_eq!(checks.len(), 2, "checks: {checks:?}");
        assert!(!checks[0].ok);
        assert!(
            checks[0].detail.contains("stateroot 0.1.1"),
            "{}",
            checks[0].detail
        );
        // The .exe path extracted and graded as not runnable here.
        assert!(!checks[1].ok);
        assert!(
            checks[1].detail.contains("hook command not runnable"),
            "{}",
            checks[1].detail
        );
    }
}
