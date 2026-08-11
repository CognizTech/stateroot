//! Hermes transcript reader: `~/.hermes/state.db` (SQLite, authoritative).
//!
//! Format (Hermes session-storage docs + `hermes_state.py`):
//! - `sessions(id, source, cwd, git_repo_root, started_at, ended_at, …)`
//! - `messages(session_id, role, content, tool_calls, tool_name, timestamp, …)`
//! - Legacy `~/.hermes/sessions/*.jsonl` is no longer primary — ignored.
//!
//! Project filter: prefer `git_repo_root`, else `cwd` via `cwd_matches`.
//! Open immutable (`?immutable=1`) so a live WAL store still reads.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::cursor::open_immutable;
use super::kimi::rfc3339_millis;
use super::{
    clean, cwd_matches, push_unique, Outcome, TailEntry, TranscriptReader, TranscriptSession,
};

const OBJECTIVE_MAX: usize = 8000;
const PROMPT_MAX: usize = 2000;
const TAIL_ENTRY_MAX: usize = 1500;
const TAIL_ENTRIES_MAX: usize = 24;

/// Hermes `state.db` reader.
pub struct HermesReader;

impl TranscriptReader for HermesReader {
    fn id(&self) -> &'static str {
        "hermes"
    }

    fn scan(&self, home: &Path, project_dir: &Path) -> Vec<TranscriptSession> {
        let mut out = Vec::new();
        for db_path in db_candidates(home) {
            let Ok(db) = open_immutable(&db_path) else {
                continue;
            };
            out.extend(scan_db(&db, project_dir));
        }
        out
    }
}

pub(crate) fn db_candidates(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(hermes_home) = std::env::var("HERMES_HOME") {
        let p = PathBuf::from(hermes_home.trim()).join("state.db");
        if p.is_file() {
            paths.push(p);
        }
    }
    let default = home.join(".hermes/state.db");
    if default.is_file() {
        paths.push(default);
    }
    paths
}

fn scan_db(db: &rusqlite::Connection, project_dir: &Path) -> Vec<TranscriptSession> {
    let mut out = Vec::new();
    let mut stmt = match db.prepare(
        "SELECT id, COALESCE(cwd,''), COALESCE(git_repo_root,''), \
         COALESCE(started_at,0), COALESCE(ended_at,0) FROM sessions",
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
                row.get::<_, f64>(3).unwrap_or(0.0),
                row.get::<_, f64>(4).unwrap_or(0.0),
            ))
        })
        .map(|mapped| mapped.flatten().collect::<Vec<_>>())
        .unwrap_or_default();

    for (id, cwd, git_root, started, ended) in rows {
        let filter_path = if !git_root.trim().is_empty() {
            git_root.as_str()
        } else {
            cwd.as_str()
        };
        if filter_path.is_empty() || !cwd_matches(filter_path, project_dir) {
            continue;
        }
        let Some(session) = load_session(db, &id, &cwd, started, ended) else {
            continue;
        };
        out.push(session);
    }
    out
}

fn load_session(
    db: &rusqlite::Connection,
    session_id: &str,
    cwd: &str,
    started: f64,
    ended: f64,
) -> Option<TranscriptSession> {
    let mut session = TranscriptSession {
        harness: "hermes",
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
        started_at: float_to_rfc3339(started),
        ended_at: float_to_rfc3339(if ended > 0.0 { ended } else { started }),
        ..Default::default()
    };
    let mut stmt = db
        .prepare(
            "SELECT COALESCE(role,''), COALESCE(content,''), COALESCE(tool_calls,''), \
             COALESCE(tool_name,''), COALESCE(timestamp,0) \
             FROM messages WHERE session_id = ?1 ORDER BY timestamp ASC, rowid ASC",
        )
        .ok()?;
    let rows = stmt
        .query_map([session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, f64>(4).unwrap_or(0.0),
            ))
        })
        .ok()?
        .flatten()
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return None;
    }
    let mut saw_assistant = false;
    for (role, content, tool_calls, tool_name, ts) in rows {
        if session.started_at.is_empty() && ts > 0.0 {
            session.started_at = float_to_rfc3339(ts);
        }
        if ts > 0.0 {
            session.ended_at = float_to_rfc3339(ts);
        }
        let role_l = role.to_lowercase();
        let text = content_text(&content);
        if !tool_calls.trim().is_empty() || !tool_name.trim().is_empty() {
            session.tool_events += 1;
        }
        match role_l.as_str() {
            "user" | "human" => {
                let cleaned = clean(&text, OBJECTIVE_MAX);
                if cleaned.is_empty() {
                    continue;
                }
                if session.objective.is_empty() {
                    session.objective = cleaned.clone();
                }
                push_unique(&mut session.user_prompts, clean(&text, PROMPT_MAX));
                push_tail(&mut session.conversation_tail, "user", &cleaned);
            }
            "assistant" | "model" => {
                saw_assistant = true;
                let cleaned = clean(&text, TAIL_ENTRY_MAX);
                if !cleaned.is_empty() {
                    push_tail(&mut session.conversation_tail, "assistant", &cleaned);
                }
            }
            "tool" | "toolresult" | "function" => {
                session.tool_events += 1;
                if text.to_lowercase().contains("error") {
                    push_unique(&mut session.failed_approaches, clean(&text, 800));
                }
            }
            _ => {}
        }
    }
    session.outcome = if saw_assistant {
        Outcome::Completed
    } else {
        Outcome::Interrupted
    };
    Some(session)
}

