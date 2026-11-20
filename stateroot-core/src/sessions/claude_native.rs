//! Claude Code native session archives — local, immutable, evidence-only.
//!
//! Claude keeps one JSONL file per session under
//! `<config root>/projects/<slug>/<session-id>.jsonl`. This module retains the
//! *settled prefix* of such a file — everything through the last completed
//! assistant message — so an archive is a coherent session boundary rather
//! than a torn snapshot of a turn still in flight. Appends are fine; a
//! rewrite or truncate cannot silently change what was retained (the source is
//! rechecked after metadata reads, exactly like the Codex archive).
//!
//! Archives live in `.stateroot/local/sessions/native-claude/` and never leave
//! the machine. [`restore`] materializes one archive as a **new** session file
//! under the caller-supplied Claude home — `create_new`, a fresh session id,
//! and the copy is parsed back before the operation is recorded. It never
//! edits, replaces or deletes an existing session file.
//!
//! Unlike the Codex archive, no record field is rewritten or dropped: Claude's
//! record types are undocumented internals, so the archived bytes are the
//! source bytes, and every manifest field is derived from them.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{harness_install::paths, safe_io};

/// Manifest schema id.
pub const ARCHIVE_SCHEMA: &str = "stateroot.claude-archive.v1";

/// One retained Claude session prefix.
///
/// Every field is derived from the source bytes or the local environment and
/// is deterministic, so retaining the same prefix twice yields the same id
/// (and therefore no duplicate archive).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeArchive {
    /// Schema tag.
    pub schema: String,
    /// Content-addressed archive id (blake3 over this manifest with `id` empty).
    pub id: String,
    /// Claude session id (`sessionId` in the records, else the file stem).
    pub source_session_id: String,
    /// The live session file this prefix came from.
    pub source_path: PathBuf,
    /// Append-only source generation file name, relative to `sources/`.
    pub source_object: String,
    /// sha256 of the exact archived prefix bytes.
    pub raw_sha256: String,
    /// Digest of the parsed records (catches a manifest/bytes mismatch).
    pub prefix_digest: String,
    /// Archived prefix length in bytes.
    pub byte_length: usize,
    /// Archived record count.
    pub record_count: usize,
    /// Working directory the session ran in.
    pub cwd: PathBuf,
    /// Resolved Claude config root (honors `CLAUDE_CONFIG_DIR`).
    pub claude_home: PathBuf,
    /// Claude Code version recorded in the session records.
    pub cli_version: String,
    /// Host platform.
    pub platform: String,
    /// StateRoot project id when the local manifest is readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Lineage root at capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_root: Option<String>,
    /// Lineage ref at capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_lineage: Option<String>,
    /// Type of the last archived record (for inspection, never a verdict).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_record_type: Option<String>,
}

/// A retained boundary plus why capture is not the whole live file.
#[derive(Debug)]
pub(crate) struct RetainedBoundary {
    /// The retained archive.
    pub archive: NativeArchive,
    /// Named reason when the live file holds an unsettled tail.
    pub suffix_reason: Option<String>,
}

/// Local archive root for this project.
pub fn archive_dir(project: &Path) -> PathBuf {
    super::store_dir(project).join("native-claude")
}

fn archive_path(project: &Path, id: &str) -> Result<PathBuf, String> {
    if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(
            "expected full native archive digest (use session list or session show)".into(),
        );
    }
    Ok(archive_dir(project).join(id))
}

fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn raw_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn json_digest(value: &impl Serialize) -> Result<String, String> {
    serde_json::to_vec(value)
        .map(|v| digest(&v))
        .map_err(|e| e.to_string())
}

fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The archived slice of one session file, and why it is not the whole file.
struct Settled {
    end: usize,
    suffix_reason: Option<String>,
}

/// True for a record that closes a turn: an assistant message that carries
/// non-empty text. A bare `tool_use` reply is mid-turn (its results are still
/// pending), so it never ends the prefix.
fn closes_turn(value: &Value) -> bool {
    // Subagent (sidechain) traffic never closes a session turn: a boundary
    // settled inside a subagent exchange yields a "session" with no main
    // thread, which the native harness then refuses to resume.
    if value
        .get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return false;
    }
    value.get("type").and_then(Value::as_str) == Some("assistant")
        && value
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    block.get("type").and_then(Value::as_str) == Some("text")
                        && block
                            .get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| !text.trim().is_empty())
                })
            })
}

