//! Qualified local Codex recovery, separate from canonical cross-harness transfer.
//!
//! Protocol qualified with codex-cli 0.149.1's generated experimental v2 schema
//! and a real isolated app-server: fork.path reads an immutable rollout; the
//! fork prepends session_meta and rewrites transport timestamps. Every source
//! record's other fields must survive. Never synthesize a native rollout.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{harness_install::paths, safe_io};

pub const QUALIFIED_VERSION: &str = "0.149.1";
const ARCHIVE_SCHEMA: &str = "stateroot.codex-archive.v1";
const RESTORE_SCHEMA: &str = "stateroot.codex-restore.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeArchive {
    pub schema: String,
    pub id: String,
    pub source_thread_id: String,
    pub source_session_id: String,
    pub source_path: PathBuf,
    pub source_object: String,
    pub byte_length: usize,
    pub project_id: Option<String>,
    pub capture_root: Option<String>,
    pub capture_lineage: Option<String>,
    pub cwd: PathBuf,
    pub codex_home: PathBuf,
    pub cli_version: String,
    pub platform: String,
    pub config_files: BTreeMap<String, String>,
    pub raw_sha256: String,
    pub prefix_digest: String,
    pub record_count: usize,
    pub last_turn_id: String,
    pub turn_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_status: Option<String>,
    #[serde(skip)]
    pub turn_ids: Vec<String>,
    #[serde(skip)]
    pub turn_statuses: Vec<String>,
    #[serde(skip)]
    pub native_recovery_issue: Option<String>,
    pub last_context: Value,
    pub model_provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreReceipt {
    pub schema: String,
    pub operation_id: String,
    pub archive_id: String,
    pub source_thread_id: String,
    pub stage: String,
    pub destination_thread_id: Option<String>,
    pub destination_session_id: Option<String>,
    pub destination_path: Option<PathBuf>,
    pub prefix_digest: Option<String>,
    pub native_turns_digest: Option<String>,
    pub effective_config_digest: Option<String>,
    pub effective_envelope: Option<Value>,
    pub observed_continuation: Option<Value>,
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

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("native evidence missing {key}"))
}

fn uuid(value: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| "native evidence requires complete UUID identities".into())
}

fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

pub fn archive_dir(project: &Path) -> PathBuf {
    super::store_dir(project).join("native-codex")
}

fn archive_path(project: &Path, id: &str) -> Result<PathBuf, String> {
    if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(
            "expected full native archive digest (use session list or session show)".into(),
        );
    }
    Ok(archive_dir(project).join(id))
}

fn operation_path(project: &Path, operation: &str) -> Result<PathBuf, String> {
    uuid(operation)?;
    Ok(archive_dir(project)
        .join("operations")
        .join(format!("{operation}.json")))
}

/// Comparison-stable form for a path that exists on this host: canonicalize
/// (resolves Windows 8.3 aliases like RUNNER~1 → runneradmin), then fold the
/// UNC prefix/separators via the shared host-path normalization. Paths that
/// do not exist keep their normalized lexical form.
fn comparable_dir(path: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    PathBuf::from(crate::path_identity::normalize_host_path(
        &canonical.to_string_lossy(),
    ))
}

/// Fingerprints only: configuration/credentials are never copied or restored.
/// This qualifies local file configuration at capture, not unknown historical
/// launch arguments, remote policy, environment, or a reconstructed host.
fn config_files(codex_home: &Path, cwd: &Path) -> Result<BTreeMap<String, String>, String> {
    // Normalize both inputs first: the fingerprint map must not change when
    // the same home/project arrives in a different host form (Windows short
    // vs long vs UNC), or fingerprints stop being home-portable.
    let codex_home = comparable_dir(codex_home);
    let cwd = comparable_dir(cwd);
    let mut names = BTreeSet::from([
        codex_home.join("config.toml"),
        codex_home.join("managed_config.toml"),
        PathBuf::from("/etc/codex/config.toml"),
        PathBuf::from("/etc/codex/managed_config.toml"),
    ]);
    for ancestor in cwd.ancestors() {
        names.insert(ancestor.join(".codex/config.toml"));
    }
    names
        .into_iter()
        .map(|p| {
            let hash = match std::fs::read(&p) {
                Ok(bytes) => digest(&bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => "absent".into(),
                Err(e) => return Err(format!("config fingerprint {}: {e}", p.display())),
            };
            let label = p
                .strip_prefix(&codex_home)
                .map(|r| format!("native/{}", r.display()))
                .unwrap_or_else(|_| p.to_string_lossy().into_owned());
            Ok((label, hash))
        })
        .collect()
}

/// All fields are retained except the outer transport timestamp, which the
/// qualified native fork rewrites. Payload timestamps and metadata remain.
fn normalized_records(bytes: &[u8]) -> Result<Vec<Value>, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let mut value: Value =
                serde_json::from_str(line).map_err(|e| format!("corrupt native record: {e}"))?;
            let object = value
                .as_object_mut()
                .ok_or("native record is not an object")?;
            object.remove("timestamp");
            Ok(value)
        })
        .collect()
}

fn native_recovery_issue(records: &[Value]) -> Option<String> {
    records.iter().any(|r|r["type"]=="event_msg" && r["payload"]["type"]=="error").then(||
        "Codex 0.149.1 fork persistence drops standalone error events; raw archive retained but exact native recovery is unavailable".into())
}

struct SettledPrefix {
    end: usize,
    turn_ids: Vec<String>,
    turn_statuses: Vec<String>,
    context: Value,
    suffix_reason: Option<String>,
}

pub(crate) struct RetainedBoundary {
    pub archive: NativeArchive,
    /// A retained historical boundary is not complete capture of a newer tail.
    pub suffix_reason: Option<String>,
}

