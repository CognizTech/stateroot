//! Local soul store (M3): user-global canonical at `~/.stateroot/soul/`
//! (versioned via history snapshots, provenance headers) + project overlay at
//! `<project>/.stateroot/soul/OVERLAY.md`. Evolution is approval-gated — the
//! only writers here are `soul import/generate/edit` (user-authoring) and
//! `proposals approve` (everything else).
//!
//! The per-harness projection ports the server renderer's deterministic
//! rules (`app/core/stateroot/soul_format.py`): useful-heads section filter,
//! voice-mask + comment line removal, prose fallback, per-harness line caps
//! and a framing-derived emphasis line.

use std::path::{Path, PathBuf};

use crate::local_store::{self, now_rfc3339};

/// Directory name under home / project holding the soul.
pub const SOUL_DIR: &str = ".stateroot/soul";
/// Canonical file name.
pub const CANONICAL_FILE: &str = "SOUL.md";
/// Overlay file name (project scope).
pub const OVERLAY_FILE: &str = "OVERLAY.md";
/// History subdir for versioned snapshots.
pub const HISTORY_DIR: &str = "history";

/// Errors from the soul store.
#[derive(Debug, thiserror::Error)]
pub enum SoulError {
    /// Local filesystem failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Import source produced nothing usable.
    #[error("{0}")]
    Empty(String),
}

fn canonical_path(home: &Path) -> PathBuf {
    home.join(SOUL_DIR).join(CANONICAL_FILE)
}

fn overlay_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir)
        .join("soul")
        .join(OVERLAY_FILE)
}

/// Read the user-global canonical soul, if present.
pub fn read_canonical(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(canonical_path(home)).ok()?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Read the project overlay, if present.
pub fn read_overlay(project_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(overlay_path(project_dir)).ok()?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Write the canonical soul, snapshotting any existing version into
/// `history/<ts>.md` first (append-only versioning). `origin` becomes part of
/// the provenance header when the content lacks one. Returns a human note.
pub fn write_canonical(
    home: &Path,
    content: &str,
    origin: Option<&str>,
) -> Result<String, SoulError> {
    let dir = home.join(SOUL_DIR);
    std::fs::create_dir_all(&dir)?;
    let path = canonical_path(home);
    let mut snapshot = String::new();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if !existing.trim().is_empty() {
            let history = dir.join(HISTORY_DIR);
            std::fs::create_dir_all(&history)?;
            let stamp = now_rfc3339().replace([':', '-'], "");
            let snap = history.join(format!("{stamp}.md"));
            std::fs::write(&snap, &existing)?;
            snapshot = format!(" (previous version → {})", snap.display());
        }
    }
    let mut body = content.trim().to_string();
    if let Some(origin) = origin {
        if !body.contains("<!-- stateroot:soul") && !body.contains("<!-- imported from") {
            body = format!(
                "<!-- stateroot:soul origin={}; at={} -->\n{}",
                origin.replace([';', '\n'], " "),
                now_rfc3339(),
                body
            );
        }
    }
    std::fs::write(&path, format!("{body}\n"))?;
    Ok(format!("soul written to {}{snapshot}", path.display()))
}

/// Import sources for `soul import`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSource {
    /// OpenClaw identity pack (lifted `openclaw_identity` discovery).
    Openclaw,
    /// Hermes `~/.hermes/SOUL.md`.
    Hermes,
}

/// Load importable soul content with provenance. Returns (content, origin).
pub fn import(home: &Path, source: ImportSource) -> Result<(String, String), SoulError> {
    match source {
        ImportSource::Openclaw => {
            let packs = crate::openclaw_identity::discover_openclaw_identities(home);
            let Some(pack) = packs.first() else {
                return Err(SoulError::Empty(
                    "no openclaw identity pack found (SOUL/IDENTITY/USER.md in an openclaw workspace)"
                        .into(),
                ));
            };
            Ok((pack.identity_markdown.clone(), "openclaw".into()))
        }
        ImportSource::Hermes => {
            let path = home.join(".hermes/SOUL.md");
            let text = std::fs::read_to_string(&path)
                .map_err(|_| SoulError::Empty(format!("no hermes soul at {}", path.display())))?;
            let text = text.trim().to_string();
            if text.is_empty() {
                return Err(SoulError::Empty(format!(
                    "empty hermes soul at {}",
                    path.display()
                )));
            }
            Ok((text, "hermes-agent".into()))
        }
    }
}

