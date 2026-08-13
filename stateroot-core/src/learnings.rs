//! Local learnings + memory notes (M3).
//!
//! Same category-md format as the server variant:
//! `- **statement** <!-- id: …; confidence: 0.7; label: observed; sources: …; scope: …; status: … -->`
//!
//! Scopes: user (`~/.stateroot/learnings/`) and project
//! (`.stateroot/learnings/`). Explicit `learn record` / MCP `learn_record`
//! writes learnings and memories as **active** immediately so the next
//! harness inherits them. Distill still lands in `_candidates/` (lifecycle
//! candidate → proposed → active, gated by local proposals) because those
//! notes are inferred, not stated.
//!
//! Memory notes share the scoping ladder: `memory.md` per scope with
//! `<!-- visibility: shared|private -->` markers on the note lines;
//! foreign-origin `memory_save` notes land as session candidates.

use std::path::{Path, PathBuf};

use crate::local_store;

/// Learnings directory name under a scope root.
pub const LEARNINGS_DIR: &str = "learnings";
/// Candidates subdirectory (quarantine).
pub const CANDIDATES_DIR: &str = "_candidates";
/// Rejected archive (audit trail, never deleted).
pub const REJECTED_FILE: &str = "_rejected.md";

/// Errors from the learnings store.
#[derive(Debug, thiserror::Error)]
pub enum LearningsError {
    /// Local filesystem failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// One learning record (on-disk contract).
#[derive(Debug, Clone, PartialEq)]
pub struct Learning {
    /// Stable id (`lrn_<hex>`).
    pub id: String,
    /// The statement.
    pub statement: String,
    /// Category (file stem).
    pub category: String,
    /// Confidence 0..1.
    pub confidence: f64,
    /// `observed` | `inferred`.
    pub label: String,
    /// Source references (free-form).
    pub sources: String,
    /// `user` | `workspace` | `project` | `session_candidate`.
    pub scope: String,
    /// `candidate` | `proposed` | `active` | `rejected`.
    pub status: String,
    /// Root hash when this learning became active (empty until promoted).
    pub active_at_root: String,
    /// Id of the learning that superseded this one (empty when current).
    pub superseded_by: String,
}

impl Learning {
    /// A new candidate with a fresh id.
    pub fn candidate(
        statement: &str,
        category: &str,
        confidence: f64,
        sources: &str,
        scope: &str,
    ) -> Self {
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(format!("{statement}\0{scope}").as_bytes());
        let hex = format!("{:x}", hasher.finalize());
        Self {
            id: format!("lrn_{}", &hex[..10]),
            statement: statement.into(),
            category: category.into(),
            confidence,
            label: "observed".into(),
            sources: sources.into(),
            scope: scope.into(),
            status: "candidate".into(),
            active_at_root: String::new(),
            superseded_by: String::new(),
        }
    }

    /// Render one markdown bullet.
    pub fn render_bullet(&self) -> String {
        let mut meta = format!(
            "id: {}; confidence: {:.2}; label: {}; sources: {}; scope: {}; status: {}",
            self.id, self.confidence, self.label, self.sources, self.scope, self.status
        );
        if !self.active_at_root.is_empty() {
            meta.push_str(&format!("; active_at_root: {}", self.active_at_root));
        }
        if !self.superseded_by.is_empty() {
            meta.push_str(&format!("; superseded_by: {}", self.superseded_by));
        }
        format!("- **{}** <!-- {} -->", self.statement, meta)
    }
}

/// Parse one bullet line (same grammar as the CLI reader).
pub fn parse_bullet(line: &str, category: &str) -> Option<Learning> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("- **")?;
    let (statement, rest) = rest.split_once("**")?;
    let statement = statement.trim();
    if statement.is_empty() {
        return None;
    }
    let comment_start = rest.find("<!--")?;
    let comment_end = rest[comment_start..].find("-->")?;
    let comment = &rest[comment_start + 4..comment_start + comment_end];
    let mut id = String::new();
    let mut confidence = 0.0f64;
    let mut label = String::new();
    let mut sources = String::new();
    let mut scope = String::from("project");
    let mut status = String::from("active");
    let mut active_at_root = String::new();
    let mut superseded_by = String::new();
    for part in comment.split(';') {
        let Some((key, value)) = part.trim().split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "id" => id = value.to_string(),
            "confidence" => confidence = value.parse().unwrap_or(0.0),
            "label" => label = value.to_string(),
            "sources" => sources = value.to_string(),
            "scope" if !value.is_empty() => scope = value.to_string(),
            "status" if !value.is_empty() => status = value.to_string(),
            "active_at_root" if !value.is_empty() => active_at_root = value.to_string(),
            "superseded_by" if !value.is_empty() => superseded_by = value.to_string(),
            _ => {}
        }
    }
    if id.is_empty() {
        return None;
    }
    Some(Learning {
        id,
        statement: statement.to_string(),
        category: category.to_string(),
        confidence,
        label,
        sources,
        scope,
        status,
        active_at_root,
        superseded_by,
    })
}