/// Split the session file into the retained prefix and its unsettled tail.
///
/// Every prefix line must parse: a corrupt record is an error, never a skipped
/// line, because a silently shortened archive would misrepresent the session.
fn settled_prefix(bytes: &[u8]) -> Result<Settled, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    let mut offset = 0usize;
    let mut last_settled: Option<usize> = None;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value =
            serde_json::from_str(trimmed).map_err(|e| format!("corrupt native record: {e}"))?;
        if closes_turn(&value) {
            last_settled = Some(offset);
        }
    }
    let end = last_settled.unwrap_or(offset);
    let suffix_reason = match last_settled {
        None => Some(
            "no completed assistant turn in the file; the whole file was captured, but it is not a settled boundary"
                .to_string(),
        ),
        Some(_) if end < offset => Some(
            "unsettled tail after the last completed assistant message; the archive keeps the settled prefix"
                .to_string(),
        ),
        Some(_) => None,
    };
    Ok(Settled { end, suffix_reason })
}

/// Parse every archived line as an object (strict, for the record digest).
fn parse_records(bytes: &[u8]) -> Result<Vec<Value>, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let value: Value =
                serde_json::from_str(line).map_err(|e| format!("corrupt native record: {e}"))?;
            if !value.is_object() {
                return Err("native record is not an object".into());
            }
            Ok(value)
        })
        .collect()
}

/// First non-empty string value for `key` across the records.
fn first_text<'a>(records: &'a [Value], key: &str) -> Option<&'a str> {
    records
        .iter()
        .filter_map(|record| record.get(key).and_then(Value::as_str))
        .find(|value| !value.trim().is_empty())
}

/// Called by the session sync while holding its store lock. Raw archives stay
/// in the established `local/sessions` boundary and are immutable.
pub(crate) fn retain(
    project: &Path,
    home: &Path,
    source: &Path,
) -> Result<RetainedBoundary, String> {
    let bytes = std::fs::read(source).map_err(|e| e.to_string())?;
    let boundary = settled_prefix(&bytes)?;
    let raw = &bytes[..boundary.end];
    let records = parse_records(raw)?;
    if records.is_empty() {
        return Err("empty native source".into());
    }

    let session_id = first_text(&records, "sessionId")
        .map(str::to_string)
        .or_else(|| {
            source
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
        })
        .ok_or("native source has no session identity")?;
    uuid::Uuid::parse_str(&session_id)
        .map_err(|_| "native evidence requires complete UUID identities".to_string())?;

    let cwd_raw = first_text(&records, "cwd").ok_or("native source has no cwd")?;
    if !crate::transcripts::cwd_matches(cwd_raw, project) {
        return Err("native recovery unavailable: source workspace differs".into());
    }
    let cwd = PathBuf::from(cwd_raw)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(cwd_raw));

    let claude_home = paths::claude_root(home);
    let mut archive = NativeArchive {
        schema: ARCHIVE_SCHEMA.into(),
        id: String::new(),
        source_session_id: session_id.clone(),
        source_path: source.to_path_buf(),
        source_object: String::new(),
        raw_sha256: raw_sha256(raw),
        prefix_digest: json_digest(&records)?,
        byte_length: raw.len(),
        record_count: records.len(),
        cwd: cwd.clone(),
        claude_home: claude_home.clone(),
        cli_version: first_text(&records, "version")
            .unwrap_or("unknown")
            .to_string(),
        platform: platform(),
        project_id: crate::local_store::read_manifest(project)
            .ok()
            .flatten()
            .and_then(|manifest| manifest["project_id"].as_str().map(str::to_owned)),
        capture_root: crate::roots::latest_root(project).ok().flatten(),
        capture_lineage: crate::roots::lineage_refname_checked(project).ok(),
        last_record_type: records
            .last()
            .and_then(|record| record.get("type"))
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    archive.source_object = retain_source(project, &session_id, raw)?;
    archive.id = json_digest(&archive)?;

    let target = archive_path(project, &archive.id)?;
    if target.exists() {
        return Ok(RetainedBoundary {
            archive: load(project, &archive.id)?,
            suffix_reason: boundary.suffix_reason,
        });
    }
    // Recheck the captured prefix after the metadata reads. Appends are fine;
    // a truncate/rewrite cannot silently change the source being retained.
    if !std::fs::read(source)
        .map_err(|e| e.to_string())?
        .starts_with(raw)
    {
        return Err("native source changed during archive capture".into());
    }
    std::fs::create_dir_all(archive_dir(project)).map_err(|e| e.to_string())?;
    let temp = tempfile::Builder::new()
        .prefix(".capture-")
        .tempdir_in(archive_dir(project))
        .map_err(|e| e.to_string())?;
    safe_io::atomic_replace_json(
        &temp.path().join("manifest.json"),
        &serde_json::to_value(&archive).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(temp.path(), &target).map_err(|e| e.to_string())?;
    Ok(RetainedBoundary {
        archive,
        suffix_reason: boundary.suffix_reason,
    })
}

/// One append-only native source per observed generation. A rewritten history
/// creates another generation; old bytes and archive references never move.
fn retain_source(project: &Path, session_id: &str, raw: &[u8]) -> Result<String, String> {
    let dir = archive_dir(project).join("sources");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let prefix = format!("{session_id}-");
    for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&prefix) {
            continue;
        }
        let existing = std::fs::read(entry.path()).map_err(|e| e.to_string())?;
        if existing.starts_with(raw) {
            return Ok(name);
        }
        if raw.starts_with(&existing) {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(entry.path())
                .map_err(|e| e.to_string())?;
            file.write_all(&raw[existing.len()..])
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            return Ok(name);
        }
    }
    let name = format!("{session_id}-{}.jsonl", raw_sha256(raw));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(dir.join(&name))
        .map_err(|e| e.to_string())?;
    file.write_all(raw)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(name)
}