/// Match the native terminal contract: task_complete.error is failed;
/// turn_aborted is interrupted; a later successful completion preserves an
/// already-failed/interrupted status. All payloads remain in the raw prefix.
/// Unknown/torn/in-progress suffixes stay explicit instead of masquerading as
/// successful fresh capture of an older boundary.
fn settled_prefix(bytes: &[u8]) -> Result<SettledPrefix, String> {
    let mut offset = 0;
    let mut boundary = None;
    let mut active: Option<usize> = None;
    let mut ids = Vec::new();
    let mut statuses = Vec::new();
    let mut contexts: Vec<Option<Value>> = Vec::new();
    let mut issue = None;
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        let value: Value = match serde_json::from_slice(line) {
            Ok(v) => v,
            Err(error) => {
                issue = Some(format!("corrupt native record at byte {offset}: {error}"));
                break;
            }
        };
        offset += line.len();
        let payload = &value["payload"];
        match (value["type"].as_str(), payload["type"].as_str()) {
            (Some("event_msg"), Some("task_started")) => {
                let id = text(payload, "turn_id")?;
                uuid(id)?;
                if active.is_some() || ids.iter().any(|v| v == id) {
                    issue = Some(format!("overlapping or duplicate native turn {id}"));
                    break;
                }
                ids.push(id.to_string());
                statuses.push("inProgress".to_string());
                contexts.push(None);
                active = Some(ids.len() - 1);
            }
            (Some("turn_context"), _) => {
                let id = text(payload, "turn_id")?;
                if let Some(index) = ids.iter().position(|known| known == id) {
                    contexts[index] = Some(payload.clone());
                } else {
                    issue = Some(format!("native context has no matching started turn {id}"));
                    break;
                }
            }
            (Some("event_msg"), Some("error")) => {
                // Native history treats these two control errors as nonfatal;
                // other ErrorEvents mark the active turn failed, but do not
                // themselves establish a settled capture boundary.
                let info = &payload["codex_error_info"];
                if info != "thread_rollback_failed"
                    && info.get("active_turn_not_steerable").is_none()
                {
                    if let Some(index) = active {
                        statuses[index] = "failed".into();
                    }
                }
            }
            (Some("event_msg"), Some(terminal @ ("task_complete" | "turn_aborted"))) => {
                let event_id = if terminal == "task_complete" {
                    Some(text(payload, "turn_id")?)
                } else {
                    payload["turn_id"].as_str()
                };
                // Native abort events may have a null id. The native history
                // builder prefers an exact known id and then the active turn.
                let index = event_id
                    .and_then(|id| ids.iter().position(|known| known == id))
                    .or(active);
                let Some(index) = index else {
                    issue = Some(format!("native {terminal} has no matching started turn"));
                    break;
                };
                if terminal == "turn_aborted" {
                    statuses[index] = "interrupted".into();
                } else if !payload["error"].is_null() {
                    statuses[index] = "failed".into();
                } else if matches!(statuses[index].as_str(), "completed" | "inProgress") {
                    statuses[index] = "completed".into();
                }
                if active == Some(index) {
                    active = None;
                }
                if statuses.iter().all(|s| s != "inProgress") {
                    if let Some(Some(context)) = contexts.last() {
                        boundary = Some(SettledPrefix {
                            end: offset,
                            turn_ids: ids.clone(),
                            turn_statuses: statuses.clone(),
                            context: context.clone(),
                            suffix_reason: None,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    let detail = issue.unwrap_or_else(|| match active {
        Some(index) if statuses[index] == "inProgress" => {
            format!("in-progress native turn {}", ids[index])
        }
        Some(index) => format!(
            "native turn {} has no terminal marker (status {})",
            ids[index], statuses[index]
        ),
        None if contexts.last().is_some_and(Option::is_none) => format!(
            "terminal native turn {} lacks configuration context",
            ids.last().map(String::as_str).unwrap_or("unknown")
        ),
        None => "native records after the last qualified terminal boundary".into(),
    });
    let mut boundary = boundary.ok_or_else(|| {
        format!("native recovery unavailable: no qualified settled turn; {detail}")
    })?;
    if boundary.end != bytes.len() {
        boundary.suffix_reason=Some(format!("retained historical boundary through {}; {} source bytes remain outside capture: {detail}",boundary.turn_ids.last().map(String::as_str).unwrap_or("unknown"),bytes.len()-boundary.end));
    }
    Ok(boundary)
}

/// Called by the existing session sync while holding its store lock. Raw
/// archives stay in the established local/sessions boundary and are immutable.
pub(crate) fn retain(
    project: &Path,
    home: &Path,
    source: &Path,
) -> Result<RetainedBoundary, String> {
    retain_observed(project, &paths::codex_root(home), source)
}

#[cfg(test)]
fn retain_in(project: &Path, native_home: &Path, source: &Path) -> Result<NativeArchive, String> {
    retain_observed(project, native_home, source).map(|result| result.archive)
}

fn retain_observed(
    project: &Path,
    native_home: &Path,
    source: &Path,
) -> Result<RetainedBoundary, String> {
    let bytes = std::fs::read(source).map_err(|e| e.to_string())?;
    let boundary = settled_prefix(&bytes)?;
    let raw = &bytes[..boundary.end];
    let turn_ids = boundary.turn_ids;
    let turn_statuses = boundary.turn_statuses;
    let last_context = boundary.context;
    let records = normalized_records(raw)?;
    let meta = &records.first().ok_or("empty native source")?["payload"];
    if records[0]["type"] != "session_meta"
        || meta["cli_version"] != QUALIFIED_VERSION
        || meta["history_mode"] != "legacy"
    {
        return Err(format!("native recovery unavailable: qualified envelope is Codex {QUALIFIED_VERSION} legacy rollout"));
    }
    let thread = text(meta, "id")?;
    uuid(thread)?;
    let session = text(meta, "session_id")?;
    uuid(session)?;
    let cwd = PathBuf::from(text(meta, "cwd")?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if cwd != project.canonicalize().map_err(|e| e.to_string())?
        || last_context["cwd"] != cwd.to_string_lossy().as_ref()
    {
        return Err("native recovery unavailable: source workspace differs".into());
    }
    let codex_home = native_home.canonicalize().map_err(|e| e.to_string())?;
    let mut archive = NativeArchive {
        schema: ARCHIVE_SCHEMA.into(),
        id: String::new(),
        source_thread_id: thread.into(),
        source_session_id: session.into(),
        source_path: source.to_path_buf(),
        cwd: cwd.clone(),
        source_object: String::new(),
        byte_length: raw.len(),
        project_id: crate::local_store::read_manifest(project)
            .ok()
            .flatten()
            .and_then(|v| v["project_id"].as_str().map(str::to_owned)),
        capture_root: git2::Repository::discover(project).ok().and_then(|repo| {
            crate::roots::lineage_refname_checked(project)
                .ok()
                .and_then(|name| {
                    repo.find_reference(&name)
                        .ok()
                        .and_then(|r| r.target())
                        .map(|id| id.to_string())
                })
        }),
        capture_lineage: crate::roots::lineage_refname_checked(project).ok(),
        codex_home: codex_home.clone(),
        cli_version: QUALIFIED_VERSION.into(),
        platform: platform(),
        config_files: config_files(&codex_home, &cwd)?,
        raw_sha256: raw_sha256(raw),
        prefix_digest: json_digest(&records)?,
        record_count: records.len(),
        last_turn_id: turn_ids.last().ok_or("no settled turn")?.clone(),
        turn_count: turn_ids.len(),
        terminal_status: turn_statuses.last().cloned(),
        turn_ids,
        turn_statuses,
        native_recovery_issue: native_recovery_issue(&records),
        last_context,
        model_provider: text(meta, "model_provider")?.into(),
    };
    archive.source_object = retain_source(project, thread, raw)?;
    archive.id = json_digest(&archive)?;
    let target = archive_path(project, &archive.id)?;
    if target.exists() {
        return Ok(RetainedBoundary {
            archive: load(project, &archive.id)?,
            suffix_reason: boundary.suffix_reason,
        });
    }
    // Recheck the captured prefix after metadata/config reads. Appends are fine;
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

/// One append-only native source per observed generation. Rewritten histories
/// create another generation; old bytes and boundary references never move.
/// Full turns are derived from the selected prefix instead of repeated in every
/// manifest, keeping retained payload storage linear for ordinary appends.
fn retain_source(project: &Path, thread: &str, raw: &[u8]) -> Result<String, String> {
    let dir = archive_dir(project).join("sources");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let prefix = format!("{thread}-");
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
    let name = format!("{thread}-{}.jsonl", raw_sha256(raw));
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

fn archive_bytes(project: &Path, archive: &NativeArchive) -> Result<Vec<u8>, String> {
    use std::io::Read;
    if Path::new(&archive.source_object).components().count() != 1
        || !archive
            .source_object
            .starts_with(&format!("{}-", archive.source_thread_id))
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
        return Err("native archive corrupt: exact historical bytes unavailable; live history is not a substitute".into());
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

pub fn load(project: &Path, id: &str) -> Result<NativeArchive, String> {
    let mut archive = manifest(project, id)?;
    let bytes = archive_bytes(project, &archive)?;
    let records = normalized_records(&bytes)?;
    if json_digest(&records)? != archive.prefix_digest || records.len() != archive.record_count {
        return Err(
            "native archive corrupt: digest mismatch; newer live history is not a substitute"
                .into(),
        );
    }
    let boundary = settled_prefix(&bytes)?;
    if boundary.end != bytes.len()
        || boundary.turn_ids.len() != archive.turn_count
        || boundary.turn_ids.last() != Some(&archive.last_turn_id)
        || boundary.context != archive.last_context
        || archive
            .terminal_status
            .as_ref()
            .is_some_and(|status| Some(status) != boundary.turn_statuses.last())
    {
        return Err("native archive boundary identity mismatch".into());
    }
    archive.turn_ids = boundary.turn_ids;
    archive.turn_statuses = boundary.turn_statuses;
    archive.native_recovery_issue = native_recovery_issue(&records);
    Ok(archive)
}

pub fn inventory(project: &Path) -> Vec<Value> {
    let mut rows = Vec::new();
    if let Ok(entries) = std::fs::read_dir(archive_dir(project)) {
        for entry in entries.flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            if id.len() != 64 {
                continue;
            }
            rows.push(match manifest(project, &id) {
                Ok(a) => json!({"archive_id":id,"source_thread_id":a.source_thread_id,"last_turn_id":a.last_turn_id,"terminal_status":a.terminal_status,"capture_root":a.capture_root,"verification":"manifest verified; payload checked on exact selection","storage":"local only"}),
                Err(e) => json!({"archive_id":id,"native_archive_verified":false,"unavailable":e}),
            });
        }
    }
    rows.sort_by_key(|v| v["archive_id"].as_str().unwrap_or_default().to_owned());
    rows
}

/// One bounded stdio client for the installed native harness, not an agent.
struct Rpc {
    child: Child,
    input: ChildStdin,
    messages: Receiver<Result<Value, String>>,
    next: u64,
}

impl Drop for Rpc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Rpc {
    fn start(
        executable: &Path,
        archive: &NativeArchive,
        native_home: &Path,
    ) -> Result<Self, String> {
        let output = Command::new(executable)
            .arg("--version")
            .output()
            .map_err(|e| format!("native Codex unavailable: {e}"))?;
        if !output.status.success()
            || String::from_utf8_lossy(&output.stdout).trim()
                != format!("codex-cli {}", archive.cli_version)
            || archive.cli_version != QUALIFIED_VERSION
        {
            return Err(format!(
                "unsupported native runner; qualified version is {QUALIFIED_VERSION}"
            ));
        }
        let mut child = Command::new(executable)
            .args(["app-server", "--listen", "stdio://"])
            .env("CODEX_HOME", native_home)
            .current_dir(&archive.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let input = child.stdin.take().ok_or("native stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("native stdout unavailable")?;
        let (tx, messages) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let value = line
                    .map_err(|e| e.to_string())
                    .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()));
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        let mut rpc = Self {
            child,
            input,
            messages,
            next: 0,
        };
        let init = rpc.call("initialize", json!({"clientInfo":{"name":"stateroot","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
        if Path::new(text(&init, "codexHome")?)
            .canonicalize()
            .map_err(|e| e.to_string())?
            != native_home
        {
            return Err("native runtime home differs from captured envelope".into());
        }
        rpc.send(json!({"method":"initialized"}))?;
        Ok(rpc)
    }

    fn send(&mut self, value: Value) -> Result<(), String> {
        writeln!(self.input, "{value}")
            .and_then(|_| self.input.flush())
            .map_err(|e| e.to_string())
    }

    fn receive(&mut self, deadline: Instant) -> Result<Value, String> {
        let value = self
            .messages
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| {
                "native response unavailable/timed out; operation outcome may be uncertain"
                    .to_string()
            })??;
        if value.get("id").is_some() && value.get("method").is_some() {
            // This client cannot adjudicate native approvals or implement tools.
            self.send(json!({"id":value["id"],"error":{"code":-32601,"message":"use the native Codex UI for this request"}}))?;
        }
        Ok(value)
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        self.send(json!({"id":id,"method":method,"params":params}))?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let value = self.receive(deadline)?;
            if value["id"] == id {
                return if value.get("error").is_some() {
                    Err(format!("native {method}: {}", value["error"]))
                } else {
                    Ok(value["result"].clone())
                };
            }
        }
    }
}

fn validate_envelope(archive: &NativeArchive, project: &Path, home: &Path) -> Result<(), String> {
    if let Some(issue) = &archive.native_recovery_issue {
        return Err(issue.clone());
    }
    if platform() != "linux-x86_64"
        || archive.platform != platform()
        || archive.cwd != project.canonicalize().map_err(|e| e.to_string())?
    {
        return Err("native recovery unavailable: qualified platform is linux-x86_64 with the original workspace path".into());
    }
    if archive.config_files != config_files(&paths::codex_root(home), &archive.cwd)? {
        return Err(
            "native recovery unavailable: native configuration files changed since capture".into(),
        );
    }
    Ok(())
}

fn verify_effective(archive: &NativeArchive, response: &Value) -> Result<(), String> {
    let context = &archive.last_context;
    let expected_sandbox = match context["sandbox_policy"]["type"].as_str() {
        Some("read-only") => "readOnly",
        Some("workspace-write") => "workspaceWrite",
        Some("danger-full-access") => "dangerFullAccess",
        _ => return Err("unsupported native permission envelope".into()),
    };
    if response["model"] != context["model"]
        || response["modelProvider"] != archive.model_provider
        || response["approvalPolicy"] != context["approval_policy"]
        || response["approvalsReviewer"] != context["approvals_reviewer"]
        || response["sandbox"]["type"] != expected_sandbox
        || response["reasoningEffort"] != context["effort"]
        || response["cwd"] != archive.cwd.to_string_lossy().as_ref()
        || response["runtimeWorkspaceRoots"] != context["workspace_roots"]
    {
        return Err("native fork created but effective model/configuration/permissions differ; continuation not qualified".into());
    }
    // Policy shape has different native wire casing; compare all substantive
    // permission fields too, including writable roots and network permission.
    for (raw, wire) in [
        ("network_access", "networkAccess"),
        ("writable_roots", "writableRoots"),
        ("exclude_tmpdir_env_var", "excludeTmpdirEnvVar"),
        ("exclude_slash_tmp", "excludeSlashTmp"),
    ] {
        if let Some(expected) = context["sandbox_policy"].get(raw) {
            if response["sandbox"].get(wire) != Some(expected) {
                return Err(format!("native permission field changed: {raw}"));
            }
        }
    }
    Ok(())
}

fn verify_prefix(
    archive: &NativeArchive,
    thread: &Value,
    native_home: &Path,
) -> Result<(PathBuf, String), String> {
    let id = text(thread, "id")?;
    uuid(id)?;
    if id == archive.source_thread_id
        || thread["sessionId"] != id
        || thread["forkedFromId"] != archive.source_thread_id
        || thread["cwd"] != archive.cwd.to_string_lossy().as_ref()
        || thread["cliVersion"] != archive.cli_version
    {
        return Err("native destination identity/ancestry/workspace/version mismatch".into());
    }
    let path = comparable_dir(&PathBuf::from(text(thread, "path")?));
    let home = comparable_dir(native_home);
    let source = comparable_dir(&archive.source_path);
    if !path.starts_with(&home) || path == source {
        return Err("native destination is outside qualified home or aliases source".into());
    }
    let records = normalized_records(&std::fs::read(&path).map_err(|e| e.to_string())?)?;
    let prefix = records
        .get(1..=archive.record_count)
        .ok_or("native destination historical prefix incomplete")?;
    if records[0]["payload"]["id"] != id
        || records[0]["payload"]["session_id"] != id
        || records[0]["payload"]["forked_from_id"] != archive.source_thread_id
        || json_digest(&prefix)? != archive.prefix_digest
    {
        return Err("native destination historical prefix digest mismatch".into());
    }
    let turns = thread["turns"]
        .as_array()
        .ok_or("native turns unavailable")?;
    let historical = turns
        .get(..archive.turn_ids.len())
        .ok_or("native historical turns missing")?;
    for ((turn, id), status) in historical
        .iter()
        .zip(&archive.turn_ids)
        .zip(&archive.turn_statuses)
    {
        if turn["id"] != *id || turn["status"] != *status || turn["itemsView"] != "full" {
            return Err("native historical full turn identities/status/items differ".into());
        }
    }
    Ok((path, json_digest(&historical)?))
}

fn save_receipt(project: &Path, receipt: &RestoreReceipt) -> Result<(), String> {
    safe_io::atomic_replace_json(
        &operation_path(project, &receipt.operation_id)?,
        &serde_json::to_value(receipt).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

pub fn receipt(project: &Path, operation: &str) -> Result<RestoreReceipt, String> {
    receipt_snapshot(project, operation).map(|(receipt, _)| receipt)
}

fn receipt_snapshot(project: &Path, operation: &str) -> Result<(RestoreReceipt, Vec<u8>), String> {
    let bytes = std::fs::read(operation_path(project, operation)?).map_err(|e| e.to_string())?;
    let receipt: RestoreReceipt = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if receipt.schema != RESTORE_SCHEMA || receipt.operation_id != operation {
        return Err("unsupported native operation receipt".into());
    }
    if !matches!(
        receipt.stage.as_str(),
        "prepared"
            | "fork_requested"
            | "fork_created"
            | "prefix_verified"
            | "continuation_observed"
    ) {
        return Err("unsupported native operation stage; original receipt retained".into());
    }
    Ok((receipt, bytes))
}

/// Read the first native metadata record, not thread/list's derived database:
/// 0.149.1 can report the inherited source's threadSource after reloading.
fn reconcile(
    archive: &NativeArchive,
    operation: &str,
    native_home: &Path,
) -> Result<Option<String>, String> {
    let marker = format!("stateroot_restore_{operation}");
    let mut ids = BTreeSet::new();
    for root in [
        native_home.join("sessions"),
        native_home.join("archived_sessions"),
    ] {
        for path in
            crate::transcripts::walk_files(&root, &|p| p.extension().is_some_and(|e| e == "jsonl"))
        {
            let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
            let line = BufReader::new(file)
                .lines()
                .next()
                .transpose()
                .map_err(|e| e.to_string())?
                .unwrap_or_default();
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let meta = &value["payload"];
            if meta["thread_source"] == marker && meta["forked_from_id"] == archive.source_thread_id
            {
                ids.insert(text(meta, "id")?.to_string());
            }
        }
    }
    if ids.len() > 1 {
        return Err(
            "ambiguous interrupted native restore: multiple destinations; none resumed".into(),
        );
    }
    Ok(ids.into_iter().next())
}

/// Restore creates one distinct native fork, verifies it, and stops before any
/// inference. The caller supplies a stable UUID so retries reconcile the same
/// operation. Pending-but-unlocated outcomes never launch a duplicate fork.
pub fn restore(
    project: &Path,
    home: &Path,
    archive_id: &str,
    operation: &str,
    executable: &Path,
) -> Result<RestoreReceipt, String> {
    let archive = load(project, archive_id)?;
    validate_envelope(&archive, project, home)?;
    let native_home = paths::codex_root(home)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let op_path = operation_path(project, operation)?;
    let _lock = safe_io::ResourceLock::acquire(op_path.with_extension("lock"))
        .map_err(|e| e.to_string())?;
    let mut state = if op_path.exists() {
        receipt(project, operation)?
    } else {
        RestoreReceipt {
            schema: RESTORE_SCHEMA.into(),
            operation_id: operation.into(),
            archive_id: archive_id.into(),
            source_thread_id: archive.source_thread_id.clone(),
            stage: "prepared".into(),
            destination_thread_id: None,
            destination_session_id: None,
            destination_path: None,
            prefix_digest: None,
            native_turns_digest: None,
            effective_config_digest: None,
            effective_envelope: None,
            observed_continuation: None,
        }
    };
    if state.archive_id != archive_id || state.source_thread_id != archive.source_thread_id {
        return Err("operation id already belongs to a different native source".into());
    }
    let mut rpc = Rpc::start(executable, &archive, &native_home)?;
    let config = rpc.call(
        "config/read",
        json!({"cwd":archive.cwd,"includeLayers":true}),
    )?;
    let config_digest = json_digest(&config)?;
    if state
        .effective_config_digest
        .as_ref()
        .is_some_and(|v| v != &config_digest)
    {
        return Err("native effective configuration changed during operation".into());
    }
    state.effective_config_digest = Some(config_digest);
    let response = if state.stage == "prepared" {
        let materialized = archive_dir(project)
            .join("operations")
            .join(format!("{operation}-source.jsonl"));
        safe_io::atomic_replace(&materialized, &archive_bytes(project, &archive)?)
            .map_err(|e| e.to_string())?;
        state.stage = "fork_requested".into();
        save_receipt(project, &state)?;
        let result = rpc.call("thread/fork",json!({"threadId":archive.source_thread_id,"path":materialized,"lastTurnId":archive.turn_ids.last(),"deferGoalContinuation":true,"threadSource":format!("stateroot_restore_{operation}")}))?;
        state.destination_thread_id = Some(text(&result["thread"], "id")?.into());
        state.stage = "fork_created".into();
        let mut envelope = result.clone();
        envelope
            .as_object_mut()
            .ok_or("invalid native envelope")?
            .remove("thread");
        state.effective_envelope = Some(envelope);
        save_receipt(project, &state)?;
        verify_effective(&archive, &result)?;
        result
    } else {
        let id = match state.destination_thread_id.clone().or(reconcile(&archive,operation,&native_home)?) {
            Some(id) => id,
            None => return Err("native restore outcome uncertain: no matching destination located; retry reconciles only, never forks again".into()),
        };
        state.destination_thread_id = Some(id.clone());
        // Read is non-mutating: a retry must not automatically resume a goal.
        rpc.call("thread/read", json!({"threadId":id,"includeTurns":true}))?
    };
    let (path, turns_digest) = verify_prefix(&archive, &response["thread"], &native_home)?;
    if state.prefix_digest.is_none()
        && response["thread"]["turns"]
            .as_array()
            .is_none_or(|v| v.len() != archive.turn_count)
    {
        return Err("native destination advanced before historical prefix verification; continuation is unqualified".into());
    }
    if let Some(envelope) = &state.effective_envelope {
        verify_effective(&archive, envelope)?;
    }
    verify_reconciled_settings(&archive, &path)?;
    if state
        .native_turns_digest
        .as_ref()
        .is_some_and(|v| v != &turns_digest)
    {
        return Err("native historical projection changed".into());
    }
    state.destination_path = Some(path);
    state.destination_session_id = Some(text(&response["thread"], "sessionId")?.into());
    state.prefix_digest = Some(archive.prefix_digest);
    state.native_turns_digest = Some(turns_digest);
    if matches!(state.stage.as_str(), "fork_created" | "fork_requested") {
        state.stage = "prefix_verified".into();
    }
    // Reverify the retained object, never silently trust the live source.
    load(project, archive_id)?;
    save_receipt(project, &state)?;
    let materialized = archive_dir(project)
        .join("operations")
        .join(format!("{operation}-source.jsonl"));
    if materialized.exists() {
        std::fs::remove_file(materialized).map_err(|e| e.to_string())?;
    }
    Ok(state)
}

// The native runner, rather than StateRoot, performs continuation. Inspection
// observes completed native turns and their full output without inferring that
// the output is correct/useful or issuing another model request.
pub fn inspect_operation(
    project: &Path,
    home: &Path,
    operation: &str,
    executable: &Path,
) -> Result<RestoreReceipt, String> {
    let (mut state, snapshot) = receipt_snapshot(project, operation)?;
    let archive = load(project, &state.archive_id)?;
    validate_envelope(&archive, project, home)?;
    if state.prefix_digest.as_deref() != Some(&archive.prefix_digest) {
        return Err("native prefix was not verified before continuation".into());
    }
    let native_home = paths::codex_root(home)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let id = state
        .destination_thread_id
        .clone()
        .ok_or("native destination unavailable")?;
    let mut rpc = Rpc::start(executable, &archive, &native_home)?;
    let current_config = rpc.call(
        "config/read",
        json!({"cwd":archive.cwd,"includeLayers":true}),
    )?;
    if state.effective_config_digest.as_deref() != Some(&json_digest(&current_config)?) {
        return Err("native effective configuration changed since prefix verification".into());
    }
    let response = rpc.call("thread/read", json!({"threadId":id,"includeTurns":true}))?;
    let (destination, historical) = verify_prefix(&archive, &response["thread"], &native_home)?;
    if state.native_turns_digest.as_deref() != Some(&historical) {
        return Err("native historical projection changed".into());
    }
    let turns = response["thread"]["turns"]
        .as_array()
        .ok_or("native turns unavailable")?;
    let completed: Vec<Value> = turns
        .iter()
        .skip(archive.turn_count)
        .filter(|t| t["status"] == "completed" && t["itemsView"] == "full")
        .cloned()
        .collect();
    if !completed.is_empty() {
        verify_continuation_context(&archive, &destination, &completed)?;
        state.stage = "continuation_observed".into();
        state.observed_continuation = Some(
            json!({"turns":completed,"claim":"native completed turns observed; semantic usefulness requires review"}),
        );
    }
    publish_inspection(project, &snapshot, &state)?;
    Ok(state)
}

/// Native reads run without holding the operation lock. Publication compares
/// the entire exact snapshot under the same lock used by restore, so a stale
/// reader cannot erase a newer inspection (including fields it does not know).
fn publish_inspection(
    project: &Path,
    expected: &[u8],
    state: &RestoreReceipt,
) -> Result<(), String> {
    let path = operation_path(project, &state.operation_id)?;
    let _lock =
        safe_io::ResourceLock::acquire(path.with_extension("lock")).map_err(|e| e.to_string())?;
    if std::fs::read(&path).map_err(|e| e.to_string())? != expected {
        return Err(
            "operation changed during native inspection; retry with its current receipt".into(),
        );
    }
    save_receipt(project, state)
}

fn verify_continuation_context(
    archive: &NativeArchive,
    path: &Path,
    turns: &[Value],
) -> Result<(), String> {
    let records = normalized_records(&std::fs::read(path).map_err(|e| e.to_string())?)?;
    for turn in turns {
        let contexts: Vec<&Value> = records
            .iter()
            .skip(archive.record_count + 1)
            .filter(|r| r["type"] == "turn_context" && r["payload"]["turn_id"] == turn["id"])
            .map(|r| &r["payload"])
            .collect();
        if contexts.is_empty() {
            return Err("native continuation lacks full turn configuration evidence".into());
        }
        for context in contexts {
            for key in [
                "model",
                "cwd",
                "workspace_roots",
                "approval_policy",
                "approvals_reviewer",
                "sandbox_policy",
                "permission_profile",
                "effort",
                "collaboration_mode",
            ] {
                if context.get(key) != archive.last_context.get(key) {
                    return Err(format!(
                        "native continuation outside qualified envelope: changed {key}"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn verify_reconciled_settings(archive: &NativeArchive, path: &Path) -> Result<(), String> {
    let records = normalized_records(&std::fs::read(path).map_err(|e| e.to_string())?)?;
    let settings = records
        .iter()
        .skip(archive.record_count + 1)
        .find(|v| v["payload"]["type"] == "thread_settings_applied")
        .map(|v| &v["payload"]["thread_settings"])
        .ok_or("interrupted native fork lacks effective settings evidence")?;
    for (native, context) in [
        ("model", "model"),
        ("approval_policy", "approval_policy"),
        ("approvals_reviewer", "approvals_reviewer"),
        ("permission_profile", "permission_profile"),
        ("cwd", "cwd"),
        ("reasoning_effort", "effort"),
        ("collaboration_mode", "collaboration_mode"),
    ] {
        if settings.get(native).is_none() || settings[native] != archive.last_context[context] {
            return Err(format!("reconciled native settings differ: {native}"));
        }
    }
    if settings["model_provider_id"] != archive.model_provider {
        return Err("reconciled provider differs".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const THREAD: &str = "01900000-0000-7000-8000-000000000001";
    const TURN1: &str = "01900000-0000-7000-8000-000000000002";
    const TURN2: &str = "01900000-0000-7000-8000-000000000003";
    const TURN3: &str = "01900000-0000-7000-8000-000000000005";
    const DEST: &str = "01900000-0000-7000-8000-000000000004";

    fn context(cwd: &Path, turn: &str) -> Value {
        json!({"turn_id":turn,"cwd":cwd,"workspace_roots":[cwd],"model":"qualified-model","approval_policy":"never","approvals_reviewer":"user","sandbox_policy":{"type":"read-only"},"permission_profile":{"type":"managed","network":"restricted"},"effort":"low","collaboration_mode":{"mode":"default"}})
    }

    fn turn(cwd: &Path, id: &str, content: &str) -> Vec<Value> {
        vec![
            json!({"type":"event_msg","payload":{"type":"task_started","turn_id":id}}),
            json!({"type":"turn_context","payload":context(cwd,id)}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":content}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"remembered"}]}}),
            json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":id}}),
        ]
    }

    fn write_records(path: &Path, records: &[Value]) -> Vec<u8> {
        let bytes = records
            .iter()
            .map(|v| format!("{v}\n"))
            .collect::<String>()
            .into_bytes();
        std::fs::write(path, &bytes).unwrap();
        bytes
    }

    fn setup() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        PathBuf,
        Vec<Value>,
        NativeArchive,
    ) {
        let project = tempfile::tempdir().unwrap();
        let native = tempfile::tempdir().unwrap();
        let cwd = project.path().canonicalize().unwrap();
        let path = native.path().join("source.jsonl");
        let mut records = vec![
            json!({"type":"session_meta","payload":{"id":THREAD,"session_id":THREAD,"cwd":cwd,"cli_version":QUALIFIED_VERSION,"history_mode":"legacy","model_provider":"openai"}}),
        ];
        records.extend(turn(&cwd, TURN1, "orchard cedar-482; quantity 17"));
        write_records(&path, &records);
        let a = retain_in(&cwd, native.path(), &path).unwrap();
        (project, native, path, records, a)
    }

    #[test]
    fn append_boundaries_share_bytes_and_rewrites_preserve_prior_generation() {
        let (project, native, path, mut records, first) = setup();
        assert!(
            !project.path().join(".git").exists(),
            "capture must not initialize Git"
        );
        let old = archive_bytes(project.path(), &first).unwrap();
        records.extend(turn(
            &project.path().canonicalize().unwrap(),
            TURN2,
            "second turn",
        ));
        let new = write_records(&path, &records);
        let second = retain_in(project.path(), native.path(), &path).unwrap();
        assert_eq!(first.source_object, second.source_object);
        assert_ne!(first.id, second.id);
        assert_eq!(
            archive_bytes(project.path(), &load(project.path(), &first.id).unwrap()).unwrap(),
            old
        );
        let sources = archive_dir(project.path()).join("sources");
        assert_eq!(std::fs::read_dir(&sources).unwrap().count(), 1);
        assert_eq!(
            std::fs::metadata(sources.join(&second.source_object))
                .unwrap()
                .len(),
            new.len() as u64
        );
        println!("archive growth: first boundary {} bytes; second boundary {} bytes; retained source bytes {} (one generation)",old.len(),new.len(),new.len());
        records[3]["payload"]["content"][0]["text"] = json!("divergent history");
        write_records(&path, &records);
        let divergent = retain_in(project.path(), native.path(), &path).unwrap();
        assert_ne!(first.source_object, divergent.source_object);
        assert!(load(project.path(), &first.id).is_ok());
        assert_eq!(std::fs::read_dir(&sources).unwrap().count(), 2);
    }

    #[test]
    fn removed_live_source_recovers_but_missing_or_corrupt_archive_is_unavailable() {
        let (project, _native, path, _, archive) = setup();
        std::fs::remove_file(path).unwrap();
        assert!(load(project.path(), &archive.id).is_ok());
        let source = archive_dir(project.path())
            .join("sources")
            .join(&archive.source_object);
        let bytes = std::fs::read(&source).unwrap();
        std::fs::write(&source, b"corrupt").unwrap();
        assert!(load(project.path(), &archive.id)
            .unwrap_err()
            .contains("corrupt"));
        std::fs::write(&source, &bytes).unwrap();
        std::fs::remove_file(&source).unwrap();
        assert!(load(project.path(), &archive.id)
            .unwrap_err()
            .contains("unavailable"));
        let report = crate::fidelity::native_session_report(project.path(), &archive.id, None);
        assert_eq!(report["native_archive_verified"], false);
        assert_eq!(report["native_fork_created"], false);
    }

    #[test]
    fn interrupted_suffix_keeps_previous_settled_boundary_and_unknown_format_refuses() {
        let (project, native, path, mut records, first) = setup();
        records.push(json!({"type":"event_msg","payload":{"type":"task_started","turn_id":TURN2}}));
        write_records(&path, &records);
        let next = retain_in(project.path(), native.path(), &path).unwrap();
        assert_eq!(first.id, next.id);
        let partial = retain_observed(project.path(), native.path(), &path).unwrap();
        assert!(partial
            .suffix_reason
            .unwrap()
            .contains("in-progress native turn"));
        records[0]["payload"]["cli_version"] = json!("0.150.0");
        write_records(&path, &records);
        assert!(retain_in(project.path(), native.path(), &path)
            .unwrap_err()
            .contains("qualified envelope"));
        assert!(settled_prefix(b"{broken}\n").is_err());
        records[0]["payload"]["cli_version"] = json!(QUALIFIED_VERSION);
        records[1]["payload"]["turn_id"] = json!("short-id");
        write_records(&path, &records);
        assert!(retain_in(project.path(), native.path(), &path)
            .unwrap_err()
            .contains("complete UUID"));
    }

    #[test]
    fn failed_and_aborted_turns_preserve_status_and_allow_later_settled_capture() {
        for terminal in ["failed", "interrupted"] {
            let (project, native, path, mut records, _first) = setup();
            let mut middle = turn(
                &project.path().canonicalize().unwrap(),
                TURN2,
                "middle turn",
            );
            if terminal == "failed" {
                middle.last_mut().unwrap()["payload"]["error"] =
                    json!({"message":"native failure","codex_error_info":"other"});
            } else {
                middle.last_mut().unwrap()["payload"] =
                    json!({"type":"turn_aborted","turn_id":TURN2,"reason":"interrupted"});
            }
            records.extend(middle);
            write_records(&path, &records);
            let middle = retain_observed(project.path(), native.path(), &path).unwrap();
            assert!(middle.suffix_reason.is_none());
            assert_eq!(middle.archive.terminal_status.as_deref(), Some(terminal));
            // A trailing successful task_complete cannot relabel a failed or
            // interrupted turn as successful in the native history projection.
            records.push(
                json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":TURN2}}),
            );
            records.extend(turn(
                &project.path().canonicalize().unwrap(),
                TURN3,
                "later success",
            ));
            let source_bytes = write_records(&path, &records);
            let captured = retain_observed(project.path(), native.path(), &path).unwrap();
            assert!(captured.suffix_reason.is_none());
            let archive = load(project.path(), &captured.archive.id).unwrap();
            assert_eq!(archive.last_turn_id, TURN3);
            assert_eq!(archive.turn_ids, [TURN1, TURN2, TURN3]);
            assert_eq!(archive.turn_statuses, ["completed", terminal, "completed"]);
            assert_eq!(
                archive_bytes(project.path(), &archive).unwrap(),
                source_bytes
            );
            let (_, _, mut fork) = fork_fixture(&archive, &records, native.path());
            fork["turns"] = json!(archive
                .turn_ids
                .iter()
                .zip(&archive.turn_statuses)
                .map(|(id, status)| json!({"id":id,"status":status,"items":[],"itemsView":"full"}))
                .collect::<Vec<_>>());
            assert!(verify_prefix(&archive, &fork, native.path()).is_ok());
            fork["turns"][1]["status"] = json!("completed");
            assert!(verify_prefix(&archive, &fork, native.path()).is_err());
        }
    }

    #[test]
    fn abort_without_context_or_id_can_progress_to_a_later_qualified_turn() {
        let (project, native, path, mut records, _) = setup();
        records.push(json!({"type":"event_msg","payload":{"type":"task_started","turn_id":TURN2}}));
        records.push(
            json!({"type":"event_msg","payload":{"type":"turn_aborted","turn_id":null,"reason":"interrupted"}}),
        );
        write_records(&path, &records);
        assert!(retain_observed(project.path(), native.path(), &path)
            .unwrap()
            .suffix_reason
            .unwrap()
            .contains("lacks configuration context"));
        records.extend(turn(
            &project.path().canonicalize().unwrap(),
            TURN3,
            "later success",
        ));
        write_records(&path, &records);
        let captured = retain_observed(project.path(), native.path(), &path).unwrap();
        assert!(captured.suffix_reason.is_none());
        assert_eq!(
            captured.archive.turn_statuses,
            ["completed", "interrupted", "completed"]
        );
    }

    #[test]
    fn archives_without_terminal_status_metadata_keep_their_original_identity() {
        let (project, _native, _, _, mut archive) = setup();
        archive.terminal_status = None;
        archive.id.clear();
        archive.id = json_digest(&archive).unwrap();
        let path = archive_path(project.path(), &archive.id)
            .unwrap()
            .join("manifest.json");
        safe_io::atomic_replace_json(&path, &serde_json::to_value(&archive).unwrap()).unwrap();
        let loaded = load(project.path(), &archive.id).unwrap();
        assert_eq!(loaded.id, archive.id);
        assert!(loaded.terminal_status.is_none());
        assert_eq!(loaded.turn_statuses, ["completed"]);
    }

    #[test]
    fn native_error_events_preserve_failed_status_and_nonfatal_control_errors() {
        for (info, expected) in [
            (json!("other"), "failed"),
            (json!("thread_rollback_failed"), "completed"),
            (
                json!({"active_turn_not_steerable":{"turn_kind":"review"}}),
                "completed",
            ),
        ] {
            let (project, _native, _path, mut records, _) = setup();
            let mut middle = turn(project.path(), TURN2, "middle turn");
            middle.insert(middle.len()-1,json!({"type":"event_msg","payload":{"type":"error","message":"native error","codex_error_info":info}}));
            records.extend(middle);
            records.extend(turn(project.path(), TURN3, "later success"));
            let bytes = records.iter().map(|v| format!("{v}\n")).collect::<String>();
            let boundary = settled_prefix(bytes.as_bytes()).unwrap();
            assert!(boundary.suffix_reason.is_none());
            assert_eq!(boundary.turn_statuses, ["completed", expected, "completed"]);
        }
    }

    #[test]
    fn standalone_error_payload_is_retained_but_native_roundtrip_is_explicitly_unavailable() {
        let (project, native, path, mut records, _) = setup();
        let terminal = records.pop().unwrap();
        records.push(json!({"type":"event_msg","payload":{"type":"error","message":"legacy error","codex_error_info":"other"}}));
        records.push(terminal);
        let raw = write_records(&path, &records);
        let capture = retain_observed(project.path(), native.path(), &path).unwrap();
        assert!(capture.suffix_reason.is_none());
        assert_eq!(capture.archive.turn_statuses, ["failed"]);
        let archive = load(project.path(), &capture.archive.id).unwrap();
        assert_eq!(archive_bytes(project.path(), &archive).unwrap(), raw);
        assert!(archive
            .native_recovery_issue
            .as_deref()
            .unwrap()
            .contains("drops standalone error events"));
        let operation = uuid::Uuid::new_v4().to_string();
        let error = restore(
            project.path(),
            native.path(),
            &archive.id,
            &operation,
            Path::new("must-not-start-native-runner"),
        )
        .unwrap_err();
        assert!(error.contains("raw archive retained"));
        assert!(!operation_path(project.path(), &operation).unwrap().exists());
        let report = crate::fidelity::native_session_report(project.path(), &archive.id, None);
        assert_eq!(report["native_archive_verified"], true);
        assert!(report["unavailable"]
            .as_str()
            .unwrap()
            .contains("standalone error events"));
    }

    fn fork_fixture(
        archive: &NativeArchive,
        source: &[Value],
        native: &Path,
    ) -> (PathBuf, Vec<Value>, Value) {
        let path = native.join("destination.jsonl");
        let mut records = vec![
            json!({"type":"session_meta","payload":{"id":DEST,"session_id":DEST,"forked_from_id":THREAD}}),
        ];
        records.extend(source.iter().cloned().map(|mut v| {
            v["timestamp"] = json!("new transport timestamp");
            v
        }));
        write_records(&path, &records);
        let thread = json!({"id":DEST,"sessionId":DEST,"forkedFromId":THREAD,"cwd":archive.cwd,"cliVersion":QUALIFIED_VERSION,"path":path,"turns":[{"id":TURN1,"items":[],"itemsView":"full","status":"completed"}]});
        (path, records, thread)
    }

    #[test]
    fn prefix_checks_full_identity_payload_and_ancestry_but_permits_new_turns() {
        let (_project, native, _path, source, archive) = setup();
        let (path, mut records, mut thread) = fork_fixture(&archive, &source, native.path());
        let (_, historical) = verify_prefix(&archive, &thread, native.path()).unwrap();
        records.extend(turn(&archive.cwd, TURN2, "new native turn"));
        write_records(&path, &records);
        thread["turns"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":TURN2,"status":"completed","itemsView":"full","items":[]}));
        assert_eq!(
            verify_prefix(&archive, &thread, native.path()).unwrap().1,
            historical
        );
        thread["turns"][0]["id"] = json!(TURN2);
        assert!(verify_prefix(&archive, &thread, native.path()).is_err());
        thread["turns"][0]["id"] = json!(TURN1);
        thread["forkedFromId"] = json!(DEST);
        assert!(verify_prefix(&archive, &thread, native.path()).is_err());
        thread["forkedFromId"] = json!(THREAD);
        records[4]["payload"]["content"][0]["text"] = json!("changed historical content");
        write_records(&path, &records);
        assert!(verify_prefix(&archive, &thread, native.path())
            .unwrap_err()
            .contains("digest mismatch"));
    }

    #[test]
    fn configuration_fingerprints_are_home_portable_but_changes_are_visible() {
        let (project, native, _, _, archive) = setup();
        let fresh = tempfile::tempdir().unwrap();
        assert_eq!(
            archive.config_files,
            config_files(fresh.path(), project.path()).unwrap()
        );
        std::fs::write(native.path().join("config.toml"), "model = 'different'\n").unwrap();
        assert_ne!(
            archive.config_files,
            config_files(native.path(), project.path()).unwrap()
        );
        let response = json!({"model":"qualified-model","modelProvider":"openai","approvalPolicy":"never","approvalsReviewer":"user","sandbox":{"type":"readOnly"},"reasoningEffort":"low","cwd":archive.cwd,"runtimeWorkspaceRoots":[archive.cwd]});
        assert!(verify_effective(&archive, &response).is_ok());
        let mut changed = response;
        changed["model"] = json!("different");
        assert!(verify_effective(&archive, &changed).is_err());
    }

    #[test]
    fn changed_native_continuation_model_is_unqualified_even_with_matching_history() {
        let (_project, native, _source, history, archive) = setup();
        let (path, mut records, _thread) = fork_fixture(&archive, &history, native.path());
        let completed =
            vec![json!({"id":TURN2,"status":"completed","itemsView":"full","items":[]})];
        records.extend(turn(&archive.cwd, TURN2, "continue"));
        write_records(&path, &records);
        assert!(verify_continuation_context(&archive, &path, &completed).is_ok());
        let context = records
            .iter_mut()
            .rev()
            .find(|r| r["type"] == "turn_context")
            .unwrap();
        context["payload"]["model"] = json!("different-model");
        write_records(&path, &records);
        assert!(verify_continuation_context(&archive, &path, &completed)
            .unwrap_err()
            .contains("changed model"));
    }

    #[cfg(unix)]
    #[test]
    fn changed_native_runner_version_refuses_before_app_server_start() {
        use std::os::unix::fs::PermissionsExt;
        let (_project, native, _, _, archive) = setup();
        let runner = native.path().join("unsupported-codex");
        std::fs::write(&runner,"#!/bin/sh\nif [ \"$1\" = --version ]; then printf '%s\\n' 'codex-cli 0.150.0'; else exit 99; fi\n").unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();
        let error = Rpc::start(&runner, &archive, native.path())
            .err()
            .expect("version refused");
        assert!(error.contains("unsupported native runner"));
    }

    #[test]
    fn uncertain_restore_reconciles_only_exact_operation_and_refuses_duplicates() {
        let (_project, native, _, _, archive) = setup();
        let operation = uuid::Uuid::new_v4().to_string();
        assert!(reconcile(&archive, &operation, native.path())
            .unwrap()
            .is_none());
        let dir = native.path().join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let marker = json!({"type":"session_meta","payload":{"id":DEST,"forked_from_id":THREAD,"thread_source":format!("stateroot_restore_{operation}")}});
        write_records(&dir.join("one.jsonl"), std::slice::from_ref(&marker));
        assert_eq!(
            reconcile(&archive, &operation, native.path())
                .unwrap()
                .as_deref(),
            Some(DEST)
        );
        let mut second = marker;
        second["payload"]["id"] = json!(uuid::Uuid::new_v4().to_string());
        write_records(&dir.join("two.jsonl"), &[second]);
        assert!(reconcile(&archive, &operation, native.path())
            .unwrap_err()
            .contains("ambiguous"));
    }

    #[test]
    fn stale_concurrent_inspection_cannot_replace_newer_continuation_receipt() {
        let (project, _native, _, _, archive) = setup();
        let operation = uuid::Uuid::new_v4().to_string();
        let initial = RestoreReceipt {
            schema: RESTORE_SCHEMA.into(),
            operation_id: operation.clone(),
            archive_id: archive.id.clone(),
            source_thread_id: THREAD.into(),
            stage: "prefix_verified".into(),
            destination_thread_id: Some(DEST.into()),
            destination_session_id: Some(DEST.into()),
            destination_path: None,
            prefix_digest: Some(archive.prefix_digest),
            native_turns_digest: Some("historical".into()),
            effective_config_digest: Some("config".into()),
            effective_envelope: None,
            observed_continuation: None,
        };
        save_receipt(project.path(), &initial).unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (published_tx, published_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            // Reader A snapshots first, but finishes its old native read only
            // after B has published completed continuation evidence.
            let reader_project = project.path();
            let reader_operation = &operation;
            let stale = scope.spawn(move || {
                let (old, snapshot) = receipt_snapshot(reader_project, reader_operation).unwrap();
                ready_tx.send(()).unwrap();
                published_rx.recv().unwrap();
                publish_inspection(reader_project, &snapshot, &old)
            });
            ready_rx.recv().unwrap();
            let (mut newer, snapshot) = receipt_snapshot(project.path(), &operation).unwrap();
            newer.stage = "continuation_observed".into();
            newer.observed_continuation =
                Some(json!({"turns":[{"id":TURN2,"status":"completed"}]}));
            publish_inspection(project.path(), &snapshot, &newer).unwrap();
            published_tx.send(()).unwrap();
            assert!(stale
                .join()
                .unwrap()
                .unwrap_err()
                .contains("operation changed"));
            assert_eq!(
                serde_json::to_value(receipt(project.path(), &operation).unwrap()).unwrap(),
                serde_json::to_value(newer).unwrap()
            );
        });
    }

    /// Explicit opt-in, real installed native app-server; no model turn is
    /// requested here. The external qualification drives native continuation.
    #[test]
    #[ignore = "requires isolated native qualification paths and Codex 0.149.1"]
    fn real_native_archive_restore() {
        let project = PathBuf::from(std::env::var("STATEROOT_NATIVE_TEST_WORKSPACE").unwrap());
        let source = PathBuf::from(std::env::var("STATEROOT_NATIVE_TEST_SOURCE").unwrap());
        let source_home =
            PathBuf::from(std::env::var("STATEROOT_NATIVE_TEST_SOURCE_HOME").unwrap());
        let user = PathBuf::from(std::env::var("HOME").unwrap());
        crate::local_store::init_skeleton(
            &project,
            "native-qualification",
            "native-qualification",
            "test",
        )
        .unwrap();
        let (meta, events) = crate::transcripts::codex::parse_session_file(&source).unwrap();
        let canonical =
            crate::sessions::extract::canonical_from_codex(&meta, &events, &source, &project)
                .unwrap();
        crate::sessions::write_session(&project, &canonical).unwrap();
        let archive = retain_in(&project, &source_home, &source).unwrap();
        let operation = std::env::var("STATEROOT_NATIVE_TEST_OPERATION").unwrap();
        let state = restore(&project, &user, &archive.id, &operation, Path::new("codex")).unwrap();
        assert_eq!(state.stage, "prefix_verified");
        let again = restore(&project, &user, &archive.id, &operation, Path::new("codex")).unwrap();
        assert_eq!(state.destination_thread_id, again.destination_thread_id);
        // Simulate losing the response after the native fork already persisted.
        let mut interrupted = state.clone();
        interrupted.stage = "fork_requested".into();
        interrupted.destination_thread_id = None;
        interrupted.destination_path = None;
        interrupted.prefix_digest = None;
        interrupted.native_turns_digest = None;
        interrupted.effective_envelope = None;
        save_receipt(&project, &interrupted).unwrap();
        let recovered =
            restore(&project, &user, &archive.id, &operation, Path::new("codex")).unwrap();
        assert_eq!(state.destination_thread_id, recovered.destination_thread_id);
        println!(
            "NATIVE_QUALIFICATION {}",
            serde_json::to_string(&recovered).unwrap()
        );
    }
}
