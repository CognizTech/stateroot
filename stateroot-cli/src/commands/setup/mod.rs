//! `stateroot setup` — the setup wizard (Phase A).
//!
//! Local variant includes identity, harnesses, and skills. Engine: a section
//! registry driven through a [`Prompter`] trait so every flow is
//! headless-testable. Depths: `--quick` (defaults everywhere), `--full`
//! (default, asks everything), `--blank-slate` (nothing pre-selected).
//! Sections that detect prior configuration offer "reconfigure? [y/N]".
//! `--dry-run` prints planned writes only; `--yes` accepts all defaults;
//! `--config FILE` (YAML) supplies answers non-interactively with the same
//! keys the prompts use.

pub mod harnesses;
pub mod identity;
pub mod skills;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use async_trait::async_trait;

use super::{note, stdin_is_tty, Ctx};

/// Wizard depth. Quick/BlankSlate arrive with the full CLI flags.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Defaults everywhere; only essential questions.
    Quick,
    /// Ask everything (default).
    Full,
    /// Nothing pre-selected (reconfigure even when state exists).
    BlankSlate,
}

/// Options parsed from the CLI.
#[derive(Debug, Clone, Default)]
pub struct WizardOptions {
    /// Section ids to run (empty = all).
    pub only: Vec<String>,
    /// Wizard depth.
    pub depth: DepthChoice,
    /// Print planned writes without touching disk.
    pub dry_run: bool,
    /// Accept all defaults.
    pub yes: bool,
    /// YAML file with scripted answers.
    pub config_file: Option<PathBuf>,
}

/// Depth as parsed (default resolves to Full). Quick/BlankSlate are
/// part of the wizard contract; the M1 CLI only exposes Full.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DepthChoice {
    Quick,
    #[default]
    Full,
    BlankSlate,
}

impl DepthChoice {
    /// Resolve to the effective depth.
    pub fn resolve(&self) -> Depth {
        match self {
            DepthChoice::Quick => Depth::Quick,
            DepthChoice::Full => Depth::Full,
            DepthChoice::BlankSlate => Depth::BlankSlate,
        }
    }
}

/// Context shared by all sections.
pub struct WizardCtx {
    /// The normal CLI context (config, clients, stores).
    pub core: Ctx,
    /// Resolved home directory (`STATEROOT_TEST_HOME` override supported).
    pub home: PathBuf,
    /// When true, sections record planned writes instead of performing them.
    pub dry_run: bool,
    /// Effective depth.
    pub depth: Depth,
    /// True for `--yes`, `--config`, or a non-tty stdin: sections must not
    /// block on open-ended waits (the fire drill prints and skips instead).
    pub non_interactive: bool,
    /// Recap of writes performed/planned (section: action lines).
    pub recap: Vec<(&'static str, Vec<String>)>,
}

impl WizardCtx {
    /// Record an action in the recap.
    pub fn record(&mut self, section: &'static str, actions: Vec<String>) {
        if !actions.is_empty() {
            self.recap.push((section, actions));
        }
    }
}

/// Interactive question abstraction (dialoguer in prod, scripted in tests).
#[async_trait]
pub trait Prompter: Send {
    /// yes/no with a default.
    async fn confirm(&mut self, key: &str, prompt: &str, default: bool) -> Result<bool>;
    /// Free text with a default (empty default = optional).
    async fn input(&mut self, key: &str, prompt: &str, default: &str) -> Result<String>;
    /// Pick exactly one of `items`; returns the chosen index.
    #[allow(dead_code)]
    async fn select(
        &mut self,
        key: &str,
        prompt: &str,
        items: &[String],
        default: usize,
    ) -> Result<usize>;
    /// Pick any subset of `items`; returns chosen indices.
    async fn multi_select(
        &mut self,
        key: &str,
        prompt: &str,
        items: &[String],
        defaults: &[bool],
    ) -> Result<Vec<usize>>;
}

/// Production prompter backed by dialoguer.
pub struct DialoguerPrompter;

#[async_trait]
impl Prompter for DialoguerPrompter {
    async fn confirm(&mut self, _key: &str, prompt: &str, default: bool) -> Result<bool> {
        Ok(dialoguer::Confirm::new()
            .with_prompt(prompt.to_string())
            .default(default)
            .interact()?)
    }