/// Wrap imported/generated content in a provenance header (never doubled).
pub fn with_provenance(content: &str, origin: &str) -> String {
    if content.contains("<!-- imported from") || content.contains("<!-- stateroot:soul") {
        return content.trim().to_string();
    }
    format!(
        "<!-- imported from {} on {} by stateroot soul import -->\n{}",
        origin,
        now_rfc3339(),
        content.trim()
    )
}

/// Answers for the deterministic generate flow (same Q&A as the server
/// variant's `draft_soul_from_answers`).
#[derive(Debug, Clone, Default)]
pub struct GenerateAnswers {
    /// Tone / communication style.
    pub tone: String,
    /// How proactive the agent may be.
    pub initiative: String,
    /// Explanation depth.
    pub depth: String,
    /// Boundaries (privacy / identity / global behavior).
    pub boundaries: String,
    /// Principles.
    pub principles: String,
    /// Disagreement handling.
    pub disagreement: String,
    /// Desired example (optional).
    pub desired: String,
    /// Undesired example (optional).
    pub undesired: String,
}

/// Deterministic soul draft (ported template, zero model calls).
pub fn draft_from_answers(answers: &GenerateAnswers) -> String {
    let tone = default_str(&answers.tone, "direct and concise");
    let initiative = default_str(
        &answers.initiative,
        "medium — propose next steps, wait for go-ahead",
    );
    let depth = default_str(&answers.depth, "enough to decide, not a lecture");
    let boundaries = default_str(
        &answers.boundaries,
        "do not silently change identity, privacy, or global behavior",
    );
    let principles = default_str(
        &answers.principles,
        "optimize for correctness and the user's stated goal",
    );
    let disagreement = default_str(
        &answers.disagreement,
        "state the disagreement once with evidence, then follow the user",
    );
    let mut lines = vec![
        format!(
            "<!-- stateroot:soul origin=generate; source=generate; at={} -->",
            now_rfc3339()
        ),
        "# Soul".into(),
        String::new(),
        "## Communication".into(),
        format!("- Tone: {tone}"),
        format!("- Initiative: {initiative}"),
        format!("- Explanation depth: {depth}"),
        String::new(),
        "## Principles".into(),
        format!("- {principles}"),
        String::new(),
        "## Boundaries".into(),
        format!("- {boundaries}"),
        String::new(),
        "## Disagreement handling".into(),
        format!("- {disagreement}"),
        String::new(),
        "## Quality standard".into(),
        "- Prefer verifiable work over guesswork. Do not imitate a celebrity or fictional voice."
            .into(),
    ];
    if !answers.desired.trim().is_empty() {
        lines.extend([
            String::new(),
            "## Desired behavior".into(),
            format!("- {}", answers.desired.trim()),
        ]);
    }
    if !answers.undesired.trim().is_empty() {
        lines.extend([
            String::new(),
            "## Undesired behavior".into(),
            format!("- {}", answers.undesired.trim()),
        ]);
    }
    lines.join("\n").trim_end().to_string() + "\n"
}