/// Exact archived bytes for one manifest (never the live session file).
fn archive_bytes(project: &Path, archive: &NativeArchive) -> Result<Vec<u8>, String> {
    use std::io::Read;
    if Path::new(&archive.source_object).components().count() != 1
        || !archive
            .source_object
            .starts_with(&format!("{}-", archive.source_session_id))
    {
        return Err("unsupported native source object locator".into());
    }
    let file = std::fs::File::open(
        archive_dir(project)
            .join("sources")
            .join(&archive.source_object),
    )
    .map_err(|e| format!("native archive unavailable: {e}"))?;
    let mut bytes = Vec::new();
    file.take(archive.byte_length as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() != archive.byte_length || raw_sha256(&bytes) != archive.raw_sha256 {
        return Err(
            "native archive corrupt: exact historical bytes unavailable; live history is not a substitute"
                .into(),
        );
    }
    Ok(bytes)
}

fn manifest(project: &Path, id: &str) -> Result<NativeArchive, String> {
    let dir = archive_path(project, id)?;
    let mut archive: NativeArchive = serde_json::from_slice(
        &std::fs::read(dir.join("manifest.json"))
            .map_err(|e| format!("native archive unavailable: {e}"))?,
    )
    .map_err(|e| format!("native manifest corrupt: {e}"))?;
    if archive.schema != ARCHIVE_SCHEMA || archive.id != id {
        return Err("unsupported native archive identity".into());
    }
    archive.id.clear();
    if json_digest(&archive)? != id {
        return Err("native archive manifest digest mismatch".into());
    }
    archive.id = id.into();
    Ok(archive)
}

/// Load and verify one archive against its exact retained bytes.
pub fn load(project: &Path, id: &str) -> Result<NativeArchive, String> {
    let archive = manifest(project, id)?;
    let bytes = archive_bytes(project, &archive)?;
    let records = parse_records(&bytes)?;
    if json_digest(&records)? != archive.prefix_digest || records.len() != archive.record_count {
        return Err(
            "native archive corrupt: digest mismatch; newer live history is not a substitute"
                .into(),
        );
    }
    let boundary = settled_prefix(&bytes)?;
    if boundary.end != bytes.len() {
        return Err("native archive boundary identity mismatch".into());
    }
    Ok(archive)
}

/// Every retained archive for this project, malformed ones reported honestly.
pub fn inventory(project: &Path) -> Vec<Value> {
    let mut rows = Vec::new();
    if let Ok(entries) = std::fs::read_dir(archive_dir(project)) {
        for entry in entries.flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            if id.len() != 64 {
                continue;
            }
            rows.push(match manifest(project, &id) {
                Ok(archive) => json!({
                    "archive_id": id,
                    "source_session_id": archive.source_session_id,
                    "byte_length": archive.byte_length,
                    "record_count": archive.record_count,
                    "last_record_type": archive.last_record_type,
                    "capture_root": archive.capture_root,
                    "verification": "manifest verified; payload checked on exact selection",
                    "storage": "local only",
                }),
                Err(e) => {
                    json!({"archive_id": id, "native_archive_verified": false, "unavailable": e})
                }
            });
        }
    }
    rows.sort_by_key(|row| row["archive_id"].as_str().unwrap_or_default().to_owned());
    rows
}

