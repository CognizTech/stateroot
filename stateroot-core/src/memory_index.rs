//! Local FTS5 index over curated memory, wiki pages, episodic, and transcripts.
//!
//! Lives at `.stateroot/local/memory.sqlite` (unsynced via `.stateroot/local/`).

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use crate::hot_apex;
use crate::local_store;
use crate::wiki;

/// Relative path of the index DB under `.stateroot/`.
pub const INDEX_DB_REL: &str = "local/memory.sqlite";

/// One recall hit.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallHit {
    /// Source kind: memory | page | episodic | transcript | user.
    pub kind: String,
    /// Path or source id.
    pub path: String,
    /// Snippet text.
    pub text: String,
    /// Rank score (higher is better; from bm25 inverted).
    pub score: f64,
    /// Whether the entry is marked private.
    pub private: bool,
}

/// Errors from the memory index.
#[derive(Debug, thiserror::Error)]
pub enum MemoryIndexError {
    /// SQLite failure.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// IO failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

fn db_path(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join(INDEX_DB_REL)
}

fn open(project_dir: &Path) -> Result<Connection, MemoryIndexError> {
    let path = db_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(&path)?;
    // Regular table is the source of truth for listing/LIKE; FTS5 mirrors `text`
    // for MATCH queries. FTS5 alone is awkward for full-table scans.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS docs (
           id INTEGER PRIMARY KEY,
           kind TEXT NOT NULL,
           path TEXT NOT NULL,
           text TEXT NOT NULL,
           private INTEGER NOT NULL DEFAULT 0
         );
         CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
           text,
           content='docs',
           content_rowid='id',
           tokenize = 'porter unicode61'
         );",
    )?;
    Ok(conn)
}

