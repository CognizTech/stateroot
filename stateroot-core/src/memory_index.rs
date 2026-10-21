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
    /// Native/canonical source locator retained for navigating omitted history.
    pub source: String,
    /// Returned from a committed previous generation while refresh was unavailable.
    pub stale: bool,
    /// Source kind: memory | page | episodic | transcript | user.
    pub kind: String,
    /// Path or source id.
    pub path: String,
    /// Snippet text.
    pub text: String,
    /// Rank score (higher is better): reciprocal-rank-fusion contribution,
    /// comparable only within one merged result set.
    pub score: f64,
    /// Whether the entry is marked private.
    pub private: bool,
}

/// Errors from the memory index.
#[derive(Debug, thiserror::Error)]
pub enum MemoryIndexError {
    /// A required source is unreadable; keep the previous committed generation.
    #[error("index stale: {0}")]
    Source(String),
    /// Bounded writer contention; previous committed generation remains intact.
    #[error("index stale: {0}")]
    Lock(#[from] crate::safe_io::LockError),
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
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    // Rollback journals on supported Windows/WSL locking surfaces; no WAL opt-in.
    conn.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA cache_spill=OFF;")?;
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
    let columns = conn
        .prepare("PRAGMA table_info(docs)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    for column in ["source", "session_id", "harness"] {
        if !columns.iter().any(|name| name == column) {
            conn.execute_batch(&format!(
                "ALTER TABLE docs ADD COLUMN {column} TEXT NOT NULL DEFAULT ''"
            ))?;
        }
    }
    Ok(conn)
}

fn open_read(project_dir: &Path) -> Result<Connection, MemoryIndexError> {
    let conn = Connection::open_with_flags(
        db_path(project_dir),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    conn.busy_timeout(std::time::Duration::from_millis(100))?;
    Ok(conn)
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SourceStamp {
    kind: String,
    path: PathBuf,
    len: u64,
    modified: u128,
    wal_len: u64,
    wal_modified: u128,
}

fn stamp(kind: &str, path: PathBuf) -> SourceStamp {
    let metadata = |path: &Path| {
        fs::metadata(path)
            .map(|meta| {
                (
                    meta.len(),
                    meta.modified()
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|time| time.as_nanos())
                        .unwrap_or(0),
                )
            })
            .unwrap_or((0, 0))
    };
    let (len, modified) = metadata(&path);
    let (wal_len, wal_modified) = metadata(&PathBuf::from(format!("{}-wal", path.display())));
    SourceStamp {
        kind: kind.into(),
        path,
        len,
        modified,
        wal_len,
        wal_modified,
    }
}

fn source_inventory(project_dir: &Path, home: &Path) -> Result<Vec<SourceStamp>, MemoryIndexError> {
    let started = std::time::Instant::now();
    let deadline = started + std::time::Duration::from_secs(1);
    let mut visited = 0;
    let root = local_store::root(project_dir);
    let mut files = vec![
        ("curated".into(), root.join(local_store::MEMORY_CORE_PATH)),
        ("curated".into(), root.join(local_store::EPISODIC_PATH)),
        ("curated".into(), crate::user_profile::path(home)),
        ("curated".into(), home.join(hot_apex::GLOBAL_MEMORY_PATH)),
        (
            "binding".into(),
            home.join(".kimi-code/session_index.jsonl"),
        ),
        (
            "boundary".into(),
            root.join(crate::tombstones::TOMBSTONES_REL),
        ),
        (
            "boundary".into(),
            root.join(crate::tombstones::LEGACY_TOMBSTONES_REL),
        ),
    ];
    files.extend(
        crate::transcripts::metadata_files(
            &root.join(wiki::PAGES_DIR),
            &|path| path.extension().is_some_and(|ext| ext == "md"),
            &mut visited,
            deadline,
        )?
        .into_iter()
        .map(|path| ("curated".into(), path)),
    );
    let mut scopes = vec!["project".into(), "user".into(), "workspace".into()];
    if let Some(slug) = crate::learnings::bound_domain(project_dir) {
        scopes.push(format!("domain:{slug}"));
    }
    for scope in scopes {
        files.extend(
            crate::transcripts::metadata_files(
                &crate::learnings::scope_root(project_dir, home, &scope),
                &|path| path.extension().is_some_and(|ext| ext == "md"),
                &mut visited,
                deadline,
            )?
            .into_iter()
            .map(|path| ("curated".into(), path)),
        );
    }
    files.extend(crate::transcripts::bundle::source_files(
        home,
        &mut visited,
        deadline,
    )?);
    files.extend(
        crate::transcripts::metadata_files(
            &crate::sessions::store_dir(project_dir),
            &|path| path.extension().is_some_and(|ext| ext == "jsonl"),
            &mut visited,
            deadline,
        )?
        .into_iter()
        .map(|path| ("canonical".into(), path)),
    );
    if files.len() > 20_000 {
        return Err(MemoryIndexError::Source(
            "metadata freshness budget exceeded (20000 source files); committed index retained"
                .into(),
        ));
    }
    files.sort();
    files.dedup();
    let mut inventory = Vec::with_capacity(files.len());
    for (kind, path) in files {
        if started.elapsed() > std::time::Duration::from_secs(1) {
            return Err(MemoryIndexError::Source(
                "source metadata freshness budget exceeded (1s); committed index retained".into(),
            ));
        }
        inventory.push(stamp(&kind, path));
    }
    Ok(inventory)
}

fn metadata_fingerprint(sources: &[SourceStamp]) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "metadata-v2:{:x}",
        Sha256::digest(serde_json::to_vec(sources).expect("source metadata serializes"))
    )
}

fn stored_sources(conn: &Connection) -> Vec<SourceStamp> {
    conn.query_row("SELECT value FROM meta WHERE key='sources'", [], |row| {
        row.get::<_, String>(0)
    })
    .ok()
    .and_then(|text| serde_json::from_str(&text).ok())
    .unwrap_or_default()
}

fn content_fingerprint(project_dir: &Path, home: &Path) -> Result<String, MemoryIndexError> {
    Ok(metadata_fingerprint(&source_inventory(project_dir, home)?))
}

