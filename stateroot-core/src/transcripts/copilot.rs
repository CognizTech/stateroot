//! Copilot (VS Code) transcript reader: the Copilot Chat session store
//! `session-store.db` (SQLite) under the editor's globalStorage.
//!
//! Format (observed 2026-09-07 from a live VS Code install; Microsoft marks
//! the store unstable — defensive reads only):
//! - `sessions(id, cwd, repository, host_type, branch, summary, agent_name,
//!   agent_description, created_at, updated_at)` — timestamps are ISO text.
//! - `turns(id, session_id, turn_index, user_message, assistant_response,
//!   timestamp)`.
//!
//! Project filter: `sessions.cwd` via the shared `cwd_matches`. Opens through
//! `open_readonly` (live `mode=ro`, immutable fallback) — the store is written
//! while Copilot is running, and the live session is the one canon wants.
//!
//! Truth contract: only `sessions` + `turns` are read. `checkpoints`,
//! `session_files`, `session_refs` and the FTS index exist but their payloads
//! are unverified — nothing is invented from them.

use std::path::{Path, PathBuf};

use super::cursor::open_readonly;
use super::{
    clean, cwd_matches, push_unique, Outcome, TailEntry, TranscriptReader, TranscriptSession,
};

const OBJECTIVE_MAX: usize = 8000;
const PROMPT_MAX: usize = 2000;
const TAIL_ENTRY_MAX: usize = 1500;
const TAIL_ENTRIES_MAX: usize = 24;

/// Copilot Chat session-store reader.
pub struct CopilotReader;

/// A session row plus its turns in turn_index order:
/// `(turn_index, user_message, assistant_response, timestamp)`.
pub(crate) struct RawSession {
    pub(crate) id: String,
    pub(crate) cwd: String,
    pub(crate) summary: String,
    pub(crate) agent_name: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) turns: Vec<(i64, String, String, String)>,
}

/// Candidate locations of the Copilot Chat session store (Linux, WSL/remote
/// server, macOS, Windows shapes). Cursor's fork is deliberately absent: the
/// owner confirms Copilot Chat does not run there.
pub(crate) fn db_candidates(home: &Path) -> Vec<PathBuf> {
    [
        ".config/Code/User/globalStorage/github.copilot-chat/session-store.db",
        ".vscode-server/data/User/globalStorage/github.copilot-chat/session-store.db",
        "Library/Application Support/Code/User/globalStorage/github.copilot-chat/session-store.db",
        "AppData/Roaming/Code/User/globalStorage/github.copilot-chat/session-store.db",
    ]
    .iter()
    .map(|rel| home.join(rel))
    .filter(|p| p.is_file())
    .collect()
}

/// Every Copilot session belonging to `project_dir` with its raw turns.
pub(crate) fn raw_sessions(db: &rusqlite::Connection, project_dir: &Path) -> Vec<RawSession> {
    let mut out = Vec::new();
    let mut stmt = match db.prepare(
        "SELECT id, COALESCE(cwd,''), COALESCE(summary,''), COALESCE(agent_name,''), \
         COALESCE(created_at,''), COALESCE(updated_at,'') FROM sessions",
    ) {
        Ok(stmt) => stmt,
        Err(_) => return out,
    };
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map(|mapped| mapped.flatten().collect::<Vec<_>>())
        .unwrap_or_default();

    for (id, cwd, summary, agent_name, created_at, updated_at) in rows {
        if cwd.is_empty() || !cwd_matches(&cwd, project_dir) {
            continue;
        }
        let mut stmt = match db.prepare(
            "SELECT turn_index, COALESCE(user_message,''), COALESCE(assistant_response,''), \
             COALESCE(timestamp,'') FROM turns WHERE session_id = ?1 ORDER BY turn_index ASC",
        ) {
            Ok(stmt) => stmt,
            Err(_) => continue,
        };
        let turns: Vec<(i64, String, String, String)> = stmt
            .query_map([&id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map(|mapped| mapped.flatten().collect())
            .unwrap_or_default();
        if turns.is_empty() {
            continue;
        }
        out.push(RawSession {
            id,
            cwd,
            summary,
            agent_name,
            created_at,
            updated_at,
            turns,
        });
    }
    out
}

fn push_tail(tail: &mut Vec<TailEntry>, role: &'static str, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    tail.push(TailEntry {
        role,
        text: text.chars().take(TAIL_ENTRY_MAX).collect(),
    });
    if tail.len() > TAIL_ENTRIES_MAX {
        let drop = tail.len() - TAIL_ENTRIES_MAX;
        tail.drain(0..drop);
    }
}

