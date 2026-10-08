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
    /// Rank score (higher is better; from bm25 inverted).
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

/// Bounded searchable recent window, with original session/source references.
fn recent_projection(bundle: &serde_json::Value) -> String {
    const RECENT_CHARS: usize = 32_000;
    let mut parts = Vec::new();
    let mut remaining = RECENT_CHARS;
    if let Some(messages) = bundle["messages"].as_array() {
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
    format!("session={} harness={} source={} partial-recall-window=recent-32000-chars; native-window={}; parent/tool context outside the window may be unavailable; full evidence remains at source\n{}",bundle["session_id"].as_str().unwrap_or(""),bundle["harness"].as_str().unwrap_or(""),bundle["source_path"].as_str().unwrap_or(""),bundle["windowing"].as_str().unwrap_or("canonical recent messages"),parts.join("\n"))
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
    let mut hits = Vec::new();
    let fts_query = build_fts_query(q);
    let mut stmt = transaction.prepare(
        "SELECT d.kind,d.path,d.text,d.private,bm25(docs_fts),d.source
         FROM docs_fts JOIN docs d ON d.id=docs_fts.rowid
         WHERE docs_fts MATCH ?1 ORDER BY bm25(docs_fts) LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![fts_query, limit.saturating_mul(4) as i64], |row| {
        Ok(RecallHit {
            kind: row.get(0)?,
            path: row.get(1)?,
            text: row.get(2)?,
            private: row.get::<_, i64>(3)? != 0,
            score: 1000.0 - row.get::<_, f64>(4)?,
            source: row.get(5)?,
            stale,
        })
    })?;
    for row in rows {
        hits.push(row?);
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
    filtered.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    filtered.dedup_by(|a, b| a.path == b.path && a.text == b.text);
    filtered.truncate(limit);
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
}