/// Scope root for `user` (home) or `project`.
fn scope_root(project_dir: &Path, home: &Path, scope: &str) -> PathBuf {
    if scope == "user" {
        home.join(".stateroot").join(LEARNINGS_DIR)
    } else {
        local_store::root(project_dir).join(LEARNINGS_DIR)
    }
}

fn read_dir(dir: &Path, status_override: Option<&str>) -> Vec<Learning> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let category = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("general")
            .to_string();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            if let Some(mut learning) = parse_bullet(line, &category) {
                if let Some(status) = status_override {
                    learning.status = status.to_string();
                }
                out.push(learning);
            }
        }
    }
    out
}

/// Read all learnings for one scope (active files + candidates).
pub fn read_scope(project_dir: &Path, home: &Path, scope: &str) -> Vec<Learning> {
    let root = scope_root(project_dir, home, scope);
    let mut out = read_dir(&root, None);
    out.extend(read_dir(&root.join(CANDIDATES_DIR), Some("candidate")));
    out.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// Append a candidate learning into `_candidates/<category>.md` (dedupe by
/// normalized statement across the scope — existing ids are kept).
pub fn append_candidate(
    project_dir: &Path,
    home: &Path,
    scope: &str,
    learning: &Learning,
) -> Result<bool, LearningsError> {
    let existing = read_scope(project_dir, home, scope);
    let normalized = normalize(&learning.statement);
    if existing
        .iter()
        .any(|l| normalize(&l.statement) == normalized)
    {
        return Ok(false);
    }
    let dir = scope_root(project_dir, home, scope).join(CANDIDATES_DIR);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.md", learning.category));
    let mut body = std::fs::read_to_string(&path).unwrap_or_default();
    if !body.ends_with('\n') && !body.is_empty() {
        body.push('\n');
    }
    body.push_str(&learning.render_bullet());
    body.push('\n');
    std::fs::write(path, body)?;
    Ok(true)
}

/// Write a learning as active immediately. Explicit `learn record` /
/// MCP `learn_record` skips the proposal gate so the next harness inherits
/// the note. Distill still lands in `_candidates/` until approved.
/// Dedupes by normalized statement; promoting an existing candidate if needed.
pub fn activate_learning(
    project_dir: &Path,
    home: &Path,
    scope: &str,
    statement: &str,
    category: &str,
    sources: &str,
) -> Result<(String, bool), LearningsError> {
    let normalized = normalize(statement);
    let existing = read_scope(project_dir, home, scope);
    if let Some(prior) = existing
        .iter()
        .find(|learning| normalize(&learning.statement) == normalized)
    {
        if prior.status != "active" {
            let _ = promote(project_dir, home, scope, &prior.id)?;
        }
        return Ok((prior.id.clone(), false));
    }
    let mut learning = Learning::candidate(statement, category, 0.85, sources, scope);
    learning.status = "active".into();
    learning.active_at_root = crate::roots::latest_root(project_dir)
        .ok()
        .flatten()
        .unwrap_or_default();
    let root = scope_root(project_dir, home, scope);
    std::fs::create_dir_all(&root)?;
    let path = root.join(format!("{}.md", learning.category));
    let mut body = std::fs::read_to_string(&path).unwrap_or_default();
    if !body.ends_with('\n') && !body.is_empty() {
        body.push('\n');
    }
    body.push_str(&learning.render_bullet());
    body.push('\n');
    std::fs::write(path, body)?;
    Ok((learning.id, true))
}

/// Promote a candidate to active (proposal-approved): move the bullet from
/// `_candidates/<cat>.md` into `<cat>.md` with `status: active`.
pub fn promote(
    project_dir: &Path,
    home: &Path,
    scope: &str,
    id: &str,
) -> Result<bool, LearningsError> {
    let root = scope_root(project_dir, home, scope);
    let candidates = read_dir(&root.join(CANDIDATES_DIR), Some("candidate"));
    let Some(mut learning) = candidates
        .into_iter()
        .find(|l| l.id == id || l.id.starts_with(id))
    else {
        return Ok(false);
    };
    // Remove from candidates file.
    let candidates_path = root
        .join(CANDIDATES_DIR)
        .join(format!("{}.md", learning.category));
    rewrite_without_id(&candidates_path, &learning.id)?;
    // Append active bullet.
    learning.status = "active".into();
    learning.active_at_root = crate::roots::latest_root(project_dir)
        .ok()
        .flatten()
        .unwrap_or_default();
    // Supersede any prior active learning with the same normalized statement.
    let normalized = normalize(&learning.statement);
    for prior in read_dir(&root, None) {
        if prior.id != learning.id
            && prior.status == "active"
            && normalize(&prior.statement) == normalized
            && prior.superseded_by.is_empty()
        {
            mark_superseded(
                &root.join(format!("{}.md", prior.category)),
                &prior.id,
                &learning.id,
            )?;
        }
    }
    let active_path = root.join(format!("{}.md", learning.category));
    let mut body = std::fs::read_to_string(&active_path).unwrap_or_default();
    if !body.ends_with('\n') && !body.is_empty() {
        body.push('\n');
    }
    body.push_str(&learning.render_bullet());
    body.push('\n');
    std::fs::write(active_path, body)?;
    Ok(true)
}

/// Reject a candidate: remove it and archive the bullet in `_rejected.md`.
pub fn reject(
    project_dir: &Path,
    home: &Path,
    scope: &str,
    id: &str,
) -> Result<bool, LearningsError> {
    let root = scope_root(project_dir, home, scope);
    let candidates = read_dir(&root.join(CANDIDATES_DIR), Some("candidate"));
    let Some(learning) = candidates
        .into_iter()
        .find(|l| l.id == id || l.id.starts_with(id))
    else {
        return Ok(false);
    };
    let candidates_path = root
        .join(CANDIDATES_DIR)
        .join(format!("{}.md", learning.category));
    rewrite_without_id(&candidates_path, &learning.id)?;
    let mut rejected = learning.clone();
    rejected.status = "rejected".into();
    let mut body = std::fs::read_to_string(root.join(REJECTED_FILE)).unwrap_or_default();
    body.push_str(&rejected.render_bullet());
    body.push('\n');
    std::fs::write(root.join(REJECTED_FILE), body)?;
    Ok(true)
}

/// Edit an active or candidate learning's statement in place.
pub fn edit(
    project_dir: &Path,
    home: &Path,
    scope: &str,
    id: &str,
    statement: &str,
) -> Result<bool, LearningsError> {
    let root = scope_root(project_dir, home, scope);
    for (dir, status_override) in [
        (root.clone(), None),
        (root.join(CANDIDATES_DIR), Some("candidate")),
    ] {
        let learnings = read_dir(&dir, status_override);
        for learning in learnings {
            if learning.id == id || learning.id.starts_with(id) {
                let path = dir.join(format!("{}.md", learning.category));
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                let mut updated = learning.clone();
                updated.statement = statement.to_string();
                // Rebuild the file line-wise: replace only the matching bullet.
                let rebuilt: String = text
                    .lines()
                    .map(|line| {
                        if parse_bullet(line, &learning.category)
                            .map(|l| l.id == learning.id)
                            .unwrap_or(false)
                        {
                            updated.render_bullet()
                        } else {
                            line.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                std::fs::write(path, rebuilt + "\n")?;
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn mark_superseded(path: &Path, id: &str, superseded_by: &str) -> Result<(), LearningsError> {
    let category = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("general")
        .to_string();
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let rebuilt: String = text
        .lines()
        .map(|line| {
            if let Some(mut learning) = parse_bullet(line, &category) {
                if learning.id == id {
                    learning.superseded_by = superseded_by.to_string();
                    return learning.render_bullet();
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, format!("{rebuilt}\n"))?;
    Ok(())
}

fn rewrite_without_id(path: &Path, id: &str) -> Result<(), LearningsError> {
    let category = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("general")
        .to_string();
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| {
            parse_bullet(line, &category)
                .map(|l| l.id != id)
                .unwrap_or(true)
        })
        .collect();
    std::fs::write(path, kept.join("\n") + "\n")?;
    Ok(())
}

fn infer_scope(category: &str, statement: &str) -> &'static str {
    let lower = statement.to_lowercase();
    let project_bound = [
        "this repo",
        "this project",
        "this crate",
        "this package",
        ".stateroot",
        "in this codebase",
        "in this workspace",
    ]
    .iter()
    .any(|m| lower.contains(m));
    if project_bound {
        return "project";
    }
    if category == "preferences" {
        "user"
    } else {
        "project"
    }
}

fn normalize(statement: &str) -> String {
    statement
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Classification of a `learn record` note (deterministic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    /// `soul` | `memory` | `skill` | `learning`.
    pub kind: String,
    /// Learning category (for kind=learning).
    pub category: String,
}

/// Classify a note into a review-loop lane.
pub fn classify_note(note: &str) -> Classification {
    let lower = note.to_lowercase();
    let has = |markers: &[&str]| markers.iter().any(|m| lower.contains(m));
    if has(&[
        "you are",
        "your name",
        "call yourself",
        "persona",
        "identity",
        "your role",
    ]) {
        return Classification {
            kind: "soul".into(),
            category: "identity".into(),
        };
    }
    if has(&[
        "how to",
        "steps to",
        "procedure",
        "workflow",
        "recipe",
        "playbook",
    ]) {
        return Classification {
            kind: "skill".into(),
            category: "procedures".into(),
        };
    }
    if has(&["actually", "instead of", "wrong", "correction", "not that"]) {
        return Classification {
            kind: "learning".into(),
            category: "corrections".into(),
        };
    }
    if has(&["prefer", "always", "never", "style"]) {
        return Classification {
            kind: "learning".into(),
            category: "preferences".into(),
        };
    }
    if has(&[
        "remember", "fact", "uses", "lives", "deadline", "version", "port ",
    ]) {
        return Classification {
            kind: "memory".into(),
            category: "facts".into(),
        };
    }
    Classification {
        kind: "learning".into(),
        category: "general".into(),
    }
}

/// Result of an explicit `learn record` / MCP `learn_record`.
#[derive(Debug, Clone)]
pub enum Recorded {
    /// Written to the active learnings store.
    Learning {
        /// Learning id (`lrn_…`).
        id: String,
        /// True when a new bullet was appended.
        new: bool,
    },
    /// Appended to `memory.md` for the scope.
    Memory {
        /// Path of the memory file.
        path: PathBuf,
    },
    /// Soul / skill still go through the proposal gate.
    NeedsProposal,
}

/// Record an explicit note. Learnings and memories activate immediately;
/// soul and skill still return [`Recorded::NeedsProposal`].
pub fn record_note(
    project_dir: &Path,
    home: &Path,
    note: &str,
    scope: &str,
    as_kind: Option<&str>,
    origin: &str,
) -> Result<(Classification, Recorded), LearningsError> {
    let class = match as_kind {
        Some(kind) => Classification {
            kind: kind.to_string(),
            category: match kind {
                "soul" => "identity",
                "skill" => "procedures",
                "memory" => "facts",
                "learning" => "general",
                other => other,
            }
            .to_string(),
        },
        None => classify_note(note),
    };
    let recorded = match class.kind.as_str() {
        "memory" => {
            let path = append_memory_note(project_dir, home, scope, note)?;
            Recorded::Memory { path }
        }
        "soul" | "skill" => Recorded::NeedsProposal,
        _ => {
            let category = if class.category.is_empty() {
                "general"
            } else {
                class.category.as_str()
            };
            let (id, new) = activate_learning(project_dir, home, scope, note, category, origin)?;
            Recorded::Learning { id, new }
        }
    };
    if !matches!(recorded, Recorded::NeedsProposal) {
        maybe_complete_first_run(project_dir, home)?;
    }
    Ok((class, recorded))
}

/// Deterministic distiller: mine episodic checkpoints + hook spool for
/// recurring correction/preference statements and emit NEW candidates
/// (dedupe against existing active + candidates in both scopes).
pub fn distill(project_dir: &Path, home: &Path) -> Vec<Learning> {
    let mut statements: Vec<(String, String)> = Vec::new(); // (note, source)
    let episodic =
        std::fs::read_to_string(local_store::root(project_dir).join(local_store::EPISODIC_PATH))
            .unwrap_or_default();
    for line in episodic.lines() {
        if let Ok(record) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(note) = record.get("note").and_then(|v| v.as_str()) {
                statements.push((note.to_string(), "episodic".into()));
            }
        }
    }
    let spool =
        std::fs::read_to_string(local_store::root(project_dir).join("spool/observations.jsonl"))
            .unwrap_or_default();
    for line in spool.lines() {
        if let Ok(record) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(note) = record
                .get("note")
                .or_else(|| record.get("content"))
                .and_then(|v| v.as_str())
            {
                statements.push((note.to_string(), "spool".into()));
            }
        }
    }

    // Candidate-worthy sentences: sentences containing correction/preference
    // markers, counted for recurrence.
    let mut counts: std::collections::BTreeMap<(String, String), (usize, String, String)> =
        std::collections::BTreeMap::new();
    for (note, source) in &statements {
        for sentence in note.split(['.', '\n']) {
            let sentence = sentence.trim();
            if sentence.len() < 8 {
                continue;
            }
            let class = classify_note(sentence);
            if class.kind != "learning" {
                continue;
            }
            let normalized = normalize(sentence);
            let entry = counts
                .entry((normalized, class.category.clone()))
                .or_insert((0, sentence.to_string(), source.clone()));
            entry.0 += 1;
        }
    }

    let existing: Vec<String> = ["user", "project"]
        .iter()
        .flat_map(|scope| read_scope(project_dir, home, scope))
        .map(|l| normalize(&l.statement))
        .collect();

    counts
        .into_iter()
        .filter(|((normalized, category), (count, _, _))| {
            // Corrections/preferences mine on first sight; neutral general
            // notes must recur to be candidate-worthy.
            (category != "general" || *count >= 2) && !existing.contains(normalized)
        })
        .map(|((normalized, category), (count, sentence, source))| {
            let confidence = (0.3 + 0.2 * (count.saturating_sub(1) as f64)).min(0.85);
            let sources = if count > 1 {
                format!("{source} ×{count}")
            } else {
                source
            };
            let scope = infer_scope(&category, &sentence);
            let _ = normalized;
            Learning::candidate(&sentence, &category, confidence, &sources, scope)
        })
        .collect()
}

/// Whether a scope has any **active** learning (candidates do not count —
/// they are not inherited by the next harness).
pub fn scope_has_learnings(project_dir: &Path, home: &Path, scope: &str) -> bool {
    read_scope(project_dir, home, scope)
        .iter()
        .any(|l| l.status == "active")
}

/// First-run / empty-scope status used to instruct harnesses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapStatus {
    /// No project-scope learnings yet.
    pub project_needs_seed: bool,
    /// No user-global learnings yet.
    pub user_needs_seed: bool,
    /// This is the first harness session after `stateroot init`.
    pub first_session: bool,
}

/// Inspect project + user scopes (and the init first-run marker).
pub fn bootstrap_status(project_dir: &Path, home: &Path) -> BootstrapStatus {
    if !local_store::is_stateroot_dir(project_dir) {
        return BootstrapStatus {
            project_needs_seed: false,
            user_needs_seed: false,
            first_session: false,
        };
    }
    let marker = read_first_run(project_dir);
    let pending = marker
        .as_ref()
        .and_then(|v| v.get("pending").and_then(|p| p.as_bool()))
        .unwrap_or(true);
    let first_harness = marker
        .as_ref()
        .and_then(|v| v.get("first_harness"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    BootstrapStatus {
        project_needs_seed: !scope_has_learnings(project_dir, home, "project"),
        user_needs_seed: !scope_has_learnings(project_dir, home, "user"),
        first_session: pending && first_harness.is_none(),
    }
}

/// Instruction injected on session start / resume. Empty string is never
/// returned for a live project — at least the keep-current reminder is present.
pub fn compose_instruction(status: &BootstrapStatus) -> String {
    let mut out = String::new();
    if status.first_session || status.project_needs_seed || status.user_needs_seed {
        out.push_str("## Learnings (seed now)\n\n");
        if status.first_session {
            out.push_str(
                "You are the first harness in this project after `stateroot init`. Before other work, record learnings so later harnesses inherit them.\n\n",
            );
        }
        if status.user_needs_seed {
            out.push_str("**Global (user) learnings are empty.** Record taste that follows the user across projects: communication, recurring methods, design/engineering judgment, boundaries. CLI: `stateroot learn record --user \"<preference>\"`. MCP: `learn_record` with `scope: \"user\"`.\n\n");
        }
        if status.project_needs_seed {
            out.push_str("**Project learnings are empty.** Record this-repo conventions: stack, layout, constraints, how this codebase is built. CLI: `stateroot learn record \"<convention>\"`. MCP: `learn_record` with `scope: \"project\"` (default).\n\n");
        }
    } else {
        out.push_str("## Learnings (keep current)\n\n");
    }
    out.push_str(
        "Every harness updates both layers. Cross-project taste → `--user` / `scope: user`. This-repo conventions → project scope. Read first (`stateroot learnings list` and `stateroot learnings list --user`); update rather than duplicate. Learnings and memories take effect immediately so the next harness inherits them. Soul and skill changes still go through `stateroot proposals`.\n",
    );
    out
}

/// Record which harness ran first after init (idempotent).
pub fn record_first_session(project_dir: &Path, harness: &str) -> Result<bool, LearningsError> {
    let path = local_store::root(project_dir).join(local_store::FIRST_RUN_PATH);
    let mut marker = read_first_run(project_dir).unwrap_or_else(|| {
        serde_json::json!({
            "schema_version": "stateroot.first_run.v1",
            "pending": true,
        })
    });
    let already = marker
        .get("first_harness")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .is_some();
    if already {
        return Ok(false);
    }
    if let Some(obj) = marker.as_object_mut() {
        obj.insert("first_harness".into(), serde_json::json!(harness));
        obj.insert(
            "first_session_at".into(),
            serde_json::json!(local_store::now_rfc3339()),
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&marker).unwrap_or_default()
        ),
    )?;
    Ok(true)
}

/// Clear the pending first-run flag once both scopes have at least one learning,
/// or once the missing scope is the only one still empty after a seed attempt
/// is no longer first-session. Called after learnings are written.
pub fn maybe_complete_first_run(project_dir: &Path, home: &Path) -> Result<(), LearningsError> {
    let status = bootstrap_status(project_dir, home);
    if status.project_needs_seed || status.user_needs_seed {
        return Ok(());
    }
    let path = local_store::root(project_dir).join(local_store::FIRST_RUN_PATH);
    let Some(mut marker) = read_first_run(project_dir) else {
        return Ok(());
    };
    if marker.get("pending").and_then(|v| v.as_bool()) == Some(false) {
        return Ok(());
    }
    if let Some(obj) = marker.as_object_mut() {
        obj.insert("pending".into(), serde_json::json!(false));
        obj.insert(
            "completed_at".into(),
            serde_json::json!(local_store::now_rfc3339()),
        );
    }
    std::fs::write(
        path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&marker).unwrap_or_default()
        ),
    )?;
    Ok(())
}

fn read_first_run(project_dir: &Path) -> Option<serde_json::Value> {
    let path = local_store::root(project_dir).join(local_store::FIRST_RUN_PATH);
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Memory notes: `<scope root>/memory.md` — lines with optional
/// `<!-- visibility: private -->` marker. Shared by default for cli-authored
/// notes; foreign-origin notes land as session candidates (M3: recorded in
/// `_candidates/memory.md` and never rendered).
pub fn append_memory_note(
    project_dir: &Path,
    home: &Path,
    scope: &str,
    content: &str,
) -> Result<PathBuf, LearningsError> {
    let root = if scope == "user" {
        home.join(".stateroot")
    } else {
        local_store::root(project_dir)
    };
    let path = root.join("memory.md");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut body = std::fs::read_to_string(&path).unwrap_or_default();
    if !body.ends_with('\n') && !body.is_empty() {
        body.push('\n');
    }
    body.push_str(&format!("- {}\n", content.trim()));
    std::fs::write(&path, body)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> (tempfile::TempDir, tempfile::TempDir) {
        let project = tempfile::tempdir().expect("project");
        std::fs::create_dir_all(project.path().join(".stateroot")).unwrap();
        let home = tempfile::tempdir().expect("home");
        (project, home)
    }

    #[test]
    fn candidate_lifecycle_roundtrip() {
        let (project, home) = dirs();
        std::fs::create_dir_all(project.path().join(".stateroot")).unwrap();
        let (_root, _) = crate::roots::create_root(project.path(), "cli", "for learning pin", None)
            .expect("snap");
        let learning = Learning::candidate(
            "always re-run clippy after edits",
            "preferences",
            0.5,
            "test",
            "project",
        );
        assert!(append_candidate(project.path(), home.path(), "project", &learning).unwrap());
        // Duplicate append is a no-op.
        assert!(!append_candidate(project.path(), home.path(), "project", &learning).unwrap());
        assert!(promote(project.path(), home.path(), "project", &learning.id).unwrap());
        let active = read_scope(project.path(), home.path(), "project");
        let promoted = active
            .iter()
            .find(|l| l.id == learning.id && l.status == "active")
            .expect("active learning");
        assert!(!promoted.active_at_root.is_empty());
    }

    #[test]
    fn reject_archives_bullet() {
        let (project, home) = dirs();
        let learning =
            Learning::candidate("never skip tests", "preferences", 0.4, "test", "project");
        append_candidate(project.path(), home.path(), "project", &learning).unwrap();
        assert!(reject(project.path(), home.path(), "project", &learning.id[..12]).unwrap());
        let rejected = std::fs::read_to_string(
            project
                .path()
                .join(".stateroot/learnings")
                .join(REJECTED_FILE),
        )
        .unwrap();
        assert!(rejected.contains("status: rejected"));
    }

    #[test]
    fn classifier_lanes() {
        assert_eq!(classify_note("you are a careful reviewer").kind, "soul");
        assert_eq!(
            classify_note("how to rotate the spool safely").kind,
            "skill"
        );
        assert_eq!(classify_note("actually the port is 9060").kind, "learning");
        assert_eq!(
            classify_note("actually the port is 9060").category,
            "corrections"
        );
        assert_eq!(classify_note("the deploy uses systemd").kind, "memory");
        assert_eq!(classify_note("prefer small diffs").category, "preferences");
    }

    #[test]
    fn distiller_mines_recurrence_and_dedupes() {
        let (project, home) = dirs();
        let episodic = project.path().join(".stateroot/memories");
        std::fs::create_dir_all(&episodic).unwrap();
        std::fs::write(
            episodic.join("episodic.jsonl"),
            concat!(
                r#"{"ts":"t1","harness":"cli","note":"actually the port is 9060"}"#,
                "\n",
                r#"{"ts":"t2","harness":"cli","note":"actually, the port is 9060!"}"#,
                "\n",
                r#"{"ts":"t3","harness":"cli","note":"neutral status update"}"#,
                "\n"
            ),
        )
        .unwrap();
        let found = distill(project.path(), home.path());
        assert_eq!(found.len(), 1, "one new candidate: {found:?}");
        assert!(found[0].confidence > 0.3, "recurrence bumps confidence");
        assert!(
            found[0].sources.contains('×'),
            "recurrence recorded: {}",
            found[0].sources
        );
        // Second distill produces nothing (already distilled content isn't re-mined? no —
        // candidates already exist → dedupe).
        append_candidate(project.path(), home.path(), "project", &found[0]).unwrap();
        assert!(distill(project.path(), home.path()).is_empty());
    }

    #[test]
    fn bootstrap_seeds_both_scopes_until_populated() {
        let project = tempfile::tempdir().expect("project");
        let home = tempfile::tempdir().expect("home");
        crate::local_store::init_skeleton(project.path(), "p1", "demo", "default").unwrap();
        let status = bootstrap_status(project.path(), home.path());
        assert!(status.project_needs_seed);
        assert!(status.user_needs_seed);
        assert!(status.first_session);
        let instruction = compose_instruction(&status);
        assert!(instruction.contains("first harness"));
        assert!(instruction.contains("Global (user) learnings are empty"));
        assert!(instruction.contains("Project learnings are empty"));
        assert!(instruction.contains("learn record --user"));

        assert!(record_first_session(project.path(), "cursor").unwrap());
        assert!(!record_first_session(project.path(), "codex").unwrap());
        let after = bootstrap_status(project.path(), home.path());
        assert!(!after.first_session);
        assert!(after.project_needs_seed);

        activate_learning(
            project.path(),
            home.path(),
            "project",
            "this repo uses uv",
            "general",
            "test",
        )
        .unwrap();
        activate_learning(
            project.path(),
            home.path(),
            "user",
            "prefer small diffs",
            "preferences",
            "test",
        )
        .unwrap();
        maybe_complete_first_run(project.path(), home.path()).unwrap();
        let done = bootstrap_status(project.path(), home.path());
        assert!(!done.project_needs_seed);
        assert!(!done.user_needs_seed);
        let keep = compose_instruction(&done);
        assert!(keep.contains("keep current"));
        assert!(keep.contains("--user"));
        assert!(!keep.contains("are empty"));
    }

    #[test]
    fn record_note_activates_learning_and_memory() {
        let (project, home) = dirs();
        std::fs::create_dir_all(project.path().join(".stateroot")).unwrap();
        let (class, recorded) = record_note(
            project.path(),
            home.path(),
            "prefer small diffs over rewrites",
            "user",
            None,
            "test",
        )
        .unwrap();
        assert_eq!(class.kind, "learning");
        let Recorded::Learning { id, new } = recorded else {
            panic!("expected active learning, got {recorded:?}");
        };
        assert!(new);
        let active = read_scope(project.path(), home.path(), "user");
        assert!(
            active.iter().any(|l| l.id == id && l.status == "active"),
            "{active:?}"
        );

        let (class, recorded) = record_note(
            project.path(),
            home.path(),
            "the deploy uses systemd",
            "project",
            None,
            "test",
        )
        .unwrap();
        assert_eq!(class.kind, "memory");
        let Recorded::Memory { path } = recorded else {
            panic!("expected memory, got {recorded:?}");
        };
        let body = std::fs::read_to_string(path).unwrap();
        assert!(body.contains("deploy uses systemd"));

        let (class, recorded) = record_note(
            project.path(),
            home.path(),
            "you are a careful reviewer",
            "project",
            None,
            "test",
        )
        .unwrap();
        assert_eq!(class.kind, "soul");
        assert!(matches!(recorded, Recorded::NeedsProposal));
    }

    #[test]
    fn infer_scope_splits_global_taste_from_repo_conventions() {
        assert_eq!(infer_scope("preferences", "prefer small diffs"), "user");
        assert_eq!(
            infer_scope("preferences", "prefer small diffs in this repo"),
            "project"
        );
        assert_eq!(
            infer_scope("corrections", "actually the port is 9060"),
            "project"
        );
    }
}
