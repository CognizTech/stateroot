//! `harnesses` section — checklist over *universally* detected harnesses
//! (binary probe + config markers, see `harness_install::detect`), an
//! explicit finalize gate before any write, then the same `install_spec`
//! machinery as `stateroot install`.
//!
//! Picker rows show their evidence: a binary-backed harness reads
//! `claude (… · binary)`, a config-dir leftover reads
//! `gemini (… · config only, binary not found)` and starts UNCHECKED — a
//! `~/.gemini` folder without the gemini binary must not auto-select.
//!
//! Checklist keys (dialoguer MultiSelect builtins): `space` toggles one row,
//! `a` toggles all/none, `enter` confirms. Non-interactive runs (`--yes`,
//! `--config`, no tty) accept the defaults — binary rows only — and skip the
//! finalize gate; scripted keys: `harnesses.picked`, `harnesses.finalize`,
//! `harnesses.finalize.retry`.

use anyhow::Result;
use async_trait::async_trait;
use stateroot_core::harness_install::detect::{detect_harnesses, Detection, Prober, SystemProber};
use stateroot_core::harness_install::registry::{self, HarnessQuirk};

use super::super::install::{
    all_specs, install_spec, render_one_agent_block, HarnessSpec, InstallToggles,
};
use super::{Depth, Prompter, WizardCtx, WizardSection};

/// Hooks column in the harnesses picker: native (Tier A), plugin (Tier B), none.
enum HooksKind {
    Native,
    Plugin,
    None,
}

fn hooks_kind_for(quirk: &HarnessQuirk) -> HooksKind {
    if quirk.hooks.is_some() {
        HooksKind::Native
    } else if quirk.tier == registry::Tier::B {
        HooksKind::Plugin
    } else {
        HooksKind::None
    }
}

/// One picker row: a registry quirk plus its detection evidence. `spec` is
/// the legacy install spec for rows that have one (the legacy 7); all other
/// rows install via `install_quirk_full`.
struct PickRow {
    quirk: &'static HarnessQuirk,
    detection: Detection,
    spec: Option<HarnessSpec>,
}

/// Harness integration section. The prober is injectable for tests.
pub struct HarnessesSection {
    prober: Box<dyn Prober + Send + Sync>,
}

impl Default for HarnessesSection {
    fn default() -> Self {
        Self {
            prober: Box::new(SystemProber),
        }
    }
}

impl HarnessesSection {
    /// Picker rows: EVERY registry row detection found in ANY form (binary
    /// or config leftover), in registry order — not just the legacy table.
    fn pick_rows(&self, ctx: &WizardCtx) -> Vec<PickRow> {
        let detections = detect_harnesses(&ctx.home, self.prober.as_ref());
        let specs = all_specs(&ctx.home);
        let mut rows = Vec::new();
        for quirk in registry::adapters() {
            let canonical = stateroot_core::skill_federation::normalize_harness(quirk.id);
            let Some(detection) = detections.iter().find(|d| d.id == canonical) else {
                continue;
            };
            if !detection.installed() {
                continue;
            }
            let spec = quirk
                .legacy_id
                .and_then(|legacy| specs.iter().find(|s| s.id == legacy))
                .map(|s| HarnessSpec {
                    id: s.id,
                    instruction_file: s.instruction_file.clone(),
                    mcp_files: s.mcp_files.clone(),
                    claude_extras: s.claude_extras,
                    guidance: s.guidance,
                });
            rows.push(PickRow {
                quirk,
                detection: detection.clone(),
                spec,
            });
        }
        rows
    }
}

fn row_label(row: &PickRow) -> String {
    let quirk = row.quirk;
    let mut parts = Vec::new();
    if quirk.instruction_file.is_some() {
        parts.push("block");
    }
    if quirk.mcp.is_some() {
        parts.push("mcp");
    }
    if quirk.id == "claude-code" {
        parts.push("skill");
    }
    parts.push(match hooks_kind_for(quirk) {
        HooksKind::Native => "hooks:native",
        HooksKind::Plugin => "hooks:plugin",
        HooksKind::None => "hooks:none",
    });
    format!(
        "{} ({} · {})",
        quirk.id,
        parts.join("+"),
        row.detection.evidence_label()
    )
}