impl TranscriptReader for CopilotReader {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn scan(&self, home: &Path, project_dir: &Path) -> Vec<TranscriptSession> {
        let mut out = Vec::new();
        for db_path in db_candidates(home) {
            let Ok(db) = open_readonly(&db_path) else {
                continue;
            };
            for raw in raw_sessions(&db, project_dir) {
                let mut session = TranscriptSession {
                    harness: "copilot",
                    session_id: raw.id.clone(),
                    cwd: raw.cwd.clone(),
                    started_at: raw.created_at.clone(),
                    ended_at: if raw.updated_at.is_empty() {
                        raw.created_at.clone()
                    } else {
                        raw.updated_at.clone()
                    },
                    ..Default::default()
                };
                let mut saw_assistant = false;
                for (_, user_message, assistant_response, ts) in &raw.turns {
                    if session.started_at.is_empty() && !ts.is_empty() {
                        session.started_at = ts.clone();
                    }
                    if session.ended_at.is_empty() && !ts.is_empty() {
                        // Fallback only: the session row's updated_at is the
                        // authoritative span end when present.
                        session.ended_at = ts.clone();
                    }
                    let user_text = clean(user_message, PROMPT_MAX);
                    if !user_text.is_empty() {
                        if session.objective.is_empty() {
                            session.objective = clean(user_message, OBJECTIVE_MAX);
                        }
                        push_unique(&mut session.user_prompts, user_text.clone());
                        push_tail(&mut session.conversation_tail, "user", &user_text);
                    }
                    if !assistant_response.trim().is_empty() {
                        let answer = clean(assistant_response, TAIL_ENTRY_MAX);
                        push_tail(&mut session.conversation_tail, "assistant", &answer);
                        saw_assistant = true;
                    }
                }
                session.outcome = if saw_assistant {
                    Outcome::Completed
                } else {
                    Outcome::Unknown
                };
                out.push(session);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal real-shape session-store.db fixture in a tempdir.
    fn seed_db(home: &Path, cwd: &str) -> PathBuf {
        let dir = home.join(".config/Code/User/globalStorage/github.copilot-chat");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let db_path = dir.join("session-store.db");
        let db = rusqlite::Connection::open(&db_path).expect("db");
        db.execute_batch(
            "CREATE TABLE sessions (id TEXT, cwd TEXT, repository TEXT, host_type TEXT, branch TEXT, summary TEXT, agent_name TEXT, agent_description TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE turns (id INTEGER, session_id TEXT, turn_index INTEGER, user_message TEXT, assistant_response TEXT, timestamp TEXT);",
        )
        .expect("schema");
        db.execute(
            "INSERT INTO sessions (id, cwd, summary, agent_name, created_at, updated_at) VALUES ('cs-1', ?1, 'demo', 'agent', '2026-09-01T10:00:00Z', '2026-09-01T10:02:00Z')",
            [cwd],
        )
        .expect("session");
        db.execute(
            "INSERT INTO turns (id, session_id, turn_index, user_message, assistant_response, timestamp) VALUES (1, 'cs-1', 0, 'wire the copilot reader', '', '2026-09-01T10:00:01Z')",
            [],
        )
        .expect("t0");
        db.execute(
            "INSERT INTO turns (id, session_id, turn_index, user_message, assistant_response, timestamp) VALUES (2, 'cs-1', 1, '', 'reader wired and tested', '2026-09-01T10:01:00Z')",
            [],
        )
        .expect("t1");
        // A session for a different workspace — must be filtered out.
        db.execute(
            "INSERT INTO sessions (id, cwd, summary, agent_name, created_at, updated_at) VALUES ('cs-2', '/elsewhere', 'other', 'agent', '2026-09-01T11:00:00Z', '2026-09-01T11:01:00Z')",
            [],
        )
        .expect("other");
        db.execute(
            "INSERT INTO turns (id, session_id, turn_index, user_message, assistant_response, timestamp) VALUES (3, 'cs-2', 0, 'unrelated', 'yes', '2026-09-01T11:00:01Z')",
            [],
        )
        .expect("t2");
        drop(db);
        db_path
    }

    #[test]
    fn copilot_reader_extracts_sessions_and_filters_workspace() {
        let project = tempfile::tempdir().expect("project");
        let home = tempfile::tempdir().expect("home");
        seed_db(
            home.path(),
            &crate::transcripts::path_for_json(project.path()),
        );

        let sessions = CopilotReader.scan(home.path(), project.path());
        assert_eq!(
            sessions.len(),
            1,
            "sessions: {:?}",
            sessions.iter().map(|s| &s.session_id).collect::<Vec<_>>()
        );
        let session = &sessions[0];
        assert_eq!(session.session_id, "cs-1");
        assert_eq!(session.outcome, Outcome::Completed);
        assert!(session.objective.contains("wire the copilot reader"));
        assert_eq!(session.user_prompts.len(), 1);
        assert_eq!(session.conversation_tail.len(), 2);
        assert_eq!(session.conversation_tail[0].role, "user");
        assert_eq!(session.conversation_tail[1].role, "assistant");
        assert_eq!(session.started_at, "2026-09-01T10:00:00Z");
        assert_eq!(session.ended_at, "2026-09-01T10:02:00Z");
    }

    /// Live-WAL regression (same class as issue #1): a committed turn still
    /// in an uncheckpointed WAL must be visible while the writer stays open.
    #[test]
    fn copilot_reader_sees_committed_wal_rows_from_live_writer() {
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let home = tempfile::tempdir().expect("home");
        let dir = home
            .path()
            .join(".config/Code/User/globalStorage/github.copilot-chat");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let db_path = dir.join("session-store.db");
        // Live writer: WAL mode, checkpoints disabled, kept OPEN for the scan.
        let writer = rusqlite::Connection::open(&db_path).expect("db");
        writer
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA wal_autocheckpoint=0;
                 CREATE TABLE sessions (id TEXT, cwd TEXT, summary TEXT, agent_name TEXT, created_at TEXT, updated_at TEXT);
                 CREATE TABLE turns (id INTEGER, session_id TEXT, turn_index INTEGER, user_message TEXT, assistant_response TEXT, timestamp TEXT);
                 PRAGMA wal_checkpoint(TRUNCATE);",
            )
            .expect("schema");
        writer
            .execute(
                "INSERT INTO sessions (id, cwd, summary, agent_name, created_at, updated_at) VALUES ('cs-live', ?1, 'live', 'agent', '2026-09-07T10:00:00Z', '2026-09-07T10:01:00Z')",
                [&cwd],
            )
            .expect("session");
        writer
            .execute(
                "INSERT INTO turns (id, session_id, turn_index, user_message, assistant_response, timestamp) VALUES (1, 'cs-live', 0, 'live turn', 'live answer', '2026-09-07T10:00:30Z')",
                [],
            )
            .expect("turn");

        // The fixture really exercises the WAL path: the immutable snapshot
        // opener cannot see the uncheckpointed rows.
        let snapshot = crate::transcripts::cursor::open_immutable(&db_path).expect("immutable");
        let visible: i64 = snapshot
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .expect("count");
        assert_eq!(visible, 0, "immutable snapshot should miss live WAL rows");
        drop(snapshot);

        let sessions = CopilotReader.scan(home.path(), project.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "cs-live");
        assert!(sessions[0].objective.contains("live turn"));
        drop(writer);
    }
}