fn content_fingerprint(project_dir: &Path, home: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let root = local_store::root(project_dir);
    for rel in [local_store::MEMORY_CORE_PATH, local_store::EPISODIC_PATH] {
        let p = root.join(rel);
        hasher.update(rel.as_bytes());
        if p.is_file() {
            hasher.update(fs::read(&p).unwrap_or_default());
        }
    }
    // Wiki pages (recursive — imported harness pages live in subdirs).
    for rel in wiki::list_pages(project_dir) {
        hasher.update(rel.as_bytes());
        hasher.update(fs::read(root.join(&rel)).unwrap_or_default());
    }
    if let Some(user) = crate::user_profile::read(home) {
        hasher.update(b"user");
        hasher.update(user.as_bytes());
    }
    if let Ok(global_memory) = hot_apex::read_text(project_dir, home, "global_memory") {
        hasher.update(b"global_memory");
        hasher.update(global_memory.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Rebuild the index when the fingerprint changed. Returns whether a rebuild ran.
pub fn rebuild_if_needed(project_dir: &Path, home: &Path) -> Result<bool, MemoryIndexError> {
    let fp = content_fingerprint(project_dir, home);
    let conn = open(project_dir)?;
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'fingerprint'",
            [],
            |row| row.get(0),
        )
        .ok();
    if stored.as_deref() == Some(fp.as_str()) {
        return Ok(false);
    }
    rebuild_with_conn(&conn, project_dir, home)?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES('fingerprint', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![fp],
    )?;
    Ok(true)
}

/// Force a full rebuild.
pub fn rebuild(project_dir: &Path, home: &Path) -> Result<(), MemoryIndexError> {
    let conn = open(project_dir)?;
    rebuild_with_conn(&conn, project_dir, home)?;
    let fp = content_fingerprint(project_dir, home);
    conn.execute(
        "INSERT INTO meta(key, value) VALUES('fingerprint', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![fp],
    )?;
    Ok(())
}

fn rebuild_with_conn(
    conn: &Connection,
    project_dir: &Path,
    home: &Path,
) -> Result<(), MemoryIndexError> {
    conn.execute("DELETE FROM docs", [])?;
    // Keep FTS in sync when using external content.
    let _ = conn.execute("INSERT INTO docs_fts(docs_fts) VALUES('delete-all')", []);
    let root = local_store::root(project_dir);

    // MEMORY.md entries
    if let Ok(text) = hot_apex::read_text(project_dir, home, "memory") {
        for entry in hot_apex::split_entries(&text) {
            insert_doc(
                conn,
                "memory",
                local_store::MEMORY_CORE_PATH,
                &entry,
                hot_apex::is_private(&entry),
            )?;
        }
    }

    // User-global MEMORY.md follows the user across projects.
    if let Ok(text) = hot_apex::read_text(project_dir, home, "global_memory") {
        for entry in hot_apex::split_entries(&text) {
            insert_doc(
                conn,
                "memory_user",
                hot_apex::GLOBAL_MEMORY_PATH,
                &entry,
                hot_apex::is_private(&entry),
            )?;
        }
    }

    // USER.md (owner recall)
    if let Some(user) = crate::user_profile::read(home) {
        insert_doc(conn, "user", "user/USER.md", &user, false)?;
    }

    // Wiki pages
    for rel in wiki::list_pages(project_dir) {
        let path = root.join(&rel);
        if let Ok(text) = fs::read_to_string(&path) {
            let private = text.contains(hot_apex::PRIVATE_MARKER);
            insert_doc(conn, "page", &rel, &text, private)?;
        }
    }

    // Episodic
    let episodic = root.join(local_store::EPISODIC_PATH);
    if let Ok(text) = fs::read_to_string(episodic) {
        for (i, line) in text.lines().enumerate() {
            if let Ok(record) = serde_json::from_str::<serde_json::Value>(line) {
                let note = record
                    .get("note")
                    .or_else(|| record.get("content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if note.is_empty() {
                    continue;
                }
                let id = record
                    .get("source_id")
                    .or_else(|| record.get("id"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("episodic:{i}"));
                insert_doc(conn, "episodic", &id, note, false)?;
            }
        }
    }

    // Transcript bundles (best-effort; may be empty without harness homes)
    let bundles = crate::transcripts::bundle::build_bundles(home, project_dir, None, 500_000);
    for (i, bundle) in bundles.iter().enumerate() {
        let text = serde_json::to_string(bundle).unwrap_or_default();
        if text.trim().is_empty() {
            continue;
        }
        let snippet = if text.len() > 20_000 {
            format!("{}…", &text[..20_000])
        } else {
            text
        };
        let sid = bundle
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("transcript:{i}"));
        insert_doc(conn, "transcript", &sid, &snippet, false)?;
    }

    Ok(())
}

fn insert_doc(
    conn: &Connection,
    kind: &str,
    path: &str,
    text: &str,
    private: bool,
) -> Result<(), MemoryIndexError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO docs(kind, path, text, private) VALUES(?1, ?2, ?3, ?4)",
        params![kind, path, text, if private { 1 } else { 0 }],
    )?;
    // Mirror into FTS (external-content table needs explicit insert).
    let rowid: i64 = conn.last_insert_rowid();
    let _ = conn.execute(
        "INSERT INTO docs_fts(rowid, text) VALUES(?1, ?2)",
        params![rowid, text],
    );
    Ok(())
}

/// Search. When `owner` is false, private docs are excluded.
pub fn search(
    project_dir: &Path,
    home: &Path,
    query: &str,
    limit: usize,
    owner: bool,
) -> Result<Vec<RecallHit>, MemoryIndexError> {
    let _ = rebuild_if_needed(project_dir, home)?;
    let conn = open(project_dir)?;
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    // Prefer reliable substring scoring; layer FTS ranks on top when MATCH works.
    let mut hits = like_fallback(&conn, q, limit.saturating_mul(4).max(20), owner)?;
    let fts_query = build_fts_query(q);
    if let Ok(mut stmt) = conn.prepare(
        "SELECT d.kind, d.path, d.text, d.private, bm25(docs_fts) AS rank
         FROM docs_fts
         JOIN docs d ON d.id = docs_fts.rowid
         WHERE docs_fts MATCH ?1
         ORDER BY rank LIMIT ?2",
    ) {
        if let Ok(rows) = stmt.query_map(params![fts_query, limit as i64], |row| {
            let rank: f64 = row.get(4).unwrap_or(0.0);
            Ok(RecallHit {
                kind: row.get(0)?,
                path: row.get(1)?,
                text: row.get(2)?,
                score: 1000.0 - rank, // prefer FTS hits
                private: row.get::<_, i64>(3).unwrap_or(0) != 0,
            })
        }) {
            for hit in rows.flatten() {
                if !owner && hit.private {
                    continue;
                }
                if let Some(existing) = hits
                    .iter_mut()
                    .find(|h| h.path == hit.path && h.text == hit.text)
                {
                    existing.score = existing.score.max(hit.score);
                } else {
                    hits.push(hit);
                }
            }
        }
    }
    hits.retain(|h| owner || !h.private);
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(limit);
    // Indexed docs can be whole transcripts (~100KB) — return an excerpt
    // around the match, or MCP/tool budgets blow up (a real claude-code
    // incident: one recall hit exceeded the maximum tool result).
    for hit in &mut hits {
        hit.text = excerpt_around(&hit.text, q);
    }
    Ok(hits)
}

/// The excerpt budget for one recall hit (chars, not tokens).
const RECALL_EXCERPT_CHARS: usize = 1600;

/// Cap `text` to a window around the first query-token match, with ellipsis
/// marks where cut. Char-boundary safe; short texts pass through untouched.
fn excerpt_around(text: &str, query: &str) -> String {
    let total = text.chars().count();
    if total <= RECALL_EXCERPT_CHARS {
        return text.to_string();
    }
    let lower = text.to_lowercase();
    let pos = query
        .split_whitespace()
        .filter(|t| t.len() > 1)
        .filter_map(|t| lower.find(&t.to_lowercase()))
        .min()
        .unwrap_or(0);
    let char_pos = lower[..pos].chars().count();
    let start = char_pos.saturating_sub(400);
    let chars: Vec<char> = text.chars().collect();
    let end = (start + RECALL_EXCERPT_CHARS).min(total);
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(chars[start..end].iter());
    if end < total {
        out.push('…');
    }
    out
}

fn build_fts_query(q: &str) -> String {
    let tokens: Vec<String> = q
        .split_whitespace()
        .filter(|t| t.len() > 1)
        .map(|t| {
            let cleaned: String = t
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            cleaned
        })
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return format!("\"{}\"", q.replace('"', ""));
    }
    // OR so a single matching token still hits (AND was too strict for short queries).
    tokens.join(" OR ")
}