/// Print the finalize plan: exactly what will be written per picked harness.
fn print_write_plan(home: &std::path::Path, rows: &[&PickRow]) {
    println!();
    println!("Planned writes ({} harness(es)):", rows.len());
    for row in rows {
        let quirk = row.quirk;
        let mut writes: Vec<String> = Vec::new();
        if let Some(rel) = quirk.instruction_file {
            writes.push(format!("instruction block → {}", home.join(rel).display()));
        }
        if let Some(target) = quirk.mcp {
            writes.push(format!(
                "MCP registration → {}",
                home.join(target.path).display()
            ));
        }
        if quirk.id == "claude-code" {
            writes.push("skill copy + slash stub".to_string());
        }
        if let Some(target) = quirk.hooks {
            writes.push(format!("lifecycle hooks → {}", target.path));
        } else {
            match quirk.id {
                "hermes" => writes.push(
                    "no hook files yet — stateroot hermes plugin planned (resume via MCP bridge)"
                        .to_string(),
                ),
                "crush" => writes.push("managed — no files".to_string()),
                _ if quirk.tier == registry::Tier::B
                    && stateroot_core::harness_install::plugins::ts_plugin_path(quirk)
                        .is_some() =>
                {
                    writes.push("generated hooks plugin".to_string())
                }
                _ if quirk.hooks.is_none() && quirk.mcp.is_none() => {
                    writes.push("managed — no files yet".to_string())
                }
                _ => {}
            }
        }
        for write in &writes {
            println!("  {}: {write}", quirk.id);
        }
    }
}