    async fn input(&mut self, _key: &str, prompt: &str, default: &str) -> Result<String> {
        let mut input = dialoguer::Input::<String>::new().with_prompt(prompt.to_string());
        if !default.is_empty() {
            input = input.default(default.to_string());
        }
        input = input.allow_empty(true);
        Ok(input.interact_text()?)
    }

    #[allow(dead_code)]
    async fn select(
        &mut self,
        _key: &str,
        prompt: &str,
        items: &[String],
        default: usize,
    ) -> Result<usize> {
        Ok(dialoguer::Select::new()
            .with_prompt(prompt.to_string())
            .items(items)
            .default(default.min(items.len().saturating_sub(1)))
            .interact()?)
    }

    async fn multi_select(
        &mut self,
        _key: &str,
        prompt: &str,
        items: &[String],
        defaults: &[bool],
    ) -> Result<Vec<usize>> {
        Ok(dialoguer::MultiSelect::new()
            .with_prompt(prompt.to_string())
            .items(items)
            .defaults(defaults)
            .interact()?)
    }
}

/// One scripted answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// yes/no
    Bool(bool),
    /// free text
    Text(String),
    /// single index
    Index(usize),
    /// index subset
    Indices(Vec<usize>),
}

/// Non-interactive prompter: answers keyed by prompt key; missing keys fall
/// back to the prompt's default (so `--yes` and partial `--config` work).
#[derive(Default)]
pub struct ScriptedPrompter {
    answers: HashMap<String, Answer>,
}

impl ScriptedPrompter {
    /// Empty prompter — every prompt resolves to its default (`--yes`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Prompter from an explicit answer map (unit tests).
    #[cfg(test)]
    pub fn from_answers(answers: HashMap<String, Answer>) -> Self {
        Self { answers }
    }

    /// Load answers from a YAML file (keys must match the prompt keys).
    pub fn from_yaml_file(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let value: serde_yaml::Value = serde_yaml::from_str(&text)
            .with_context(|| format!("{} is not valid YAML", path.display()))?;
        let mapping = value.as_mapping().ok_or_else(|| {
            anyhow::anyhow!("{}: expected a YAML mapping of prompt keys", path.display())
        })?;
        let mut answers = HashMap::new();
        for (key, value) in mapping {
            let key = key
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("config keys must be strings"))?
                .to_string();
            let answer = match value {
                serde_yaml::Value::Bool(b) => Answer::Bool(*b),
                serde_yaml::Value::String(s) => Answer::Text(s.clone()),
                serde_yaml::Value::Number(n) => {
                    let idx = n.as_u64().ok_or_else(|| {
                        anyhow::anyhow!("key '{key}': expected a non-negative integer")
                    })?;
                    Answer::Index(idx as usize)
                }
                serde_yaml::Value::Sequence(seq) => {
                    let mut indices = Vec::new();
                    for item in seq {
                        match item {
                            serde_yaml::Value::Number(n) => {
                                indices.push(n.as_u64().ok_or_else(|| {
                                    anyhow::anyhow!(
                                        "key '{key}': indices must be non-negative integers"
                                    )
                                })? as usize)
                            }
                            other => anyhow::bail!("key '{key}': unsupported list item {other:?}"),
                        }
                    }
                    Answer::Indices(indices)
                }
                other => anyhow::bail!("key '{key}': unsupported value {other:?}"),
            };
            answers.insert(key, answer);
        }
        Ok(Self { answers })
    }
}

#[async_trait]
impl Prompter for ScriptedPrompter {
    async fn confirm(&mut self, key: &str, _prompt: &str, default: bool) -> Result<bool> {
        match self.answers.get(key) {
            Some(Answer::Bool(b)) => Ok(*b),
            Some(other) => anyhow::bail!("answer for '{key}' must be a boolean, got {other:?}"),
            None => Ok(default),
        }
    }