fn like_fallback(
    conn: &Connection,
    q: &str,
    limit: usize,
    owner: bool,
) -> Result<Vec<RecallHit>, MemoryIndexError> {
    let terms: Vec<String> = q
        .split_whitespace()
        .filter(|t| t.len() > 1)
        .map(|t| t.to_lowercase())
        .collect();
    let mut stmt = conn.prepare("SELECT kind, path, text, private FROM docs")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut hits = Vec::new();
    for row in rows.flatten() {
        let (kind, path, text, private) = row;
        if !owner && private != 0 {
            continue;
        }
        let lower = text.to_lowercase();
        let score = if terms.is_empty() {
            if lower.contains(&q.to_lowercase()) {
                1.0
            } else {
                0.0
            }
        } else {
            terms.iter().filter(|t| lower.contains(t.as_str())).count() as f64
        };
        if score > 0.0 {
            hits.push(RecallHit {
                kind,
                path,
                text,
                score,
                private: private != 0,
            });
        }
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(limit);
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_around_caps_giant_docs_at_the_match() {
        let giant = format!("{}needle{}", "x".repeat(9000), "y".repeat(9000));
        let out = excerpt_around(&giant, "needle");
        assert!(out.chars().count() <= RECALL_EXCERPT_CHARS + 2, "bounded");
        assert!(out.contains("needle"), "match kept");
        assert!(out.starts_with('…') && out.ends_with('…'), "cut marked");
        // Short docs pass through untouched.
        assert_eq!(excerpt_around("small doc", "needle"), "small doc");
        // No match → capped from the front, marked at the cut.
        let out = excerpt_around(&"z".repeat(9000), "needle");
        assert!(out.ends_with('…') && out.chars().count() <= RECALL_EXCERPT_CHARS + 1);
    }

    #[test]
    fn recall_hits_page_and_episodic() {
        let project = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        let home = tempfile::tempdir().unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "api listens on port 7777",
            false,
        )
        .unwrap();
        wiki::write_page(
            project.path(),
            "auth",
            "JWT tokens live in crates/auth",
            "auth",
            "entity",
        )
        .unwrap();
        local_store::append_episodic(
            project.path(),
            &serde_json::json!({"note": "decided to use postgres for learnings"}),
        )
        .unwrap();
        rebuild(project.path(), home.path()).unwrap();
        let hits = search(project.path(), home.path(), "JWT auth", 5, true).unwrap();
        assert!(
            hits.iter()
                .any(|h| h.text.contains("JWT") || h.path.contains("auth")),
            "{hits:?}"
        );
        let hits2 = search(project.path(), home.path(), "postgres learnings", 5, true).unwrap();
        assert!(
            hits2
                .iter()
                .any(|h| h.text.to_lowercase().contains("postgres")),
            "{hits2:?}"
        );
        let hits3 = search(project.path(), home.path(), "7777", 5, true).unwrap();
        assert!(hits3.iter().any(|h| h.text.contains("7777")), "{hits3:?}");
    }

    #[test]
    fn external_skips_private() {
        let project = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        let home = tempfile::tempdir().unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "secret family detail",
            true,
        )
        .unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "public deploy port 80",
            false,
        )
        .unwrap();
        rebuild(project.path(), home.path()).unwrap();
        let owner = search(project.path(), home.path(), "secret family", 5, true).unwrap();
        assert!(owner.iter().any(|h| h.private), "{owner:?}");
        let external = search(project.path(), home.path(), "secret family", 5, false).unwrap();
        assert!(!external.iter().any(|h| h.private), "{external:?}");
        let pub_hits = search(project.path(), home.path(), "deploy port", 5, false).unwrap();
        assert!(
            pub_hits.iter().any(|h| h.text.contains("80")),
            "{pub_hits:?}"
        );
    }
}