/// Restore receipt schema id.
pub const RESTORE_SCHEMA: &str = "stateroot.claude-restore.v1";

/// Evidence that an archive was materialized as a new native session file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreReceipt {
    /// Schema tag.
    pub schema: String,
    /// Caller-supplied idempotency key (a UUID).
    pub operation: String,
    /// Archive the destination was written from.
    pub archive_id: String,
    /// Session id inside the archive.
    pub source_session_id: String,
    /// Session id of the restored copy (always new).
    pub destination_session_id: String,
    /// Exact file written under the Claude session store.
    pub destination_path: PathBuf,
    /// Bytes written.
    pub byte_length: usize,
    /// Records written.
    pub record_count: usize,
}

fn operations_dir(project: &Path) -> PathBuf {
    archive_dir(project).join("operations")
}

fn operation_path(project: &Path, operation: &str) -> Result<PathBuf, String> {
    uuid::Uuid::parse_str(operation).map_err(|_| "operation id must be a UUID".to_string())?;
    Ok(operations_dir(project).join(format!("{operation}.json")))
}

/// Read the receipt for one restore operation.
pub fn receipt(project: &Path, operation: &str) -> Result<RestoreReceipt, String> {
    let path = operation_path(project, operation)?;
    let receipt: RestoreReceipt = serde_json::from_slice(
        &std::fs::read(&path).map_err(|e| format!("native operation unavailable: {e}"))?,
    )
    .map_err(|e| format!("native operation corrupt: {e}"))?;
    if receipt.schema != RESTORE_SCHEMA || receipt.operation != operation {
        return Err("unsupported native operation identity".into());
    }
    Ok(receipt)
}

/// The Claude session directory for this project: an existing slug directory
/// whose decoded cwd overlaps the project, else the encoded slug.
///
/// An existing directory always wins: Claude's encoding is undocumented, and a
/// restored session is only findable if it lands beside the project's real
/// sessions.
pub(crate) fn project_session_dir(home: &Path, project: &Path) -> PathBuf {
    let root = paths::claude_projects_root(home);
    let project_norm = crate::path_identity::normalize_host_path(&project.to_string_lossy());
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let slug = entry.file_name().to_string_lossy().into_owned();
            let decoded =
                crate::path_identity::normalize_host_path(&paths::claude_slug_to_cwd(&slug));
            if decoded == project_norm
                || project_norm.starts_with(&format!("{decoded}/"))
                || decoded.starts_with(&format!("{project_norm}/"))
            {
                return entry.path();
            }
        }
    }
    root.join(paths::claude_slug_encode(&project.to_string_lossy()))
}

/// Prove a written session file is one this harness will actually read: it
/// must parse as a Claude session for this project with exactly `id`.
///
/// Both restore and transfer call this before reporting success — a file that
/// the Claude reader rejects is a silent failure otherwise.
pub fn verify_written(path: &Path, project: &Path, id: &str) -> Result<(), String> {
    let parsed = crate::transcripts::claude::parse_session(path, project);
    match parsed {
        Some(session) if session.session_id == id => Ok(()),
        _ => Err("written session did not parse as a Claude session for this project".into()),
    }
}

