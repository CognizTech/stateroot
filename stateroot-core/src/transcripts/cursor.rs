//! Cursor transcript reader: Cursor's `state.vscdb` SQLite store
//! (`<config>/Cursor/User/globalStorage/state.vscdb`).
//!
//! Format (verified against the user's 2.2 GB store, opened with
//! `?immutable=1` so a locked live store still reads):
//! - table `composerHeaders` — one row per session (composer), `value` is a
//!   JSON head `{type: "head", name, createdAt, workspaceIdentifier: {uri:
//!   {fsPath}}}`; `fsPath` may be a backslash-mangled WSL path
//!   (`\mnt\d\…`), which the shared `cwd_matches` normalization handles.
//! - table `cursorDiskKV` — message bubbles keyed
//!   `bubbleId:<composerId>:<bubbleId>`, JSON `{"_v", "type": 1 (user) | 2
//!   (assistant), "text", "createdAt", …tool metadata arrays}`.
//!
//! Truth contract: files_touched / failed_approaches stay EMPTY — the tool
//! metadata shape is not verified (only empty arrays seen), and nothing is
//! invented.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::kimi::rfc3339_millis;
use super::{clean, cwd_matches, push_unique, Outcome, TranscriptReader, TranscriptSession};

/// Cursor state-db reader.
pub struct CursorReader;

/// Candidate locations of the global state db (Linux/macOS/Windows shapes).
pub(crate) fn db_candidates(home: &Path) -> Vec<PathBuf> {
    [
        ".config/Cursor/User/globalStorage/state.vscdb",
        "Library/Application Support/Cursor/User/globalStorage/state.vscdb",
        "AppData/Roaming/Cursor/User/globalStorage/state.vscdb",
    ]
    .iter()
    .map(|rel| home.join(rel))
    .filter(|p| p.is_file())
    .collect()
}

impl TranscriptReader for CursorReader {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn scan(&self, home: &Path, project_dir: &Path) -> Vec<TranscriptSession> {
        let mut sessions = Vec::new();
        for db_path in db_candidates(home) {
            let Ok(db) = open_immutable(&db_path) else {
                continue;
            };
            sessions.extend(scan_db(&db, project_dir));
        }
        sessions
    }
}

/// Open the store read-only in immutable mode (skips the WAL so a live,
/// locked Cursor store still reads — required for the user's 2.2 GB file).
pub(crate) fn open_immutable(path: &Path) -> Result<rusqlite::Connection, rusqlite::Error> {
    let uri = format!("file:{}?immutable=1", path.display());
    rusqlite::Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
}

fn scan_db(db: &rusqlite::Connection, project_dir: &Path) -> Vec<TranscriptSession> {
    let mut out = Vec::new();
    let mut stmt = match db.prepare("SELECT composerId, value FROM composerHeaders") {
        Ok(stmt) => stmt,
        Err(_) => return out,
    };
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map(|mapped| mapped.flatten().collect::<Vec<_>>())
        .unwrap_or_default();
    for (composer_id, head_json) in rows {
        if let Some(session) = scan_composer(db, &composer_id, &head_json, project_dir) {
            out.push(session);
        }
    }
    out
}