    async fn input(&mut self, key: &str, _prompt: &str, default: &str) -> Result<String> {
        match self.answers.get(key) {
            Some(Answer::Text(s)) => Ok(s.clone()),
            Some(Answer::Bool(b)) => Ok(b.to_string()),
            Some(other) => anyhow::bail!("answer for '{key}' must be text, got {other:?}"),
            None => Ok(default.to_string()),
        }
    }

    #[allow(dead_code)]
    async fn select(
        &mut self,
        key: &str,
        _prompt: &str,
        items: &[String],
        default: usize,
    ) -> Result<usize> {
        match self.answers.get(key) {
            Some(Answer::Index(i)) => {
                if *i >= items.len() {
                    anyhow::bail!("answer for '{key}' out of range ({i} >= {})", items.len());
                }
                Ok(*i)
            }
            Some(Answer::Text(s)) => items
                .iter()
                .position(|item| item == s)
                .ok_or_else(|| anyhow::anyhow!("answer for '{key}' not in options: '{s}'")),
            Some(other) => {
                anyhow::bail!("answer for '{key}' must be an index or option text, got {other:?}")
            }
            None => Ok(default.min(items.len().saturating_sub(1))),
        }
    }

    async fn multi_select(
        &mut self,
        key: &str,
        _prompt: &str,
        items: &[String],
        defaults: &[bool],
    ) -> Result<Vec<usize>> {
        match self.answers.get(key) {
            Some(Answer::Indices(indices)) => {
                for i in indices {
                    if *i >= items.len() {
                        anyhow::bail!("answer for '{key}' out of range ({i} >= {})", items.len());
                    }
                }
                Ok(indices.clone())
            }
            Some(Answer::Text(s)) => {
                // Comma-separated option texts are accepted too.
                let mut indices = Vec::new();
                for part in s.split(',').map(|p| p.trim()).filter(|p| !p.is_empty()) {
                    let idx = items.iter().position(|item| item == part).ok_or_else(|| {
                        anyhow::anyhow!("answer for '{key}' not in options: '{part}'")
                    })?;
                    indices.push(idx);
                }
                Ok(indices)
            }
            Some(other) => anyhow::bail!("answer for '{key}' must be an index list, got {other:?}"),
            None => Ok(defaults
                .iter()
                .enumerate()
                .filter_map(|(i, d)| if *d { Some(i) } else { None })
                .collect()),
        }
    }
}

/// One wizard section.
#[async_trait]
pub trait WizardSection: Send {
    /// Stable id used by `--only` and `is_configured` markers.
    fn id(&self) -> &'static str;
    /// Human title.
    fn title(&self) -> &'static str;
    /// True when prior state exists (drives the reconfigure prompt).
    async fn is_configured(&self, ctx: &WizardCtx) -> Result<bool>;
    /// Run the section; returns action lines for the recap.
    async fn run(&self, ctx: &mut WizardCtx, prompter: &mut dyn Prompter) -> Result<Vec<String>>;
}

/// The section registry (order matters; the fire drill runs LAST — it needs
/// auth, harnesses and a workspace from the earlier sections).
pub fn registry() -> Vec<Box<dyn WizardSection>> {
    vec![
        Box::new(identity::IdentitySection),
        Box::new(harnesses::HarnessesSection::default()),
        Box::new(skills::SkillsSection),
    ]
}

/// Build the prompter for these options.
pub fn build_prompter(options: &WizardOptions) -> Result<Box<dyn Prompter>> {
    if let Some(path) = &options.config_file {
        return Ok(Box::new(ScriptedPrompter::from_yaml_file(path)?));
    }
    if options.yes || !stdin_is_tty() {
        if !options.yes && !stdin_is_tty() {
            note!("no interactive terminal — proceeding with defaults (same as --yes)");
        }
        return Ok(Box::new(ScriptedPrompter::new()));
    }
    Ok(Box::new(DialoguerPrompter))
}