/// Rebuild the index when the fingerprint changed. Returns whether a rebuild ran.
pub fn rebuild_if_needed(project_dir: &Path, home: &Path) -> Result<bool, MemoryIndexError> {
    let sources = source_inventory(project_dir, home)?;
    let fp = metadata_fingerprint(&sources);
    if let Ok(conn) = open_read(project_dir) {
        let stored = conn
            .query_row(
                "SELECT value FROM meta WHERE key='fingerprint'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok();
        if stored.as_deref() == Some(fp.as_str()) {
            return Ok(false);
        }
    }
    refresh(project_dir, home, false)?;
    Ok(true)
}

/// Force a full rebuild.
pub fn rebuild(project_dir: &Path, home: &Path) -> Result<(), MemoryIndexError> {
    refresh(project_dir, home, true)
}

fn refresh(project_dir: &Path, home: &Path, force: bool) -> Result<(), MemoryIndexError> {
    let _writer = crate::safe_io::ResourceLock::acquire_with_budget(
        local_store::root(project_dir).join("local/memory-index.lock"),
        4,
        25,
    )?;
    let sources = source_inventory(project_dir, home)?;
    let fp = metadata_fingerprint(&sources);
    let conn = open(project_dir)?;
    let previous = stored_sources(&conn);
    let full = force || previous.is_empty();
    let transaction = conn.unchecked_transaction()?;
    if full {
        transaction.execute("DELETE FROM docs", [])?;
        transaction.execute("INSERT INTO docs_fts(docs_fts) VALUES('delete-all')", [])?;
    }
    let curated_changed = full
        || sources
            .iter()
            .chain(previous.iter())
            .filter(|source| source.kind == "curated")
            .any(|source| !sources.contains(source) || !previous.contains(source));
    if curated_changed {
        if !full {
            delete_source(&transaction, "curated")?;
        }
        rebuild_with_conn(&transaction, project_dir, home)?;
    }
    let binding_changed = sources
        .iter()
        .chain(previous.iter())
        .filter(|source| source.kind == "binding")
        .any(|source| !sources.contains(source) || !previous.contains(source));
    for old in &previous {
        if old.kind != "curated" && !sources.iter().any(|source| source.path == old.path) {
            delete_source(&transaction, &old.path.to_string_lossy())?;
        }
    }
    let stones =
        crate::tombstones::load(project_dir, crate::tombstones::TombstonePolicy::FailClosed)
            .map_err(|error| {
                MemoryIndexError::Source(format!("tombstone source unavailable: {error}"))
            })?;
    for source in sources
        .iter()
        .filter(|source| !matches!(source.kind.as_str(), "curated" | "binding" | "boundary"))
    {
        if !full && previous.contains(source) && !(binding_changed && source.kind == "kimi") {
            continue;
        }
        let locator = source.path.to_string_lossy();
        if !full {
            delete_source(&transaction, &locator)?;
        }
        let bundles = if source.kind == "canonical" {
            let events = crate::transcripts::bundle::recent_jsonl(&source.path)?;
            let header = events
                .first()
                .ok_or_else(|| MemoryIndexError::Source("canonical recall source empty".into()))?;
            if header["schema_version"].as_str() != Some(crate::sessions::SCHEMA_SESSION_V1) {
                return Err(MemoryIndexError::Source(
                    "canonical recall source schema unavailable".into(),
                ));
            }
            vec![
                serde_json::json!({"session_id":header["session_id"],"harness":header["harness"],"source_path":header["source_path"],"windowing":"partial canonical recall prefix/tail; full session sync/transfer evidence remains intact","messages":events.iter().skip(1).filter(|entry|matches!(entry["type"].as_str(),Some("message"|"tool_result"))).map(|entry|serde_json::json!({"role":entry["role"],"text":entry["content"]})).collect::<Vec<_>>()}),
            ]
        } else {
            crate::transcripts::bundle::source_bundles(
                home,
                project_dir,
                &source.kind,
                &source.path,
            )?
        };
        for bundle in bundles {
            let sid = bundle["session_id"].as_str().unwrap_or("");
            let harness = bundle["harness"].as_str().unwrap_or("");
            if crate::tombstones::contains(&stones, sid, harness) {
                continue;
            }
            insert_source_doc(
                &transaction,
                "transcript",
                sid,
                &recent_projection(&bundle),
                false,
                (&locator, sid, harness),
            )?;
        }
    }
    let removed: Vec<(i64, String)> = {
        let mut stmt = transaction
            .prepare("SELECT id,text,session_id,harness FROM docs WHERE kind='transcript'")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .filter(|(_, _, sid, harness)| crate::tombstones::contains(&stones, sid, harness))
            .map(|(id, text, _, _)| (id, text))
            .collect()
    };
    for (id, text) in removed {
        transaction.execute(
            "INSERT INTO docs_fts(docs_fts,rowid,text) VALUES('delete',?1,?2)",
            params![id, text],
        )?;
        transaction.execute("DELETE FROM docs WHERE id=?1", params![id])?;
    }
    if source_inventory(project_dir, home)? != sources {
        return Err(MemoryIndexError::Source(
            "sources changed while building generation; previous index retained".into(),
        ));
    }
    for (key, value) in [
        ("fingerprint", fp),
        (
            "sources",
            serde_json::to_string(&sources).expect("source metadata serializes"),
        ),
    ] {
        transaction.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;
    }
    transaction.commit()?;
    Ok(())
}

fn delete_source(conn: &Connection, source: &str) -> Result<(), MemoryIndexError> {
    conn.execute("INSERT INTO docs_fts(docs_fts,rowid,text) SELECT 'delete',id,text FROM docs WHERE source=?1",params![source])?;
    conn.execute("DELETE FROM docs WHERE source=?1", params![source])?;
    Ok(())
}

/// Bounded opening and recent windows, with original session/source references.
fn recent_projection(bundle: &serde_json::Value) -> String {
    const RECENT_CHARS: usize = 32_000;
    const OPENING_CHARS: usize = 4_096;
    let messages = bundle["messages"].as_array();
    let total_chars = messages
        .into_iter()
        .flatten()
        .fold(0usize, |count, message| {
            count.saturating_add(message["text"].as_str().unwrap_or("").chars().count())
        });
    let has_omitted_middle = total_chars > RECENT_CHARS;
    let mut opening = Vec::new();
    if has_omitted_middle {
        let mut remaining = OPENING_CHARS;
        if let Some(messages) = messages {
            for message in messages {
                if remaining == 0 {
                    break;
                }
                let first: String = message["text"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(remaining)
                    .collect();
                remaining = remaining.saturating_sub(first.chars().count());
                opening.push(first);
            }
        }
    }
    let mut parts = Vec::new();
    let mut remaining = RECENT_CHARS - if has_omitted_middle { OPENING_CHARS } else { 0 };
    if let Some(messages) = messages {
        for message in messages.iter().rev() {
            let text = message["text"].as_str().unwrap_or("");
            let tail: String = text
                .chars()
                .rev()
                .take(remaining)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            remaining = remaining.saturating_sub(tail.chars().count());
            parts.push(tail);
            if remaining == 0 {
                break;
            }
        }
    }
    parts.reverse();
    let window = if has_omitted_middle {
        "first-4096-chars+recent-27904-chars"
    } else {
        "recent-32000-chars"
    };
    let text = if has_omitted_middle {
        format!(
            "{}\n[intervening evidence omitted from recall index; inspect source]\n{}",
            opening.join("\n"),
            parts.join("\n")
        )
    } else {
        parts.join("\n")
    };
    format!("session={} harness={} source={} partial-recall-window={window}; native-window={}; parent/tool context outside the window may be unavailable; full evidence remains at source\n{text}",bundle["session_id"].as_str().unwrap_or(""),bundle["harness"].as_str().unwrap_or(""),bundle["source_path"].as_str().unwrap_or(""),bundle["windowing"].as_str().unwrap_or("canonical recent messages"))
}

fn rebuild_with_conn(
    conn: &Connection,
    project_dir: &Path,
    home: &Path,
) -> Result<(), MemoryIndexError> {
    let root = local_store::root(project_dir);
    let mut scopes = vec![
        "project".to_string(),
        "user".to_string(),
        "workspace".to_string(),
    ];
    if let Some(slug) = crate::learnings::bound_domain(project_dir) {
        scopes.push(format!("domain:{slug}"));
    }
    for scope in scopes {
        for learning in crate::learnings::read_scope(project_dir, home, &scope) {
            if learning.status == "active" && learning.superseded_by.is_empty() {
                insert_doc(
                    conn,
                    "learning",
                    &format!("{scope}:{}", learning.id),
                    &learning.render_bullet(),
                    false,
                )?;
            }
        }
    }

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

    Ok(())
}

fn insert_doc(
    conn: &Connection,
    kind: &str,
    path: &str,
    text: &str,
    private: bool,
) -> Result<(), MemoryIndexError> {
    insert_source_doc(conn, kind, path, text, private, ("curated", "", ""))
}

fn insert_source_doc(
    conn: &Connection,
    kind: &str,
    path: &str,
    text: &str,
    private: bool,
    reference: (&str, &str, &str),
) -> Result<(), MemoryIndexError> {
    let (source, session_id, harness) = reference;
    let text = text.trim();
    if text.is_empty() {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO docs(kind,path,text,private,source,session_id,harness) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![kind, path, text, if private { 1 } else { 0 },source,session_id,harness],
    )?;
    // Mirror into FTS (external-content table needs explicit insert).
    let rowid: i64 = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO docs_fts(rowid, text) VALUES(?1, ?2)",
        params![rowid, text],
    )?;
    Ok(())
}

/// Standard reciprocal-rank-fusion constant (60): keeps top ranks distinct
/// without letting any single stream dominate by position alone.
const RRF_K: f64 = 60.0;

/// One stream's fused contribution for its 0-based rank position.
fn rrf_score(rank: usize) -> f64 {
    1.0 / (RRF_K + rank as f64 + 1.0)
}

/// Search. When `owner` is false, private docs are excluded.
pub fn search(
    project_dir: &Path,
    home: &Path,
    query: &str,
    limit: usize,
    owner: bool,
) -> Result<Vec<RecallHit>, MemoryIndexError> {
    let q = query.trim();
    if q.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let refresh_error = rebuild_if_needed(project_dir, home).err();
    let stale = refresh_error.is_some();
    let conn = open_read(project_dir)?;
    let transaction = conn.unchecked_transaction()?;
    let stones =
        crate::tombstones::load(project_dir, crate::tombstones::TombstonePolicy::FailClosed)
            .map_err(|error| {
                MemoryIndexError::Source(format!("current tombstones unavailable: {error}"))
            })?;
    let old_sources = stored_sources(&transaction);
    // Reciprocal-rank fusion: the FTS bm25 scale and the substring matched-
    // count scale are incompatible, so each stream contributes only the
    // rank-based score 1/(RRF_K + rank). Raw magnitudes never compete, each
    // stream keeps its own internal relevance order, and a minority query
    // stream cannot be starved by score-scale mismatch alone.
    let mut hits = Vec::new();
    let fts_query = build_fts_query(q);
    let mut stmt = transaction.prepare(
        "SELECT d.kind,d.path,d.text,d.private,d.source
         FROM docs_fts JOIN docs d ON d.id=docs_fts.rowid
         WHERE docs_fts MATCH ?1 ORDER BY bm25(docs_fts) LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![fts_query, limit.saturating_mul(4) as i64], |row| {
        Ok(RecallHit {
            kind: row.get(0)?,
            path: row.get(1)?,
            text: row.get(2)?,
            private: row.get::<_, i64>(3)? != 0,
            score: 0.0,
            source: row.get(4)?,
            stale,
        })
    })?;
    for (rank, row) in rows.enumerate() {
        let mut hit = row?;
        hit.score = rrf_score(rank);
        hits.push(hit);
    }
    // unicode61 folds an unspaced Han/Kana/Hangul run into ONE token, so FTS
    // MATCH only recalls exact run boundaries. Terms from those scripts get a
    // bounded substring pass over the derived docs table (never a source
    // reread), independent of whether unrelated Latin tokens already hit.
    let substring_terms = cjk_substring_terms(q);
    if !substring_terms.is_empty() {
        hits.extend(cjk_substring_hits(
            &transaction,
            &substring_terms,
            limit.saturating_mul(4).max(20),
            owner,
        )?);
    }
    if hits.is_empty() {
        hits = like_fallback(&transaction, q, limit.saturating_mul(4).max(20), owner)?;
    }
    let mut filtered = Vec::new();
    for mut hit in hits {
        if !owner && hit.private {
            continue;
        }
        if hit.kind == "transcript" {
            let (sid,harness):(String,String)=transaction.query_row(
                "SELECT session_id,harness FROM docs WHERE kind=?1 AND path=?2 AND source=?3 LIMIT 1",
                params![hit.kind,hit.path,hit.source],|row|Ok((row.get(0)?,row.get(1)?)))?;
            if crate::tombstones::contains(&stones, &sid, &harness) {
                continue;
            }
            let Some(prior) = old_sources
                .iter()
                .find(|source| source.path.to_string_lossy() == hit.source)
            else {
                continue;
            };
            if !prior.path.is_file() || stale && stamp(&prior.kind, prior.path.clone()) != *prior {
                continue;
            }
        } else if stale {
            continue;
        } // Fresh curated fallback below enforces current removals.
        hit.stale = stale;
        filtered.push(hit);
    }
    if stale {
        filtered.extend(
            source_fallback(project_dir, home, q, limit)
                .into_iter()
                .map(|mut hit| {
                    hit.stale = true;
                    hit
                })
                .filter(|hit| owner || !hit.private),
        );
    }
    // Fuse the streams: a doc found by several streams sums its reciprocal-
    // rank contributions and appears once; rank ties keep insertion order
    // (stable sort, FTS stream first), preserving Latin relevance primacy.
    let mut merged: Vec<RecallHit> = Vec::with_capacity(filtered.len());
    let mut slots: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for hit in filtered {
        let key = (hit.path.clone(), hit.text.clone());
        if let Some(&slot) = slots.get(&key) {
            merged[slot].score += hit.score;
        } else {
            slots.insert(key, merged.len());
            merged.push(hit);
        }
    }
    merged.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(limit);
    let mut filtered = merged;
    for hit in &mut filtered {
        hit.text = excerpt_around(&hit.text, q);
    }
    if filtered.is_empty() {
        if let Some(error) = refresh_error {
            return Err(error);
        }
    }
    Ok(filtered)
}

/// Bounded source fallback for degraded indexes; never pretends index freshness.
pub fn source_fallback(
    project_dir: &Path,
    home: &Path,
    query: &str,
    limit: usize,
) -> Vec<RecallHit> {
    let mut hits = Vec::new();
    let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut add = |kind: &str, path: String, text: String, private: bool| {
        if terms.iter().any(|term| text.to_lowercase().contains(term)) && hits.len() < limit {
            hits.push(RecallHit {
                source: path.clone(),
                stale: false,
                kind: kind.into(),
                path,
                text: excerpt_around(&text, query),
                score: 0.0,
                private,
            });
        }
    };
    for scope in ["project", "user", "workspace"] {
        for learning in crate::learnings::read_scope(project_dir, home, scope)
            .into_iter()
            .take(1000)
        {
            if learning.status == "active" && learning.superseded_by.is_empty() {
                add(
                    "learning",
                    format!("{scope}:{}", learning.id),
                    learning.render_bullet(),
                    false,
                );
            }
        }
    }
    if let Ok(text) = hot_apex::read_text(project_dir, home, "memory") {
        for entry in hot_apex::split_entries(&text).into_iter().take(1000) {
            add(
                "memory",
                local_store::MEMORY_CORE_PATH.into(),
                entry.clone(),
                hot_apex::is_private(&entry),
            );
        }
    }
    hits
}

/// Read interface for narrow index health projections.
#[derive(Debug, serde::Serialize)]
pub struct IndexHealth {
    /// Whether the last committed generation matches its source set.
    pub fresh: bool,
    /// Last committed source fingerprint, when available.
    pub fingerprint: Option<String>,
    /// Read/schema error; absence never implies a successful empty index.
    pub error: Option<String>,
}

/// Inspect index health without rebuilding or replacing its last generation.
pub fn health(project_dir: &Path, home: &Path) -> IndexHealth {
    let result = Connection::open_with_flags(
        db_path(project_dir),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .and_then(|conn| {
        conn.busy_timeout(std::time::Duration::from_millis(100))?;
        conn.query_row(
            "SELECT value FROM meta WHERE key='fingerprint'",
            [],
            |row| row.get::<_, String>(0),
        )
    });
    match result {
        Ok(fingerprint) => match content_fingerprint(project_dir, home) {
            Ok(current) => IndexHealth {
                fresh: fingerprint == current,
                fingerprint: Some(fingerprint),
                error: None,
            },
            Err(error) => IndexHealth {
                fresh: false,
                fingerprint: Some(fingerprint),
                error: Some(error.to_string()),
            },
        },
        Err(error) => IndexHealth {
            fresh: false,
            fingerprint: None,
            error: Some(error.to_string()),
        },
    }
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
    tokens
        .into_iter()
        .map(|token| format!("\"{token}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
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
    let mut stmt =
        conn.prepare("SELECT kind,path,text,private,source FROM docs ORDER BY id DESC LIMIT 1000")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut hits = Vec::new();
    for row in rows {
        let (kind, path, text, private, source) = row?;
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
                source,
                stale: false,
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
    // Matched-term count only fixes this stream's internal order; convert to
    // rank contributions so the merged comparison stays scale-free.
    for (rank, hit) in hits.iter_mut().enumerate() {
        hit.score = rrf_score(rank);
    }
    Ok(hits)
}

/// Query terms whose script unicode61 cannot substring-match: an unspaced
/// run of Han, Kana, or Hangul is indexed as ONE token, so FTS MATCH only
/// finds exact run boundaries. These terms get a LIKE pass over the derived
/// docs table. Punctuation is normalized away with the same keep-set as
/// `build_fts_query` (alphanumeric plus `_`/`-`), so 支付服务？ or （支付服务）
/// still carries the intended run instead of becoming literal pattern text.
/// `%` is dropped by that keep-set and `_`/`\` stay escaped by
/// `like_pattern`, so typed wildcards never widen the LIKE match. A term
/// that normalizes to nothing carried no substring intent and is dropped.
fn cjk_substring_terms(q: &str) -> Vec<String> {
    q.split_whitespace()
        .map(|t| {
            t.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect::<String>()
        })
        .filter(|t| t.len() > 1)
        .filter(|t| t.chars().any(is_unspaced_script))
        .collect()
}

/// Scripts the unicode61 tokenizer folds into single unspaced-run tokens.
fn is_unspaced_script(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF      // Hangul Jamo
        | 0x3040..=0x309F    // Hiragana
        | 0x30A0..=0x30FF    // Katakana
        | 0x3130..=0x318F    // Hangul Compatibility Jamo
        | 0x3400..=0x4DBF    // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF    // CJK Unified Ideographs
        | 0xAC00..=0xD7AF    // Hangul Syllables
        | 0xF900..=0xFAFF    // CJK Compatibility Ideographs
        | 0xFF66..=0xFF9D    // Halfwidth Katakana
        | 0x20000..=0x2A6DF  // CJK Unified Ideographs Extension B
    )
}

/// Escape a term for a SQL `LIKE ... ESCAPE '\'` substring pattern.
fn like_pattern(term: &str) -> String {
    let mut out = String::with_capacity(term.len() + 2);
    out.push('%');
    for c in term.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// Substring recall over the derived docs table for unspaced-script terms.
/// One SQL scan evaluates every term per row; SQLite LIKE folds ASCII case,
/// matching `like_fallback` for the ASCII parts of mixed terms. Rows arrive
/// ordered by matched-term count and recency; that position becomes the
/// stream's reciprocal-rank contribution, keeping this stream's scale
/// comparable to the FTS stream. The scan reads only the derived index,
/// never the native sources.
fn cjk_substring_hits(
    conn: &Connection,
    terms: &[String],
    limit: usize,
    owner: bool,
) -> Result<Vec<RecallHit>, MemoryIndexError> {
    if terms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let matched = terms
        .iter()
        .enumerate()
        .map(|(i, _)| format!("(text LIKE ?{} ESCAPE '\\')", i + 1))
        .collect::<Vec<_>>()
        .join("+");
    let sql = format!(
        "SELECT kind,path,text,private,source,matched FROM (
           SELECT id,kind,path,text,private,source,({matched}) AS matched FROM docs
         ) WHERE matched > 0 {} ORDER BY matched DESC, id DESC LIMIT ?{}",
        if owner { "" } else { "AND private = 0" },
        terms.len() + 1,
    );
    let mut values: Vec<rusqlite::types::Value> = terms
        .iter()
        .map(|term| rusqlite::types::Value::Text(like_pattern(term)))
        .collect();
    values.push(rusqlite::types::Value::Integer(limit as i64));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values), |row| {
        Ok(RecallHit {
            kind: row.get(0)?,
            path: row.get(1)?,
            text: row.get(2)?,
            private: row.get::<_, i64>(3)? != 0,
            source: row.get(4)?,
            score: 0.0,
            stale: false,
        })
    })?;
    let mut hits = Vec::new();
    for (rank, row) in rows.enumerate() {
        let mut hit = row?;
        hit.score = rrf_score(rank);
        hits.push(hit);
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_changed_source_keeps_committed_generation_and_reports_unavailable() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        let file = native_file(
            home.path(),
            project.path(),
            "broken",
            "beforecorruption committed evidence",
        );
        rebuild(project.path(), home.path()).unwrap();
        let before = open_read(project.path())
            .unwrap()
            .query_row(
                "SELECT value FROM meta WHERE key='fingerprint'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        fs::write(&file, b"{malformed native source").unwrap();
        assert!(rebuild_if_needed(project.path(), home.path()).is_err());
        let conn = open_read(project.path()).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT value FROM meta WHERE key='fingerprint'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            before
        );
        assert!(conn
            .query_row(
                "SELECT text FROM docs WHERE session_id='broken'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap()
            .contains("beforecorruption"));
        assert!(search(project.path(), home.path(), "beforecorruption", 8, true).is_err());
    }

    fn native_file(home: &Path, project: &Path, id: &str, text: &str) -> PathBuf {
        let dir = home.join(".codex/sessions/tests");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("rollout-{id}.jsonl"));
        let events = [
            serde_json::json!({"type":"session_meta","payload":{"id":id,"cwd":project,"timestamp":"2026-10-08T00:00:00Z"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":text}]}}),
        ];
        fs::write(
            &file,
            events
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        file
    }

    #[test]
    fn long_interrupted_session_recall_preserves_opening_and_recent_windows() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        let early = "firstturncanary20261015";
        let middle = "middleturncanary20261015";
        let late = "lastturncanary20261015";
        let transcript = native_file(
            home.path(),
            project.path(),
            "interrupted",
            &format!(
                "{early} {} {middle} {} {late}",
                "middle ".repeat(4_500),
                "middle ".repeat(4_500)
            ),
        );
        crate::observations::capture(
            project.path(),
            crate::observations::CaptureRequest {
                ts: "2026-10-15T00:00:00Z".into(),
                event: "user_prompt_submit".into(),
                harness: "codex".into(),
                session_id: Some("interrupted".into()),
                session_identity: "native".into(),
                event_identity: None,
                text: format!("early answer: {early}"),
                kind_hint: None,
                tool: None,
                excerpt: None,
                source: None,
                local_payload: None,
                source_status: "complete".into(),
                meta: None,
            },
        )
        .unwrap();

        let started = std::time::Instant::now();
        let early_hits = search(project.path(), home.path(), early, 5, true).unwrap();
        let early_ms = started.elapsed().as_millis();
        let started = std::time::Instant::now();
        let late_hits = search(project.path(), home.path(), late, 5, true).unwrap();
        let late_ms = started.elapsed().as_millis();
        let middle_hits = search(project.path(), home.path(), middle, 5, true).unwrap();
        let started = std::time::Instant::now();
        let raw_hits = crate::observations::filter_spool(
            project.path(),
            &crate::observations::ObservationFilter {
                query: Some(early.into()),
                limit: 5,
                ..Default::default()
            },
        );
        let raw_ms = started.elapsed().as_millis();
        eprintln!(
            "long-session result: source_bytes={} recall_early={} ({early_ms}ms) recall_late={} ({late_ms}ms) recall_middle={} observations_early={} ({raw_ms}ms) excerpt_bytes={}",
            fs::metadata(&transcript).unwrap().len(),
            early_hits.len(),
            late_hits.len(),
            middle_hits.len(),
            raw_hits.len(),
            early_hits.iter().map(|hit| hit.text.len()).sum::<usize>()
        );
        assert!(early_hits
            .iter()
            .any(|hit| { hit.kind == "transcript" && Path::new(&hit.source) == transcript }));
        assert!(late_hits
            .iter()
            .any(|hit| { hit.kind == "transcript" && Path::new(&hit.source) == transcript }));
        assert!(
            middle_hits.is_empty(),
            "omitted middle remains source-linked, not invented"
        );
        assert!(raw_hits.iter().any(|hit| hit.text.contains(early)));
        assert!(fs::read_to_string(&transcript).unwrap().contains(early));
        assert!(fs::read_to_string(&transcript).unwrap().contains(middle));

        fs::write(&transcript, b"{malformed native source").unwrap();
        assert!(search(project.path(), home.path(), early, 5, true).is_err());
        assert!(crate::observations::filter_spool(
            project.path(),
            &crate::observations::ObservationFilter {
                query: Some(early.into()),
                limit: 5,
                ..Default::default()
            }
        )
        .iter()
        .any(|hit| hit.text.contains(early)));
    }

    #[test]
    fn large_daily_history_tail_is_searchable_and_current_reads_parse_nothing() {
        use std::io::Write;
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        let file = native_file(home.path(), project.path(), "daily", "historical opener");
        let record=serde_json::json!({"type":"response_item","payload":{"type":"function_call_output","output":"historical tool output ".repeat(6000)}}).to_string()+"\n";
        let mut out =
            std::io::BufWriter::new(fs::OpenOptions::new().append(true).open(&file).unwrap());
        for _ in 0..512 {
            out.write_all(record.as_bytes()).unwrap();
        }
        let message = |text: &str| {
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":text}]}}).to_string()+"\n"
        };
        out.write_all(message("tailcanaryalpha latest actual request").as_bytes())
            .unwrap();
        out.flush().unwrap();
        drop(out);
        let cold = std::time::Instant::now();
        let hits = search(project.path(), home.path(), "tailcanaryalpha", 8, true).unwrap();
        assert!(hits
            .iter()
            .any(|hit| hit.text.contains("tailcanaryalpha") && Path::new(&hit.source) == file));
        let cold_ms = cold.elapsed().as_millis();
        let parses = crate::transcripts::bundle::source_parse_count();
        let _lock = crate::safe_io::ResourceLock::acquire(
            local_store::root(project.path()).join("local/memory-index.lock"),
        )
        .unwrap();
        let warm = std::time::Instant::now();
        for _ in 0..20 {
            assert!(
                search(project.path(), home.path(), "tailcanaryalpha", 8, true)
                    .unwrap()
                    .iter()
                    .all(|hit| !hit.stale)
            );
        }
        assert_eq!(
            crate::transcripts::bundle::source_parse_count(),
            parses,
            "current reads must not parse native evidence or take writer lock"
        );
        let warm_ms = warm.elapsed().as_millis();
        drop(_lock);
        let mut out = fs::OpenOptions::new().append(true).open(&file).unwrap();
        out.write_all(message("tailcanarybeta next actual request").as_bytes())
            .unwrap();
        drop(out);
        let changed = std::time::Instant::now();
        assert!(
            search(project.path(), home.path(), "tailcanarybeta", 8, true)
                .unwrap()
                .iter()
                .any(|hit| hit.text.contains("tailcanarybeta"))
        );
        assert_eq!(
            crate::transcripts::bundle::source_parse_count(),
            parses + 1,
            "only changed source is parsed"
        );
        eprintln!("recall measurement: native_bytes={} cold_ms={cold_ms} unchanged_20_ms={warm_ms} changed_ms={}",fs::metadata(file).unwrap().len(),changed.elapsed().as_millis());
    }

    #[test]
    fn failed_refresh_committed_fallback_respects_current_tombstones() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        native_file(
            home.path(),
            project.path(),
            "purged",
            "purgeguard retained native bytes",
        );
        native_file(
            home.path(),
            project.path(),
            "kept",
            "keepguard unchanged source",
        );
        rebuild(project.path(), home.path()).unwrap();
        crate::tombstones::record(project.path(), "purged", "codex").unwrap();
        let _lock = crate::safe_io::ResourceLock::acquire(
            local_store::root(project.path()).join("local/memory-index.lock"),
        )
        .unwrap();
        let kept = search(project.path(), home.path(), "keepguard", 8, true).unwrap();
        assert!(kept
            .iter()
            .any(|hit| hit.stale && hit.text.contains("keepguard")));
        assert!(
            search(project.path(), home.path(), "purgeguard", 8, true).is_err(),
            "purged old hits must not masquerade as fresh or empty success"
        );
    }

    #[test]
    fn native_transcript_change_invalidates_index_generation() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        rebuild(project.path(), home.path()).unwrap();
        let dir = home.path().join(".codex/sessions/test");
        fs::create_dir_all(&dir).unwrap();
        let events = [
            serde_json::json!({"type":"session_meta","payload":{"id":"native-new","cwd":project.path(),"timestamp":"2026-10-08T00:00:00Z"}}),
            serde_json::json!({"type":"response_item","timestamp":"2026-10-08T00:00:01Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"azimuthal new native evidence"}]}}),
        ];
        fs::write(
            dir.join("rollout-new.jsonl"),
            events
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        assert!(!health(project.path(), home.path()).fresh);
        assert!(search(project.path(), home.path(), "azimuthal", 10, true)
            .unwrap()
            .iter()
            .any(|hit| hit.kind == "transcript"));
        assert!(health(project.path(), home.path()).fresh);
    }

    #[test]
    fn fts_failure_rolls_back_docs_and_fingerprint() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "old committed evidence",
            false,
        )
        .unwrap();
        rebuild(project.path(), home.path()).unwrap();
        let conn = open(project.path()).unwrap();
        let old: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key='fingerprint'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // Fault-injection stand-in accepts the FTS clear command but rejects
        // mirroring a populated document, after the docs insert succeeded.
        conn.execute_batch("DROP TABLE docs_fts; CREATE TABLE docs_fts(rowid INTEGER PRIMARY KEY, text TEXT, docs_fts TEXT); CREATE TRIGGER fail_fts BEFORE INSERT ON docs_fts WHEN NEW.text IS NOT NULL AND NEW.docs_fts IS NULL BEGIN SELECT RAISE(ABORT, 'injected FTS insertion failure'); END;").unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "new uncommitted evidence",
            false,
        )
        .unwrap();
        assert!(rebuild(project.path(), home.path())
            .unwrap_err()
            .to_string()
            .contains("injected FTS insertion failure"));
        let after: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key='fingerprint'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old, after);
        let texts: String = conn
            .query_row("SELECT group_concat(text) FROM docs", [], |row| row.get(0))
            .unwrap();
        assert!(texts.contains("old committed evidence"));
        assert!(!texts.contains("new uncommitted evidence"));
    }

    #[test]
    fn learning_source_change_refreshes_recall_without_memory_write() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        rebuild(project.path(), home.path()).unwrap();
        crate::learnings::record_note(
            project.path(),
            home.path(),
            "Prefer durable nebular evidence",
            "project",
            "test-source",
        )
        .unwrap();
        let hits = search(project.path(), home.path(), "nebular", 10, true).unwrap();
        assert!(hits
            .iter()
            .any(|hit| hit.kind == "learning" && hit.text.contains("test-source")));
    }

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
            None,
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
    fn fts_skips_tombstoned_transcript_bundles() {
        let project = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = project.path().to_string_lossy().replace('\\', "/");
        let claude_dir = home.path().join(".claude/projects/-work-demo");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let event = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "purge-canary-xyzzy unique"},
            "timestamp": "2026-07-10T09:00:01Z",
            "cwd": cwd,
            "sessionId": "ses-canary",
        });
        std::fs::write(claude_dir.join("ses-canary.jsonl"), format!("{event}\n")).unwrap();
        rebuild(project.path(), home.path()).unwrap();
        let hits = search(project.path(), home.path(), "purge-canary-xyzzy", 8, true).unwrap();
        assert!(
            hits.iter()
                .any(|h| h.kind == "transcript" && h.text.contains("purge-canary-xyzzy")),
            "indexed before purge: {hits:?}"
        );
        crate::tombstones::record(project.path(), "ses-canary", "claude").unwrap();
        rebuild(project.path(), home.path()).unwrap();
        let hits = search(project.path(), home.path(), "purge-canary-xyzzy", 8, true).unwrap();
        assert!(
            hits.iter()
                .filter(|h| h.kind == "transcript")
                .all(|h| !h.text.contains("purge-canary-xyzzy")),
            "tombstoned transcript gone: {hits:?}"
        );
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

    /// Write a curated MEMORY.md directly: the real source of record, sized
    /// corpora without lock round-trips per entry.
    fn write_memory_entries(project: &Path, entries: &[String]) {
        let path = local_store::root(project).join(local_store::MEMORY_CORE_PATH);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(
            path,
            format!("# Project Memory\n{}", hot_apex::join_entries(entries)),
        )
        .unwrap();
    }

    #[test]
    fn mixed_latin_cjk_query_keeps_cjk_substring_recall() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "周五把微信支付服务迁移到生产环境并验证回调",
            false,
        )
        .unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "rollback the deploy after tests pass",
            false,
        )
        .unwrap();
        rebuild(project.path(), home.path()).unwrap();
        // Pure CJK substring recall worked via the LIKE fallback.
        let pure = search(project.path(), home.path(), "支付服务", 8, true).unwrap();
        assert!(
            pure.iter().any(|hit| hit.text.contains("支付服务")),
            "pure CJK query regressed: {pure:?}"
        );
        // A Latin FTS hit must not starve the unspaced-CJK stream.
        let mixed = search(project.path(), home.path(), "支付服务 rollback", 8, true).unwrap();
        assert!(
            mixed.iter().any(|hit| hit.text.contains("支付服务")),
            "CJK doc starved by unrelated Latin FTS hit: {mixed:?}"
        );
        assert!(
            mixed.iter().any(|hit| hit.text.contains("rollback")),
            "Latin FTS hit lost: {mixed:?}"
        );
    }

    #[test]
    fn cjk_match_beyond_newest_thousand_docs_is_recalled() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        let mut entries = vec!["周五把微信支付服务迁移到生产环境".to_string()];
        for i in 0..1200 {
            entries.push(format!("routine deploy note {i:04} stays unrelated"));
        }
        write_memory_entries(project.path(), &entries);
        rebuild(project.path(), home.path()).unwrap();
        let hits = search(project.path(), home.path(), "支付服务", 8, true).unwrap();
        assert!(
            hits.iter().any(|hit| hit.text.contains("支付服务")),
            "old CJK document hidden by the newest-1000 fallback window: {hits:?}"
        );
    }

    #[test]
    fn mixed_queries_keep_japanese_and_korean_substring_recall() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "本番環境で自動更新機能を確認する",
            false,
        )
        .unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "자동업데이트기능을확인했다",
            false,
        )
        .unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "rollback the deploy after tests pass",
            false,
        )
        .unwrap();
        rebuild(project.path(), home.path()).unwrap();
        let japanese = search(project.path(), home.path(), "自動更新 rollback", 8, true).unwrap();
        assert!(
            japanese.iter().any(|hit| hit.text.contains("自動更新")),
            "Japanese doc starved: {japanese:?}"
        );
        let korean = search(project.path(), home.path(), "업데이트 rollback", 8, true).unwrap();
        assert!(
            korean.iter().any(|hit| hit.text.contains("업데이트")),
            "Korean doc starved: {korean:?}"
        );
    }

    #[test]
    fn mixed_query_finds_unspaced_cjk_in_native_transcript() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        let file = native_file(
            home.path(),
            project.path(),
            "cjk-stream",
            "用户要求把微信支付服务接入生产环境",
        );
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "rollback the deploy plan",
            false,
        )
        .unwrap();
        rebuild(project.path(), home.path()).unwrap();
        let hits = search(project.path(), home.path(), "支付服务 rollback", 8, true).unwrap();
        assert!(
            hits.iter()
                .any(|hit| hit.kind == "transcript" && Path::new(&hit.source) == file),
            "transcript CJK stream starved: {hits:?}"
        );
    }

    #[test]
    fn doc_matched_by_both_fts_and_substring_paths_appears_once() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "微信支付服务 rollout finished on friday",
            false,
        )
        .unwrap();
        rebuild(project.path(), home.path()).unwrap();
        // Space-bounded run: FTS matches the token AND the substring pass
        // matches the text. The merged result must not duplicate the doc.
        let hits = search(project.path(), home.path(), "微信支付服务 rollout", 8, true).unwrap();
        let matching: Vec<_> = hits
            .iter()
            .filter(|hit| hit.text.contains("微信支付服务"))
            .collect();
        assert_eq!(matching.len(), 1, "duplicate merged hit: {hits:?}");
        assert!(hits.iter().any(|hit| hit.text.contains("rollout")));
    }

    #[test]
    fn cjk_no_match_query_returns_empty_without_error() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "周五把微信支付服务迁移到生产环境",
            false,
        )
        .unwrap();
        rebuild(project.path(), home.path()).unwrap();
        let hits = search(project.path(), home.path(), "存在しない語句", 8, true).unwrap();
        assert!(hits.is_empty(), "phantom CJK hits: {hits:?}");
    }

    #[test]
    fn cjk_substring_recall_crosses_punctuation_boundaries() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "部署微信支付服务。",
            false,
        )
        .unwrap();
        rebuild(project.path(), home.path()).unwrap();
        // 。 breaks the unicode61 run into 部署微信支付服务; a shorter
        // substring query must still recall the doc.
        let hits = search(project.path(), home.path(), "支付服务", 8, true).unwrap();
        assert!(
            hits.iter().any(|hit| hit.text.contains("支付服务")),
            "punctuation-bounded CJK run not recalled: {hits:?}"
        );
    }

    #[test]
    fn rebuild_and_reopen_preserves_cjk_recall() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "周五把微信支付服务迁移到生产环境",
            false,
        )
        .unwrap();
        rebuild(project.path(), home.path()).unwrap();
        let first = search(project.path(), home.path(), "支付服务", 8, true).unwrap();
        // Forced rebuild plus fresh read connections (search opens one per call).
        rebuild(project.path(), home.path()).unwrap();
        let second = search(project.path(), home.path(), "支付服务", 8, true).unwrap();
        for (label, hits) in [("first", first), ("second", second)] {
            assert!(
                hits.iter().any(|hit| hit.text.contains("支付服务")),
                "{label} search after rebuild lost CJK recall: {hits:?}"
            );
        }
    }

    #[test]
    fn latin_ranking_and_fallback_unchanged_by_cjk_support() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "rollback the deploy after tests pass",
            false,
        )
        .unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "rollback only this single step",
            false,
        )
        .unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "paymentrollbackhandler stays one token",
            false,
        )
        .unwrap();
        rebuild(project.path(), home.path()).unwrap();
        // Two-token Latin query: bm25 still ranks the richer match first.
        let hits = search(project.path(), home.path(), "rollback deploy", 8, true).unwrap();
        assert!(
            hits.first().is_some_and(|hit| hit.text.contains("deploy")),
            "Latin bm25 ranking changed: {hits:?}"
        );
        // Latin mid-token substring still falls back to LIKE when FTS is empty.
        let sub = search(project.path(), home.path(), "rollbackhandler", 8, true).unwrap();
        assert!(
            sub.iter()
                .any(|hit| hit.text.contains("paymentrollbackhandler")),
            "Latin LIKE fallback lost: {sub:?}"
        );
    }

    #[test]
    fn cjk_substring_pass_respects_private_visibility() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "家庭支付服务细节",
            true,
        )
        .unwrap();
        rebuild(project.path(), home.path()).unwrap();
        let owner = search(project.path(), home.path(), "支付服务", 8, true).unwrap();
        assert!(
            owner
                .iter()
                .any(|hit| hit.private && hit.text.contains("支付服务")),
            "owner lost private CJK recall: {owner:?}"
        );
        let external = search(project.path(), home.path(), "支付服务", 8, false).unwrap();
        assert!(
            external.is_empty(),
            "private CJK doc leaked to external recall: {external:?}"
        );
    }

    #[test]
    fn cjk_recall_measurement_over_representative_index() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        let mut entries = vec!["周五把微信支付服务迁移到生产环境".to_string()];
        for i in 0..2000 {
            entries.push(format!(
                "routine deploy note {i:04} {}",
                "filler ".repeat(100)
            ));
        }
        entries.push("最近确认自动更新功能已经启用".to_string());
        write_memory_entries(project.path(), &entries);
        let source_bytes =
            fs::metadata(local_store::root(project.path()).join(local_store::MEMORY_CORE_PATH))
                .unwrap()
                .len();
        let building = std::time::Instant::now();
        rebuild(project.path(), home.path()).unwrap();
        let rebuild_ms = building.elapsed().as_millis();
        let cold = std::time::Instant::now();
        let hits = search(
            project.path(),
            home.path(),
            "支付服务 更新 rollback",
            8,
            true,
        )
        .unwrap();
        let cold_ms = cold.elapsed().as_millis();
        assert!(hits.iter().any(|hit| hit.text.contains("支付服务")));
        assert!(hits.iter().any(|hit| hit.text.contains("更新")));
        let warm = std::time::Instant::now();
        for _ in 0..20 {
            assert!(search(project.path(), home.path(), "支付服务", 8, true)
                .unwrap()
                .iter()
                .any(|hit| hit.text.contains("支付服务")));
        }
        let warm_20_ms = warm.elapsed().as_millis();
        // Adversarial shape: many CJK terms in one query.
        let long_query = (0..48)
            .map(|i| format!("存在しない語句{i:02}"))
            .collect::<Vec<_>>()
            .join(" ");
        let adversarial = std::time::Instant::now();
        assert!(search(project.path(), home.path(), &long_query, 8, true)
            .unwrap()
            .is_empty());
        let adversarial_ms = adversarial.elapsed().as_millis();
        eprintln!(
            "cjk recall measurement: source_bytes={source_bytes} docs={} rebuild_ms={rebuild_ms} cold_mixed_ms={cold_ms} warm_20_ms={warm_20_ms} adversarial_48term_ms={adversarial_ms}",
            entries.len()
        );
    }

    #[test]
    fn mixed_query_topk_preserves_minority_cjk_stream() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        // 32 relevant Latin-only docs plus one CJK substring doc; limit 8.
        let mut entries = vec!["周五把微信支付服务迁移到生产环境".to_string()];
        entries.push("rollback rollback rollback the most relevant rollback deploy".to_string());
        for i in 0..31 {
            entries.push(format!("rollback deploy note {i:02} stays latin only"));
        }
        write_memory_entries(project.path(), &entries);
        rebuild(project.path(), home.path()).unwrap();
        let hits = search(project.path(), home.path(), "支付服务 rollback", 8, true).unwrap();
        assert!(
            hits.iter().any(|hit| hit.text.contains("支付服务")),
            "minority CJK stream starved by 32 Latin FTS hits at limit 8: {hits:?}"
        );
        assert!(
            hits.iter().any(|hit| hit.text.contains("rollback")),
            "Latin stream lost: {hits:?}"
        );
        // Meaningful Latin relevance order survives the merge: the bm25-richest
        // Latin doc still leads the Latin stream.
        let first_latin = hits
            .iter()
            .find(|hit| hit.text.contains("rollback"))
            .expect("Latin stream present");
        assert!(
            first_latin.text.contains("most relevant"),
            "Latin relevance order changed by the merge: {hits:?}"
        );
    }

    #[test]
    fn cjk_query_with_surrounding_punctuation_still_recalls_substring() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "周五把微信支付服务迁移到生产环境",
            false,
        )
        .unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "rollback the deploy after tests pass",
            false,
        )
        .unwrap();
        rebuild(project.path(), home.path()).unwrap();
        // Normal query punctuation — half-width, paired, and full-width — must
        // not become part of the literal substring pattern.
        for query in ["支付服务？", "(支付服务) rollback", "（支付服务）？"] {
            let hits = search(project.path(), home.path(), query, 8, true).unwrap();
            assert!(
                hits.iter().any(|hit| hit.text.contains("支付服务")),
                "punctuation-wrapped query {query:?} lost the CJK doc: {hits:?}"
            );
        }
    }

    #[test]
    fn cjk_substring_terms_never_become_like_wildcards() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        local_store::init_skeleton(project.path(), "p", "P", "default").unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "微信支付服务已上线",
            false,
        )
        .unwrap();
        hot_apex::add(
            project.path(),
            home.path(),
            "memory",
            "微信支付_服务配置说明",
            false,
        )
        .unwrap();
        rebuild(project.path(), home.path()).unwrap();
        // Pure control: the plain run recalls the contiguous doc.
        let pure = search(project.path(), home.path(), "支付服务", 8, true).unwrap();
        assert!(
            pure.iter()
                .any(|hit| hit.text.contains("微信支付服务已上线")),
            "pure control regressed: {pure:?}"
        );
        // `_` stays an escaped literal, never a single-char wildcard.
        let underscored = search(project.path(), home.path(), "支付_服务", 8, true).unwrap();
        assert!(
            underscored.iter().any(|hit| hit.text.contains("支付_服务")),
            "literal-underscore doc lost: {underscored:?}"
        );
        assert!(
            underscored
                .iter()
                .all(|hit| !hit.text.contains("微信支付服务已上线")),
            "`_` acted as a LIKE wildcard: {underscored:?}"
        );
        // `%` normalizes away like other query punctuation instead of wildcarding.
        let percent = search(project.path(), home.path(), "支付%服务", 8, true).unwrap();
        assert!(
            percent
                .iter()
                .any(|hit| hit.text.contains("微信支付服务已上线")),
            "normalized `%` query lost the contiguous doc: {percent:?}"
        );
        assert!(
            percent.iter().all(|hit| !hit.text.contains("支付_服务")),
            "`%` acted as a LIKE wildcard: {percent:?}"
        );
    }
}
