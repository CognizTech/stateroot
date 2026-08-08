//! `stateroot doctor` — local diagnostics only (config, store layout,
//! registry, hooks, federation). No server health checks exist in this
//! variant.

use stateroot_core::local_store;

use super::Ctx;

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