/// Materialize one retained archive as a **new** Claude session file under
/// `home`'s Claude session store.
///
/// Contract:
/// - the destination file is created with `create_new` — an existing session
///   file is never overwritten;
/// - the copy gets a fresh session id (every record's `sessionId` rewritten
///   consistently) while `uuid` / `parentUuid` / `cwd` / timestamps / version
///   stay verbatim, so the history keeps its shape;
/// - the written file is parsed back with the real Claude reader before the
///   operation is recorded, and a file that does not parse as this project's
///   session with the new id is removed and reported as a failure;
/// - the operation is idempotent: repeating it returns the original receipt,
///   and reusing an operation id for a different archive is refused.
pub fn restore(
    project: &Path,
    home: &Path,
    archive_id: &str,
    operation: &str,
) -> Result<RestoreReceipt, String> {
    let receipt_path = operation_path(project, operation)?;
    if receipt_path.exists() {
        let existing = receipt(project, operation)?;
        if existing.archive_id != archive_id {
            return Err("native operation belongs to a different archive".into());
        }
        return Ok(existing);
    }
    let archive = load(project, archive_id)?;
    let bytes = archive_bytes(project, &archive)?;
    let new_id = uuid::Uuid::new_v4().to_string();

    let mut lines = Vec::with_capacity(archive.record_count);
    for record in parse_records(&bytes)? {
        let mut record = record;
        record["sessionId"] = Value::String(new_id.clone());
        lines.push(serde_json::to_string(&record).map_err(|e| e.to_string())?);
    }
    let payload = format!("{}\n", lines.join("\n"));

    let dir = project_session_dir(home, project);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let destination = dir.join(format!("{new_id}.jsonl"));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&destination)
        .map_err(|e| format!("cannot create {}: {e}", destination.display()))?;
    use std::io::Write as _;
    file.write_all(payload.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;

    // Prove the copy is a session this harness will actually read, before the
    // operation is recorded — otherwise the failure would be invisible.
    if verify_written(&destination, project, &new_id).is_err() {
        let _ = std::fs::remove_file(&destination);
        return Err("restored session did not parse as a Claude session for this project".into());
    }

    let receipt = RestoreReceipt {
        schema: RESTORE_SCHEMA.into(),
        operation: operation.to_string(),
        archive_id: archive_id.to_string(),
        source_session_id: archive.source_session_id.clone(),
        destination_session_id: new_id,
        destination_path: destination,
        byte_length: payload.len(),
        record_count: lines.len(),
    };
    std::fs::create_dir_all(operations_dir(project)).map_err(|e| e.to_string())?;
    safe_io::atomic_replace_json(
        &receipt_path,
        &serde_json::to_value(&receipt).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(receipt)
}

/// Configuration files that shape a Claude session, fingerprinted (never
/// copied). Absent files are recorded as `absent`, not omitted.
pub fn config_fingerprints(claude_home: &Path, cwd: &Path) -> BTreeMap<String, String> {
    let mut names = std::collections::BTreeSet::from([
        claude_home.join("settings.json"),
        claude_home.join("settings.local.json"),
    ]);
    for ancestor in cwd.ancestors() {
        names.insert(ancestor.join(".claude/settings.json"));
        names.insert(ancestor.join(".claude/settings.local.json"));
    }
    names
        .into_iter()
        .map(|path| {
            let hash = match std::fs::read(&path) {
                Ok(bytes) => digest(&bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => "absent".into(),
                Err(e) => format!("unreadable: {e}"),
            };
            let label = path
                .strip_prefix(claude_home)
                .map(|relative| format!("native/{}", relative.display()))
                .unwrap_or_else(|_| path.to_string_lossy().into_owned());
            (label, hash)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("project");
        crate::local_store::init_skeleton(dir.path(), "p", "n", "default").expect("init");
        dir
    }

    fn session_file(home: &Path, id: &str, lines: &[Value]) -> PathBuf {
        let dir = paths::claude_projects_root(home).join("test-slug");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join(format!("{id}.jsonl"));
        let body: String = lines
            .iter()
            .map(|value| format!("{value}\n"))
            .collect::<String>();
        std::fs::write(&path, body).expect("write");
        path
    }

    fn user_line(session: &str, cwd: &Path) -> Value {
        json!({
            "type": "user",
            "uuid": "u-1",
            "parentUuid": null,
            "sessionId": session,
            "cwd": cwd.to_string_lossy(),
            "version": "2.1.281",
            "timestamp": "2026-11-19T00:00:00Z",
            "message": {"role": "user", "content": "hello"},
        })
    }

    fn assistant_text(session: &str, cwd: &Path, text: &str) -> Value {
        json!({
            "type": "assistant",
            "uuid": "a-1",
            "parentUuid": "u-1",
            "sessionId": session,
            "cwd": cwd.to_string_lossy(),
            "version": "2.1.281",
            "timestamp": "2026-11-19T00:00:01Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": text}]},
        })
    }

    #[test]
    fn retain_archives_settled_prefix_and_is_idempotent() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        let session = "11111111-2222-3333-4444-555555555555";
        let source = session_file(
            home.path(),
            session,
            &[
                user_line(session, project.path()),
                assistant_text(session, project.path(), "done"),
            ],
        );
        let first = retain(project.path(), home.path(), &source).expect("retain");
        assert!(first.suffix_reason.is_none(), "{:?}", first.suffix_reason);
        assert_eq!(first.archive.source_session_id, session);
        assert_eq!(first.archive.record_count, 2);
        assert_eq!(first.archive.cli_version, "2.1.281");
        assert!(archive_path(project.path(), &first.archive.id)
            .expect("path")
            .join("manifest.json")
            .is_file());

        // Same bytes → same id → no duplicate archive.
        let second = retain(project.path(), home.path(), &source).expect("retain again");
        assert_eq!(second.archive.id, first.archive.id);
        let rows = inventory(project.path());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["archive_id"], first.archive.id.as_str());
    }

    #[test]
    fn retain_keeps_the_settled_prefix_and_names_the_tail() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        let session = "11111111-2222-3333-4444-555555555556";
        // A trailing tool_use reply is mid-turn, so it must not enter the
        // prefix; the retained bytes end at the assistant text.
        let tool_turn = json!({
            "type": "assistant",
            "sessionId": session,
            "cwd": project.path().to_string_lossy(),
            "version": "2.1.281",
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}
            ]},
        });
        let source = session_file(
            home.path(),
            session,
            &[
                user_line(session, project.path()),
                assistant_text(session, project.path(), "step one"),
                tool_turn,
            ],
        );
        let retained = retain(project.path(), home.path(), &source).expect("retain");
        assert_eq!(retained.archive.record_count, 2);
        assert!(
            retained
                .suffix_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("unsettled tail")),
            "{:?}",
            retained.suffix_reason
        );
        let archived = std::fs::read(
            archive_dir(project.path())
                .join("sources")
                .join(&retained.archive.source_object),
        )
        .expect("source");
        assert_eq!(archived.len(), retained.archive.byte_length);
        assert!(!String::from_utf8_lossy(&archived).contains("tool_use"));
    }

    #[test]
    fn a_subagent_message_never_closes_a_turn() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        let session = "11111111-2222-3333-4444-555555555670";
        let mut sidechain_user = user_line(session, project.path());
        sidechain_user["isSidechain"] = serde_json::json!(true);
        let mut sidechain_assistant = assistant_text(session, project.path(), "subagent done");
        sidechain_assistant["isSidechain"] = serde_json::json!(true);
        let source = session_file(home.path(), session, &[sidechain_user, sidechain_assistant]);
        let retained = retain(project.path(), home.path(), &source).expect("retain");
        // The whole file is captured, but explicitly not as a settled
        // boundary: a prefix that ends inside subagent traffic has no main
        // thread for the native harness to resume.
        assert_eq!(retained.archive.record_count, 2);
        assert!(
            retained
                .suffix_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("no completed assistant turn")),
            "{:?}",
            retained.suffix_reason
        );
    }

    #[test]
    fn retain_refuses_another_workspace_and_corruption() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        let session = "11111111-2222-3333-4444-555555555557";
        let foreign = session_file(
            home.path(),
            session,
            &[user_line(session, elsewhere.path())],
        );
        let err = retain(project.path(), home.path(), &foreign).expect_err("workspace");
        assert!(err.contains("workspace differs"), "{err}");

        let dir = paths::claude_projects_root(home.path()).join("test-slug");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let corrupt = dir.join("11111111-2222-3333-4444-555555555558.jsonl");
        std::fs::write(&corrupt, "{not json\n").expect("write");
        let err = retain(project.path(), home.path(), &corrupt).expect_err("corrupt");
        assert!(err.contains("corrupt native record"), "{err}");
    }

    #[test]
    fn load_detects_a_tampered_source_generation() {
        let home = tempfile::tempdir().expect("home");
        let project = project();
        let session = "11111111-2222-3333-4444-555555555559";
        let source = session_file(
            home.path(),
            session,
            &[
                user_line(session, project.path()),
                assistant_text(session, project.path(), "done"),
            ],
        );
        let retained = retain(project.path(), home.path(), &source).expect("retain");
        let object = archive_dir(project.path())
            .join("sources")
            .join(&retained.archive.source_object);
        std::fs::write(&object, "{}\n").expect("tamper");
        let err = load(project.path(), &retained.archive.id).expect_err("tampered");
        assert!(
            err.contains("corrupt") || err.contains("unavailable"),
            "{err}"
        );
    }

    fn retained_session(
        source_home: &Path,
        project: &Path,
        session: &str,
    ) -> (PathBuf, RetainedBoundary) {
        let source = session_file(
            source_home,
            session,
            &[
                user_line(session, project),
                assistant_text(session, project, "done"),
            ],
        );
        let retained = retain(project, source_home, &source).expect("retain");
        (source, retained)
    }

    #[test]
    fn restore_round_trips_into_a_fresh_home() {
        let source_home = tempfile::tempdir().expect("source home");
        let target_home = tempfile::tempdir().expect("target home");
        let project = project();
        let session = "11111111-2222-3333-4444-555555555660";
        let (_, retained) = retained_session(source_home.path(), project.path(), session);
        let operation = "01900000-0000-7000-8000-000000000660";

        let receipt = restore(
            project.path(),
            target_home.path(),
            &retained.archive.id,
            operation,
        )
        .expect("restore");
        assert_ne!(receipt.destination_session_id, session);
        assert!(receipt.destination_path.starts_with(target_home.path()));
        assert_eq!(receipt.record_count, 2);
        // Every record carries the new session id; the old one is gone.
        let text = std::fs::read_to_string(&receipt.destination_path).expect("read");
        assert!(text.contains(&receipt.destination_session_id));
        assert!(!text.contains(session));

        // The source home is untouched: still exactly one session file.
        assert_eq!(
            crate::transcripts::claude::session_files(source_home.path()).len(),
            1
        );

        // The restored session re-imports as a first-class Claude session.
        let report = crate::sessions::import_from_readers_filtered(
            target_home.path(),
            project.path(),
            Some("claude"),
            crate::tombstones::TombstonePolicy::FailClosed,
        )
        .expect("import");
        assert!(report.written >= 1, "{report:?}");
        assert!(crate::sessions::list(project.path())
            .iter()
            .any(|stored| stored.session_id == receipt.destination_session_id));
    }

    #[test]
    fn restore_is_idempotent_and_never_reuses_an_operation() {
        let source_home = tempfile::tempdir().expect("source home");
        let target_home = tempfile::tempdir().expect("target home");
        let project = project();
        let session = "11111111-2222-3333-4444-555555555661";
        let (_, retained) = retained_session(source_home.path(), project.path(), session);
        let operation = "01900000-0000-7000-8000-000000000661";

        let first = restore(
            project.path(),
            target_home.path(),
            &retained.archive.id,
            operation,
        )
        .expect("restore");
        let again = restore(
            project.path(),
            target_home.path(),
            &retained.archive.id,
            operation,
        )
        .expect("repeat");
        assert_eq!(again.destination_path, first.destination_path);
        assert_eq!(
            crate::transcripts::claude::session_files(target_home.path()).len(),
            1,
            "a repeated operation writes no second file"
        );

        // A different operation makes a distinct native session.
        let second = restore(
            project.path(),
            target_home.path(),
            &retained.archive.id,
            "01900000-0000-7000-8000-000000000662",
        )
        .expect("second operation");
        assert_ne!(second.destination_session_id, first.destination_session_id);
        assert_eq!(
            crate::transcripts::claude::session_files(target_home.path()).len(),
            2
        );

        // Reusing an operation id for another archive is refused.
        let other_archive = "a".repeat(64);
        let err = restore(
            project.path(),
            target_home.path(),
            &other_archive,
            operation,
        )
        .expect_err("operation reuse");
        assert!(err.contains("different archive"), "{err}");
        assert_eq!(
            receipt(project.path(), operation)
                .expect("receipt")
                .archive_id,
            retained.archive.id
        );
    }

    #[test]
    fn restore_refuses_an_unknown_archive() {
        let target_home = tempfile::tempdir().expect("target home");
        let project = project();
        let err = restore(
            project.path(),
            target_home.path(),
            &"b".repeat(64),
            "01900000-0000-7000-8000-000000000663",
        )
        .expect_err("unknown archive");
        assert!(
            err.contains("unavailable") || err.contains("corrupt"),
            "{err}"
        );
        assert!(crate::transcripts::claude::session_files(target_home.path()).is_empty());
    }
}
