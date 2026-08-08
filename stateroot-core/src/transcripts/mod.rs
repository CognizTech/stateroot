//! Native harness transcript readers (plan: native transcript import).
//!
//! Each reader walks one harness's on-disk session store and normalizes
//! sessions into [`TranscriptSession`]. Doctrine: NO deliberate
//! secret-pattern scanning or scrubbing anywhere — extracted text is
//! stored verbatim (privacy is declared by the user's OWN ignore files,
//! respected at the sync boundary). Harness-noise filtering
//! (INJECTED_PREFIXES, reasoning blobs, injected envelopes) stays — that
//! is structure, not secret-scanning. Empty stays empty.

pub mod bundle;
pub mod claude;
pub mod codex;
pub mod cursor;
pub mod hermes;
pub mod kimi;
pub mod openclaw;

use std::path::{Path, PathBuf};

/// Session outcome classification (deterministic, per-harness heuristics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Outcome {
    /// The session reached an assistant finale / completion event.
    Completed,
    /// The session stopped mid-flight (tool call without output, or no
    /// assistant finale at all).
    Interrupted,
    /// Cannot tell (empty or unparseable tail).
    #[default]
    Unknown,
}

impl Outcome {
    /// Stable label used in payloads and output.
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Completed => "completed",
            Outcome::Interrupted => "interrupted",
            Outcome::Unknown => "unknown",
        }
    }
}

/// One plan item from the LATEST `update_plan` call (status verbatim:
/// completed / in_progress / pending) — the residual-work view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    /// Step title (sanitized).
    pub step: String,
    /// Status exactly as recorded by the harness.
    pub status: String,
}

/// One conversation-tail message (role + text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailEntry {
    /// `user` or `assistant`.
    pub role: &'static str,
    /// Message text (sanitized, capped).
    pub text: String,
}

/// One intentionally-excluded artifact (B1): recorded so gaps are truthful
/// instead of silent (the ai-memory loss pattern).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LossNote {
    /// What was excluded (e.g. `compaction_summary`, `tool_metadata`).
    pub what: String,
    /// Why (e.g. `encrypted by harness`, `shape unverified`).
    pub reason: String,
}

/// One normalized harness session.
#[derive(Debug, Clone, Default)]
pub struct TranscriptSession {
    /// Harness id (`codex`, `claude`).
    pub harness: &'static str,
    /// Harness-native session id.
    pub session_id: String,
    /// Working directory recorded by the harness.
    pub cwd: String,
    /// RFC3339 start (session meta or first event).
    pub started_at: String,
    /// RFC3339 end (last event).
    pub ended_at: String,
    /// Completion classification.
    pub outcome: Outcome,
    /// First real user prompt (≤8000 chars, verbatim, sanitized).
    pub objective: String,
    /// All real user prompts (≤2000 chars each, sanitized).
    pub user_prompts: Vec<String>,
    /// Files written/edited (paths only — content is never reconstructed;
    /// the list is COMPLETE, previews truncate downstream).
    pub files_touched: Vec<String>,
    /// Failed approaches (error excerpts ≤800 chars, sanitized).
    pub failed_approaches: Vec<String>,
    /// Pending + in_progress step titles of the LATEST plan (backward
    /// compatible residual-work shortcut for [`Self::plan_state`]).
    pub next_steps: Vec<String>,
    /// The LATEST `update_plan` snapshot with statuses verbatim. Empty when
    /// the session never planned — never invented.
    pub plan_state: Vec<PlanStep>,
    /// ALL `compacted` running summaries, NEWEST FIRST (≤6000 chars each,
    /// up to 8). These ARE transcript content, rule-extracted.
    pub progress_summaries: Vec<String>,
    /// The last ~12 user/assistant message pairs (≤24 entries,
    /// chronological, ≤1500 chars each, injected envelopes excluded).
    pub conversation_tail: Vec<TailEntry>,
    /// Milestone accomplishment summaries: the assistant text closing each
    /// completed task (last ~30, chronological — oldest kept first,
    /// ≤1200 chars each). Empty when the session has none.
    pub milestones: Vec<String>,
    /// Total tool/function call events.
    pub tool_events: usize,
    /// Intentionally-excluded artifacts (B1) — posted as `extraction_loss`
    /// observations at import so gaps are truthful.
    pub losses: Vec<LossNote>,
}

/// Stable observation source_id for one session (server-side dedup key).
pub fn source_id(session: &TranscriptSession) -> String {
    format!("transcript:{}:{}", session.harness, session.session_id)
}

/// One harness reader.
pub trait TranscriptReader {
    /// Harness id.
    fn id(&self) -> &'static str;
    /// Scan `home` for sessions belonging to `project_dir` (exact or nested
    /// cwd), unsorted.
    fn scan(&self, home: &Path, project_dir: &Path) -> Vec<TranscriptSession>;
}