#[async_trait]
impl WizardSection for HarnessesSection {
    fn id(&self) -> &'static str {
        "harnesses"
    }

    fn title(&self) -> &'static str {
        "Harness integration (global blocks + MCP)"
    }

    async fn is_configured(&self, ctx: &WizardCtx) -> Result<bool> {
        Ok(!ctx.core.config.installed_harnesses.is_empty())
    }

    async fn run(&self, ctx: &mut WizardCtx, prompter: &mut dyn Prompter) -> Result<Vec<String>> {
        let rows = self.pick_rows(ctx);
        if rows.is_empty() {
            return Ok(vec!["no harnesses detected — nothing to do".to_string()]);
        }

        let labels: Vec<String> = rows.iter().map(row_label).collect();
        // Pre-check binary-backed rows only; config leftovers start unchecked.
        let preselect: Vec<bool> = match ctx.depth {
            Depth::BlankSlate => vec![false; rows.len()],
            _ => rows.iter().map(|r| r.detection.binary_found).collect(),
        };
        println!("space: toggle · a: all/none · enter: confirm");

        // Checklist + explicit finalize gate. Interactive runs get one retry
        // after a decline; non-interactive runs accept the selection (the
        // scripted/default answers ARE the confirmation).
        let mut attempts = 0usize;
        let picked = loop {
            attempts += 1;
            let picked = prompter
                .multi_select(
                    "harnesses.picked",
                    "Integrate into which harnesses?",
                    &labels,
                    &preselect,
                )
                .await?;
            if picked.is_empty() {
                return Ok(vec!["no harnesses selected".to_string()]);
            }
            let chosen: Vec<&PickRow> = picked.iter().map(|&i| &rows[i]).collect();
            print_write_plan(&ctx.home, &chosen);
            if ctx.non_interactive {
                break picked;
            }
            let key = if attempts == 1 {
                "harnesses.finalize"
            } else {
                "harnesses.finalize.retry"
            };
            let proceed = prompter
                .confirm(key, "Proceed with these writes?", true)
                .await?;
            if proceed {
                break picked;
            }
            if attempts >= 2 {
                return Ok(vec!["declined — no writes performed".to_string()]);
            }
            println!("Back to the checklist…");
        };

        let persona = super::super::persona::read_cache(&ctx.core.config_dir);

        let mut actions: Vec<String> = Vec::new();
        let mut installed: Vec<String> = Vec::new();
        for idx in picked {
            let row = &rows[idx];
            // Selected harnesses install with all components on — no
            // per-component interrogation (user direction: "once a harness is
            // selected, just go ahead and install").
            if let Some(spec) = &row.spec {
                let toggles = InstallToggles::default();
                if ctx.dry_run {
                    actions.push(format!(
                        "would install {} (block={}, mcp={}, extras={}, hooks={})",
                        spec.id, toggles.block, toggles.mcp, toggles.extras, toggles.hooks
                    ));
                } else {
                    // Wave-2: harness-native soul presentation per registry.
                    let persona_h =
                        super::super::persona::for_harness(&ctx.core, spec.id, persona.as_deref())
                            .await;
                    let block = render_one_agent_block(persona_h.as_deref(), spec.id);
                    for action in install_spec(&ctx.home, spec, &block, toggles) {
                        actions.push(format!("{}: {action}", spec.id));
                    }
                }
                installed.push(spec.id.to_string());
            } else {
                // Non-legacy registry row: instruction block + MCP + tier
                // installer in one shot.
                if ctx.dry_run {
                    actions.push(format!(
                        "would install {} (registry tier {:?})",
                        row.quirk.id, row.quirk.tier
                    ));
                } else {
                    let persona_h = super::super::persona::for_harness(
                        &ctx.core,
                        row.quirk.id,
                        persona.as_deref(),
                    )
                    .await;
                    let block = render_one_agent_block(persona_h.as_deref(), row.quirk.id);
                    for action in stateroot_core::harness_install::install_quirk_full(
                        &ctx.home, row.quirk, &block,
                    ) {
                        actions.push(format!("{}: {action}", row.quirk.id));
                    }
                }
                installed.push(row.quirk.id.to_string());
            }
        }

        if !ctx.dry_run {
            let mut config = ctx.core.config.clone();
            config.installed_harnesses = installed.clone();
            stateroot_core::config::save_config(&ctx.core.config_dir, &config)?;
            actions.push(format!(
                "recorded installed_harnesses: {}",
                installed.join(", ")
            ));
        } else {
            actions.push(format!(
                "would record installed_harnesses: {}",
                installed.join(", ")
            ));
        }
        Ok(actions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::setup::{Answer, ScriptedPrompter};
    use crate::commands::Ctx;
    use stateroot_core::config::AppConfig;
    use std::collections::{HashMap, HashSet};

    struct StubProber {
        present: HashSet<String>,
    }

    impl Prober for StubProber {
        fn probe(&self, cmd: &str) -> bool {
            self.present.contains(cmd)
        }
    }

    fn section_with(cmds: &[&str]) -> HarnessesSection {
        HarnessesSection {
            prober: Box::new(StubProber {
                present: cmds.iter().map(|c| c.to_string()).collect(),
            }),
        }
    }

    fn wizard_ctx(
        cwd: &std::path::Path,
        config_dir: &std::path::Path,
        home: &std::path::Path,
        non_interactive: bool,
    ) -> WizardCtx {
        WizardCtx {
            core: Ctx {
                cwd: cwd.to_path_buf(),
                config_dir: config_dir.to_path_buf(),
                config: AppConfig::default(),
            },
            home: home.to_path_buf(),
            dry_run: false,
            depth: Depth::Quick,
            non_interactive,
            recap: Vec::new(),
        }
    }

    /// Seed `.codex` + `.gemini` markers; codex gets a binary, gemini does
    /// NOT (the complaint case: config leftover without the binary).
    fn seeded_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".codex")).expect(".codex");
        std::fs::write(home.path().join(".codex/AGENTS.md"), "# Codex\n").expect("seed");
        std::fs::create_dir_all(home.path().join(".gemini")).expect(".gemini");
        home
    }

    fn saved_installed(config_dir: &std::path::Path) -> Vec<String> {
        stateroot_core::config::load_config(config_dir)
            .expect("load config")
            .installed_harnesses
    }

    #[tokio::test]
    async fn yes_mode_preselects_binary_rows_only() {
        let home = seeded_home();
        let cwd = tempfile::tempdir().expect("cwd");
        let config_dir = tempfile::tempdir().expect("config");
        let mut ctx = wizard_ctx(cwd.path(), config_dir.path(), home.path(), true);
        // Empty prompter = --yes: every prompt resolves to defaults, so the
        // preselect IS the selection and the finalize gate is skipped.
        let mut prompter = ScriptedPrompter::new();
        let section = section_with(&["codex"]);
        let actions = section
            .run(&mut ctx, &mut prompter)
            .await
            .expect("section run");

        let installed = saved_installed(config_dir.path());
        assert_eq!(installed, vec!["codex".to_string()], "actions: {actions:?}");
        // codex was installed (hooks written); the config-only gemini row was
        // not auto-selected despite its ~/.gemini leftover.
        assert!(home.path().join(".codex/hooks.json").exists());
        assert!(!home.path().join(".gemini/GEMINI.md").exists());
        // The plan/report calls the leftover out honestly.
        let joined = actions.join("\n");
        assert!(joined.contains("codex"), "actions: {joined}");
    }

    #[tokio::test]
    async fn finalize_gate_proceeds_by_default_without_answer() {
        let home = seeded_home();
        let cwd = tempfile::tempdir().expect("cwd");
        let config_dir = tempfile::tempdir().expect("config");
        let mut ctx = wizard_ctx(cwd.path(), config_dir.path(), home.path(), false);
        // No finalize answer: the gate's default is proceed (interactive Enter = yes).
        let mut prompter = ScriptedPrompter::from_answers(HashMap::from([(
            "harnesses.picked".to_string(),
            Answer::Indices(vec![0]),
        )]));
        let section = section_with(&["codex"]);
        let actions = section
            .run(&mut ctx, &mut prompter)
            .await
            .expect("section run");

        assert!(
            actions.join("\n").contains("hooks installed"),
            "actions: {actions:?}"
        );
        assert!(home.path().join(".codex/hooks.json").exists());
    }

    #[tokio::test]
    async fn finalize_gate_blocks_writes_when_explicitly_declined() {
        let home = seeded_home();
        let cwd = tempfile::tempdir().expect("cwd");
        let config_dir = tempfile::tempdir().expect("config");
        let mut ctx = wizard_ctx(cwd.path(), config_dir.path(), home.path(), false);
        // Explicit decline on both attempts: nothing may be written.
        let mut prompter = ScriptedPrompter::from_answers(HashMap::from([
            ("harnesses.picked".to_string(), Answer::Indices(vec![0])),
            ("harnesses.finalize".to_string(), Answer::Bool(false)),
            ("harnesses.finalize.retry".to_string(), Answer::Bool(false)),
        ]));
        let section = section_with(&["codex"]);
        let actions = section
            .run(&mut ctx, &mut prompter)
            .await
            .expect("section run");

        assert!(
            actions.join("\n").contains("declined"),
            "actions: {actions:?}"
        );
        assert!(!home.path().join(".codex/hooks.json").exists());
        assert!(
            !config_dir.path().join("config.toml").exists()
                || saved_installed(config_dir.path()).is_empty()
        );
    }

    #[tokio::test]
    async fn decline_then_retry_proceeds_on_second_confirm() {
        let home = seeded_home();
        let cwd = tempfile::tempdir().expect("cwd");
        let config_dir = tempfile::tempdir().expect("config");
        let mut ctx = wizard_ctx(cwd.path(), config_dir.path(), home.path(), false);
        let mut prompter = ScriptedPrompter::from_answers(HashMap::from([
            ("harnesses.picked".to_string(), Answer::Indices(vec![0])),
            ("harnesses.finalize".to_string(), Answer::Bool(false)),
            ("harnesses.finalize.retry".to_string(), Answer::Bool(true)),
        ]));
        let section = section_with(&["codex"]);
        let actions = section
            .run(&mut ctx, &mut prompter)
            .await
            .expect("section run");

        assert!(
            !actions.join("\n").contains("declined"),
            "actions: {actions:?}"
        );
        assert_eq!(
            saved_installed(config_dir.path()),
            vec!["codex".to_string()]
        );
        assert!(home.path().join(".codex/hooks.json").exists());
    }

    #[tokio::test]
    async fn finalize_confirm_runs_writes_immediately() {
        let home = seeded_home();
        let cwd = tempfile::tempdir().expect("cwd");
        let config_dir = tempfile::tempdir().expect("config");
        let mut ctx = wizard_ctx(cwd.path(), config_dir.path(), home.path(), false);
        // Pick BOTH rows (gemini explicitly opted in despite the leftover
        // warning) and confirm at the gate.
        let mut prompter = ScriptedPrompter::from_answers(HashMap::from([
            ("harnesses.picked".to_string(), Answer::Indices(vec![0, 1])),
            ("harnesses.finalize".to_string(), Answer::Bool(true)),
        ]));
        let section = section_with(&["codex"]);
        let actions = section
            .run(&mut ctx, &mut prompter)
            .await
            .expect("section run");

        let installed = saved_installed(config_dir.path());
        assert!(
            installed.contains(&"codex".to_string()),
            "actions: {actions:?}"
        );
        assert!(
            installed.contains(&"gemini".to_string()),
            "actions: {actions:?}"
        );
        assert!(home.path().join(".gemini/GEMINI.md").exists());
    }

    #[tokio::test]
    async fn openclaw_row_appears_and_installs_via_tier_path() {
        // The complaint case: OpenClaw installed (config dir present) must
        // appear in the picker even though it is not a legacy row.
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".openclaw")).expect(".openclaw");
        let cwd = tempfile::tempdir().expect("cwd");
        let config_dir = tempfile::tempdir().expect("config");
        let mut ctx = wizard_ctx(cwd.path(), config_dir.path(), home.path(), false);
        let mut prompter = ScriptedPrompter::from_answers(HashMap::from([
            ("harnesses.picked".to_string(), Answer::Indices(vec![0])),
            ("harnesses.finalize".to_string(), Answer::Bool(true)),
        ]));
        let section = section_with(&["openclaw"]);
        let actions = section
            .run(&mut ctx, &mut prompter)
            .await
            .expect("section run");
        let joined = actions.join("\n");

        assert_eq!(
            saved_installed(config_dir.path()),
            vec!["openclaw".to_string()],
            "actions: {joined}"
        );
        // Installed via the registry tier path (extensions/ native plugin),
        // not the legacy invisible plugins/ path.
        assert!(
            home.path()
                .join(".openclaw/extensions/stateroot/index.ts")
                .exists(),
            "actions: {joined}"
        );
        assert!(
            joined.contains("gateway restart") || joined.contains("enabled plugins"),
            "enable/restart note: {joined}"
        );
    }

    #[tokio::test]
    async fn hermes_row_installs_block_and_yaml_mcp() {
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".hermes")).expect(".hermes");
        let cwd = tempfile::tempdir().expect("cwd");
        let config_dir = tempfile::tempdir().expect("config");
        let mut ctx = wizard_ctx(cwd.path(), config_dir.path(), home.path(), false);
        let mut prompter = ScriptedPrompter::from_answers(HashMap::from([
            ("harnesses.picked".to_string(), Answer::Indices(vec![0])),
            ("harnesses.finalize".to_string(), Answer::Bool(true)),
        ]));
        let section = section_with(&["hermes"]);
        let actions = section
            .run(&mut ctx, &mut prompter)
            .await
            .expect("section run");
        let joined = actions.join("\n");

        assert_eq!(
            saved_installed(config_dir.path()),
            vec!["hermes".to_string()],
            "actions: {joined}"
        );
        // Instruction block in the persona file (idempotent marked block).
        let soul = std::fs::read_to_string(home.path().join(".hermes/SOUL.md")).expect("SOUL.md");
        assert!(soul.contains("<!-- stateroot:begin -->"), "soul: {soul}");
        // MCP bridge merged into config.yaml under mcp_servers.
        let config: serde_yaml::Value = serde_yaml::from_str(
            &std::fs::read_to_string(home.path().join(".hermes/config.yaml")).expect("config.yaml"),
        )
        .expect("parse config.yaml");
        let servers = config["mcp_servers"].as_mapping().expect("mcp_servers");
        let ours = &servers[serde_yaml::Value::String("stateroot".to_string())];
        assert_eq!(ours["command"], "stateroot");
        assert_eq!(ours["args"][0], "mcp-stdio");
        // No hooks in v1 — the guidance note is the honest placeholder.
        assert!(joined.contains("plugin planned"), "actions: {joined}");
    }
}