/// Run the wizard.
pub async fn run(core: Ctx, options: WizardOptions) -> Result<()> {
    let home = super::install::home_dir()?;
    let depth = options.depth.resolve();
    let non_interactive = options.yes || options.config_file.is_some() || !stdin_is_tty();
    let mut ctx = WizardCtx {
        core,
        home,
        dry_run: options.dry_run,
        depth,
        non_interactive,
        recap: Vec::new(),
    };
    let mut prompter = build_prompter(&options)?;

    let sections = registry();
    let selected: Vec<&Box<dyn WizardSection>> = if options.only.is_empty() {
        sections.iter().collect()
    } else {
        let mut picked = Vec::new();
        for id in &options.only {
            let section = sections.iter().find(|s| s.id() == id.trim());
            match section {
                Some(section) => picked.push(section),
                None => anyhow::bail!(
                    "unknown section '{id}' — expected one of: {}",
                    sections
                        .iter()
                        .map(|s| s.id())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        }
        picked
    };

    println!(
        "stateroot setup (depth: {:?}{})",
        depth,
        if options.dry_run { ", dry-run" } else { "" }
    );
    for section in selected {
        let configured = section.is_configured(&ctx).await.unwrap_or(false);
        if configured && depth != Depth::BlankSlate {
            let redo = prompter
                .confirm(
                    &format!("{}.reconfigure", section.id()),
                    &format!(
                        "'{}' looks configured already — reconfigure?",
                        section.title()
                    ),
                    false,
                )
                .await?;
            if !redo {
                println!("  [{}] skipped (already configured)", section.id());
                continue;
            }
        }
        println!("  [{}] {}", section.id(), section.title());
        let actions = section.run(&mut ctx, prompter.as_mut()).await?;
        ctx.record(section.id(), actions);
    }

    println!();
    if options.dry_run {
        println!("dry-run — planned writes (nothing was touched):");
    } else {
        println!("recap:");
    }
    if ctx.recap.is_empty() {
        println!("  (nothing to do)");
    }
    for (section, actions) in &ctx.recap {
        for action in actions {
            println!("  [{section}] {action}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scripted_prompter_answers_and_defaults() {
        let mut prompter = ScriptedPrompter::from_answers(HashMap::from([
            ("a.bool".to_string(), Answer::Bool(false)),
            ("a.text".to_string(), Answer::Text("hello".to_string())),
            ("a.index".to_string(), Answer::Index(1)),
            ("a.indices".to_string(), Answer::Indices(vec![0, 2])),
        ]));
        assert!(!prompter.confirm("a.bool", "q", true).await.expect("bool"));
        assert_eq!(
            prompter
                .input("a.text", "q", "default")
                .await
                .expect("text"),
            "hello"
        );
        assert_eq!(
            prompter
                .select("a.index", "q", &["x".into(), "y".into()], 0)
                .await
                .expect("index"),
            1
        );
        assert_eq!(
            prompter
                .multi_select(
                    "a.indices",
                    "q",
                    &["a".into(), "b".into(), "c".into()],
                    &[true, true, true]
                )
                .await
                .expect("indices"),
            vec![0, 2]
        );
        // Missing keys resolve to the prompt default.
        assert!(prompter
            .confirm("missing", "q", true)
            .await
            .expect("default"));
        assert_eq!(
            prompter
                .select("missing", "q", &["only".into()], 0)
                .await
                .expect("default index"),
            0
        );
    }

    #[tokio::test]
    async fn scripted_prompter_yaml_parsing() {
        let tmp = tempfile::tempdir().expect("tmp");
        let file = tmp.path().join("answers.yaml");
        std::fs::write(
            &file,
            "soul.make_default: true\nharnesses.picked: [0, 2]\nsoul.new_agent_name: yinyue\n",
        )
        .expect("write");
        let mut prompter = ScriptedPrompter::from_yaml_file(&file).expect("parse");
        assert!(prompter
            .confirm("soul.make_default", "q", false)
            .await
            .expect("bool"));
        assert_eq!(
            prompter
                .multi_select(
                    "harnesses.picked",
                    "q",
                    &["a".into(), "b".into(), "c".into()],
                    &[false; 3]
                )
                .await
                .expect("indices"),
            vec![0, 2]
        );
        assert_eq!(
            prompter
                .input("soul.new_agent_name", "q", "")
                .await
                .expect("text"),
            "yinyue"
        );
    }
}