/// Readers with a verified format today (cursor/kimi are intentionally
/// pending — their stores are not fabricated).
pub fn readers() -> Vec<Box<dyn TranscriptReader>> {
    vec![
        Box::new(codex::CodexReader),
        Box::new(claude::ClaudeReader),
        Box::new(cursor::CursorReader),
        Box::new(kimi::KimiReader),
        Box::new(openclaw::OpenClawReader),
        Box::new(hermes::HermesReader),
    ]
}

/// Harnesses whose transcript format is not yet implemented, with the honest
/// note to surface. All four current harnesses have verified readers now —
/// this stays as the seam for future additions.
pub fn pending_reader_notes() -> Vec<(&'static str, &'static str)> {
    Vec::new()
}

/// Scan every implemented reader and merge, sorted by `started_at`.
pub fn scan_all(home: &Path, project_dir: &Path) -> Vec<TranscriptSession> {
    let mut sessions: Vec<TranscriptSession> = readers()
        .iter()
        .flat_map(|reader| reader.scan(home, project_dir))
        .collect();
    sessions.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    sessions
}

// ---------------------------------------------------------------------
// shared reader helpers
// ---------------------------------------------------------------------

/// Recursively collect files under `root` matching `pred` (missing root = empty).
pub(crate) fn walk_files(root: &Path, pred: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_into(root, pred, &mut out);
    out
}

fn walk_into(dir: &Path, pred: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_into(&path, pred, out);
        } else if pred(&path) {
            out.push(path);
        }
    }
}

/// Normalize a path for cross-platform cwd comparison:
/// - strip `\\?\` / `\\.\` verbatim prefixes (Windows `canonicalize` output);
/// - convert `\` → `/`;
/// - map WSL mounts `/mnt/<letter>/…` → `<letter>:/…` (letter lowercased);
/// - lowercase drive-letter paths wholesale (Windows is case-insensitive;
///   the session store mixes `d:\…` and `D:\…` for the same project);
/// - trim trailing `/` (a bare `d:/` or `/` root stays).
pub(crate) fn normalize_path(raw: &str) -> String {
    let mut s = raw.trim().replace('\\', "/");
    for prefix in ["//?/", "//./"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
            break;
        }
    }
    if let Some(rest) = s.strip_prefix("/mnt/") {
        let mut chars = rest.chars();
        if let (Some(letter), Some('/')) = (chars.next(), chars.next()) {
            if letter.is_ascii_alphabetic() {
                s = format!("{}:/{}", letter.to_ascii_lowercase(), &rest[2..]);
            }
        }
    }
    let is_drive = s.len() >= 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic();
    if is_drive {
        s = s.to_lowercase();
    }
    while s.len() > 1 && s.ends_with('/') && !(is_drive && s.len() == 3) {
        s.pop();
    }
    s
}

/// True when `session_cwd` IS the project or nests inside it (boundary
/// semantics preserved after normalization: `/foo/bar2` must NOT match
/// project `/foo/bar`).
pub(crate) fn cwd_matches(session_cwd: &str, project_dir: &Path) -> bool {
    let cwd = normalize_path(session_cwd);
    let project = normalize_path(&project_dir.to_string_lossy());
    cwd == project
        || (cwd.len() > project.len()
            && cwd.starts_with(&project)
            && cwd.as_bytes()[project.len()] == b'/')
}

/// Trim + truncate one extracted string (verbatim — never scrubbed).
pub(crate) fn clean(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Push `value` when non-empty and not already present (order-preserving dedup).
pub(crate) fn push_unique(list: &mut Vec<String>, value: String) {
    if !value.is_empty() && !list.contains(&value) {
        list.push(value);
    }
}

/// Extract file-write targets from a shell command string: `>`/`>>`
/// redirect targets and `tee <path>` arguments. Reads are not writes —
/// `cmd > /dev/null` and fd redirects (`2>`) are ignored, and candidates
/// must look like plausible paths (junk operators/fragments rejected).
pub(crate) fn shell_write_targets(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        // `tee path` (tee writes; only the immediate argument).
        if token == "tee" {
            if let Some(target) = tokens.get(i + 1) {
                if plausible_path(target) {
                    push_unique(&mut out, target.to_string());
                }
            }
        }
        // `> path` / `>> path` / `>path` (but not `2>`, `>&2`, `>>>`, `/dev/...`).
        let redirect = token.strip_prefix(">>").or_else(|| token.strip_prefix('>'));
        if let Some(rest) = redirect {
            let target = if rest.is_empty() {
                tokens.get(i + 1).copied().unwrap_or("")
            } else {
                rest
            };
            let target = target.trim_matches(|c| c == '"' || c == '\'');
            if plausible_path(target) && !target.starts_with("/dev/") {
                push_unique(&mut out, target.to_string());
            }
        }
        i += 1;
    }
    out
}