fn content_text(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            return match value {
                Value::String(s) => s,
                Value::Array(blocks) => blocks
                    .iter()
                    .filter_map(|b| {
                        b.get("text")
                            .and_then(|t| t.as_str())
                            .or_else(|| b.as_str())
                            .map(str::to_string)
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                Value::Object(map) => map
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or(trimmed)
                    .to_string(),
                _ => trimmed.to_string(),
            };
        }
    }
    trimmed.to_string()
}

fn float_to_rfc3339(ts: f64) -> String {
    if ts <= 0.0 {
        return String::new();
    }
    // Hermes stores unix seconds (float); tolerate millis.
    let ms = if ts > 1e12 {
        ts as i64
    } else {
        (ts * 1000.0) as i64
    };
    rfc3339_millis(ms)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_reader_scans_state_db() {
        let project = tempfile::tempdir().expect("project");
        let cwd = crate::transcripts::path_for_json(project.path());
        let home = tempfile::tempdir().expect("home");
        let db_path = home.path().join(".hermes/state.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).expect("mkdir");
        let db = rusqlite::Connection::open(&db_path).expect("db");
        db.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY, cwd TEXT, git_repo_root TEXT,
                started_at REAL, ended_at REAL
             );
             CREATE TABLE messages (
                session_id TEXT, role TEXT, content TEXT,
                tool_calls TEXT, tool_name TEXT, timestamp REAL
             );",
        )
        .expect("schema");
        db.execute(
            "INSERT INTO sessions (id, cwd, git_repo_root, started_at, ended_at) VALUES (?1,?2,'',1700000000,1700000060)",
            rusqlite::params!["ses-h1", cwd],
        )
        .expect("session");
        db.execute(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_name, timestamp) VALUES (?1,'user',?2,'','',1700000001)",
            rusqlite::params!["ses-h1", "draft the weekly brief"],
        )
        .expect("u");
        db.execute(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_name, timestamp) VALUES (?1,'assistant',?2,'','',1700000002)",
            rusqlite::params!["ses-h1", "here is the brief"],
        )
        .expect("a");
        drop(db);

        let sessions = HermesReader.scan(home.path(), project.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "ses-h1");
        assert!(sessions[0].objective.contains("weekly brief"));
        assert_eq!(sessions[0].outcome, Outcome::Completed);
    }

    #[test]
    fn hermes_reader_filters_other_cwd() {
        let project = tempfile::tempdir().expect("project");
        let home = tempfile::tempdir().expect("home");
        let db_path = home.path().join(".hermes/state.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).expect("mkdir");
        let db = rusqlite::Connection::open(&db_path).expect("db");
        db.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, cwd TEXT, git_repo_root TEXT, started_at REAL, ended_at REAL);
             CREATE TABLE messages (session_id TEXT, role TEXT, content TEXT, tool_calls TEXT, tool_name TEXT, timestamp REAL);",
        )
        .expect("schema");
        db.execute(
            "INSERT INTO sessions (id, cwd, git_repo_root, started_at, ended_at) VALUES ('x','/elsewhere','',1,2)",
            [],
        )
        .expect("s");
        drop(db);
        assert!(HermesReader.scan(home.path(), project.path()).is_empty());
    }
}
