//! Local learnings + memory notes (M3).
//!
//! Same category-md format as the server variant:
//! `- **statement** <!-- id: …; confidence: 0.7; label: observed; sources: …; scope: …; status: … -->`
//!
//! Scopes: user (`~/.stateroot/learnings/`), workspace
//! (`~/.stateroot/workspaces/{id}/learnings/`), project
//! (`.stateroot/learnings/`), and domain (`~/.stateroot/domains/{slug}/learnings/`).
//! Explicit `learn record` / MCP `learn_record` always writes a **learning**.
//! Facts go to `memory_save` / `memory.md`. Distill persists mined notes as
//! active project/general learnings (no keyword classification).
//!
//! Memory notes share the scoping ladder: `memory.md` per scope with
//! `<!-- visibility: shared|private -->` markers on the note lines.

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

/// Resolve workspace id for a project (registry, then manifest project_id).
pub fn workspace_id_for(project_dir: &Path) -> Option<String> {
    if let Ok(config_dir) = crate::config::config_dir() {
        if let Ok(registry) = crate::config::load_registry(&config_dir) {
            let key = project_dir
                .canonicalize()
                .unwrap_or_else(|_| project_dir.to_path_buf());
            let key_s = key.to_string_lossy().to_string();
            if let Some(entry) = registry.projects.get(&key_s) {
                if !entry.workspace_id.is_empty() {
                    return Some(entry.workspace_id.clone());
                }
            }
            // Also try non-canonical key (tests often use temp paths as-is).
            let raw = project_dir.to_string_lossy().to_string();
            if let Some(entry) = registry.projects.get(&raw) {
                if !entry.workspace_id.is_empty() {
                    return Some(entry.workspace_id.clone());
                }
            }
        }
    }
    local_store::read_manifest(project_dir)
        .ok()
        .flatten()
        .and_then(|m| {
            m.get("workspace_id")
                .or_else(|| m.get("project_id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .filter(|s| !s.is_empty())
}

/// Bound domain slug from the project manifest (`domain` field), if any.
pub fn bound_domain(project_dir: &Path) -> Option<String> {
    local_store::read_manifest(project_dir)
        .ok()
        .flatten()
        .and_then(|m| m.get("domain").and_then(|v| v.as_str()).map(str::to_string))
        .filter(|s| !s.is_empty())
}

/// Scope root for `user` | `workspace` | `domain:<slug>` | `project`.
fn scope_root(project_dir: &Path, home: &Path, scope: &str) -> PathBuf {
    if scope == "user" {
        return home.join(".stateroot").join(LEARNINGS_DIR);
    }
    if scope == "workspace" {
        let ws = workspace_id_for(project_dir).unwrap_or_else(|| "default".into());
        return home
            .join(".stateroot")
            .join("workspaces")
            .join(ws)
            .join(LEARNINGS_DIR);
    }
    if let Some(slug) = scope.strip_prefix("domain:") {
        return home
            .join(".stateroot")
            .join("domains")
            .join(slug)
            .join(LEARNINGS_DIR);
    }
    if scope == "domain" {
        let slug = bound_domain(project_dir).unwrap_or_else(|| "default".into());
        return home
            .join(".stateroot")
            .join("domains")
            .join(slug)
            .join(LEARNINGS_DIR);
    }
    local_store::root(project_dir).join(LEARNINGS_DIR)
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
/// MCP `learn_record` and distill both activate so the next harness inherits
/// the note. Dedupes by normalized statement; promoting an existing candidate if needed.
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

/// Category stem for an explicit learning. Defaults to `general` — no
/// keyword routing. Callers that already chose a stem may pass it via
/// [`activate_learning`] directly.
pub fn learning_category(_note: &str) -> &'static str {
    "general"
}

/// Record an explicit learning. Always a learning — the caller chose this
/// tool. Facts belong on `memory_save`; identity on soul; procedures on
/// `skill_propose`. Scope comes from the caller's flag, never from keywords.
pub fn record_note(
    project_dir: &Path,
    home: &Path,
    note: &str,
    scope: &str,
    origin: &str,
) -> Result<(String, bool, &'static str), LearningsError> {
    let category = learning_category(note);
    let (id, new) = activate_learning(project_dir, home, scope, note, category, origin)?;
    maybe_complete_first_run(project_dir, home)?;
    Ok((id, new, category))
}

/// Deterministic distiller: mine episodic checkpoints + hook spool and emit
/// unique statement strings for the wiki inbox (does **not** activate learnings).
/// Taste still goes through `learn_record` / `record_note`.
/// No keyword classification into soul/skill/memory.
pub fn distill(project_dir: &Path, home: &Path) -> Vec<Learning> {
    distill_statements(project_dir, home)
        .into_iter()
        .map(|(sentence, sources, confidence)| {
            Learning::candidate(&sentence, "general", confidence, &sources, "project")
        })
        .collect()
}

/// Mine unique statements from episodic + spool (normalized dedupe against
/// existing learnings and existing inbox bullets). Returns
/// `(statement, sources, confidence)`.
pub fn distill_statements(project_dir: &Path, home: &Path) -> Vec<(String, String, f64)> {
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

    let mut counts: std::collections::BTreeMap<String, (usize, String, String)> =
        std::collections::BTreeMap::new();
    for (note, source) in &statements {
        for sentence in note.split(['.', '\n']) {
            let sentence = sentence.trim();
            if sentence.len() < 8 {
                continue;
            }
            let normalized = normalize(sentence);
            let entry =
                counts
                    .entry(normalized)
                    .or_insert((0, sentence.to_string(), source.clone()));
            entry.0 += 1;
        }
    }

    let mut existing: std::collections::BTreeSet<String> =
        ["user", "workspace", "project", "domain"]
            .iter()
            .flat_map(|scope| read_scope(project_dir, home, scope))
            .map(|l| normalize(&l.statement))
            .collect();
    if let Some(slug) = bound_domain(project_dir) {
        existing.extend(
            read_scope(project_dir, home, &format!("domain:{slug}"))
                .into_iter()
                .map(|l| normalize(&l.statement)),
        );
    }
    // Also skip bullets already in the wiki inbox.
    let inbox = local_store::root(project_dir)
        .join(crate::wiki::PAGES_DIR)
        .join(crate::wiki::INBOX_PAGE);
    if let Ok(text) = std::fs::read_to_string(inbox) {
        for line in text.lines() {
            if let Some(b) = line.trim().strip_prefix("- ") {
                existing.insert(normalize(b));
            }
        }
    }

    counts
        .into_iter()
        .filter(|(normalized, _)| !existing.contains(normalized))
        .map(|(_normalized, (count, sentence, source))| {
            let confidence = (0.3 + 0.2 * (count.saturating_sub(1) as f64)).min(0.85);
            let sources = if count > 1 {
                format!("{source} ×{count}")
            } else {
                source
            };
            (sentence, sources, confidence)
        })
        .collect()
}

/// Run deterministic compile into the wiki inbox. Returns bullets added.
pub fn distill_to_inbox(project_dir: &Path, home: &Path) -> Result<usize, LearningsError> {
    let stmts = distill_statements(project_dir, home);
    let bullets: Vec<String> = stmts.into_iter().map(|(s, _, _)| s).collect();
    crate::wiki::append_inbox_bullets(project_dir, &bullets)
        .map_err(|e| LearningsError::Io(std::io::Error::other(e.to_string())))
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
                "You are the first harness in this project after `stateroot init`. Before other work, seed learnings as **judgment rules** another harness can apply — not a stack inventory.\n\n",
            );
        }
        if status.user_needs_seed {
            out.push_str("**Global (user) learnings are empty.** Record 2–7 taste notes that follow this human across projects (communication, methods, design/engineering judgment). CLI: `stateroot learn record --user \"Prefer small, reviewable diffs over rewrites. Do not restyle adjacent files.\"`. MCP: `learn_record` with `scope: \"user\"`.\n\n");
        }
        if status.project_needs_seed {
            out.push_str("**Project learnings are empty.** Record 2–7 quality bars / preferred patterns / anti-patterns for *this* repo. Not \"this is a TypeScript monorepo\" (that is `memory_save`). CLI: `stateroot learn record \"<judgment>. <when / what never>.\"`. MCP: `learn_record` with `scope: \"project\"`.\n\n");
        }
    } else {
        out.push_str("## Learnings (keep current)\n\n");
    }
    out.push_str(
        "Format: `<prefer X over Y / never Z>. <when it applies and what not to do>.` Read first (`stateroot learnings list` and `stateroot learnings list --user`); update rather than duplicate. `learn record` always writes a learning. Facts go to `memory_save`.\n",
    );
    out
}

/// One active shared learning to surface beside work-state lineage in digests.
pub fn highlight_for_digest(project_dir: &Path, home: &Path) -> Option<String> {
    let latest = crate::roots::latest_root(project_dir).ok().flatten();
    let mut pool: Vec<Learning> = Vec::new();
    for scope in ["project", "user", "workspace"] {
        pool.extend(read_scope(project_dir, home, scope));
    }
    if let Some(slug) = bound_domain(project_dir) {
        pool.extend(read_scope(project_dir, home, &format!("domain:{slug}")));
    }
    let active: Vec<&Learning> = pool
        .iter()
        .filter(|l| l.status == "active" && l.superseded_by.is_empty())
        .collect();
    if active.is_empty() {
        return None;
    }
    let tied = latest.as_ref().and_then(|latest| {
        active
            .iter()
            .find(|l| !l.active_at_root.is_empty() && l.active_at_root == *latest)
    });
    let pick = tied.or_else(|| {
        active.iter().find(|l| l.scope == "user").or_else(|| {
            active.iter().max_by(|a, b| {
                a.confidence
                    .partial_cmp(&b.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        })
    })?;
    Some(format!(
        "Shared learning ({}): {}",
        pick.scope, pick.statement
    ))
}

/// Active learnings across inherited scopes for resume/hook digests.
pub fn collect_active_for_digest(project_dir: &Path, home: &Path) -> Vec<Learning> {
    let mut pool: Vec<Learning> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for scope in ["project", "user", "workspace"] {
        for learning in read_scope(project_dir, home, scope) {
            if learning.status == "active"
                && learning.superseded_by.is_empty()
                && seen.insert(learning.id.clone())
            {
                pool.push(learning);
            }
        }
    }
    if let Some(slug) = bound_domain(project_dir) {
        for learning in read_scope(project_dir, home, &format!("domain:{slug}")) {
            if learning.status == "active"
                && learning.superseded_by.is_empty()
                && seen.insert(learning.id.clone())
            {
                pool.push(learning);
            }
        }
    }
    pool.sort_by(|left, right| {
        right
            .confidence
            .partial_cmp(&left.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    pool
}

/// Render the Durable Preferences block for resume/hook digests.
pub fn compose_durable_preferences_section(learnings: &[Learning]) -> String {
    if learnings.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Durable Preferences\n\n");
    for learning in learnings {
        out.push_str(&format!(
            "- {} ({:.2}, {})\n",
            learning.statement, learning.confidence, learning.scope
        ));
    }
    out.push('\n');
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

/// Memory notes: curated hot apex at `memories/MEMORY.md` (project) via
/// [`crate::hot_apex`]. Legacy `.stateroot/memory.md` is migrated once.
pub fn append_memory_note(
    project_dir: &Path,
    home: &Path,
    scope: &str,
    content: &str,
) -> Result<PathBuf, LearningsError> {
    crate::hot_apex::ensure_migrated(project_dir, home);
    // User-scope facts still land in project MEMORY (facts are project-portable);
    // the memory tool's `target=user` is for USER.md profile notes.
    let _ = scope;
    match crate::hot_apex::add(project_dir, home, "memory", content, false) {
        Ok(result) => {
            if let Some(path) = result.path {
                Ok(path)
            } else {
                Ok(local_store::root(project_dir).join(local_store::MEMORY_CORE_PATH))
            }
        }
        Err(err) => Err(LearningsError::Io(std::io::Error::other(err.to_string()))),
    }
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
    fn learning_category_defaults_to_general() {
        assert_eq!(learning_category("actually the port is 9060"), "general");
        assert_eq!(learning_category("prefer small diffs"), "general");
        assert_eq!(
            learning_category("Laiq is a TypeScript/Python monorepo"),
            "general"
        );
    }

    #[test]
    fn distiller_mines_notes_and_dedupes() {
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
        assert!(
            found.len() >= 2,
            "mines sentences without keyword skip: {found:?}"
        );
        assert!(found
            .iter()
            .all(|l| l.category == "general" && l.scope == "project"));
        // After activating, distill dedupes.
        for note in &found {
            activate_learning(
                project.path(),
                home.path(),
                "project",
                &note.statement,
                "general",
                &note.sources,
            )
            .unwrap();
        }
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
        assert!(instruction.contains("judgment"));
        assert!(
            !instruction.contains("stack, layout"),
            "must not invite inventory dumps: {instruction}"
        );

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
    fn record_note_always_writes_a_learning() {
        let (project, home) = dirs();
        std::fs::create_dir_all(project.path().join(".stateroot")).unwrap();
        let (id, new, category) = record_note(
            project.path(),
            home.path(),
            "prefer small diffs over rewrites",
            "user",
            "test",
        )
        .unwrap();
        assert!(new);
        assert_eq!(category, "general");
        let active = read_scope(project.path(), home.path(), "user");
        assert!(
            active.iter().any(|l| l.id == id && l.status == "active"),
            "{active:?}"
        );

        let (id, new, category) = record_note(
            project.path(),
            home.path(),
            "the deploy uses systemd",
            "project",
            "test",
        )
        .unwrap();
        assert!(new);
        assert_eq!(category, "general");
        let active = read_scope(project.path(), home.path(), "project");
        assert!(
            active.iter().any(|l| l.id == id && l.status == "active"),
            "{active:?}"
        );

        let (_, new, _) = record_note(
            project.path(),
            home.path(),
            "you are a careful reviewer",
            "project",
            "test",
        )
        .unwrap();
        assert!(new);
        let active = read_scope(project.path(), home.path(), "project");
        assert!(
            active
                .iter()
                .any(|l| l.statement.contains("careful reviewer") && l.status == "active"),
            "{active:?}"
        );
    }

    #[test]
    fn scope_flag_not_keyword_routing() {
        let (project, home) = dirs();
        std::fs::create_dir_all(project.path().join(".stateroot")).unwrap();
        let (_, _, _) = record_note(
            project.path(),
            home.path(),
            "prefer small diffs",
            "user",
            "test",
        )
        .unwrap();
        let user = read_scope(project.path(), home.path(), "user");
        assert!(user.iter().any(|l| l.statement.contains("prefer small")));
        let (_, _, _) = record_note(
            project.path(),
            home.path(),
            "prefer small diffs in this repo",
            "project",
            "test",
        )
        .unwrap();
        let project_notes = read_scope(project.path(), home.path(), "project");
        assert!(project_notes
            .iter()
            .any(|l| l.statement.contains("this repo")));
    }

    #[test]
    fn workspace_and_domain_scopes_are_isolated() {
        let (project, home) = dirs();
        std::fs::create_dir_all(project.path().join(".stateroot")).unwrap();
        let (_, new_ws, _) = record_note(
            project.path(),
            home.path(),
            "workspace-wide bar",
            "workspace",
            "test",
        )
        .expect("workspace record");
        assert!(new_ws);
        let (_, new_dom, _) = record_note(
            project.path(),
            home.path(),
            "domain-specific bar",
            "domain:rust",
            "test",
        )
        .expect("domain record");
        assert!(new_dom);
        assert!(read_scope(project.path(), home.path(), "workspace")
            .iter()
            .any(|l| l.statement.contains("workspace-wide")));
        assert!(read_scope(project.path(), home.path(), "domain:rust")
            .iter()
            .any(|l| l.statement.contains("domain-specific")));
        assert!(read_scope(project.path(), home.path(), "project")
            .iter()
            .all(|l| !l.statement.contains("workspace-wide")));
        let digest = collect_active_for_digest(project.path(), home.path());
        assert!(digest.iter().any(|l| l.scope == "workspace"));
        // domain:rust is readable but only enters digests when the project manifest binds that slug.
        assert!(!digest.iter().any(|l| l.scope == "domain:rust"));
    }
}
