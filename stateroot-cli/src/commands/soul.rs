//! `stateroot soul …` — canonical soul, overlay, projections (M3, all local).

use std::io::Read as _;

use anyhow::Result;
use stateroot_core::soul as core_soul;

use super::{note, stdin_is_tty, Ctx};

fn home(ctx: &Ctx) -> Result<std::path::PathBuf> {
    let _ = ctx;
    stateroot_core::harness_install::home_dir().map_err(|e| anyhow::anyhow!(e))
}

/// Refresh the persona cache used by resume/install blocks (write-through).
pub(crate) fn refresh_persona_cache_pub(ctx: &Ctx) {
    refresh_persona_cache(ctx)
}

fn refresh_persona_cache(ctx: &Ctx) {
    let home = match home(ctx) {
        Ok(home) => home,
        Err(_) => return,
    };
    if let Some(soul) = core_soul::read_canonical(&home) {
        let projection = core_soul::render_projection(&soul, None);
        if !projection.trim().is_empty() {
            let _ = std::fs::create_dir_all(&ctx.config_dir);
            let _ = std::fs::write(super::persona::cache_path(&ctx.config_dir), projection);
        }
    }
    super::install::refresh_global_instruction_blocks(&ctx.config_dir, &home);
}

/// `stateroot soul show [--harness <id>]`
pub fn show(ctx: &Ctx, harness: Option<&str>) -> Result<()> {
    let home = home(ctx)?;
    let canonical = core_soul::read_canonical(&home);
    let overlay = core_soul::read_overlay(&ctx.cwd);
    if canonical.is_none() && overlay.is_none() {
        println!(
            "soul: none (run `stateroot soul generate` or `stateroot soul import --from openclaw`)"
        );
        return Ok(());
    }
    if let Some(soul) = &canonical {
        println!("## Canonical (user)\n\n{soul}\n");
    }
    if let Some(overlay) = &overlay {
        println!("## Project overlay\n\n{overlay}\n");
    }
    if let Some(soul) = &canonical {
        let projection = core_soul::render_projection(soul, harness);
        if !projection.trim().is_empty() {
            if let Some(h) = harness {
                let id = stateroot_core::skill_federation::normalize_harness(h);
                println!(
                    "## Projection ({})\n",
                    stateroot_core::skill_federation::display_name(&id)
                );
            }
            print!("{projection}");
        }
    }
    Ok(())
}

/// `stateroot soul edit` — user-authoring direct write via $EDITOR (with
/// history snapshot; evolution from agents goes through proposals instead).
pub fn edit(ctx: &Ctx) -> Result<()> {
    let home = home(ctx)?;
    let path = home
        .join(core_soul::SOUL_DIR)
        .join(core_soul::CANONICAL_FILE);
    if !path.exists() {
        std::fs::create_dir_all(path.parent().expect("soul dir"))?;
        std::fs::write(&path, "# Soul\n\n")?;
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    if !stdin_is_tty() {
        anyhow::bail!(
            "soul edit needs an interactive terminal (or write via `soul propose --stdin`)"
        );
    }
    let status = std::process::Command::new(editor).arg(&path).status()?;
    if !status.success() {
        anyhow::bail!("editor exited non-zero");
    }
    // Snapshot the pre-edit version by writing through the store.
    let content = std::fs::read_to_string(&path)?;
    let note = core_soul::write_canonical(&home, &content, None)?;
    println!("{note}");
    refresh_persona_cache(ctx);
    Ok(())
}

/// `stateroot soul import --from openclaw|hermes|<path>`
pub fn import(ctx: &Ctx, from: &str) -> Result<()> {
    let home = home(ctx)?;
    let (content, origin) = match from {
        "openclaw" => core_soul::import(&home, core_soul::ImportSource::Openclaw)
            .map_err(|e| anyhow::anyhow!(e))?,
        "hermes" => core_soul::import(&home, core_soul::ImportSource::Hermes)
            .map_err(|e| anyhow::anyhow!(e))?,
        path => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("cannot read {path}: {e}"))?;
            let text = text.trim().to_string();
            if text.is_empty() {
                anyhow::bail!("empty soul file: {path}");
            }
            (text, format!("file: {path}"))
        }
    };
    let provenanced = core_soul::with_provenance(&content, &origin);
    let note = core_soul::write_canonical(&home, &provenanced, None)?;
    println!("imported ({origin})");
    println!("{note}");
    refresh_persona_cache(ctx);
    Ok(())
}

/// `stateroot soul generate [--yes] [--apply]` — deterministic Q&A draft.
pub fn generate(ctx: &Ctx, yes: bool, apply: bool) -> Result<()> {
    let home = home(ctx)?;
    let answers = if yes || !stdin_is_tty() {
        note!("(using default answers — deterministic draft)");
        core_soul::GenerateAnswers::default()
    } else {
        core_soul::GenerateAnswers {
            tone: prompt_line("Tone / communication style", "direct and concise")?,
            initiative: prompt_line(
                "Initiative (how proactive?)",
                "medium — propose next steps, wait for go-ahead",
            )?,
            depth: prompt_line("Explanation depth", "enough to decide, not a lecture")?,
            boundaries: prompt_line(
                "Boundaries (privacy / identity / global behavior)",
                "do not silently change identity, privacy, or global behavior",
            )?,
            principles: prompt_line(
                "Principles",
                "optimize for correctness and the user's stated goal",
            )?,
            disagreement: prompt_line(
                "Disagreement handling",
                "state the disagreement once with evidence, then follow the user",
            )?,
            desired: prompt_line("Desired example (optional)", "")?,
            undesired: prompt_line("Undesired example (optional)", "")?,
        }
    };
    let draft = core_soul::draft_from_answers(&answers);
    if apply {
        let note = core_soul::write_canonical(&home, &draft, Some("generate"))?;
        println!("{note}");
        refresh_persona_cache(ctx);
    } else {
        print!("{draft}");
        note!(
            "(draft only — pass --apply to activate, or `soul propose --stdin` for the gated flow)"
        );
    }
    Ok(())
}

fn prompt_line(label: &str, default: &str) -> Result<String> {
    let value: String = dialoguer::Input::new()
        .with_prompt(format!("{label} [{default}]"))
        .allow_empty(true)
        .interact_text()?;
    let value = value.trim().to_string();
    Ok(if value.is_empty() {
        default.to_string()
    } else {
        value
    })
}

/// `stateroot soul propose [--file F | --stdin]` — the gated evolution flow:
/// a proposal, never a direct write.
pub fn propose(ctx: &Ctx, file: Option<&str>, stdin: bool, rationale: Option<&str>) -> Result<()> {
    ctx.require_project()?;
    let content = if let Some(path) = file {
        std::fs::read_to_string(path)?
    } else if stdin {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        anyhow::bail!("pass --file <path> or --stdin");
    };
    if content.trim().is_empty() {
        anyhow::bail!("empty soul content — nothing proposed");
    }
    let proposal = stateroot_core::proposals::create(
        &ctx.cwd,
        "soul",
        "soul update (proposed)",
        rationale.unwrap_or("soul propose"),
        serde_json::json!({"content": content.trim()}),
        serde_json::json!({"source": "soul propose"}),
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    println!("proposal {} created (pending)", proposal.id);
    println!(
        "approve with: stateroot proposals approve {}",
        &proposal.id[..8]
    );
    Ok(())
}