fn scan_composer(
    db: &rusqlite::Connection,
    composer_id: &str,
    head_json: &str,
    project_dir: &Path,
) -> Option<TranscriptSession> {
    let head: Value = serde_json::from_str(head_json).ok()?;
    let cwd = head
        .pointer("/workspaceIdentifier/uri/fsPath")
        .or_else(|| head.pointer("/workspaceIdentifier/uri/path"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if cwd.is_empty() || !cwd_matches(&cwd, project_dir) {
        return None;
    }

    let mut stmt = db
        .prepare("SELECT value FROM cursorDiskKV WHERE key GLOB ?1")
        .ok()?;
    let pattern = format!("bubbleId:{composer_id}:*");
    let rows = stmt
        .query_map([pattern], |row| row.get::<_, String>(0))
        .map(|mapped| mapped.flatten().collect::<Vec<_>>())
        .unwrap_or_default();

    let mut session = TranscriptSession {
        harness: "cursor",
        session_id: composer_id.to_string(),
        cwd,
        ..Default::default()
    };
    let mut bubbles: Vec<(String, Value)> = Vec::new();
    for value in rows {
        let Ok(bubble) = serde_json::from_str::<Value>(&value) else {
            continue;
        };
        let created = bubble
            .get("createdAt")
            .map(|v| match v {
                Value::Number(n) => n
                    .as_i64()
                    .map(rfc3339_millis)
                    .unwrap_or_else(|| n.to_string()),
                Value::String(s) => s.clone(),
                _ => String::new(),
            })
            .unwrap_or_default();
        bubbles.push((created, bubble));
    }
    if bubbles.is_empty() {
        return None;
    }
    bubbles.sort_by(|a, b| a.0.cmp(&b.0));

    let mut last_was_assistant_text = false;
    for (created, bubble) in &bubbles {
        if session.started_at.is_empty() {
            session.started_at = created.clone();
        }
        session.ended_at = created.clone();
        let bubble_type = bubble.get("type").and_then(|v| v.as_i64()).unwrap_or(0);
        let text = bubble.get("text").and_then(|v| v.as_str()).unwrap_or("");
        match bubble_type {
            1 => {
                // user
                if !super::codex::is_injected(text) {
                    let prompt = clean(text, 2000);
                    if !prompt.is_empty() {
                        if session.objective.is_empty() {
                            session.objective = clean(text, 8000);
                        }
                        push_unique(&mut session.user_prompts, prompt.clone());
                        super::codex::push_tail(&mut session, "user", prompt);
                    }
                }
                last_was_assistant_text = false;
            }
            2 => {
                // assistant
                if !text.trim().is_empty() {
                    super::codex::push_tail(&mut session, "assistant", clean(text, 1500));
                    last_was_assistant_text = true;
                }
                let tool_results = bubble
                    .get("toolResults")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                session.tool_events += tool_results;
            }
            _ => {}
        }
    }
    session.outcome = if last_was_assistant_text {
        Outcome::Completed
    } else {
        Outcome::Unknown
    };
    // B1: the tool metadata shape is unverified — tool call DETAILS are
    // excluded; record that honestly when the session had any.
    if session.tool_events > 0 {
        session.losses.push(super::LossNote {
            what: "tool_metadata".to_string(),
            reason: "shape unverified — tool call details excluded".to_string(),
        });
    }
    Some(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Build a minimal real-shape state.vscdb fixture (composerHeaders +
    /// cursorDiskKV bubbles) in a tempdir.
    fn seed_db(home: &Path, cwd: &str) -> PathBuf {
        let dir = home.join(".config/Cursor/User/globalStorage");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let db_path = dir.join("state.vscdb");
        let db = rusqlite::Connection::open(&db_path).expect("db");
        db.execute_batch(
            "CREATE TABLE composerHeaders (composerId TEXT, workspaceId TEXT, createdAt INTEGER, lastUpdatedAt INTEGER, isArchived INTEGER, isSubagent INTEGER, recency INTEGER, checkpointAt INTEGER, value TEXT);
             CREATE TABLE cursorDiskKV (key TEXT, value TEXT);",
        )
        .expect("schema");
        let head = serde_json::json!({
            "type": "head",
            "composerId": "cmp-1",
            "name": "demo session",
            "createdAt": 1781508855958i64,
            "workspaceIdentifier": {"id": "w1", "uri": {"fsPath": cwd}}
        });
        db.execute(
            "INSERT INTO composerHeaders (composerId, value) VALUES ('cmp-1', ?1)",
            [serde_json::to_string(&head).expect("head")],
        )
        .expect("insert head");
        let bubbles = [
            (
                "b-1",
                1,
                "implement the playlist downloader (sk-proj-AbCdEfGhIjKlMnOpQrStUvWx)",
                "2026-07-01T10:00:01Z",
            ),
            ("b-2", 2, "starting with the schema", "2026-07-01T10:00:02Z"),
            (
                "b-3",
                2,
                "schema migrated, downloading works",
                "2026-07-01T10:00:03Z",
            ),
        ];
        for (bubble_id, bubble_type, text, created) in bubbles {
            let bubble = serde_json::json!({
                "_v": 3,
                "type": bubble_type,
                "text": text,
                "createdAt": created,
                "toolResults": if bubble_id == "b-3" { serde_json::json!(["r1"]) } else { serde_json::json!([]) },
            });
            db.execute(
                "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
                [
                    format!("bubbleId:cmp-1:{bubble_id}"),
                    serde_json::to_string(&bubble).expect("bubble"),
                ],
            )
            .expect("insert bubble");
        }
        // A composer for a different workspace — must be filtered out.
        let other_head = serde_json::json!({
            "type": "head",
            "composerId": "cmp-2",
            "createdAt": 1781508855958i64,
            "workspaceIdentifier": {"id": "w2", "uri": {"fsPath": "/elsewhere"}}
        });
        db.execute(
            "INSERT INTO composerHeaders (composerId, value) VALUES ('cmp-2', ?1)",
            [serde_json::to_string(&other_head).expect("head")],
        )
        .expect("insert other");
        drop(db);
        db_path
    }

    #[test]
    fn cursor_reader_extracts_session_and_filters_workspace() {
        let project = tempfile::tempdir().expect("project");
        let home = tempfile::tempdir().expect("home");
        seed_db(
            home.path(),
            &crate::transcripts::path_for_json(project.path()),
        );

        let sessions = CursorReader.scan(home.path(), project.path());
        assert_eq!(
            sessions.len(),
            1,
            "sessions: {:?}",
            sessions.iter().map(|s| &s.session_id).collect::<Vec<_>>()
        );
        let session = &sessions[0];
        assert_eq!(session.session_id, "cmp-1");
        assert_eq!(session.outcome, Outcome::Completed);
        assert!(session
            .objective
            .starts_with("implement the playlist downloader"));
        // Doctrine: verbatim — credential-looking strings are NOT scrubbed.
        assert!(session
            .objective
            .contains("sk-proj-AbCdEfGhIjKlMnOpQrStUvWx"));
        assert!(!session.objective.contains("[REDACTED]"));
        assert_eq!(session.user_prompts.len(), 1);
        assert_eq!(session.conversation_tail.len(), 3);
        assert_eq!(session.conversation_tail[0].role, "user");
        assert_eq!(session.conversation_tail[2].role, "assistant");
        assert_eq!(session.tool_events, 1);
        assert_eq!(session.started_at, "2026-07-01T10:00:01Z");
        assert_eq!(session.ended_at, "2026-07-01T10:00:03Z");
        // Truth contract: tool metadata is not parsed — empty, not invented.
        assert!(session.files_touched.is_empty());
        assert!(session.failed_approaches.is_empty());
    }
}