/// Plausibility filter for extracted write targets: real paths have
/// length, alphanumerics, and a path separator or extension — junk like
/// `=`, `>`, `\n`, or `audit.json;` does not.
fn plausible_path(token: &str) -> bool {
    let t = token.trim();
    if t.len() < 3 {
        return false;
    }
    if t.ends_with(&[';', '|', '&'][..]) || t.contains("&&") {
        return false;
    }
    if matches!(t, "=" | ">" | ">>" | "-" | "--") {
        return false;
    }
    if !t.chars().any(|c| c.is_alphanumeric()) {
        return false;
    }
    t.contains('/') || t.contains('.')
}

/// Parse the timestamp field of one JSONL event line.
pub(crate) fn event_timestamp(value: &serde_json::Value) -> Option<String> {
    value
        .get("timestamp")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_write_targets_finds_redirects_and_tee() {
        let targets = shell_write_targets("cat > /tmp/out.txt <<'EOF'");
        assert_eq!(targets, vec!["/tmp/out.txt"]);
        let targets = shell_write_targets("echo hi >> log.txt 2>/dev/null");
        assert_eq!(targets, vec!["log.txt"]);
        let targets = shell_write_targets("echo hi | tee -a /proj/notes.md");
        // `-a` is a flag — the path follows it, but our minimal extractor
        // only takes the immediate argument; document the limitation.
        assert!(targets.is_empty());
        let targets = shell_write_targets("echo hi | tee /proj/notes.md");
        assert_eq!(targets, vec!["/proj/notes.md"]);
        let targets = shell_write_targets("ls -la");
        assert!(targets.is_empty());
    }

    #[test]
    fn shell_write_targets_rejects_junk_fragments() {
        // Regression (real Laiq evidence): junk entries `\n`, `audit.json;`,
        // `=` landed in Files Touched.
        let targets = shell_write_targets("cat > = && cat > audit.json; echo done > \\n");
        assert!(targets.is_empty(), "targets: {targets:?}");
        // Metachar-tailed and operator candidates rejected; real paths kept.
        let targets = shell_write_targets("cat > src/real.rs; echo x >> docs/ok.md && ls");
        assert_eq!(targets, vec!["docs/ok.md"]);
        // Bare `>` with nothing plausible after it yields nothing.
        let targets = shell_write_targets("foo > | bar");
        assert!(targets.is_empty());
    }

    #[test]
    fn cwd_boundary_matching() {
        let project = Path::new("/work/demo");
        assert!(cwd_matches("/work/demo", project));
        assert!(cwd_matches("/work/demo/sub/dir", project));
        assert!(!cwd_matches("/work/demo-other", project));
        assert!(!cwd_matches("/work", project));
    }

    #[test]
    fn cwd_matching_windows_verbatim_case_and_wsl() {
        // The init bug: canonicalize on Windows returns the verbatim form.
        let verbatim = Path::new(r"\\?\D:\SAAS\Laiq");
        assert!(
            cwd_matches(r"D:\SAAS\Laiq", verbatim),
            "verbatim project vs plain session cwd"
        );
        assert!(
            cwd_matches(r"D:\SAAS\Laiq\apps", verbatim),
            "nested under verbatim"
        );
        // Case-insensitive for drive-letter paths (real store mixes both).
        let project = Path::new(r"D:\siderai\SiderClaw");
        assert!(cwd_matches(r"d:\siderai\SiderClaw", project));
        assert!(cwd_matches(r"D:\siderai\SiderClaw", project));
        // WSL mount ↔ drive letter, both directions.
        assert!(cwd_matches(
            "/mnt/d/siderai/skillsAgent",
            Path::new(r"D:\siderai\skillsAgent")
        ));
        assert!(cwd_matches(
            r"D:\siderai\skillsAgent",
            Path::new("/mnt/d/siderai/skillsAgent")
        ));
        // Non-boundary sibling must NOT match.
        assert!(!cwd_matches(r"D:\SAAS\Laiq2", Path::new(r"D:\SAAS\Laiq")));
        // POSIX sanity: exact + nested match, case stays sensitive.
        assert!(cwd_matches("/home/u/proj", Path::new("/home/u/proj")));
        assert!(!cwd_matches("/home/u/proj2", Path::new("/home/u/proj")));
        assert!(!cwd_matches("/Home/u/proj", Path::new("/home/u/proj")));
    }

    #[test]
    fn normalize_path_edges() {
        assert_eq!(normalize_path(r"\\?\D:\SAAS\Laiq"), "d:/saas/laiq");
        assert_eq!(normalize_path(r"\\.\D:\SAAS"), "d:/saas");
        assert_eq!(
            normalize_path("/mnt/d/siderai/skillsAgent"),
            "d:/siderai/skillsagent"
        );
        assert_eq!(normalize_path(r"D:\SAAS\Laiq\"), "d:/saas/laiq");
        assert_eq!(normalize_path("d:/"), "d:/");
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path("/work/demo/"), "/work/demo");
    }
}
