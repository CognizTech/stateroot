//! Local identity discovery and import setup section.

use anyhow::Result;
use async_trait::async_trait;

use super::{Prompter, WizardCtx, WizardSection};

#[derive(Debug)]
struct Candidate {
    label: String,
    origin: String,
    persona: Option<String>,
    user: Option<String>,
}

pub struct IdentitySection;

#[async_trait]
impl WizardSection for IdentitySection {
    fn id(&self) -> &'static str {
        "identity"
    }

    fn title(&self) -> &'static str {
        "Identity"
    }

    async fn is_configured(&self, ctx: &WizardCtx) -> Result<bool> {
        Ok(stateroot_core::soul::read_canonical(&ctx.home).is_some()
            || stateroot_core::user_profile::read(&ctx.home).is_some())
    }

    async fn run(&self, ctx: &mut WizardCtx, prompter: &mut dyn Prompter) -> Result<Vec<String>> {
        let candidates = discover(&ctx.home);
        if candidates.is_empty() {
            return Ok(vec!["no local OpenClaw or Hermes identity found".into()]);
        }
        let chosen = if candidates.len() == 1 {
            0
        } else {
            let labels = candidates
                .iter()
                .map(|c| c.label.clone())
                .collect::<Vec<_>>();
            prompter
                .select(
                    "identity.source",
                    "Choose one identity source (sources are never mixed)",
                    &labels,
                    0,
                )
                .await?
        };
        let candidate = &candidates[chosen];
        let mut actions = Vec::new();
        if let Some(persona) = candidate
            .persona
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            if ctx.dry_run {
                actions.push(format!("would import persona from {}", candidate.label));
            } else {
                actions.push(stateroot_core::soul::write_canonical(
                    &ctx.home,
                    persona,
                    Some(&candidate.origin),
                )?);
                super::super::soul::refresh_persona_cache_pub(&ctx.core);
            }
        }
        if let Some(user) = candidate.user.as_deref().filter(|s| !s.trim().is_empty()) {
            if ctx.dry_run {
                actions.push(format!(
                    "would import user profile from {}",
                    candidate.label
                ));
            } else {
                actions.push(stateroot_core::user_profile::write(
                    &ctx.home,
                    user,
                    Some(&candidate.origin),
                )?);
            }
        }
        Ok(actions)
    }
}

fn discover(home: &std::path::Path) -> Vec<Candidate> {
    let mut out = stateroot_core::openclaw_identity::discover_openclaw_identities(home)
        .into_iter()
        .map(|pack| Candidate {
            label: pack.label,
            origin: format!("openclaw:{}", pack.workspace.display()),
            persona: (!pack.persona_markdown.trim().is_empty()).then_some(pack.persona_markdown),
            user: pack.user_markdown,
        })
        .collect::<Vec<_>>();

    let hermes = stateroot_core::soul::hermes_home(home);
    let persona = read_nonempty(hermes.join("SOUL.md"));
    let user = read_nonempty(hermes.join("memories/USER.md"));
    if persona.is_some() || user.is_some() {
        out.push(Candidate {
            label: format!("Hermes ({})", hermes.display()),
            origin: format!("hermes:{}", hermes.display()),
            persona,
            user,
        });
    }
    out
}

fn read_nonempty(path: std::path::PathBuf) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    (!text.trim().is_empty()).then(|| text.trim().to_string())
}
