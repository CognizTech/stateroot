//! `stateroot doctor` — local diagnostics only (config, store layout,
//! registry, hooks, federation). No server health checks exist in this
//! variant.

use std::path::Path;

use serde_json::Value;
use stateroot_core::harness_install::paths;
use stateroot_core::harness_install::registry::{self, HookFormat};
use stateroot_core::local_store;

use super::Ctx;

/// Budget for one hook binary's `--version` probe — a hung custom binary
/// must never block `stateroot doctor` (the row reports not-runnable).
const HOOK_VERSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, serde::Serialize)]
struct Check {
    label: String,
    ok: bool,
    detail: String,
    hard: bool,
    /// Recommended repair command, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    repair: Option<String>,
}

/// Run `stateroot doctor [--json]`. Returns a process exit code (0 ok, 1 hard failure).
pub async fn run(ctx: &Ctx, json_out: bool) -> anyhow::Result<i32> {
    let mut checks: Vec<Check> = Vec::new();

    // Config dir + file.
    checks.push(Check {
        label: "config dir".into(),
        ok: true,
        detail: ctx.config_dir.display().to_string(),
        hard: false,
        repair: None,
    });

    // Self-update crash journal (global config home, not the project store —
    // self-update runs outside any project).
    if let Some(check) = update_journal_check(&ctx.config_dir) {
        checks.push(check);
    }

    // Harness registry contract parses.
    match stateroot_core::skill_federation::load_registry() {
        Ok(reg) => checks.push(Check {
            label: "harness registry".into(),
            ok: true,
            detail: format!("{} harnesses", reg.harnesses.len()),
            hard: false,
            repair: None,
        }),
        Err(err) => checks.push(Check {
            label: "harness registry".into(),
            ok: false,
            detail: err,
            hard: true,
            repair: None,
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
            repair: None,
        });
        let handoff_path = root.join(local_store::HANDOFF_CURRENT_PATH);
        let handoff = handoff_path.is_file();
        let handoff_valid = !handoff || local_store::read_handoff_local(&ctx.cwd).is_ok();
        checks.push(Check {
            label: "current handoff".into(),
            ok: handoff_valid,
            detail: if handoff {
                if handoff_valid {
                    "present".into()
                } else {
                    "unreadable — run `stateroot handoff repair`".into()
                }
            } else {
                "none yet".into()
            },
            hard: false,
            repair: (!handoff_valid).then(|| "stateroot handoff repair".to_string()),
        });
        let auto_skip = root.join("local/automatic-snapshot-skip.json");
        if let Ok(text) = std::fs::read_to_string(&auto_skip) {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                let at = value.get("at").and_then(Value::as_str).unwrap_or("");
                let detail = value
                    .get("detail")
                    .and_then(Value::as_str)
                    .unwrap_or("last automatic snapshot exceeded its scan budget");
                checks.push(Check {
                    label: "automatic snapshot".into(),
                    ok: false,
                    detail: format!("{detail} (skipped at {at}; clears after the next successful automatic snapshot)"),
                    hard: false,
                    repair: None,
                });
            }
        }
        // Common generated directories that are large and NOT excluded are
        // the usual cause of a blown automatic-snapshot budget. Entries AND
        // bytes are summed with a hard cap so doctor stays fast on huge
        // trees; one level of nesting is checked because monorepo layouts
        // hide these dirs below the root (server/.venv, app/node_modules).
        let rules = stateroot_core::sync_engine::ignore::IgnoreRules::load(&ctx.cwd);
        let mut candidates: Vec<String> = Vec::new();
        for name in [
            "node_modules",
            ".venv",
            "dist",
            "target",
            "build",
            "__pycache__",
        ] {
            candidates.push(name.to_string());
            if let Ok(top) = std::fs::read_dir(&ctx.cwd) {
                for entry in top.flatten() {
                    let entry_name = entry.file_name().to_string_lossy().into_owned();
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
                        && !entry_name.starts_with('.')
                    {
                        candidates.push(format!("{entry_name}/{name}"));
                    }
                }
            }
        }
        for name in candidates {
            let dir = ctx.cwd.join(&name);
            if !dir.is_dir() || rules.is_ignored(&name, true) {
                continue;
            }
            let mut entries = 0u64;
            let mut bytes = 0u64;
            let mut stack = vec![dir];
            let capped = 'scan: loop {
                let Some(dir) = stack.pop() else { break false };
                let Ok(rd) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in rd.flatten() {
                    entries += 1;
                    if let Ok(meta) = entry.metadata() {
                        if meta.is_file() {
                            bytes += meta.len();
                        }
                    }
                    if entries >= 2_000 {
                        break 'scan true;
                    }
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        stack.push(entry.path());
                    }
                }
            };
            if entries >= 200 || bytes >= 32 * 1024 * 1024 {
                let size = if bytes >= 1024 * 1024 {
                    format!("{} MiB", bytes / (1024 * 1024))
                } else {
                    format!("{} KiB", bytes / 1024)
                };
                checks.push(Check {
                    label: "generated directory".into(),
                    ok: false,
                    detail: format!(
                        "{name}/ holds {size}{} and is not ignored — add `{name}/` to the root .gitignore or .staterootignore so automatic snapshots stay bounded",
                        if capped { "+ (capped)".into() } else { String::new() }
                    ),
                    hard: false,
                    repair: None,
                });
            }
        }
        // DrvFs-mounted working copies (WSL `/mnt/<drive>`) have coarse stat
        // semantics: scans cost more and mtime races are likelier.
        if cfg!(target_os = "linux") {
            let on_mount = ctx
                .cwd
                .components()
                .take(2)
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                == ["/", "mnt"];
            if on_mount && std::env::var_os("WSL_DISTRO_NAME").is_some() {
                checks.push(Check {
                    label: "filesystem".into(),
                    ok: true,
                    detail: "WSL-mounted working copy — automatic snapshots run with bounded scans; keep generated trees ignored".into(),
                    hard: false,
                    repair: None,
                });
            }
        }
    } else {
        checks.push(Check {
            label: "project".into(),
            ok: true,
            detail: "not in a stateroot project (init to create one)".into(),
            hard: false,
            repair: None,
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
        repair: None,
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
                repair: None,
            });
            if quirk.id == "pi" {
                checks.push(Check {
                    label: "Pi skill isolation".into(),
                    ok: true,
                    detail: "StateRoot launches use `stateroot harness run pi` with ambient .agents skill discovery disabled; pass --ambient-skills to opt in".into(),
                    hard: false,
                    repair: None,
                });
            }
        }
        if !any {
            checks.push(Check {
                label: "identity delivery".into(),
                ok: true,
                detail: "no harnesses detected on this machine".into(),
                hard: false,
                repair: None,
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
                    repair: None,
                });
                for issue in issues {
                    if !json_out {
                        println!("  {issue}");
                    }
                }
            }
            Err(err) => checks.push(Check {
                label: "skill federation".into(),
                ok: false,
                detail: err,
                hard: false,
                repair: None,
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
            repair: None,
        });
        match stateroot_core::rules::ensure_product_intent(&home) {
            Ok(_) => {
                let n = stateroot_core::rules::list_all(&ctx.cwd, &home).len();
                checks.push(Check {
                    label: "shared rules".into(),
                    ok: true,
                    detail: format!("{n} rule(s); product-intent always on"),
                    hard: false,
                    repair: None,
                });
            }
            Err(err) => checks.push(Check {
                label: "shared rules".into(),
                ok: false,
                detail: err.to_string(),
                hard: false,
                repair: None,
            }),
        }
        // Continuity chain: not "is it installed" but "is anything flowing"
        // — duplicate managed blocks, last captured checkpoint per harness,
        // and the legacy outbox pile.
        checks.extend(continuity_chain_checks(&home, &ctx.cwd));
        // Active continuity runtime: service registration/liveness,
        // projection freshness, corrupt obligation events, and unresolved
        // lifecycle contradictions.
        checks.extend(continuity_runtime_checks(ctx));
    }

    for (label, ok, detail) in super::editor_extensions::doctor_checks(ctx).await {
        checks.push(Check {
            label,
            ok,
            detail,
            hard: false,
            repair: None,
        });
    }

    // C1: the typed per-harness integration health document (also the WS4
    // seam). Best-effort: home resolution failure never fails doctor.
    let integrations = super::install::home_dir().ok().map(|home| {
        let probes = test_cmd_probes();
        let probe = stateroot_core::skill_federation::binary_probe(probes.as_deref());
        let project = local_store::is_stateroot_dir(&ctx.cwd).then(|| ctx.cwd.clone());
        stateroot_core::harness_install::health::integration_health(
            &home,
            project.as_deref(),
            &probe,
            &ctx.config.installed_harnesses,
        )
    });

    let mut hard_failures = 0;
    let mut soft_warnings = 0;
    for check in &checks {
        if !check.ok {
            if check.hard {
                hard_failures += 1;
            } else {
                soft_warnings += 1;
            }
        }
    }
    if json_out {
        // Base checks (config/store/registry/hooks/...) and detected
        // integration readiness are SEPARATE verdicts: `ok` covers base hard
        // failures only; integration rows are a readiness report, and a
        // missing-but-undetected harness is never a hard failure.
        let integrations_summary = integrations.as_ref().map(|health| {
            let (working, configured, missing, unknown) = health.counts();
            serde_json::json!({
                "summary": health.summary_line(),
                "observed_working": working,
                "configured": configured,
                "missing": missing,
                "unknown": unknown,
                // Degraded = degraded identity-delivery tier (registry
                // policy). Rows with open problems are listed separately —
                // the two never blur.
                "degraded": health
                    .harnesses
                    .iter()
                    .filter(|row| row.degraded.is_some())
                    .map(|row| row.harness.clone())
                    .collect::<Vec<_>>(),
                "with_problems": health
                    .harnesses
                    .iter()
                    .filter(|row| !row.problems.is_empty())
                    .map(|row| row.harness.clone())
                    .collect::<Vec<_>>(),
                "evidence_problems": health.evidence_problems,
            })
        });
        let payload = serde_json::json!({
            "schema_version": "stateroot.doctor.v1",
            "generated_at": local_store::now_rfc3339(),
            "ok": hard_failures == 0,
            "hard_failures": hard_failures,
            "warnings": soft_warnings,
            "checks": checks,
            "integrations": integrations,
            "integrations_summary": integrations_summary,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(if hard_failures > 0 { 1 } else { 0 });
    }
    for check in &checks {
        let mark = if check.ok { "ok" } else { "!!" };
        print!("  [{mark}] {} — {}", check.label, check.detail);
        if let Some(repair) = &check.repair {
            print!(" → `{repair}`");
        }
        println!();
    }
    if let Some(integrations) = &integrations {
        if !integrations.harnesses.is_empty() {
            println!("integrations: {}", integrations.summary_line());
            for row in integrations
                .harnesses
                .iter()
                .filter(|r| !r.problems.is_empty())
            {
                println!(
                    "  {}: {} → {}",
                    row.harness,
                    row.problems.join("; "),
                    row.repair.join(", ")
                );
            }
        }
    }
    if hard_failures > 0 {
        println!("base checks: {hard_failures} hard failure(s), {soft_warnings} warning(s)");
        Ok(1)
    } else if soft_warnings > 0 {
        println!("doctor: base checks pass with {soft_warnings} warning(s)");
        Ok(0)
    } else {
        println!("doctor: all local checks pass");
        Ok(0)
    }
}

/// Active-continuity runtime checks (all soft): the resident service, the
/// machine-local projection, obligation-event integrity, and lifecycle
/// contradictions the assessment already derived.
fn continuity_runtime_checks(ctx: &Ctx) -> Vec<Check> {
    let mut out = Vec::new();
    let cfg = &ctx.config.continuity;
    if !cfg.enabled {
        out.push(Check {
            label: "continuity service".into(),
            ok: true,
            detail: "disabled in config ([continuity] enabled = false)".into(),
            hard: false,
            repair: None,
        });
        return out;
    }
    let registration = stateroot_core::continuity::read_service_registration(&ctx.config_dir);
    let heartbeat = stateroot_core::continuity::read_service_heartbeat(&ctx.config_dir);
    // Verified liveness only: a fresh beat whose pid fails identity
    // verification (wrong binary/args/namespace/start token/config) is NOT
    // a running service — it reads as degraded, never claimed live.
    let running = super::service::service_live_for(&ctx.config_dir, cfg.poll_interval_seconds).0;
    let stale_detail = match &heartbeat {
        Some(beat) => format!("last beat {} (pid {})", beat.beat_at, beat.pid),
        None => "no heartbeat recorded".to_string(),
    };
    match &registration {
        Some(reg) => out.push(Check {
            label: "continuity service".into(),
            ok: running,
            detail: if running {
                format!("registered ({}), heartbeating", reg.kind)
            } else {
                format!(
                    "registered ({}) but not heartbeating — {stale_detail}",
                    reg.kind
                )
            },
            hard: false,
            repair: (!running).then(|| "stateroot service restart".to_string()),
        }),
        None => out.push(Check {
            label: "continuity service".into(),
            ok: false,
            detail:
                "not registered — degraded background coverage (hooks/CLI reconcile on activity)"
                    .into(),
            hard: false,
            repair: Some("stateroot service install".to_string()),
        }),
    }

    // Projection freshness (this project's machine-local projection).
    if stateroot_core::local_store::is_stateroot_dir(&ctx.cwd) {
        match stateroot_core::continuity::read_projection(&ctx.cwd) {
            Some(assessment) => {
                let fresh = !stateroot_core::continuity::service_beat_stale(
                    &assessment.generated_at,
                    &stateroot_core::local_store::now_rfc3339(),
                    cfg.poll_interval_seconds,
                );
                out.push(Check {
                    label: "continuity projection".into(),
                    ok: fresh,
                    detail: format!(
                        "generated {} · {} attention item(s)",
                        assessment.generated_at,
                        assessment.attention.len()
                    ),
                    hard: false,
                    repair: None,
                });
                out.push(Check {
                    label: "obligation events".into(),
                    ok: assessment.corrupt_obligation_events == 0,
                    detail: format!(
                        "{} corrupt event line(s) preserved in obligations/events.jsonl",
                        assessment.corrupt_obligation_events
                    ),
                    hard: false,
                    repair: None,
                });
                let contradictions = assessment
                    .attention
                    .iter()
                    .filter(|item| {
                        matches!(
                            item.kind.as_str(),
                            "plan_closure" | "plan_receipt_pending" | "handoff_stale"
                        )
                    })
                    .count();
                out.push(Check {
                    label: "lifecycle contradictions".into(),
                    ok: contradictions == 0,
                    detail: if contradictions == 0 {
                        "none unresolved".into()
                    } else {
                        format!("{contradictions} unresolved (see `stateroot status`)")
                    },
                    hard: false,
                    repair: None,
                });
            }
            None => out.push(Check {
                label: "continuity projection".into(),
                ok: false,
                detail: "no projection yet — run `stateroot status` or `stateroot service run`"
                    .into(),
                hard: false,
                repair: None,
            }),
        }
    }
    out
}

/// Hidden test seam (mirrors `STATEROOT_TEST_HOME`): when
/// `STATEROOT_TEST_CMD_PROBES` is set, bare-binary detection answers from
/// this comma-separated allowlist instead of probing the host PATH.
pub(crate) fn test_cmd_probes() -> Option<Vec<String>> {
    std::env::var("STATEROOT_TEST_CMD_PROBES").ok().map(|raw| {
        raw.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// Every stateroot hook command found in `path` — the shared core
/// implementation lives in the integration-health module so doctor and the
/// health seam read configs identically.
use stateroot_core::harness_install::health::{binary_of_command, extract_hook_commands};

/// Run one hook binary's `--version` and grade it against this cli.
/// The probe is BOUNDED: a hung custom binary must never block doctor.
fn check_one_binary(harness_id: &str, binary: &str, probe: &dyn Fn(&str) -> bool) -> Check {
    let label = format!("hook binary ({harness_id})");
    if binary == "stateroot" && !probe("stateroot") {
        return Check {
            label,
            ok: false,
            detail: "hook command `stateroot` not found on PATH".into(),
            hard: false,
            repair: None,
        };
    }
    if binary != "stateroot" && !Path::new(binary).is_file() {
        return Check {
            label,
            ok: false,
            detail: format!("hook command not runnable: {binary}"),
            hard: false,
            repair: None,
        };
    }
    match super::bounded_run(binary, &["--version"], HOOK_VERSION_PROBE_TIMEOUT, true) {
        Some(run) if run.success => {
            let stdout = run.stdout;
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
                    repair: None,
                }
            } else {
                Check {
                    label,
                    ok: false,
                    detail: format!(
                        "{harness_id} hook binary is stateroot {version} — run `stateroot self-update` on this machine"
                    ),
                    hard: false,
                    repair: Some("stateroot self-update".to_string()),
                }
            }
        }
        _ => Check {
            label,
            ok: false,
            detail: format!("hook command not runnable: {binary}"),
            hard: false,
            repair: None,
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

/// Self-update crash journal: the updater writes `<config>/update-journal.json`
/// before parking the old binary and clears it only after the new binary's
/// `--version` readback confirms the target. A leftover in-progress journal
/// means the process died mid-swap — surface it (a warning, never a gate).
fn update_journal_check(config_dir: &Path) -> Option<Check> {
    let path = super::update::update_journal_path(config_dir);
    let text = std::fs::read_to_string(&path).ok()?;
    let label = "self-update".to_string();
    let Ok(journal) = serde_json::from_str::<Value>(&text) else {
        return Some(Check {
            label,
            ok: false,
            detail: format!("update journal is unreadable — delete {}", path.display()),
            hard: false,
            repair: None,
        });
    };
    let from = journal
        .get("from_version")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let to = journal
        .get("to_version")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let at = journal
        .get("started_at")
        .and_then(Value::as_str)
        .unwrap_or("?");
    // A completed rollback is a recovered failure, not an interruption — the
    // record stays visible without crying wolf.
    if journal.get("status").and_then(Value::as_str) == Some("rolled_back") {
        return Some(Check {
            label,
            ok: true,
            detail: format!(
                "last update (from {from} to {to} at {at}) failed; previous binary restored"
            ),
            hard: false,
            repair: None,
        });
    }
    Some(Check {
        label,
        ok: false,
        detail: format!(
            "update interrupted (from {from} to {to} at {at}) — rerun `stateroot self-update`"
        ),
        hard: false,
        repair: Some("stateroot self-update".to_string()),
    })
}

/// Recursive directory size in bytes (best-effort; unreadable entries
/// contribute zero).
fn dir_size(path: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let p = entry.path();
            if p.is_dir() {
                dir_size(&p)
            } else {
                entry.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

/// Human-readable byte size (`512 B`, `12.4 KB`, `2.3 MB`, `1.1 GB`).
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Continuity chain: not "is it installed" but "is anything flowing".
/// Per hooked harness — a duplicate-block lint on the managed hook config
/// (the 152-block kimi pile that silenced a session) and the last captured
/// checkpoint attributed to it. Plus the legacy outbox pile (queued for the
/// removed server sync, never delivered, previously invisible).
fn continuity_chain_checks(home: &Path, project_dir: &Path) -> Vec<Check> {
    let mut checks = Vec::new();

    // Store footprint: growth you can see never becomes a surprise (sizes
    // only, never content). The episodic journal is append-only by design;
    // this line is how an operator watches it.
    let root = stateroot_core::local_store::root(project_dir);
    if root.is_dir() {
        let total = dir_size(&root);
        let episodic = std::fs::metadata(root.join("memories/episodic.jsonl"))
            .map(|m| m.len())
            .unwrap_or(0);
        let local = dir_size(&root.join("local"));
        let spool = dir_size(&root.join("spool"));
        checks.push(Check {
            label: "store footprint".into(),
            ok: true,
            detail: format!(
                "{} total · episodic {} · search {} · spool {}",
                human_size(total),
                human_size(episodic),
                human_size(local),
                human_size(spool)
            ),
            hard: false,
            repair: None,
        });
    }

    // Finalize outbox: snap/finalize/ingest queued by stop/session_end.
    // Leftover ops without an ingest_key are the pre-revival server-sync
    // queue and can be deleted.
    let outbox = stateroot_core::local_store::root(project_dir)
        .join(stateroot_core::local_store::OUTBOX_PATH);
    if let Ok(text) = std::fs::read_to_string(&outbox) {
        let mut finalize = 0usize;
        let mut legacy = 0usize;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(op)
                    if op.get("ingest_key").and_then(|v| v.as_str()).is_some()
                        && stateroot_core::local_store::FINALIZE_KINDS
                            .contains(&op.get("kind").and_then(|v| v.as_str()).unwrap_or("")) =>
                {
                    finalize += 1;
                }
                _ => legacy += 1,
            }
        }
        if finalize > 0 {
            checks.push(Check {
                label: "finalize outbox".into(),
                ok: true,
                detail: format!(
                    "{finalize} op(s) queued for `_drain-finalize` (snap/finalize/ingest)"
                ),
                hard: false,
                repair: None,
            });
        }
        if legacy > 0 {
            checks.push(Check {
                label: "legacy outbox".into(),
                ok: false,
                detail: format!(
                    "{legacy} op(s) queued for the removed server-sync — never delivered; safe to delete {}",
                    outbox.display()
                ),
                hard: false,
                repair: None,
            });
        }
    }

    // Session-boundary journal (repair Phase 3): state-grouped report with
    // retained errors, and a recovery kick (doctor is a safe entrypoint).
    super::drain_finalize::kick(project_dir);
    let journal_lines = super::drain_finalize::report(project_dir);
    if !journal_lines.iter().all(|l| l.contains("no boundary jobs")) {
        checks.push(Check {
            label: "boundary journal".into(),
            ok: !journal_lines.iter().any(|l| l.contains("manual_attention")),
            detail: journal_lines.join("\n"),
            hard: false,
            repair: None,
        });
    }

    // Collaboration boundary: machine-local / per-person paths must not be
    // git-tracked — they churn on every session and fight on every pull.
    if let Ok(output) = std::process::Command::new("git")
        .args(["ls-files", "--", ".stateroot"])
        .current_dir(project_dir)
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let tracked: Vec<&str> = text
                .lines()
                .map(|line| line.trim_start_matches(".stateroot/"))
                .filter(|rel| {
                    stateroot_core::local_store::COLLAB_LOCAL_PATHS
                        .iter()
                        .any(|p| {
                            let p = *p;
                            (p.ends_with('/') && rel.starts_with(p)) || *rel == p
                        })
                })
                .collect();
            if !tracked.is_empty() {
                checks.push(Check {
                    label: "collab boundary".into(),
                    ok: false,
                    detail: format!(
                        "{} machine-local/per-person path(s) tracked in git ({}…) — `git rm --cached` them; `.stateroot/.gitignore` covers the rest",
                        tracked.len(),
                        tracked.first().copied().unwrap_or("")
                    ),
                    hard: false,
                    repair: None,
                });
            }
        }
    }

    // Last durable capture per harness — the WS1 observation store only
    // (never the authored episodic journal), with unreadable evidence
    // surfaced as diagnosed, distinct from absent.
    let trail = stateroot_core::observations::capture_trail(project_dir);
    let last_by_harness = &trail.last_by_harness;

    for quirk in registry::ADAPTERS {
        let Some(target) = quirk.hooks else {
            continue;
        };
        if !registry::quirk_detected(home, quirk) {
            continue;
        }
        let config = paths::hook_target_candidates(home, quirk)
            .into_iter()
            .find(|path| {
                if target.format == HookFormat::NativePlugin {
                    path.is_dir()
                } else {
                    path.is_file()
                }
            });
        let Some(config) = config else {
            continue;
        };
        let mut ok = true;
        let mut detail: Vec<String> = Vec::new();
        // Count registrations with the SAME parser health/doctor use — real
        // TOML/JSON decoding, never raw text matching.
        let commands =
            stateroot_core::harness_install::health::extract_hook_commands(&config, target.format);
        let blocks = commands.len();
        if blocks > quirk.event_map.len() && !quirk.event_map.is_empty() {
            ok = false;
            detail.push(format!(
                "{blocks} stateroot hook entries (> {} events — duplicates; run `stateroot install`)",
                quirk.event_map.len()
            ));
        } else if blocks == 0 && target.format == HookFormat::TomlHooks {
            ok = false;
            detail.push("no stateroot hook blocks found".to_string());
        }
        match last_by_harness.get(quirk.id) {
            Some(ts) => detail.push(format!("last captured {ts}")),
            None => detail.push("no checkpoints captured yet".into()),
        }
        for diagnosed in &trail.diagnosed {
            if diagnosed
                .strip_prefix("spool/segments/")
                .and_then(|rest| rest.split("__").next())
                == Some(quirk.id)
            {
                detail.push(format!("capture evidence diagnosed: {diagnosed}"));
            }
        }
        checks.push(Check {
            label: format!("chain ({})", quirk.id),
            ok,
            detail: detail.join(" · "),
            hard: false,
            repair: (!ok).then(|| "stateroot install".to_string()),
        });
    }
    checks
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn human_size_formats_units() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(12_800), "12.5 KB");
        assert_eq!(human_size(2_300_000), "2.2 MB");
        assert_eq!(human_size(1_610_612_736), "1.5 GB");
    }

    #[test]
    fn dir_size_counts_recursively() {
        let tmp = tempfile::tempdir().expect("tmp");
        write(&tmp.path().join("a/b/c.txt"), "1234");
        write(&tmp.path().join("d.txt"), "12");
        assert_eq!(dir_size(tmp.path()), 6);
        assert_eq!(dir_size(&tmp.path().join("missing")), 0);
    }

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
            "[[hooks]]\ncommand = \"stateroot hook session_start --harness kimi-code\"\nevent = \"SessionStart\"\n",
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

    #[test]
    fn update_journal_check_grades_interrupted_rolled_back_and_unreadable() {
        let dir = tempfile::tempdir().expect("dir");
        assert!(
            update_journal_check(dir.path()).is_none(),
            "no journal, no check"
        );

        let journal = |status: &str| {
            let mut entry = json!({
                "from_version": "0.1.9",
                "to_version": "v0.2.0",
                "started_at": "2026-09-29T01:02:03Z",
            });
            if !status.is_empty() {
                entry["status"] = json!(status);
            }
            std::fs::write(
                dir.path().join("update-journal.json"),
                serde_json::to_string_pretty(&entry).expect("json"),
            )
            .expect("journal");
        };

        // Interrupted (explicit in_progress, and a status-less legacy journal).
        for status in ["in_progress", ""] {
            journal(status);
            let check = update_journal_check(dir.path()).expect("check");
            assert!(!check.ok, "status {status:?}");
            assert!(!check.hard, "an interrupted update warns, never hard-fails");
            assert!(
                check
                    .detail
                    .contains("update interrupted (from 0.1.9 to v0.2.0 at 2026-09-29T01:02:03Z)"),
                "{}",
                check.detail
            );
            assert!(
                check.detail.contains("stateroot self-update"),
                "{}",
                check.detail
            );
        }

        // Rolled back: informational, never the crash warning.
        journal("rolled_back");
        let check = update_journal_check(dir.path()).expect("check");
        assert!(check.ok, "{}", check.detail);
        assert!(!check.detail.contains("interrupted"), "{}", check.detail);
        assert!(
            check.detail.contains("previous binary restored"),
            "{}",
            check.detail
        );

        // Unreadable journal: warn, don't crash.
        std::fs::write(dir.path().join("update-journal.json"), b"not json").expect("journal");
        let check = update_journal_check(dir.path()).expect("check");
        assert!(!check.ok);
        assert!(!check.hard);
        assert!(check.detail.contains("unreadable"), "{}", check.detail);
    }
}