fn default_str(value: &str, default: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

const USEFUL_HEADS: &[&str] = &[
    "principle",
    "boundar",
    "disagreement",
    "quality",
    "standard",
    "prefer",
    "initiative",
    "explanation",
    "communication",
    "style",
    "tone",
    "working relationship",
    "persona",
    "identity",
    "user",
];

const VOICE_MASK_MARKERS: &[&str] = &[
    "sound like",
    "talk like",
    "imitate",
    "in the voice of",
    "as if you were",
    "persona of",
];

fn is_voice_mask(line: &str) -> bool {
    let lower = line.to_lowercase();
    VOICE_MASK_MARKERS.iter().any(|m| lower.contains(m))
}

fn is_useful_heading(heading: &str) -> bool {
    let lower = heading.to_lowercase();
    USEFUL_HEADS.iter().any(|h| lower.contains(h))
}

/// Compact working-relationship projection. Harness-aware per the server
/// rules: identical content, per-harness caps and an emphasis line.
pub fn render_projection(soul: &str, harness: Option<&str>) -> String {
    let trimmed = soul.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut kept = vec!["## Working relationship".to_string(), String::new()];
    let mut heading = String::new();
    let mut capture: Vec<String> = Vec::new();
    let lines: Vec<&str> = trimmed.lines().collect();
    for line in lines.iter() {
        if line.starts_with('#') {
            flush_section(&mut kept, &heading, &capture);
            heading = line.trim_start_matches('#').trim().to_string();
            capture.clear();
            continue;
        }
        if is_voice_mask(line) || line.starts_with("<!--") {
            continue;
        }
        capture.push(line.to_string());
    }
    flush_section(&mut kept, &heading, &capture);
    if kept.len() <= 2 {
        let prose: Vec<String> = lines
            .iter()
            .filter(|line| {
                !line.trim().is_empty()
                    && !line.starts_with('#')
                    && !line.starts_with("<!--")
                    && !is_voice_mask(line)
            })
            .take(12)
            .map(|line| line.to_string())
            .collect();
        kept = vec!["## Working relationship".to_string(), String::new()];
        kept.extend(prose);
    }
    while matches!(kept.last(), Some(l) if l.trim().is_empty()) {
        kept.pop();
    }
    match harness {
        Some(harness) => present_for_harness(kept, harness),
        None => kept.join("\n").trim_end().to_string() + "\n",
    }
}

fn flush_section(kept: &mut Vec<String>, heading: &str, capture: &[String]) {
    let body = capture.join("\n").trim().to_string();
    if body.is_empty() || heading.is_empty() {
        return;
    }
    if !is_useful_heading(heading) {
        return;
    }
    kept.push(format!("### {heading}"));
    kept.push(String::new());
    kept.push(body);
}

/// Per-harness presentation bounds (content lines identical — only emphasis
/// and the cap differ). Ported from the server renderer.
fn harness_line_cap(harness: &str) -> Option<usize> {
    match crate::skill_federation::normalize_harness(harness).as_str() {
        "skillsagent" => None,
        "claude" | "codex" => Some(16),
        "cursor" => Some(14),
        "openclaw" | "hermes" => Some(12),
        "kimi" => Some(8),
        _ => Some(16),
    }
}

fn harness_emphasis_line(harness: &str) -> String {
    let id = crate::skill_federation::normalize_harness(harness);
    let Ok(reg) = crate::skill_federation::load_registry() else {
        return String::new();
    };
    let Some(entry) = reg.harnesses.iter().find(|h| h.id == id) else {
        return String::new();
    };
    let framing = entry.framing.as_str();
    let text = framing
        .split(':')
        .nth(1)
        .map(str::trim)
        .unwrap_or(framing.trim());
    let text = text
        .strip_prefix("Emphasize ")
        .or_else(|| text.strip_prefix("emphasize "))
        .unwrap_or(text);
    if text.is_empty() {
        String::new()
    } else {
        format!("Priority: {text}")
    }
}

fn present_for_harness(kept: Vec<String>, harness: &str) -> String {
    let body: Vec<String> = kept.into_iter().skip(2).collect();
    let body: Vec<String> = match harness_line_cap(harness) {
        Some(cap) => body
            .into_iter()
            .filter(|line| !line.trim().is_empty())
            .take(cap)
            .collect(),
        None => body,
    };
    let mut out = vec!["## Working relationship".to_string(), String::new()];
    let emphasis = harness_emphasis_line(harness);
    if !emphasis.is_empty() {
        out.push(emphasis);
        out.push(String::new());
    }
    out.extend(body);
    out.join("\n").trim_end().to_string() + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answers() -> GenerateAnswers {
        GenerateAnswers {
            tone: "warm but terse".into(),
            ..Default::default()
        }
    }

    #[test]
    fn draft_uses_answers_and_defaults() {
        let draft = draft_from_answers(&answers());
        assert!(draft.contains("stateroot:soul origin=generate"));
        assert!(draft.contains("- Tone: warm but terse"));
        assert!(draft.contains("do not silently change identity"));
    }

    #[test]
    fn write_canonical_snapshots_history() {
        let tmp = tempfile::tempdir().expect("tmp");
        let home = tmp.path();
        write_canonical(home, "# Soul\n\nv1", None).expect("v1");
        write_canonical(home, "# Soul\n\nv2", None).expect("v2");
        assert_eq!(read_canonical(home).as_deref(), Some("# Soul\n\nv2"));
        let history: Vec<_> = std::fs::read_dir(home.join(SOUL_DIR).join(HISTORY_DIR))
            .unwrap()
            .collect();
        assert_eq!(history.len(), 1, "one snapshot");
    }

    #[test]
    fn projection_filters_and_caps_per_harness() {
        let soul = "# Soul\n\n## Communication\n\n- Tone: direct\n\n## Gossip\n\n- chatter\n\n## Principles\n\n- be correct\n";
        let plain = render_projection(soul, None);
        assert!(plain.contains("### Communication"));
        assert!(plain.contains("### Principles"));
        assert!(!plain.contains("Gossip"), "{plain}");
        let kimi = render_projection(soul, Some("kimi"));
        assert!(kimi.starts_with("## Working relationship"));
        let body_lines = kimi.lines().filter(|l| !l.trim().is_empty()).count();
        assert!(body_lines <= 8 + 2, "kimi cap: {kimi}");
    }
}
