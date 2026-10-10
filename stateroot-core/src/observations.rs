//! Durable hook-observation evidence store.
//!
//! Layout under `.stateroot/spool/`:
//! - `observations.jsonl` — legacy v1 spool. Never rotated, never cleared,
//!   never rewritten by capture; rows keep their original bytes and their
//!   `obs_<line>` references stay append-stable. Read through the same CLI
//!   as before; references that predate a historical clear/rotation report
//!   unavailable rather than inventing a mapping.
//! - `segments/<harness>__<session>.jsonl` — immutable, session-addressed
//!   v2 segments. Capture appends one record per line under a mandatory
//!   short-budget `ResourceLock`, syncs before acknowledging, and terminates
//!   a torn tail before appending so a killed writer never corrupts the next
//!   record. No age-based deletion, pruning, or truncation.
//! - `segments/<stem>.frontier.json` — small per-session durable frontier
//!   (record count + last capture id/ts), written under the capture lock
//!   after every durable append. `session_watermark` reads ONLY this bounded
//!   file; a missing/corrupt frontier is surfaced distinctly and rebuilt by
//!   `recover_session_frontier` (recovery scans the segment — stop never
//!   does).
//! - `segments/<stem>.sealed.json` — seal marker written by the post-ingest
//!   drainer (never synchronously at stop). Segments without it are
//!   `pending-seal`.
//! - `current.json` — small current projection (watermark, counters, recent
//!   refs). Rebuildable from segments; never the source of truth.
//!
//! Dedup key (A2): project/lineage (the store location), canonical harness,
//! verified native session, and verified native event identity. With no host
//! event identity the capture mints a `cap_` id and labels the identity
//! unavailable — distinct events are never collapsed by text hash. Same
//! identity with different content is an explicit conflict; both bodies stay.//!
//! Provider calls: zero. Everything here is local files.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::local_store;
use crate::safe_io::{self, ResourceLock};

/// Schema tag written into every v2 record.
pub const OBSERVATION_SCHEMA_V2: &str = "stateroot.observation.v2";
/// Schema tag of the small current projection.
pub const PROJECTION_SCHEMA_V1: &str = "stateroot.observations.projection.v1";
/// Schema tag of the per-session durable frontier.
pub const FRONTIER_SCHEMA_V1: &str = "stateroot.observations.frontier.v1";
/// Recent refs kept in the current projection.
const PROJECTION_RECENT: usize = 32;
/// Maximum length of one sanitized filename component.
const COMPONENT_MAX: usize = 80;

/// One spool observation. Legacy rows use the line-based `obs_<line>` id;
/// v2 rows use their stable `capture_id`.
#[derive(Debug, Clone)]
pub struct Observation {
    /// Stable id (`obs_<line>` for legacy rows, `cap_…` for v2 rows).
    pub id: String,
    /// 1-based line number inside its origin file.
    pub line_no: usize,
    /// RFC3339 timestamp when present.
    pub ts: String,
    /// Canonical hook event name.
    pub event: String,
    /// Harness id.
    pub harness: String,
    /// Captured text body (uncapped for v2 rows).
    pub text: String,
    /// Optional kind hint from the hook.
    pub kind_hint: Option<String>,
    /// Optional tool name from harness payload.
    pub tool: Option<String>,
    /// Optional bounded excerpt (list views; never the evidence).
    pub excerpt: Option<String>,
    /// `foreign` when evidence suggests another initialized project/worktree.
    pub scope_status: Option<String>,
    /// `legacy` (observations.jsonl) or `segment` (immutable v2 segment).
    pub origin: String,
    /// Stable captured-event reference (v2 rows only).
    pub capture_id: Option<String>,
    /// Capture status: `legacy`, `captured`, or `conflict`.
    pub status: String,
    /// For `conflict` rows, the earlier record with the same event identity.
    pub conflict_with: Option<String>,
    /// Session id when known.
    pub session_id: Option<String>,
    /// `native` / `minted` / `unavailable` session identity provenance.
    pub session_identity: Option<String>,
    /// sha256 hex of the full captured text (v2 rows only).
    pub text_digest: Option<String>,
}

/// Filter options for listing/searching observations.
#[derive(Debug, Clone, Default)]
pub struct ObservationFilter {
    /// Match `kind_hint` or `event` (case-insensitive substring).
    pub kind: Option<String>,
    /// Match harness id (case-insensitive exact).
    pub harness: Option<String>,
    /// Match RFC3339 prefix (inclusive lower bound).
    pub since: Option<String>,
    /// Match RFC3339 prefix (inclusive upper bound).
    pub until: Option<String>,
    /// Case-insensitive substring over text/excerpt/tool fields.
    pub query: Option<String>,
    /// Maximum rows (0 = unlimited).
    pub limit: usize,
}

/// Full v2 observation record (the durable evidence shape).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationRecord {
    /// Always [`OBSERVATION_SCHEMA_V2`].
    pub schema: String,
    /// Stable captured-event reference (`cap_…`), minted at capture.
    pub capture_id: String,
    /// RFC3339 capture time.
    pub ts: String,
    /// Canonical hook event name.
    pub event: String,
    /// Canonical harness id.
    pub harness: String,
    /// Project id from the manifest (provenance; dedup scope is the store).
    #[serde(default)]
    pub project_id: Option<String>,
    /// Lineage refname at capture (provenance for fork/merge review).
    #[serde(default)]
    pub lineage: Option<String>,
    /// Session id (native when the harness sent one).
    #[serde(default)]
    pub session_id: Option<String>,
    /// `native` | `minted` | `unavailable`.
    #[serde(default)]
    pub session_identity: Option<String>,
    /// Verified native event identity or `{"status": "unavailable"}`.
    #[serde(default)]
    pub event_identity: Option<EventIdentity>,
    /// Full captured text — never truncated at capture.
    pub text: String,
    /// sha256 hex of `text`.
    #[serde(default)]
    pub text_digest: String,
    /// Byte length of `text`.
    #[serde(default)]
    pub text_len: usize,
    #[serde(default)]
    pub kind_hint: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    /// Bounded excerpt for digests/list views.
    #[serde(default)]
    pub excerpt: Option<String>,
    /// Capture outcome + source integrity.
    #[serde(default)]
    pub capture: CaptureInfo,
    /// The authorized captured source (the raw hook payload), uncapped.
    #[serde(default)]
    pub source: Option<SourceRef>,
    /// Unknown payload fields, kept as observed metadata.
    #[serde(default)]
    pub meta: Option<Value>,
}

/// Verified native event identity carried by a record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventIdentity {
    /// `native` when a known adapter field carried it, else `unavailable`.
    pub status: String,
    /// Payload field the identity came from (e.g. `tool_use_id`).
    #[serde(default)]
    pub field: Option<String>,
    /// The identity value.
    #[serde(default)]
    pub value: Option<String>,
}

/// Capture outcome + source integrity of one record.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaptureInfo {
    /// `captured` | `replay` | `conflict`.
    #[serde(default = "default_status")]
    pub status: String,
    /// For `replay` sightings: the original record.
    #[serde(default)]
    pub replay_of: Option<String>,
    /// For `conflict` rows: the earlier record with the same identity.
    #[serde(default)]
    pub conflict_with: Option<String>,
    /// `complete` | `transport_read_error` — when a transport limit prevents
    /// full capture, the loss is surfaced here, never acknowledged complete.
    #[serde(default = "default_source_status")]
    pub source_status: String,
}

fn default_status() -> String {
    "captured".into()
}

fn default_source_status() -> String {
    "complete".into()
}

/// The authorized captured source (raw hook payload) with its digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRef {
    /// The original payload JSON.
    pub payload: Value,
    /// sha256 hex of the canonical payload serialization.
    pub digest: String,
    /// Byte length of the canonical serialization.
    pub bytes: usize,
}

/// Everything the writer needs to durably store one observation.
#[derive(Debug, Clone)]
pub struct CaptureRequest {
    /// RFC3339 capture time.
    pub ts: String,
    /// Canonical hook event name.
    pub event: String,
    /// Canonical harness id.
    pub harness: String,
    /// Session id after adapter tagging.
    pub session_id: Option<String>,
    /// `native` | `minted` | `unavailable`.
    pub session_identity: String,
    /// Verified native event identity `(payload field, value)`, if any.
    pub event_identity: Option<(String, String)>,
    /// Full captured text (uncapped).
    pub text: String,
    pub kind_hint: Option<String>,
    pub tool: Option<String>,
    /// Bounded excerpt for list views.
    pub excerpt: Option<String>,
    /// The raw hook payload (authorized captured source).
    pub source: Option<Value>,
    /// `complete` | `transport_read_error`.
    pub source_status: String,
    /// Unknown payload fields as observed metadata.
    pub meta: Option<Value>,
}

/// What the durable store did with one capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStatus {
    /// New durable record appended and synced.
    Captured,
    /// Same identity + same content seen before — only a replay sighting was
    /// recorded; the event is NOT a second occurrence.
    Replay,
    /// Same identity, different content — explicit conflict; both bodies
    /// retained.
    Conflict,
}

/// Result of one durable capture.
#[derive(Debug)]
pub struct CaptureOutcome {
    /// The stored record (for `Replay`, the replay sighting row).
    pub record: ObservationRecord,
    pub status: CaptureStatus,
    /// Segment file the record was appended to.
    pub segment: PathBuf,
    /// 1-based line number inside the segment.
    pub line_no: usize,
    /// Set for `Replay`/`Conflict`: the earlier record's capture id.
    pub related: Option<String>,
    /// False when the segment append succeeded but the small projection
    /// update failed (evidence is durable; the projection rebuilds).
    pub projection_synced: bool,
    /// The projection failure detail when `projection_synced` is false.
    pub projection_error: Option<String>,
    /// False when the segment append succeeded but the per-session frontier
    /// write failed (evidence is durable; the frontier is rebuilt by the
    /// next capture or by `recover_session_frontier`).
    pub frontier_synced: bool,
    /// The frontier failure detail when `frontier_synced` is false.
    pub frontier_error: Option<String>,
}

/// Capture failures — acquisition/storage failures always propagate.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// The segment lock could not be acquired inside the budget.
    #[error("observation segment lock: {0}")]
    Lock(#[from] safe_io::LockError),
    /// Filesystem failure while writing/syncing the segment.
    #[error("io error on observation segment: {0}")]
    Io(#[from] std::io::Error),
    /// The record could not be serialized.
    #[error("observation record serialization: {0}")]
    Json(#[from] serde_json::Error),
}

/// Capture watermark: the durable frontier of the store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaptureWatermark {
    /// Segment file name of the last durably published record.
    #[serde(default)]
    pub segment: Option<String>,
    /// Total durable event records across segments (captured + conflict).
    #[serde(default)]
    pub records: u64,
    /// Capture id of the last durably published record.
    #[serde(default)]
    pub last_capture_id: Option<String>,
    /// Timestamp of the last durably published record.
    #[serde(default)]
    pub last_ts: Option<String>,
}

/// A malformed/torn evidence line, preserved and inspectable.
#[derive(Debug, Clone)]
pub struct CorruptLine {
    /// File containing the line (spool-relative).
    pub file: String,
    /// 1-based line number.
    pub line_no: usize,
    /// The raw line content (lossy for invalid UTF-8).
    pub raw: String,
    /// Why it is not a valid record (`invalid JSON`, `unrecognized schema`, …).
    pub reason: String,
}

/// A file that exists but could not be read (permission denied, is a
/// directory, invalid UTF-8) — surfaced, never silently treated as empty.
#[derive(Debug, Clone)]
pub struct UnreadableFile {
    /// File (spool-relative).
    pub file: String,
    /// The read failure.
    pub error: String,
}

/// Retention/corruption health of the observation store (WS2 status/doctor).
#[derive(Debug, Clone, Default)]
pub struct SpoolHealth {
    /// Number of session segments.
    pub segments: usize,
    /// Segments with a seal marker.
    pub sealed: usize,
    /// Segment file names with records but no seal yet.
    pub pending_seal: Vec<String>,
    /// Durable event records (captured + conflict).
    pub records: u64,
    /// Replay sightings (duplicate deliveries; not second occurrences).
    pub replays: u64,
    /// Explicit identity conflicts.
    pub conflicts: u64,
    /// Records whose captured source is incomplete/missing.
    pub source_unavailable: u64,
    /// Malformed/torn lines across the legacy spool and segments.
    pub corrupt: Vec<CorruptLine>,
    /// Legacy spool / segment / frontier files that exist but could not be
    /// read — an unreadable file is NEVER an empty one.
    pub read_failures: Vec<UnreadableFile>,
    /// Segments without a frontier file (pre-frontier store, or a crash
    /// between the durable append and the frontier write) — recoverable via
    /// [`recover_session_frontier`].
    pub missing_frontier: Vec<String>,
    /// Rows in the legacy spool.
    pub legacy_rows: u64,
    /// Current watermark.
    pub watermark: CaptureWatermark,
}

/// One segment file and its seal state.
#[derive(Debug, Clone)]
pub struct SegmentInfo {
    /// File name inside `segments/`.
    pub file: String,
    /// Full path.
    pub path: PathBuf,
    /// A seal marker exists.
    pub sealed: bool,
    /// Parsed event records (captured + conflict; replay sightings excluded).
    pub records: u64,
    /// File size in bytes.
    pub bytes: u64,
}

/// Honest resolution of a user-facing observation reference.
#[derive(Debug)]
pub enum ObservationLookup {
    /// Found and parseable.
    Found(Box<Observation>),
    /// The reference pointed at evidence that no longer exists (historical
    /// clear/rotation) or at a line that is not a parseable record.
    Unavailable(String),
    /// No such reference.
    NotFound,
}

// ---------------------------------------------------------------------
// paths
// ---------------------------------------------------------------------

fn spool_dir(project_dir: &Path) -> PathBuf {
    local_store::root(project_dir).join("spool")
}

/// The legacy v1 spool (`spool/observations.jsonl`).
pub fn legacy_spool_path(project_dir: &Path) -> PathBuf {
    spool_dir(project_dir).join("observations.jsonl")
}

/// The immutable session-segment directory (`spool/segments/`).
pub fn segments_dir(project_dir: &Path) -> PathBuf {
    spool_dir(project_dir).join("segments")
}

/// The small current projection (`spool/current.json`).
pub fn projection_path(project_dir: &Path) -> PathBuf {
    spool_dir(project_dir).join("current.json")
}

fn projection_lock_path(project_dir: &Path) -> PathBuf {
    spool_dir(project_dir).join("current.lock")
}

fn seal_path(segment: &Path) -> PathBuf {
    segment.with_extension("sealed.json")
}

fn current_seal(segment: &Path) -> bool {
    let read = |path: &Path| {
        bounded_metadata(path, 16 * 1024)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    };
    let Some(seal) = read(&seal_path(segment)) else {
        return false;
    };
    let Some(frontier) = read(&frontier_path(segment)) else {
        return false;
    };
    seal["schema"] == "stateroot.observations.seal.v1"
        && frontier["schema"] == FRONTIER_SCHEMA_V1
        && seal["last_capture_id"] == frontier["last_capture_id"]
        && seal["records"] == frontier["records"]
        && segment
            .metadata()
            .is_ok_and(|metadata| seal["bytes"].as_u64() == Some(metadata.len()))
}

/// The per-session durable frontier for a segment (`<stem>.frontier.json`).
fn frontier_path(segment: &Path) -> PathBuf {
    segment.with_extension("frontier.json")
}

/// Filename-safe component: ASCII alnum/`-`/`_` only; long inputs are
/// truncated with a short content hash so distinct sessions never collide.
fn sanitize_component(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.len() <= COMPONENT_MAX {
        return cleaned;
    }
    let hash = format!("{:x}", Sha256::digest(cleaned.as_bytes()));
    format!("{}-{}", &cleaned[..COMPONENT_MAX], &hash[..8])
}

fn segment_file(harness: &str, session_id: Option<&str>) -> String {
    let session = session_id.unwrap_or("nosession");
    format!(
        "{}__{}.jsonl",
        sanitize_component(harness),
        sanitize_component(session)
    )
}

fn sha256_hex(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// Mint a unique capture id: UUIDv7 (time-ordered, collision-free without a
/// process-local sequence or a pid component). Ordering is never
/// load-bearing (dedup keys on event identity).
fn mint_capture_id() -> String {
    format!("cap_{}", uuid::Uuid::now_v7())
}

// ---------------------------------------------------------------------
// capture (write path)
// ---------------------------------------------------------------------

/// Durably store one observation: lock the session segment, dedup by
/// verified native event identity, append + sync before acknowledging, then
/// update the small current projection. Failures propagate — a capture is
/// never acknowledged before its record is durable.
pub fn capture(project_dir: &Path, req: CaptureRequest) -> Result<CaptureOutcome, CaptureError> {
    let dir = segments_dir(project_dir);
    std::fs::create_dir_all(&dir)?;
    let file = segment_file(&req.harness, req.session_id.as_deref());
    let segment = dir.join(&file);
    let _lock = ResourceLock::acquire(segment.with_extension("lock"))?;

    // Read existing lines for identity dedup (segment = dedup scope:
    // project/lineage + harness + session are the segment itself). Only an
    // ABSENT segment means an empty history: a permission, directory, or
    // invalid-UTF-8 read failure propagates — it is never silently treated
    // as "no priors", which would turn the dedup below into a false success.
    let existing = match std::fs::read_to_string(&segment) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(CaptureError::Io(err)),
    };
    let parsed: Vec<(usize, Value)> = existing
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            serde_json::from_str::<Value>(line)
                .ok()
                .map(|v| (idx + 1, v))
        })
        .collect();

    let text_digest = sha256_hex(&req.text);
    let (source, source_digest) = match &req.source {
        Some(payload) => {
            let canonical = serde_json::to_string(payload).unwrap_or_default();
            (
                Some(SourceRef {
                    digest: sha256_hex(&canonical),
                    bytes: canonical.len(),
                    payload: payload.clone(),
                }),
                Some(sha256_hex(&canonical)),
            )
        }
        None => (None, None),
    };

    let project_id = local_store::read_manifest(project_dir)
        .ok()
        .flatten()
        .and_then(|m| {
            m.get("project_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    let lineage = Some(crate::roots::lineage_refname(project_dir));

    // Identity dedup: only a verified native event identity can dedup; text
    // hashes never collapse distinct events. The dedup scope is the full key
    // (project/lineage + harness + session + canonical event + identity):
    // the segment pins project+harness+session, and the match additionally
    // requires the same canonical event (pre/post tool use share a
    // `tool_use_id` without being the same event) and the same lineage (a
    // replayed identity on another lineage is new evidence, not a replay).
    // An unlineaged prior record is never PROVEN same-lineage, so it never
    // dedups. ALL same-identity versions are gathered: a replay of any
    // retained version references that version instead of appending a
    // duplicate conflict.
    let priors: Vec<&Value> = req
        .event_identity
        .as_ref()
        .map(|(field, value)| {
            parsed
                .iter()
                .filter(|(_, v)| {
                    v.get("capture")
                        .and_then(|c| c.get("status"))
                        .and_then(|s| s.as_str())
                        != Some("replay")
                        && v.get("event").and_then(|e| e.as_str()) == Some(req.event.as_str())
                        && v.get("event_identity")
                            .and_then(|e| e.get("field"))
                            .and_then(|f| f.as_str())
                            == Some(field.as_str())
                        && v.get("event_identity")
                            .and_then(|e| e.get("value"))
                            .and_then(|f| f.as_str())
                            == Some(value.as_str())
                        && match (
                            v.get("lineage").and_then(|l| l.as_str()),
                            lineage.as_deref(),
                        ) {
                            (Some(prior_lineage), Some(current_lineage)) => {
                                prior_lineage == current_lineage
                            }
                            // Unknown lineage is never proven equal.
                            _ => false,
                        }
                })
                .map(|(_, v)| v)
                .collect()
        })
        .unwrap_or_default();

    let event_identity = match &req.event_identity {
        Some((field, value)) => EventIdentity {
            status: "native".into(),
            field: Some(field.clone()),
            value: Some(value.clone()),
        },
        None => EventIdentity {
            status: "unavailable".into(),
            field: None,
            value: None,
        },
    };
    let build = |status: &str,
                 replay_of: Option<String>,
                 conflict_with: Option<String>,
                 full: bool|
     -> ObservationRecord {
        ObservationRecord {
            schema: OBSERVATION_SCHEMA_V2.into(),
            capture_id: mint_capture_id(),
            ts: req.ts.clone(),
            event: req.event.clone(),
            harness: req.harness.clone(),
            project_id: project_id.clone(),
            lineage: lineage.clone(),
            session_id: req.session_id.clone(),
            session_identity: Some(req.session_identity.clone()),
            event_identity: Some(event_identity.clone()),
            // Replay sightings carry no body — the identical body is already
            // durable under the original record; digests on the sighting
            // keep the equality check auditable.
            text: if full {
                req.text.clone()
            } else {
                String::new()
            },
            text_digest: text_digest.clone(),
            text_len: if full { req.text.len() } else { 0 },
            kind_hint: req.kind_hint.clone(),
            tool: req.tool.clone(),
            excerpt: req.excerpt.clone(),
            capture: CaptureInfo {
                status: status.into(),
                replay_of,
                conflict_with,
                source_status: req.source_status.clone(),
            },
            source: if full { source.clone() } else { None },
            meta: req.meta.clone(),
        }
    };

    // Search EVERY same-identity version for an exact content digest first:
    // a replay of a retained conflicting version references that version —
    // it never appends a second copy of the same body as a new conflict.
    let digest_match = priors.iter().find(|prior| {
        prior
            .get("text_digest")
            .and_then(|v| v.as_str())
            .is_some_and(|d| d == text_digest)
            && prior
                .get("source")
                .and_then(|s| s.get("digest"))
                .and_then(|v| v.as_str())
                == source_digest.as_deref()
    });
    let capture_id_of = |record: &Value| {
        record
            .get("capture_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let (status, related, record) = if let Some(prior) = digest_match {
        // Replay of an identical delivery: record a sighting marker, never
        // a second occurrence of the event.
        let prior_id = capture_id_of(prior);
        let sighting = build("replay", Some(prior_id.clone()), None, false);
        (CaptureStatus::Replay, Some(prior_id), sighting)
    } else if let Some(earliest) = priors.first() {
        // Same identity, no matching content among the retained versions:
        // explicit conflict with the earliest version, both bodies retained.
        let prior_id = capture_id_of(earliest);
        let record = build("conflict", None, Some(prior_id.clone()), true);
        (CaptureStatus::Conflict, Some(prior_id), record)
    } else {
        let record = build("captured", None, None, true);
        (CaptureStatus::Captured, None, record)
    };

    // Append: a torn tail (killed writer) is terminated first so its bytes
    // stay their own — corrupt but preserved — line and never glue onto the
    // new record.
    let mut line = serde_json::to_string(&record)?;
    line.push('\n');
    let torn_tail = !existing.is_empty() && !existing.ends_with('\n');
    let line_no = existing.lines().count() + 1;
    {
        let mut file_handle = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&segment)?;
        if torn_tail {
            file_handle.write_all(b"\n")?;
        }
        file_handle.write_all(line.as_bytes())?;
        file_handle.sync_all()?;
    }

    // Per-session durable frontier, written under the still-held capture
    // lock after the durable append. `session_watermark` reads only this
    // bounded file — the full-segment scan happens here at capture (the
    // writer already parsed the segment for dedup) or in explicit recovery.
    let (frontier_synced, frontier_error) = match sync_frontier(&segment, &file, &parsed, &record) {
        Ok(()) => (true, None),
        Err(err) => (false, Some(err.to_string())),
    };

    // Small current projection, rebuilt incrementally under its own lock.
    // Failure here never denies the durable append; it is reported honestly.
    let (projection_synced, projection_error) = match update_projection(project_dir, &file, &record)
    {
        Ok(()) => (true, None),
        Err(err) => (false, Some(err.to_string())),
    };

    Ok(CaptureOutcome {
        record,
        status,
        segment,
        line_no,
        related,
        projection_synced,
        projection_error,
        frontier_synced,
        frontier_error,
    })
}

/// The on-disk per-session frontier (`<stem>.frontier.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SessionFrontier {
    #[serde(default)]
    schema: String,
    #[serde(default)]
    segment: String,
    #[serde(default)]
    records: u64,
    #[serde(default)]
    last_capture_id: Option<String>,
    #[serde(default)]
    last_ts: Option<String>,
    #[serde(default)]
    updated_at: String,
}

impl PartialEq for SessionFrontier {
    // `updated_at` is bookkeeping — equality covers the durable facts only.
    fn eq(&self, other: &Self) -> bool {
        self.schema == other.schema
            && self.segment == other.segment
            && self.records == other.records
            && self.last_capture_id == other.last_capture_id
            && self.last_ts == other.last_ts
    }
}

/// The frontier implied by the already-parsed segment rows plus the record
/// just appended. Replay sightings never move the frontier.
fn frontier_from_parsed(
    file: &str,
    parsed: &[(usize, Value)],
    appended: Option<&ObservationRecord>,
) -> SessionFrontier {
    let mut frontier = SessionFrontier {
        schema: FRONTIER_SCHEMA_V1.into(),
        segment: file.to_string(),
        ..SessionFrontier::default()
    };
    let mut fold =
        |schema: Option<&str>, status: Option<&str>, capture_id: Option<&str>, ts: Option<&str>| {
            if schema != Some(OBSERVATION_SCHEMA_V2) || status == Some("replay") {
                return;
            }
            frontier.records += 1;
            frontier.last_capture_id = capture_id.map(str::to_string);
            frontier.last_ts = ts.map(str::to_string);
        };
    for (_, value) in parsed {
        fold(
            value.get("schema").and_then(|v| v.as_str()),
            value
                .get("capture")
                .and_then(|c| c.get("status"))
                .and_then(|s| s.as_str()),
            value.get("capture_id").and_then(|v| v.as_str()),
            value.get("ts").and_then(|v| v.as_str()),
        );
    }
    if let Some(record) = appended {
        fold(
            Some(record.schema.as_str()),
            Some(record.capture.status.as_str()),
            Some(record.capture_id.as_str()),
            Some(record.ts.as_str()),
        );
    }
    frontier
}

/// Write the session frontier when it changed (a replay sighting that does
/// not move the frontier skips the write). Atomic replace: a reader never
/// observes a torn frontier.
fn sync_frontier(
    segment: &Path,
    file: &str,
    parsed: &[(usize, Value)],
    appended: &ObservationRecord,
) -> std::io::Result<()> {
    let frontier = frontier_from_parsed(file, parsed, Some(appended));
    let path = frontier_path(segment);
    let unchanged = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<SessionFrontier>(&text).ok())
        .is_some_and(|existing| existing == frontier);
    if unchanged {
        return Ok(());
    }
    let mut frontier = frontier;
    frontier.updated_at = local_store::now_rfc3339();
    let value = serde_json::to_value(&frontier).map_err(std::io::Error::other)?;
    safe_io::atomic_replace_json(&path, &value)
}

/// The current projection on disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Projection {
    #[serde(default)]
    schema: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    watermark: CaptureWatermark,
    #[serde(default)]
    counters: ProjectionCounters,
    #[serde(default)]
    recent: Vec<ProjectionEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProjectionCounters {
    #[serde(default)]
    captured: u64,
    #[serde(default)]
    replays: u64,
    #[serde(default)]
    conflicts: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectionEntry {
    capture_id: String,
    ts: String,
    event: String,
    harness: String,
    #[serde(default)]
    session_id: Option<String>,
    status: String,
    #[serde(default)]
    excerpt: Option<String>,
}

fn update_projection(
    project_dir: &Path,
    segment_file: &str,
    record: &ObservationRecord,
) -> Result<(), CaptureError> {
    let _lock = ResourceLock::acquire(projection_lock_path(project_dir))?;
    let path = projection_path(project_dir);
    let mut projection: Projection = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(|p: &Projection| p.schema == PROJECTION_SCHEMA_V1)
        .unwrap_or_default();
    projection.schema = PROJECTION_SCHEMA_V1.into();
    projection.updated_at = record.ts.clone();
    match record.capture.status.as_str() {
        "replay" => projection.counters.replays += 1,
        "conflict" => projection.counters.conflicts += 1,
        _ => projection.counters.captured += 1,
    }
    if record.capture.status != "replay" {
        projection.watermark = CaptureWatermark {
            segment: Some(segment_file.to_string()),
            records: projection.counters.captured + projection.counters.conflicts,
            last_capture_id: Some(record.capture_id.clone()),
            last_ts: Some(record.ts.clone()),
        };
        projection.recent.push(ProjectionEntry {
            capture_id: record.capture_id.clone(),
            ts: record.ts.clone(),
            event: record.event.clone(),
            harness: record.harness.clone(),
            session_id: record.session_id.clone(),
            status: record.capture.status.clone(),
            excerpt: record.excerpt.clone(),
        });
        if projection.recent.len() > PROJECTION_RECENT {
            let overflow = projection.recent.len() - PROJECTION_RECENT;
            projection.recent.drain(..overflow);
        }
    }
    let value = serde_json::to_value(&projection)?;
    safe_io::atomic_replace_json(&path, &value)?;
    Ok(())
}

// ---------------------------------------------------------------------
// sealing (session boundary)
// ---------------------------------------------------------------------

/// Seal a session's segment after ingestion: write the
/// `<stem>.sealed.json` marker atomically with the segment's final stats.
/// Idempotent. Returns `None` when the session has no segment (nothing was
/// ever captured from it). Never deletes or truncates anything — this is the
/// non-destructive replacement for the historic stop-time spool clearing.
/// Sealing is the post-ingest drainer's job; the stop hook never seals
/// synchronously (stop latency stays bounded on long segments).
pub fn seal_session(
    project_dir: &Path,
    harness: &str,
    session_id: Option<&str>,
) -> std::io::Result<Option<PathBuf>> {
    let segment = segments_dir(project_dir).join(segment_file(harness, session_id));
    if !segment.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&segment).unwrap_or_default();
    let mut records = 0u64;
    let mut last_capture_id: Option<String> = None;
    let mut last_ts: Option<String> = None;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("schema").and_then(|v| v.as_str()) != Some(OBSERVATION_SCHEMA_V2) {
            continue;
        }
        if value
            .get("capture")
            .and_then(|c| c.get("status"))
            .and_then(|s| s.as_str())
            == Some("replay")
        {
            continue;
        }
        records += 1;
        last_capture_id = value
            .get("capture_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        last_ts = value.get("ts").and_then(|v| v.as_str()).map(str::to_string);
    }
    let marker = serde_json::json!({
        "schema": "stateroot.observations.seal.v1",
        "sealed_at": local_store::now_rfc3339(),
        "segment": segment.file_name().and_then(|n| n.to_str()).unwrap_or(""),
        "records": records,
        "bytes": segment.metadata().map(|m| m.len()).unwrap_or(0),
        "last_capture_id": last_capture_id,
        "last_ts": last_ts,
    });
    let path = seal_path(&segment);
    safe_io::atomic_replace_json(&path, &marker)?;
    Ok(Some(path))
}

// ---------------------------------------------------------------------
// reads: union of the legacy spool and every segment
// ---------------------------------------------------------------------

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn opt_str(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn foreign_scope_status(project_dir: &Path, text: &str) -> Option<String> {
    let current = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());
    for token in text.split_whitespace() {
        if !token.contains(".stateroot") {
            continue;
        }
        let path = token.trim_matches(|c: char| "{}[],'\"".contains(c));
        if path.is_empty() {
            continue;
        }
        let candidate = PathBuf::from(path);
        let root = if candidate.file_name().and_then(|n| n.to_str()) == Some(".stateroot") {
            candidate.parent().map(Path::to_path_buf)
        } else if candidate.ends_with(".stateroot/manifest.json") {
            candidate
                .parent()
                .and_then(|p| p.parent())
                .map(Path::to_path_buf)
        } else {
            None
        };
        let Some(root) = root else {
            continue;
        };
        let root = root.canonicalize().unwrap_or(root);
        if root != current && local_store::is_stateroot_dir(&root) {
            return Some("foreign".into());
        }
    }
    if text.contains("possible_project_mismatch") {
        return Some("possible_project_mismatch".into());
    }
    None
}

fn parse_legacy_line(project_dir: &Path, line_no: usize, line: &str) -> Option<Observation> {
    let value: Value = serde_json::from_str(line).ok()?;
    // v2 rows never live in the legacy file; a schema tag there means the
    // row belongs to the segment world and is read through its segment.
    if value.get("schema").and_then(|v| v.as_str()) == Some(OBSERVATION_SCHEMA_V2) {
        return None;
    }
    let text = str_field(&value, "text");
    let scope_status = foreign_scope_status(project_dir, &text);
    Some(Observation {
        id: format!("obs_{line_no}"),
        line_no,
        ts: str_field(&value, "ts"),
        event: str_field(&value, "event"),
        harness: str_field(&value, "harness"),
        text,
        kind_hint: opt_str(&value, "kind_hint"),
        tool: opt_str(&value, "tool"),
        excerpt: opt_str(&value, "excerpt"),
        scope_status,
        origin: "legacy".into(),
        capture_id: None,
        status: "legacy".into(),
        conflict_with: None,
        session_id: None,
        session_identity: None,
        text_digest: None,
    })
}

fn parse_segment_line(project_dir: &Path, line_no: usize, line: &str) -> Option<Observation> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("schema").and_then(|v| v.as_str()) != Some(OBSERVATION_SCHEMA_V2) {
        return None;
    }
    let capture = value.get("capture").cloned().unwrap_or(Value::Null);
    // Replay sightings are delivery markers, not event occurrences — they
    // surface in health/watermark, not in the observation listing.
    if capture.get("status").and_then(|s| s.as_str()) == Some("replay") {
        return None;
    }
    let text = str_field(&value, "text");
    let scope_status = foreign_scope_status(project_dir, &text);
    let capture_id = opt_str(&value, "capture_id");
    Some(Observation {
        id: capture_id
            .clone()
            .unwrap_or_else(|| format!("seg_{line_no}")),
        line_no,
        ts: str_field(&value, "ts"),
        event: str_field(&value, "event"),
        harness: str_field(&value, "harness"),
        text,
        kind_hint: opt_str(&value, "kind_hint"),
        tool: opt_str(&value, "tool"),
        excerpt: opt_str(&value, "excerpt"),
        scope_status,
        origin: "segment".into(),
        capture_id,
        status: capture
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("captured")
            .to_string(),
        conflict_with: capture
            .get("conflict_with")
            .and_then(|s| s.as_str())
            .map(str::to_string),
        session_id: opt_str(&value, "session_id"),
        session_identity: opt_str(&value, "session_identity"),
        text_digest: opt_str(&value, "text_digest"),
    })
}

fn load_legacy(project_dir: &Path) -> Vec<Observation> {
    let Ok(text) = std::fs::read_to_string(legacy_spool_path(project_dir)) else {
        return Vec::new();
    };
    text.lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            parse_legacy_line(project_dir, idx + 1, trimmed)
        })
        .collect()
}

fn load_segments(project_dir: &Path) -> Vec<Observation> {
    let mut rows = Vec::new();
    let Ok(entries) = std::fs::read_dir(segments_dir(project_dir)) else {
        return rows;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(obs) = parse_segment_line(project_dir, idx + 1, trimmed) {
                rows.push(obs);
            }
        }
    }
    rows
}

/// Load all observations: legacy spool rows plus every segment record
/// (replay sightings excluded — see [`health`]), oldest first by capture
/// time within each origin. Empty vec when no store exists.
pub fn load_spool(project_dir: &Path) -> Vec<Observation> {
    let mut rows = load_legacy(project_dir);
    rows.extend(load_segments(project_dir));
    rows
}

fn matches_filter(obs: &Observation, filter: &ObservationFilter) -> bool {
    if let Some(kind) = filter.kind.as_deref() {
        let kind_lower = kind.to_ascii_lowercase();
        let event_match = obs.event.to_ascii_lowercase().contains(&kind_lower);
        let hint_match = obs
            .kind_hint
            .as_deref()
            .map(|h| h.to_ascii_lowercase().contains(&kind_lower))
            .unwrap_or(false);
        if !event_match && !hint_match {
            return false;
        }
    }
    if let Some(harness) = filter.harness.as_deref() {
        if !obs.harness.eq_ignore_ascii_case(harness) {
            return false;
        }
    }
    if let Some(since) = filter.since.as_deref() {
        if obs.ts.as_str() < since {
            return false;
        }
    }
    if let Some(until) = filter.until.as_deref() {
        if !obs.ts.is_empty() && obs.ts.as_str() > until {
            return false;
        }
    }
    if let Some(query) = filter.query.as_deref() {
        let q = query.to_ascii_lowercase();
        let hay = format!(
            "{} {} {} {}",
            obs.text,
            obs.excerpt.as_deref().unwrap_or(""),
            obs.tool.as_deref().unwrap_or(""),
            obs.event
        )
        .to_ascii_lowercase();
        if !hay.contains(&q) {
            return false;
        }
    }
    true
}

/// Filter observations from the store (legacy + segments).
pub fn filter_spool(project_dir: &Path, filter: &ObservationFilter) -> Vec<Observation> {
    let mut rows: Vec<Observation> = load_spool(project_dir)
        .into_iter()
        .filter(|obs| matches_filter(obs, filter))
        .collect();
    if filter.limit > 0 && rows.len() > filter.limit {
        rows.truncate(filter.limit);
    }
    rows
}

/// Find one observation by id: `obs_<line>` resolves ONLY against the
/// legacy spool (append-only, so the reference stays stable); `cap_…`
/// resolves against the segments. No cross-mapping is ever invented.
pub fn get_observation(project_dir: &Path, id: &str) -> Option<Observation> {
    match resolve(project_dir, id) {
        ObservationLookup::Found(obs) => Some(*obs),
        _ => None,
    }
}

/// Resolve a reference honestly: legacy `obs_N` beyond the current legacy
/// line count was cleared or rotated before the store stopped rewriting the
/// legacy file — report it unavailable rather than mapping it elsewhere.
pub fn resolve(project_dir: &Path, id: &str) -> ObservationLookup {
    if let Some(rest) = id.strip_prefix("obs_") {
        let Ok(line_no) = rest.parse::<usize>() else {
            return ObservationLookup::NotFound;
        };
        let path = legacy_spool_path(project_dir);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return ObservationLookup::Unavailable(format!(
                "{id} is a legacy line reference but the legacy spool is gone — the evidence was cleared before immutable segments shipped"
            ));
        };
        let lines: Vec<&str> = text.lines().collect();
        let Some(line) = lines.get(line_no.wrapping_sub(1)) else {
            return ObservationLookup::Unavailable(format!(
                "{id} predates a historical clear/rotation — the legacy spool now holds {} line(s)",
                lines.len()
            ));
        };
        return match parse_legacy_line(project_dir, line_no, line.trim()) {
            Some(obs) => ObservationLookup::Found(Box::new(obs)),
            None => ObservationLookup::Unavailable(format!(
                "{id} points at a line that is not a parseable legacy record (corrupt evidence, preserved on disk)"
            )),
        };
    }
    if id.starts_with("cap_") {
        return match archive_lookup(project_dir, id) {
            Some(record) => {
                ObservationLookup::Found(Box::new(observation_from_record(project_dir, &record)))
            }
            None => ObservationLookup::NotFound,
        };
    }
    ObservationLookup::NotFound
}

fn observation_from_record(project_dir: &Path, record: &ObservationRecord) -> Observation {
    let scope_status = foreign_scope_status(project_dir, &record.text);
    Observation {
        id: record.capture_id.clone(),
        line_no: 0,
        ts: record.ts.clone(),
        event: record.event.clone(),
        harness: record.harness.clone(),
        text: record.text.clone(),
        kind_hint: record.kind_hint.clone(),
        tool: record.tool.clone(),
        excerpt: record.excerpt.clone(),
        scope_status,
        origin: "segment".into(),
        capture_id: Some(record.capture_id.clone()),
        status: record.capture.status.clone(),
        conflict_with: record.capture.conflict_with.clone(),
        session_id: record.session_id.clone(),
        session_identity: record.session_identity.clone(),
        text_digest: Some(record.text_digest.clone()),
    }
}

/// Archive lookup: fetch a durable record by its stable capture id from any
/// segment (including replay sighting rows).
pub fn archive_lookup(project_dir: &Path, capture_id: &str) -> Option<ObservationRecord> {
    let dir = segments_dir(project_dir);
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in text.lines() {
            let Ok(record) = serde_json::from_str::<ObservationRecord>(line) else {
                continue;
            };
            if record.capture_id == capture_id {
                return Some(record);
            }
        }
    }
    None
}

/// The capture watermark: read the projection, or derive it from the
/// segments when the projection is absent/corrupt (it is rebuildable).
pub fn capture_watermark(project_dir: &Path) -> CaptureWatermark {
    let from_projection = std::fs::read_to_string(projection_path(project_dir))
        .ok()
        .and_then(|text| serde_json::from_str::<Projection>(&text).ok())
        .filter(|p| p.schema == PROJECTION_SCHEMA_V1)
        .map(|p| p.watermark);
    if let Some(watermark) = from_projection.filter(|w| w.last_capture_id.is_some()) {
        return watermark;
    }
    // Derive: last record across segments by timestamp.
    let mut watermark = CaptureWatermark::default();
    let dir = segments_dir(project_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return watermark;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in text.lines() {
            let Ok(record) = serde_json::from_str::<ObservationRecord>(line) else {
                continue;
            };
            if record.capture.status == "replay" {
                continue;
            }
            watermark.records += 1;
            // An undated record never advances the timestamped frontier.
            let newer = match (watermark.last_ts.as_deref(), record.ts.is_empty()) {
                (_, true) => false,
                (None, false) => true,
                (Some(prev), false) => record.ts.as_str() >= prev,
            };
            if newer {
                watermark.last_capture_id = Some(record.capture_id.clone());
                watermark.last_ts = Some(record.ts.clone());
                watermark.segment = file
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_string);
            }
        }
    }
    watermark
}

/// Session-scoped durable frontier: the honest tri-state of one session's
/// watermark. Bounded — reading it NEVER opens the segment.
#[derive(Debug, Clone)]
pub enum SessionWatermark {
    /// No segment and no frontier — the session never captured here. An
    /// absent frontier is reported, never invented.
    Absent,
    /// The durable frontier as of the last acknowledged append.
    Present(CaptureWatermark),
    /// A frontier exists but is unreadable/corrupt, or the segment exists
    /// without a frontier (pre-frontier store, or a crash between the
    /// durable append and the frontier write). The watermark is derivable
    /// only by scanning the segment — that scan is
    /// [`recover_session_frontier`], a recovery action, never a stop-time
    /// fallback.
    Unreadable(String),
}

/// Session-scoped capture watermark: a bounded read of the per-session
/// frontier file written under the capture lock after every durable append.
/// Never scans the segment; see [`SessionWatermark`] for the tri-state.
pub fn session_watermark(project_dir: &Path, harness: &str, session_id: &str) -> SessionWatermark {
    let file = segment_file(harness, Some(session_id));
    let segment = segments_dir(project_dir).join(&file);
    let path = frontier_path(&segment);
    if !path.is_file() {
        return if segment.is_file() {
            SessionWatermark::Unreadable(format!(
                "segment {file} exists without a frontier — recover with recover_session_frontier"
            ))
        } else {
            SessionWatermark::Absent
        };
    }
    let text = match bounded_metadata(&path, 16 * 1024) {
        Ok(text) => text,
        Err(err) => {
            return SessionWatermark::Unreadable(format!("frontier for {file} unreadable: {err}"));
        }
    };
    match serde_json::from_slice::<SessionFrontier>(&text) {
        Ok(frontier)
            if frontier.schema == FRONTIER_SCHEMA_V1
                && frontier.segment == file
                && (frontier.records == 0 || frontier.last_capture_id.is_some()) =>
        {
            SessionWatermark::Present(CaptureWatermark {
                segment: Some(if frontier.segment.is_empty() {
                    file
                } else {
                    frontier.segment
                }),
                records: frontier.records,
                last_capture_id: frontier.last_capture_id,
                last_ts: frontier.last_ts,
            })
        }
        _ => SessionWatermark::Unreadable(format!(
            "frontier for {file} is corrupt — recover with recover_session_frontier"
        )),
    }
}

fn bounded_metadata(path: &Path, budget: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > budget {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "metadata size exceeds bounded read budget",
        ));
    }
    let mut bytes = Vec::new();
    file.take(budget + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > budget {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "metadata grew beyond bounded read budget",
        ));
    }
    Ok(bytes)
}

/// Bounded current brief only; never scans retained evidence at a stop hook.
pub fn recent_brief(
    project_dir: &Path,
    harness: &str,
    session_id: Option<&str>,
    count: usize,
) -> std::io::Result<Vec<String>> {
    let path = projection_path(project_dir);
    let bytes = match bounded_metadata(&path, 128 * 1024) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let projection: Projection = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
    if projection.schema != PROJECTION_SCHEMA_V1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "current brief schema unavailable",
        ));
    }
    let mut out: Vec<_> = projection
        .recent
        .iter()
        .rev()
        .filter(|entry| entry.harness == harness && entry.session_id.as_deref() == session_id)
        .take(count.min(PROJECTION_RECENT))
        .map(|entry| {
            format!(
                "[capture {}] {}",
                entry.capture_id,
                entry
                    .excerpt
                    .as_deref()
                    .unwrap_or("captured evidence; see source")
            )
        })
        .collect();
    out.reverse();
    Ok(out)
}

/// Acknowledge exactly the frozen frontier after ingestion, without scanning or
/// deleting evidence and without acknowledging captures appended afterwards.
pub fn acknowledge_frontier(
    project_dir: &Path,
    harness: &str,
    session_id: &str,
    ingest_key: &str,
    watermark: &CaptureWatermark,
) -> std::io::Result<PathBuf> {
    let expected = segment_file(harness, Some(session_id));
    if watermark.segment.as_deref() != Some(expected.as_str())
        || watermark.last_capture_id.is_none()
        || ingest_key.is_empty()
        || ingest_key.contains(['/', '\\'])
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "capture acknowledgement binding invalid",
        ));
    }
    let segment = segments_dir(project_dir).join(&expected);
    let _lock =
        ResourceLock::acquire(segment.with_extension("lock")).map_err(std::io::Error::other)?;
    let path = local_store::root(project_dir)
        .join("spool/acknowledgements")
        .join(format!("{ingest_key}.json"));
    let receipt = serde_json::json!({"schema":"stateroot.observations.ack.v1","ingest_key":ingest_key,"harness":harness,"session_id":session_id,"watermark":watermark});
    if path.is_file() {
        let existing: Value = serde_json::from_slice(&bounded_metadata(&path, 16 * 1024)?)
            .map_err(std::io::Error::other)?;
        if existing != receipt {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "capture acknowledgement key conflict",
            ));
        }
    } else {
        safe_io::atomic_replace_json(&path, &receipt)?;
    }
    if let SessionWatermark::Present(current) = session_watermark(project_dir, harness, session_id)
    {
        if current.records == watermark.records
            && current.last_capture_id == watermark.last_capture_id
        {
            safe_io::atomic_replace_json(
                &seal_path(&segment),
                &serde_json::json!({"schema":"stateroot.observations.seal.v1","sealed_at":local_store::now_rfc3339(),"ingest_key":ingest_key,"segment":expected,"bytes":segment.metadata()?.len(),"records":watermark.records,"last_capture_id":watermark.last_capture_id,"last_ts":watermark.last_ts}),
            )?;
        }
    }
    Ok(path)
}

/// Recovery: derive one session's frontier by scanning its whole segment
/// and rewrite `<stem>.frontier.json`. This is the ONLY full-segment scan
/// for watermark purposes — an explicit recovery action, never a stop-time
/// fallback. Returns `None` when the session never captured (no segment).
pub fn recover_session_frontier(
    project_dir: &Path,
    harness: &str,
    session_id: &str,
) -> std::io::Result<Option<CaptureWatermark>> {
    let file = segment_file(harness, Some(session_id));
    let segment = segments_dir(project_dir).join(&file);
    let _lock =
        ResourceLock::acquire(segment.with_extension("lock")).map_err(std::io::Error::other)?;
    let text = match std::fs::read_to_string(&segment) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let parsed: Vec<(usize, Value)> = text
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            serde_json::from_str::<Value>(line)
                .ok()
                .map(|v| (idx + 1, v))
        })
        .collect();
    let mut frontier = frontier_from_parsed(&file, &parsed, None);
    frontier.updated_at = local_store::now_rfc3339();
    let value = serde_json::to_value(&frontier).map_err(std::io::Error::other)?;
    safe_io::atomic_replace_json(&frontier_path(&segment), &value)?;
    Ok(Some(CaptureWatermark {
        segment: Some(file),
        records: frontier.records,
        last_capture_id: frontier.last_capture_id,
        last_ts: frontier.last_ts,
    }))
}

/// List every segment with its seal state.
pub fn list_segments(project_dir: &Path) -> Vec<SegmentInfo> {
    let mut out = Vec::new();
    let dir = segments_dir(project_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    for path in files {
        let file = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let bytes = path.metadata().map(|m| m.len()).unwrap_or(0);
        let records = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|v| {
                        v.get("capture")
                            .and_then(|c| c.get("status"))
                            .and_then(|s| s.as_str())
                            .map(str::to_string)
                    })
                    .is_some_and(|status| status != "replay")
            })
            .count() as u64;
        out.push(SegmentInfo {
            sealed: current_seal(&path),
            file,
            path,
            records,
            bytes,
        });
    }
    out
}

fn corrupt_line(file: &str, line_no: usize, raw: &str, reason: &str) -> CorruptLine {
    CorruptLine {
        file: file.into(),
        line_no,
        raw: raw.to_string(),
        reason: reason.into(),
    }
}

/// Corruption/retention health of the store: captured, replayed,
/// conflicting, corrupt/torn, source-unavailable and pending-seal facts.
/// Malformed evidence is never dropped here — it is reported with its raw
/// bytes so an operator can inspect it. Files that cannot be READ at all
/// are surfaced as read failures — never silently treated as empty.
pub fn health(project_dir: &Path) -> SpoolHealth {
    let mut health = SpoolHealth {
        watermark: capture_watermark(project_dir),
        ..SpoolHealth::default()
    };

    // Legacy spool rows + corrupt lines.
    match std::fs::read_to_string(legacy_spool_path(project_dir)) {
        Ok(text) => {
            for (idx, line) in text.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(_) => health.legacy_rows += 1,
                    Err(err) => health.corrupt.push(corrupt_line(
                        "observations.jsonl",
                        idx + 1,
                        trimmed,
                        &format!("invalid JSON: {err}"),
                    )),
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => health.read_failures.push(UnreadableFile {
            file: "observations.jsonl".into(),
            error: err.to_string(),
        }),
    }

    // Segments.
    let dir = segments_dir(project_dir);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        files.sort();
        for path in files {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            health.segments += 1;
            if current_seal(&path) {
                health.sealed += 1;
            } else {
                health.pending_seal.push(name.clone());
            }
            // Frontier state: missing (recoverable), unreadable, or corrupt
            // are all surfaced — never conflated with an absent segment.
            let frontier = frontier_path(&path);
            let frontier_name = frontier
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if frontier.is_file() {
                match std::fs::read_to_string(&frontier) {
                    Ok(text) => {
                        let valid = serde_json::from_str::<SessionFrontier>(&text)
                            .is_ok_and(|f| f.schema == FRONTIER_SCHEMA_V1);
                        if !valid {
                            health.corrupt.push(corrupt_line(
                                &format!("segments/{frontier_name}"),
                                1,
                                text.trim(),
                                "corrupt frontier — recover_session_frontier rebuilds it",
                            ));
                        }
                    }
                    Err(err) => health.read_failures.push(UnreadableFile {
                        file: format!("segments/{frontier_name}"),
                        error: err.to_string(),
                    }),
                }
            } else {
                health.missing_frontier.push(name.clone());
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(err) => {
                    health.read_failures.push(UnreadableFile {
                        file: format!("segments/{name}"),
                        error: err.to_string(),
                    });
                    continue;
                }
            };
            for (idx, line) in text.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let rel = format!("segments/{name}");
                match serde_json::from_str::<Value>(trimmed) {
                    Err(err) => health.corrupt.push(corrupt_line(
                        &rel,
                        idx + 1,
                        trimmed,
                        &format!("invalid JSON (torn or corrupted write): {err}"),
                    )),
                    Ok(value) => {
                        if value.get("schema").and_then(|v| v.as_str())
                            != Some(OBSERVATION_SCHEMA_V2)
                        {
                            health.corrupt.push(corrupt_line(
                                &rel,
                                idx + 1,
                                trimmed,
                                "unrecognized schema",
                            ));
                            continue;
                        }
                        match value
                            .get("capture")
                            .and_then(|c| c.get("status"))
                            .and_then(|s| s.as_str())
                        {
                            Some("replay") => health.replays += 1,
                            Some("conflict") => health.conflicts += 1,
                            _ => health.records += 1,
                        }
                        let source_missing = value
                            .get("capture")
                            .and_then(|c| c.get("source_status"))
                            .and_then(|s| s.as_str())
                            != Some("complete")
                            || value.get("source").is_none();
                        if source_missing {
                            health.source_unavailable += 1;
                        }
                    }
                }
            }
        }
    }
    health
}

/// Read-only per-harness durable capture evidence for health surfaces (WS3
/// C1): the latest durable capture timestamp per harness from the immutable
/// v2 segments and the legacy v1 spool, plus every evidence file/line that
/// exists but could not be read or parsed.
///
/// This is the ONLY capture evidence health may promote on — the authored
/// episodic journal (checkpoints, hand-written notes) is never capture
/// evidence, so an ordinary `stateroot checkpoint` or a forged "via hook"
/// note can never read as a working integration. Unreadable/corrupt sources
/// are diagnosed here — an unreadable store is NOT the same as no captures.
#[derive(Debug, Clone, Default)]
pub struct CaptureTrail {
    /// Last durable capture ts per harness (max RFC3339 across the store).
    pub last_by_harness: std::collections::BTreeMap<String, String>,
    /// Evidence problems (bounded list): files that could not be read and
    /// lines that could not be parsed. Empty means the store read clean.
    pub diagnosed: Vec<String>,
}

/// Cap on recorded evidence problems; the store stays inspectable without
/// flooding a health document with one row per torn line.
const TRAIL_DIAGNOSED_MAX: usize = 16;

/// Compute the durable capture trail. Read-only and BOUNDED: per-session
/// durable frontier files (never the raw segment bodies) plus a tail-bounded
/// window of the legacy spool — health is a compact projection, not a raw
/// full-history inventory. Never writes, never rebuilds missing frontiers
/// (recovery is an explicit action elsewhere): evidence that cannot speak
/// for itself — an unreadable directory, an unsupported schema, an invalid
/// timestamp, omitted history — is DIAGNOSED, never repaired, migrated,
/// pruned, or silently treated as absent. The raw full inventory remains
/// available through the read-only spool health surface.
pub fn capture_trail(project_dir: &Path) -> CaptureTrail {
    let mut trail = CaptureTrail::default();
    let mut diagnosed_overflow = 0usize;
    let mut note = |trail: &mut CaptureTrail, msg: String| {
        if trail.diagnosed.len() < TRAIL_DIAGNOSED_MAX {
            trail.diagnosed.push(msg);
        } else {
            diagnosed_overflow += 1;
        }
    };
    let record = |trail: &mut CaptureTrail, harness: &str, ts: &str| {
        if harness.is_empty() || ts.is_empty() {
            return;
        }
        let slot = trail
            .last_by_harness
            .entry(harness.to_string())
            .or_default();
        if ts > slot.as_str() {
            *slot = ts.to_string();
        }
    };

    // Legacy v1 spool: a bounded TAIL window. The spool is append-only
    // history that is never rotated, so the recent rows are the evidence;
    // anything beyond the window is explicitly omitted, never scanned.
    match bounded_tail(&legacy_spool_path(project_dir), LEGACY_TRAIL_WINDOW) {
        Ok((text, truncated)) => {
            if truncated {
                note(
                    &mut trail,
                    format!(
                        "spool/observations.jsonl exceeds the {} KiB evidence window — only the most recent rows inspected; earlier history omitted (preserved on disk, never modified)",
                        LEGACY_TRAIL_WINDOW / 1024
                    ),
                );
            }
            for (idx, line) in text.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let window_line = || format!("spool/observations.jsonl line {}", idx + 1);
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(value) => {
                        // Typed recognition: a legacy row is capture evidence
                        // only when it is an untagged observation row with a
                        // harness, an event, and a valid RFC3339 timestamp.
                        // Authored prose, v2 rows, and schema-tagged rows of
                        // any other shape are diagnosed — never promoted.
                        match value.get("schema").and_then(|v| v.as_str()) {
                            None => {}
                            Some(OBSERVATION_SCHEMA_V2) => {
                                note(
                                    &mut trail,
                                    format!(
                                        "{}: v2 record in the legacy spool — read through its segment",
                                        window_line()
                                    ),
                                );
                                continue;
                            }
                            Some(_) => {
                                note(
                                    &mut trail,
                                    format!(
                                        "{}: unsupported schema — unrecognized record",
                                        window_line()
                                    ),
                                );
                                continue;
                            }
                        }
                        let harness = str_field(&value, "harness");
                        let event = str_field(&value, "event");
                        let ts = str_field(&value, "ts");
                        if harness.is_empty() || event.is_empty() {
                            note(
                                &mut trail,
                                format!(
                                    "{}: unrecognized legacy record (no harness/event) — not capture evidence",
                                    window_line()
                                ),
                            );
                            continue;
                        }
                        if ts.is_empty() || chrono::DateTime::parse_from_rfc3339(&ts).is_err() {
                            note(
                                &mut trail,
                                format!(
                                    "{}: legacy record without a valid RFC3339 timestamp — not capture evidence",
                                    window_line()
                                ),
                            );
                            continue;
                        }
                        record(&mut trail, &harness, &ts);
                    }
                    Err(err) => note(
                        &mut trail,
                        format!("{}: invalid JSON ({err})", window_line()),
                    ),
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => note(
            &mut trail,
            format!("spool/observations.jsonl unreadable: {err}"),
        ),
    }

    // Immutable v2 segments: each segment's small durable frontier carries
    // the record count and last captured ts. The raw segment body is NEVER
    // read here — the frontier is the durable boundary, and anything it
    // cannot vouch for is diagnosed instead of scanned.
    let dir = segments_dir(project_dir);
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            let mut files: Vec<PathBuf> = Vec::new();
            for (index, entry) in entries.enumerate() {
                if index >= TRAIL_DIRECTORY_ENTRY_MAX {
                    note(
                        &mut trail,
                        format!(
                            "spool/segments exceeds the {TRAIL_DIRECTORY_ENTRY_MAX} directory-entry budget — remaining entries omitted from compact health and preserved on disk"
                        ),
                    );
                    break;
                }
                match entry {
                    Ok(entry) => {
                        let path = entry.path();
                        if path.extension().is_some_and(|x| x == "jsonl") {
                            files.push(path);
                        }
                    }
                    Err(err) => note(
                        &mut trail,
                        format!("spool/segments directory entry unreadable: {err}"),
                    ),
                }
            }
            files.sort();
            if files.len() > SEGMENT_FRONTIER_SCAN_MAX {
                note(
                    &mut trail,
                    format!(
                        "spool/segments holds {} segment(s) — only the first {SEGMENT_FRONTIER_SCAN_MAX} frontiers inspected; the rest are omitted from compact health (read-only full inventory: spool health)",
                        files.len()
                    ),
                );
                files.truncate(SEGMENT_FRONTIER_SCAN_MAX);
            }
            for path in files {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                let Some((harness, _)) = name.split_once("__").filter(|(h, _)| !h.is_empty())
                else {
                    note(
                        &mut trail,
                        format!(
                            "spool/segments/{name}: unrecognized segment name (expected <harness>__<session>.jsonl) — evidence omitted"
                        ),
                    );
                    continue;
                };
                let frontier = match bounded_metadata(&frontier_path(&path), 16 * 1024) {
                    Ok(bytes) => bytes,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        note(
                            &mut trail,
                            format!(
                                "spool/segments/{name}: no durable frontier — capture evidence omitted (recoverable via recover_session_frontier)"
                            ),
                        );
                        continue;
                    }
                    Err(err) => {
                        note(
                            &mut trail,
                            format!("spool/segments/{name}: frontier unreadable: {err}"),
                        );
                        continue;
                    }
                };
                let frontier: SessionFrontier = match serde_json::from_slice(&frontier) {
                    Ok(frontier) => frontier,
                    Err(err) => {
                        note(
                            &mut trail,
                            format!("spool/segments/{name}: frontier not parseable ({err})"),
                        );
                        continue;
                    }
                };
                if frontier.schema != FRONTIER_SCHEMA_V1 {
                    note(
                        &mut trail,
                        format!(
                            "spool/segments/{name}: unsupported frontier schema — evidence omitted"
                        ),
                    );
                    continue;
                }
                let claimed = if frontier.segment.is_empty() {
                    name.as_str()
                } else {
                    frontier.segment.as_str()
                };
                if claimed != name {
                    note(
                        &mut trail,
                        format!(
                            "spool/segments/{name}: frontier claims segment {claimed} — evidence omitted"
                        ),
                    );
                    continue;
                }
                // A replay-only/empty session holds no event evidence.
                if frontier.records == 0 {
                    continue;
                }
                let Some(ts) = frontier.last_ts.as_deref().filter(|ts| !ts.is_empty()) else {
                    note(
                        &mut trail,
                        format!(
                            "spool/segments/{name}: frontier records {} event(s) but has no last timestamp — evidence omitted",
                            frontier.records
                        ),
                    );
                    continue;
                };
                if chrono::DateTime::parse_from_rfc3339(ts).is_err() {
                    note(
                        &mut trail,
                        format!(
                            "spool/segments/{name}: frontier timestamp is not valid RFC3339 — evidence omitted"
                        ),
                    );
                    continue;
                }
                record(&mut trail, harness, ts);
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => note(&mut trail, format!("spool/segments unreadable: {err}")),
    }
    if diagnosed_overflow > 0 {
        trail.diagnosed.push(format!(
            "… and {diagnosed_overflow} more evidence problem(s)"
        ));
    }
    trail
}

/// Bounded tail window inspected from the legacy v1 spool for health
/// evidence — the full file is append-only history and is never scanned.
const LEGACY_TRAIL_WINDOW: u64 = 256 * 1024;

/// Cap on per-session frontier files inspected for one trail; overflow is
/// explicitly diagnosed as omitted (compact health, not a full inventory).
const SEGMENT_FRONTIER_SCAN_MAX: usize = 1024;
/// Bound names inspected as well as frontier bytes; non-segment entries
/// must not turn the compact health projection into an unlimited listing.
const TRAIL_DIRECTORY_ENTRY_MAX: usize = SEGMENT_FRONTIER_SCAN_MAX * 4;

/// Read at most the LAST `budget` bytes of `path` as UTF-8 text. When the
/// file is larger, the first partial line of the window is dropped and the
/// result is flagged truncated — the caller diagnoses the omitted history.
fn bounded_tail(path: &Path, budget: u64) -> std::io::Result<(String, bool)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let truncated = len > budget;
    if truncated {
        file.seek(SeekFrom::Start(len - budget))?;
    }
    let mut bytes = Vec::new();
    file.take(budget).read_to_end(&mut bytes)?;
    let mut text = String::from_utf8(bytes)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    if truncated {
        match text.find('\n') {
            Some(pos) => {
                text.drain(..=pos);
            }
            None => text.clear(),
        }
    }
    Ok((text, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_ack_replays_seal_recovery_without_acknowledging_later_capture() {
        let dir = project();
        capture(dir.path(), request("first captured evidence")).unwrap();
        let first = match session_watermark(dir.path(), "cursor", "s-1") {
            SessionWatermark::Present(value) => value,
            other => panic!("{other:?}"),
        };
        let receipt =
            acknowledge_frontier(dir.path(), "cursor", "s-1", "ack-first", &first).unwrap();
        let segment = segments_dir(dir.path()).join(first.segment.as_ref().unwrap());
        assert!(current_seal(&segment));
        std::fs::remove_file(seal_path(&segment)).unwrap(); // Crash after receipt, before seal.
        acknowledge_frontier(dir.path(), "cursor", "s-1", "ack-first", &first).unwrap();
        assert!(current_seal(&segment));
        capture(dir.path(), request("later captured evidence")).unwrap();
        assert!(!current_seal(&segment));
        acknowledge_frontier(dir.path(), "cursor", "s-1", "ack-first", &first).unwrap();
        let stored: Value = serde_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap();
        assert_eq!(stored["watermark"]["records"], 1);
        assert!(
            !current_seal(&segment),
            "later capture remains unacknowledged by old receipt"
        );
        assert!(std::fs::read_to_string(segment)
            .unwrap()
            .contains("later captured evidence"));
    }

    #[test]
    fn oversized_frontier_and_current_brief_are_honestly_unavailable() {
        let dir = project();
        capture(dir.path(), request("retained evidence")).unwrap();
        let segment = segments_dir(dir.path()).join("cursor__s-1.jsonl");
        std::fs::write(frontier_path(&segment), vec![b'x'; 17 * 1024]).unwrap();
        assert!(
            matches!(session_watermark(dir.path(),"cursor","s-1"),SessionWatermark::Unreadable(reason) if reason.contains("bounded"))
        );
        std::fs::write(projection_path(dir.path()), vec![b'x'; 129 * 1024]).unwrap();
        assert!(recent_brief(dir.path(), "cursor", Some("s-1"), 5).is_err());
        assert!(std::fs::read_to_string(segment)
            .unwrap()
            .contains("retained evidence"));
    }

    #[test]
    fn current_brief_never_borrows_another_session() {
        let dir = project();
        let mut first = request("first");
        first.excerpt = Some("first request".into());
        capture(dir.path(), first).unwrap();
        let mut second = request("second");
        second.session_id = Some("s-2".into());
        second.excerpt = Some("second request".into());
        capture(dir.path(), second).unwrap();
        let brief = recent_brief(dir.path(), "cursor", Some("s-1"), 5).unwrap();
        assert_eq!(brief.len(), 1);
        assert!(brief[0].contains("first request"));
        assert!(!brief[0].contains("second request"));
    }
    use serde_json::json;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tmpdir");
        local_store::init_skeleton(dir.path(), "p1", "demo", "default").unwrap();
        dir
    }

    fn request(text: &str) -> CaptureRequest {
        CaptureRequest {
            ts: "2026-10-08T06:00:00Z".into(),
            event: "post_tool_use".into(),
            harness: "cursor".into(),
            session_id: Some("s-1".into()),
            session_identity: "native".into(),
            event_identity: None,
            text: text.into(),
            kind_hint: None,
            tool: Some("Bash".into()),
            excerpt: None,
            source: Some(json!({"tool_name": "Bash"})),
            source_status: "complete".into(),
            meta: None,
        }
    }

    #[test]
    fn capture_trail_reads_durable_evidence_not_prose() {
        let dir = project();
        // Empty store: absent, and nothing diagnosed.
        let trail = capture_trail(dir.path());
        assert!(trail.last_by_harness.is_empty());
        assert!(trail.diagnosed.is_empty());

        // A real durable capture lands per harness.
        capture(dir.path(), request("hook payload")).unwrap();
        let trail = capture_trail(dir.path());
        assert_eq!(
            trail.last_by_harness.get("cursor").map(String::as_str),
            Some("2026-10-08T06:00:00Z"),
            "{:?}",
            trail.last_by_harness
        );
        assert!(trail.diagnosed.is_empty(), "{:?}", trail.diagnosed);

        // The compact trail reads the durable frontier, never the raw
        // segment body: a torn append BEYOND the frontier does not move the
        // durable evidence and is not re-inventoried here (the read-only
        // raw inventory is spool health's job — see `health`).
        let segment = segments_dir(dir.path()).join("cursor__s-1.jsonl");
        let mut text = std::fs::read_to_string(&segment).unwrap();
        text.push_str("{torn\n");
        std::fs::write(&segment, text).unwrap();
        let trail = capture_trail(dir.path());
        assert_eq!(
            trail.last_by_harness.get("cursor").map(String::as_str),
            Some("2026-10-08T06:00:00Z"),
            "the durable frontier still carries the capture: {:?}",
            trail.last_by_harness
        );
        // …while the read-only raw inventory still sees the tear.
        let health = health(dir.path());
        assert!(
            health
                .corrupt
                .iter()
                .any(|c| c.file.contains("cursor__s-1.jsonl")),
            "{:?}",
            health.corrupt
        );
    }

    #[test]
    fn capture_trail_counts_legacy_rows_and_skips_replays() {
        let dir = project();
        let spool = legacy_spool_path(dir.path());
        std::fs::create_dir_all(spool.parent().unwrap()).unwrap();
        std::fs::write(
            &spool,
            "{\"ts\":\"2026-01-01T00:00:00Z\",\"harness\":\"kimi-code\",\"event\":\"stop\",\"text\":\"legacy\"}\nnot json\n",
        )
        .unwrap();
        let trail = capture_trail(dir.path());
        assert_eq!(
            trail.last_by_harness.get("kimi-code").map(String::as_str),
            Some("2026-01-01T00:00:00Z")
        );
        assert!(
            trail
                .diagnosed
                .iter()
                .any(|d| d.contains("observations.jsonl")),
            "{:?}",
            trail.diagnosed
        );

        // A replay sighting never moves a harness's last capture.
        let mut replay = request("dup");
        replay.event_identity = Some(("tool_use_id".into(), "t-1".into()));
        capture(dir.path(), replay.clone()).unwrap();
        let mut again = replay.clone();
        again.ts = "2026-10-08T07:00:00Z".into();
        let outcome = capture(dir.path(), again).unwrap();
        assert_eq!(outcome.status, CaptureStatus::Replay);
        let trail = capture_trail(dir.path());
        assert_eq!(
            trail.last_by_harness.get("cursor").map(String::as_str),
            Some("2026-10-08T06:00:00Z"),
            "replay must not advance the trail: {:?}",
            trail.last_by_harness
        );
    }

    #[test]
    fn capture_trail_never_reads_raw_segment_bodies() {
        // The compact trail is a frontier read: raw segment bodies can be
        // complete garbage and the durable frontier evidence still flows —
        // proof there is no full-history scan left in this path.
        let dir = project();
        capture(dir.path(), request("durable")).unwrap();
        let segment = segments_dir(dir.path()).join("cursor__s-1.jsonl");
        std::fs::write(&segment, "{total garbage\n").unwrap();
        let trail = capture_trail(dir.path());
        assert_eq!(
            trail.last_by_harness.get("cursor").map(String::as_str),
            Some("2026-10-08T06:00:00Z"),
            "{:?}",
            trail.last_by_harness
        );
        assert!(
            trail.diagnosed.is_empty(),
            "frontier evidence is complete — nothing to diagnose: {:?}",
            trail.diagnosed
        );
    }

    #[test]
    fn capture_trail_diagnoses_missing_and_broken_frontiers() {
        let dir = project();
        let segments = segments_dir(dir.path());
        std::fs::create_dir_all(&segments).unwrap();
        // A segment with NO frontier: evidence omitted, diagnosed, never
        // repaired or scanned (recovery is recover_session_frontier).
        std::fs::write(segments.join("kimi__s-7.jsonl"), "{}\n").unwrap();
        let trail = capture_trail(dir.path());
        assert!(!trail.last_by_harness.contains_key("kimi"));
        assert!(
            trail
                .diagnosed
                .iter()
                .any(|d| d.contains("kimi__s-7.jsonl") && d.contains("no durable frontier")),
            "{:?}",
            trail.diagnosed
        );

        // A corrupt frontier: diagnosed, not evidence.
        std::fs::write(segments.join("cursor__s-2.jsonl"), "{}\n").unwrap();
        std::fs::write(segments.join("cursor__s-2.frontier.json"), "{not json").unwrap();
        let trail = capture_trail(dir.path());
        assert!(!trail.last_by_harness.contains_key("cursor"));
        assert!(
            trail
                .diagnosed
                .iter()
                .any(|d| d.contains("cursor__s-2.jsonl") && d.contains("not parseable")),
            "{:?}",
            trail.diagnosed
        );

        // An unsupported frontier schema: diagnosed, not evidence.
        std::fs::write(segments.join("pi__s-1.jsonl"), "{}\n").unwrap();
        std::fs::write(
            segments.join("pi__s-1.frontier.json"),
            serde_json::to_string(&json!({
                "schema": "stateroot.observations.frontier.v0",
                "segment": "pi__s-1.jsonl",
                "records": 3,
                "last_ts": "2026-10-08T06:00:00Z"
            }))
            .unwrap(),
        )
        .unwrap();
        let trail = capture_trail(dir.path());
        assert!(!trail.last_by_harness.contains_key("pi"));
        assert!(
            trail
                .diagnosed
                .iter()
                .any(|d| d.contains("pi__s-1.jsonl") && d.contains("unsupported frontier schema")),
            "{:?}",
            trail.diagnosed
        );
    }

    #[test]
    fn capture_trail_validates_frontier_identity_and_timestamps() {
        let dir = project();
        let segments = segments_dir(dir.path());
        std::fs::create_dir_all(&segments).unwrap();
        // Frontier claiming a different segment file: omitted.
        std::fs::write(segments.join("cursor__s-3.jsonl"), "{}\n").unwrap();
        std::fs::write(
            segments.join("cursor__s-3.frontier.json"),
            serde_json::to_string(&json!({
                "schema": FRONTIER_SCHEMA_V1,
                "segment": "cursor__other.jsonl",
                "records": 1,
                "last_ts": "2026-10-08T06:00:00Z"
            }))
            .unwrap(),
        )
        .unwrap();
        // Frontier with records but an invalid timestamp: omitted.
        std::fs::write(segments.join("kimi__s-8.jsonl"), "{}\n").unwrap();
        std::fs::write(
            segments.join("kimi__s-8.frontier.json"),
            serde_json::to_string(&json!({
                "schema": FRONTIER_SCHEMA_V1,
                "segment": "kimi__s-8.jsonl",
                "records": 2,
                "last_ts": "yesterday-ish"
            }))
            .unwrap(),
        )
        .unwrap();
        // Frontier with records but NO timestamp: omitted.
        std::fs::write(segments.join("pi__s-2.jsonl"), "{}\n").unwrap();
        std::fs::write(
            segments.join("pi__s-2.frontier.json"),
            serde_json::to_string(&json!({
                "schema": FRONTIER_SCHEMA_V1,
                "segment": "pi__s-2.jsonl",
                "records": 1
            }))
            .unwrap(),
        )
        .unwrap();
        // A replay-only frontier (records 0) is honest absence, not a problem.
        std::fs::write(segments.join("codex__s-1.jsonl"), "{}\n").unwrap();
        std::fs::write(
            segments.join("codex__s-1.frontier.json"),
            serde_json::to_string(&json!({
                "schema": FRONTIER_SCHEMA_V1,
                "segment": "codex__s-1.jsonl",
                "records": 0
            }))
            .unwrap(),
        )
        .unwrap();
        let trail = capture_trail(dir.path());
        assert!(
            trail.last_by_harness.is_empty(),
            "{:?}",
            trail.last_by_harness
        );
        assert!(
            trail
                .diagnosed
                .iter()
                .any(|d| d.contains("cursor__s-3.jsonl") && d.contains("claims segment")),
            "{:?}",
            trail.diagnosed
        );
        assert!(
            trail
                .diagnosed
                .iter()
                .any(|d| d.contains("kimi__s-8.jsonl") && d.contains("not valid RFC3339")),
            "{:?}",
            trail.diagnosed
        );
        assert!(
            trail
                .diagnosed
                .iter()
                .any(|d| d.contains("pi__s-2.jsonl") && d.contains("no last timestamp")),
            "{:?}",
            trail.diagnosed
        );
        assert!(
            !trail.diagnosed.iter().any(|d| d.contains("codex__s-1")),
            "replay-only frontier is not a problem: {:?}",
            trail.diagnosed
        );
    }

    #[test]
    fn capture_trail_diagnoses_an_unreadable_segments_directory() {
        // `segments` existing as a FILE makes read_dir fail with
        // NotADirectory — an unreadable directory is diagnosed, distinct
        // from an absent one (no spool at all says nothing).
        let dir = project();
        let spool = dir.path().join(".stateroot/spool");
        std::fs::create_dir_all(&spool).unwrap();
        std::fs::write(spool.join("segments"), "not a directory").unwrap();
        let trail = capture_trail(dir.path());
        assert!(trail.last_by_harness.is_empty());
        assert!(
            trail
                .diagnosed
                .iter()
                .any(|d| d.contains("spool/segments unreadable")),
            "{:?}",
            trail.diagnosed
        );
    }

    #[test]
    fn capture_trail_legacy_rows_are_typed_and_bounded() {
        let dir = project();
        let spool = legacy_spool_path(dir.path());
        std::fs::create_dir_all(spool.parent().unwrap()).unwrap();
        std::fs::write(
            &spool,
            concat!(
                // Real legacy capture row: evidence.
                "{\"ts\":\"2026-01-01T00:00:00Z\",\"harness\":\"kimi-code\",\"event\":\"stop\",\"text\":\"legacy\"}\n",
                // Authored prose carrying harness/ts but no event: NOT
                // capture evidence — diagnosed unrecognized.
                "{\"ts\":\"2026-01-02T00:00:00Z\",\"harness\":\"cursor\",\"note\":\"checkpoint via cursor hook\"}\n",
                // Invalid timestamp: NOT evidence.
                "{\"ts\":\"last tuesday\",\"harness\":\"pi\",\"event\":\"stop\"}\n",
                // A v2-tagged row in the legacy file: diagnosed, read via segments.
                "{\"schema\":\"stateroot.observation.v2\",\"ts\":\"2026-01-03T00:00:00Z\",\"harness\":\"codex\",\"event\":\"stop\"}\n",
                // Any other schema tag: unsupported.
                "{\"schema\":\"something.else.v9\",\"ts\":\"2026-01-04T00:00:00Z\",\"harness\":\"zero\",\"event\":\"stop\"}\n",
            ),
        )
        .unwrap();
        let trail = capture_trail(dir.path());
        assert_eq!(
            trail.last_by_harness.len(),
            1,
            "only the typed legacy capture counts: {:?}",
            trail.last_by_harness
        );
        assert_eq!(
            trail.last_by_harness.get("kimi-code").map(String::as_str),
            Some("2026-01-01T00:00:00Z")
        );
        for needle in [
            "no harness/event",
            "valid RFC3339",
            "v2 record in the legacy spool",
            "unsupported schema",
        ] {
            assert!(
                trail.diagnosed.iter().any(|d| d.contains(needle)),
                "missing diagnosis {needle}: {:?}",
                trail.diagnosed
            );
        }
    }

    #[test]
    fn capture_trail_legacy_spool_reads_a_bounded_tail_window() {
        // Oversized legacy spool: the OLDEST rows (including an authored
        // harness/ts pair) fall outside the evidence window — omitted with
        // an explicit diagnosis, and the recent rows still produce evidence.
        let dir = project();
        let spool = legacy_spool_path(dir.path());
        std::fs::create_dir_all(spool.parent().unwrap()).unwrap();
        let mut body = String::new();
        body.push_str(
            "{\"ts\":\"2020-01-01T00:00:00Z\",\"harness\":\"ancient\",\"event\":\"stop\"}\n",
        );
        while body.len() < LEGACY_TRAIL_WINDOW as usize + 4096 {
            body.push_str(&format!("{{\"ts\":\"2021-01-01T00:00:00Z\",\"harness\":\"filler\",\"event\":\"stop\",\"text\":\"{}\"}}\n", "x".repeat(900)));
        }
        body.push_str(
            "{\"ts\":\"2026-02-02T00:00:00Z\",\"harness\":\"recent\",\"event\":\"stop\"}\n",
        );
        std::fs::write(&spool, body).unwrap();
        let trail = capture_trail(dir.path());
        assert_eq!(
            trail.last_by_harness.get("recent").map(String::as_str),
            Some("2026-02-02T00:00:00Z"),
            "{:?}",
            trail.last_by_harness
        );
        assert!(
            !trail.last_by_harness.contains_key("ancient"),
            "history beyond the bounded window is omitted: {:?}",
            trail.last_by_harness
        );
        assert!(
            trail
                .diagnosed
                .iter()
                .any(|d| d.contains("evidence window") && d.contains("omitted")),
            "{:?}",
            trail.diagnosed
        );
    }

    #[test]
    fn capture_trail_diagnoses_segment_overflow_and_bad_names() {
        let dir = project();
        let segments = segments_dir(dir.path());
        std::fs::create_dir_all(&segments).unwrap();
        // A segment file whose name does not encode a harness.
        std::fs::write(segments.join("stray.jsonl"), "{}\n").unwrap();
        let trail = capture_trail(dir.path());
        assert!(
            trail
                .diagnosed
                .iter()
                .any(|d| d.contains("stray.jsonl") && d.contains("unrecognized segment name")),
            "{:?}",
            trail.diagnosed
        );
        // More segments than the compact scan budget: the overflow is
        // diagnosed as omitted, not silently dropped.
        for i in 0..=SEGMENT_FRONTIER_SCAN_MAX {
            std::fs::write(segments.join(format!("overflow__s-{i}.jsonl")), "{}\n").unwrap();
        }
        let trail = capture_trail(dir.path());
        assert!(
            trail
                .diagnosed
                .iter()
                .any(|d| d.contains("frontiers inspected") && d.contains("omitted")),
            "{:?}",
            trail.diagnosed
        );
    }

    #[test]
    fn capture_trail_bounds_directory_entries_not_only_frontier_reads() {
        let dir = project();
        let segments = segments_dir(dir.path());
        std::fs::create_dir_all(&segments).unwrap();
        for index in 0..=TRAIL_DIRECTORY_ENTRY_MAX {
            std::fs::write(segments.join(format!("unrelated-{index}.tmp")), []).unwrap();
        }
        let trail = capture_trail(dir.path());
        assert!(trail.last_by_harness.is_empty());
        assert!(trail.diagnosed.iter().any(|message| {
            message.contains("directory-entry budget") && message.contains("omitted")
        }));
        assert_eq!(
            std::fs::read_dir(&segments).unwrap().count(),
            TRAIL_DIRECTORY_ENTRY_MAX + 1
        );
    }

    #[test]
    fn load_and_filter_spool_rows() {
        let dir = project();
        let spool = legacy_spool_path(dir.path());
        std::fs::create_dir_all(spool.parent().unwrap()).unwrap();
        std::fs::write(
            &spool,
            format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "ts": "2026-08-17T10:00:00Z",
                    "event": "user_prompt_submit",
                    "harness": "cursor",
                    "text": "fix the importer",
                    "kind_hint": "correction",
                }))
                .unwrap()
            ),
        )
        .unwrap();
        let rows = filter_spool(
            dir.path(),
            &ObservationFilter {
                kind: Some("correction".into()),
                harness: Some("cursor".into()),
                limit: 10,
                ..ObservationFilter::default()
            },
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "obs_1");
        assert!(get_observation(dir.path(), "obs_1").is_some());
    }

    #[test]
    fn capture_persists_full_text_and_source_without_truncation() {
        let dir = project();
        let long = format!("{}SENTINEL", "x".repeat(5000));
        let outcome = capture(dir.path(), request(&long)).expect("capture");
        assert_eq!(outcome.status, CaptureStatus::Captured);
        let record = archive_lookup(dir.path(), &outcome.record.capture_id).expect("archived");
        assert!(record.text.ends_with("SENTINEL"), "full body retained");
        assert_eq!(record.text_len, long.len());
        assert_eq!(record.text_digest, sha256_hex(&long));
        assert_eq!(
            record.source.as_ref().map(|s| s.payload.clone()),
            Some(json!({"tool_name": "Bash"}))
        );
    }

    #[test]
    fn same_identity_replay_yields_one_event_and_a_sighting() {
        let dir = project();
        let mut req = request("hello");
        req.event_identity = Some(("tool_use_id".into(), "tu-1".into()));
        let first = capture(dir.path(), req.clone()).expect("first");
        assert_eq!(first.status, CaptureStatus::Captured);
        let second = capture(dir.path(), req).expect("replay");
        assert_eq!(second.status, CaptureStatus::Replay);
        assert_eq!(
            second.related.as_deref(),
            Some(first.record.capture_id.as_str())
        );
        let rows = load_spool(dir.path());
        assert_eq!(rows.len(), 1, "replay never fabricates a second occurrence");
        let health = health(dir.path());
        assert_eq!(health.records, 1);
        assert_eq!(health.replays, 1);
    }

    #[test]
    fn distinct_events_with_identical_text_are_never_collapsed() {
        let dir = project();
        for idx in 0..8 {
            let mut req = request("identical text");
            req.event_identity = Some(("tool_use_id".into(), format!("tu-{idx}")));
            capture(dir.path(), req).expect("capture");
        }
        // …and with no host identity at all, same-text events still stand alone.
        for _ in 0..2 {
            capture(dir.path(), request("identical text")).expect("no identity");
        }
        assert_eq!(load_spool(dir.path()).len(), 10);
    }

    #[test]
    fn same_identity_different_content_is_an_explicit_conflict() {
        let dir = project();
        let mut req = request("original body");
        req.event_identity = Some(("tool_use_id".into(), "tu-9".into()));
        let first = capture(dir.path(), req.clone()).expect("first");
        req.text = "rewritten body".into();
        let second = capture(dir.path(), req).expect("conflict");
        assert_eq!(second.status, CaptureStatus::Conflict);
        assert_eq!(
            second.related.as_deref(),
            Some(first.record.capture_id.as_str())
        );
        // Both bodies retained.
        let original = archive_lookup(dir.path(), &first.record.capture_id).expect("original");
        let rewrite = archive_lookup(dir.path(), &second.record.capture_id).expect("rewrite");
        assert_eq!(original.text, "original body");
        assert_eq!(rewrite.text, "rewritten body");
        assert_eq!(health(dir.path()).conflicts, 1);
    }

    #[test]
    fn torn_tail_is_terminated_and_reported_never_accepted() {
        let dir = project();
        let outcome = capture(dir.path(), request("durable")).expect("capture");
        // Simulate a killed writer: partial JSON without a trailing newline.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&outcome.segment)
            .expect("open");
        file.write_all(b"{\"schema\":\"stateroot.observation.v2\",\"cap")
            .expect("torn write");
        drop(file);
        let next = capture(dir.path(), request("next")).expect("next capture");
        assert_eq!(next.status, CaptureStatus::Captured);
        let rows = load_spool(dir.path());
        assert_eq!(rows.len(), 2, "torn line is not a record");
        let after = health(dir.path());
        assert_eq!(after.corrupt.len(), 1);
        assert!(after.corrupt[0].reason.contains("invalid JSON"));
        assert!(after.corrupt[0].raw.contains("\"cap"));
    }

    #[test]
    fn watermark_tracks_the_durable_frontier() {
        let dir = project();
        let first = capture(dir.path(), request("a")).expect("a");
        let watermark = capture_watermark(dir.path());
        assert_eq!(watermark.records, 1);
        assert_eq!(
            watermark.last_capture_id.as_deref(),
            Some(first.record.capture_id.as_str())
        );
        // Projection removed → derived from segments (rebuildable).
        std::fs::remove_file(projection_path(dir.path())).expect("drop projection");
        let derived = capture_watermark(dir.path());
        assert_eq!(derived.records, 1);
        assert_eq!(
            derived.last_capture_id.as_deref(),
            Some(first.record.capture_id.as_str())
        );
    }

    #[test]
    fn seal_marks_a_session_segment_and_health_reports_pending() {
        let dir = project();
        capture(dir.path(), request("work")).expect("capture");
        let before = health(dir.path());
        assert_eq!(before.pending_seal.len(), 1);
        let marker = seal_session(dir.path(), "cursor", Some("s-1"))
            .expect("seal")
            .expect("segment existed");
        assert!(marker.is_file());
        let after = health(dir.path());
        assert_eq!(after.sealed, 1);
        assert!(after.pending_seal.is_empty());
        // Unknown session: nothing to seal, never an invention.
        assert!(seal_session(dir.path(), "cursor", Some("nope"))
            .expect("ok")
            .is_none());
    }

    #[test]
    fn legacy_reference_beyond_the_file_reports_unavailable() {
        let dir = project();
        let spool = legacy_spool_path(dir.path());
        std::fs::create_dir_all(spool.parent().unwrap()).unwrap();
        std::fs::write(
            &spool,
            "{\"ts\":\"t\",\"event\":\"e\",\"harness\":\"h\",\"text\":\"one\"}\n",
        )
        .unwrap();
        match resolve(dir.path(), "obs_1") {
            ObservationLookup::Found(obs) => assert_eq!(obs.text, "one"),
            _ => panic!("obs_1 must resolve"),
        }
        match resolve(dir.path(), "obs_5") {
            ObservationLookup::Unavailable(reason) => {
                assert!(reason.contains("clear"), "{reason}")
            }
            _ => panic!("obs_5 must report unavailable, never an invented mapping"),
        }
        match resolve(dir.path(), "cap_missing") {
            ObservationLookup::NotFound => {}
            _ => panic!("unknown capture id"),
        }
    }

    #[test]
    fn capture_ids_are_unique_across_rapid_writes() {
        let dir = project();
        let mut ids = std::collections::HashSet::new();
        for _ in 0..50 {
            let outcome = capture(dir.path(), request("x")).expect("capture");
            assert!(ids.insert(outcome.record.capture_id.clone()));
        }
    }

    #[test]
    fn pre_and_post_events_sharing_a_tool_id_are_distinct_events() {
        let dir = project();
        let mut pre = request("call the tool");
        pre.event = "pre_tool_use".into();
        pre.event_identity = Some(("tool_use_id".into(), "tu-shared".into()));
        let mut post = request("tool replied");
        post.event = "post_tool_use".into();
        post.event_identity = Some(("tool_use_id".into(), "tu-shared".into()));
        let first = capture(dir.path(), pre).expect("pre");
        let second = capture(dir.path(), post).expect("post");
        assert_eq!(first.status, CaptureStatus::Captured);
        assert_eq!(
            second.status,
            CaptureStatus::Captured,
            "a post_tool_use is never a replay/conflict of its pre_tool_use"
        );
        assert_eq!(load_spool(dir.path()).len(), 2);
        // The true replay of the SAME event + identity still dedups.
        let mut post_again = request("tool replied");
        post_again.event = "post_tool_use".into();
        post_again.event_identity = Some(("tool_use_id".into(), "tu-shared".into()));
        post_again.source = Some(json!({"tool_name": "Bash"}));
        let replay = capture(dir.path(), post_again).expect("replay");
        assert_eq!(replay.status, CaptureStatus::Replay);
        assert_eq!(load_spool(dir.path()).len(), 2);
    }

    #[test]
    fn session_watermark_scopes_to_one_session() {
        let dir = project();
        capture(dir.path(), request("a")).expect("a");
        let mut other = request("b");
        other.session_id = Some("s-2".into());
        capture(dir.path(), other).expect("b");
        let s1 = match session_watermark(dir.path(), "cursor", "s-1") {
            SessionWatermark::Present(watermark) => watermark,
            other => panic!("s-1 frontier must be present: {other:?}"),
        };
        assert_eq!(s1.records, 1);
        let s2 = match session_watermark(dir.path(), "cursor", "s-2") {
            SessionWatermark::Present(watermark) => watermark,
            other => panic!("s-2 frontier must be present: {other:?}"),
        };
        assert_eq!(s2.records, 1);
        assert_ne!(s1.last_capture_id, s2.last_capture_id);
        assert!(matches!(
            session_watermark(dir.path(), "cursor", "never"),
            SessionWatermark::Absent
        ));
    }

    #[test]
    fn corrupt_legacy_line_is_exposed_not_silently_skipped() {
        let dir = project();
        let spool = legacy_spool_path(dir.path());
        std::fs::create_dir_all(spool.parent().unwrap()).unwrap();
        std::fs::write(
            &spool,
            "{\"ts\":\"t\",\"event\":\"e\",\"harness\":\"h\",\"text\":\"ok\"}\n{not json\n",
        )
        .unwrap();
        let health = health(dir.path());
        assert_eq!(health.legacy_rows, 1);
        assert_eq!(health.corrupt.len(), 1);
        assert_eq!(health.corrupt[0].line_no, 2);
        assert_eq!(health.corrupt[0].raw, "{not json");
    }

    /// A planted v2 record line for lineage/digest fixtures. `lineage: None`
    /// writes a null lineage (an unlineaged record, never proven equal).
    fn planted_record(
        capture_id: &str,
        identity: &str,
        text: &str,
        lineage: Option<&str>,
    ) -> String {
        let source_payload = json!({"tool_name": "Bash"});
        let canonical = serde_json::to_string(&source_payload).unwrap();
        serde_json::json!({
            "schema": OBSERVATION_SCHEMA_V2,
            "capture_id": capture_id,
            "ts": "2026-10-08T06:00:00Z",
            "event": "post_tool_use",
            "harness": "cursor",
            "lineage": lineage,
            "session_id": "s-1",
            "session_identity": "native",
            "event_identity": {"status": "native", "field": "tool_use_id", "value": identity},
            "text": text,
            "text_digest": sha256_hex(text),
            "text_len": text.len(),
            "capture": {"status": "captured", "source_status": "complete"},
            "source": {"payload": source_payload, "digest": sha256_hex(&canonical), "bytes": canonical.len()},
        })
        .to_string()
    }

    fn plant_segment_line(dir: &Path, line: &str) {
        let segments = segments_dir(dir);
        std::fs::create_dir_all(&segments).expect("segments dir");
        let segment = segments.join("cursor__s-1.jsonl");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(segment)
            .expect("open segment");
        use std::io::Write as _;
        writeln!(file, "{line}").expect("plant line");
    }

    #[test]
    fn conflicting_version_replay_references_the_retained_version() {
        let dir = project();
        let mut original = request("body-x");
        original.event_identity = Some(("tool_use_id".into(), "tu-v".into()));
        let first = capture(dir.path(), original.clone()).expect("original");
        assert_eq!(first.status, CaptureStatus::Captured);
        let mut rewrite = original.clone();
        rewrite.text = "body-y".into();
        let conflict = capture(dir.path(), rewrite.clone()).expect("conflict");
        assert_eq!(conflict.status, CaptureStatus::Conflict);
        assert_eq!(
            conflict.related.as_deref(),
            Some(first.record.capture_id.as_str())
        );
        // Replay of the CONFLICTING version's exact content references that
        // version — a second copy of the same body is never appended.
        let replay_of_rewrite = capture(dir.path(), rewrite).expect("replay of rewrite");
        assert_eq!(replay_of_rewrite.status, CaptureStatus::Replay);
        assert_eq!(
            replay_of_rewrite.related.as_deref(),
            Some(conflict.record.capture_id.as_str())
        );
        // Replay of the original references the original.
        let replay_of_original = capture(dir.path(), original).expect("replay of original");
        assert_eq!(replay_of_original.status, CaptureStatus::Replay);
        assert_eq!(
            replay_of_original.related.as_deref(),
            Some(first.record.capture_id.as_str())
        );
        assert_eq!(
            load_spool(dir.path()).len(),
            2,
            "no duplicate conflict was appended"
        );
        let health = health(dir.path());
        assert_eq!(health.records, 1);
        assert_eq!(health.conflicts, 1);
        assert_eq!(health.replays, 2);
        // The frontier counts event records only — sightings never move it.
        match session_watermark(dir.path(), "cursor", "s-1") {
            SessionWatermark::Present(watermark) => assert_eq!(watermark.records, 2),
            other => panic!("frontier must be present: {other:?}"),
        }
    }

    #[test]
    fn identity_dedup_requires_proven_lineage() {
        let make_req = || {
            let mut req = request("alpha");
            req.event_identity = Some(("tool_use_id".into(), "tu-planted".into()));
            req
        };
        // Unlineaged prior record: never proven same-lineage, so an identical
        // redelivery is new evidence — not a replay, not a conflict.
        let dir = project();
        plant_segment_line(
            dir.path(),
            &planted_record("cap_planted", "tu-planted", "alpha", None),
        );
        let outcome = capture(dir.path(), make_req()).expect("capture");
        assert_eq!(
            outcome.status,
            CaptureStatus::Captured,
            "unknown lineage ≠ proven equal"
        );
        assert_eq!(load_spool(dir.path()).len(), 2);

        // A DIFFERENT proven lineage: also new evidence.
        let dir = project();
        let other_lineage = format!("{}-other", crate::roots::lineage_refname(dir.path()));
        plant_segment_line(
            dir.path(),
            &planted_record("cap_planted", "tu-planted", "alpha", Some(&other_lineage)),
        );
        let outcome = capture(dir.path(), make_req()).expect("capture");
        assert_eq!(
            outcome.status,
            CaptureStatus::Captured,
            "another lineage is new evidence"
        );

        // Control: same proven lineage + identical content dedups as a replay.
        let dir = project();
        let same_lineage = crate::roots::lineage_refname(dir.path());
        plant_segment_line(
            dir.path(),
            &planted_record("cap_planted", "tu-planted", "alpha", Some(&same_lineage)),
        );
        let outcome = capture(dir.path(), make_req()).expect("capture");
        assert_eq!(outcome.status, CaptureStatus::Replay);
        assert_eq!(outcome.related.as_deref(), Some("cap_planted"));
        assert_eq!(
            load_spool(dir.path()).len(),
            1,
            "the planted record is the only event"
        );
    }

    #[test]
    fn session_watermark_is_a_bounded_frontier_read() {
        let dir = project();
        let out = capture(dir.path(), request("frontier")).expect("capture");
        // Corrupt the segment itself AFTER the capture: the bounded watermark
        // read never opens the segment, so it still reports the frontier.
        std::fs::write(&out.segment, b"\xff\xfe not utf-8").expect("corrupt segment");
        match session_watermark(dir.path(), "cursor", "s-1") {
            SessionWatermark::Present(watermark) => {
                assert_eq!(watermark.records, 1);
                assert_eq!(
                    watermark.last_capture_id.as_deref(),
                    Some(out.record.capture_id.as_str())
                );
            }
            other => panic!("frontier must be present: {other:?}"),
        }
        // An unrelated session's frontier is independent — its own captures
        // and another session's corruption never cross over.
        let mut other = request("unrelated");
        other.session_id = Some("s-2".into());
        capture(dir.path(), other).expect("s-2 capture");
        match session_watermark(dir.path(), "cursor", "s-2") {
            SessionWatermark::Present(watermark) => assert_eq!(watermark.records, 1),
            other => panic!("s-2 frontier must be present: {other:?}"),
        }
        // The corrupted segment surfaces in health — not in the watermark.
        let health = health(dir.path());
        assert!(
            health
                .read_failures
                .iter()
                .any(|f| f.file.contains("cursor__s-1.jsonl")),
            "{:?}",
            health.read_failures
        );
    }

    #[test]
    fn corrupt_or_missing_frontier_is_unreadable_never_absent() {
        let dir = project();
        let out = capture(dir.path(), request("evidence")).expect("capture");
        let frontier = out.segment.with_extension("frontier.json");
        assert!(frontier.is_file(), "capture wrote the frontier");
        // Corrupt frontier: surfaced distinctly — never as "never captured".
        std::fs::write(&frontier, "{not json").expect("corrupt frontier");
        match session_watermark(dir.path(), "cursor", "s-1") {
            SessionWatermark::Unreadable(reason) => {
                assert!(reason.contains("corrupt"), "{reason}");
            }
            other => panic!("corrupt frontier must be Unreadable: {other:?}"),
        }
        // Missing frontier with an existing segment: same honest state.
        std::fs::remove_file(&frontier).expect("drop frontier");
        match session_watermark(dir.path(), "cursor", "s-1") {
            SessionWatermark::Unreadable(reason) => {
                assert!(reason.contains("without a frontier"), "{reason}");
            }
            other => panic!("missing frontier must be Unreadable: {other:?}"),
        }
        // Recovery derives the watermark from the segment and rewrites it.
        let recovered = recover_session_frontier(dir.path(), "cursor", "s-1")
            .expect("recover")
            .expect("segment existed");
        assert_eq!(recovered.records, 1);
        assert_eq!(
            recovered.last_capture_id.as_deref(),
            Some(out.record.capture_id.as_str())
        );
        match session_watermark(dir.path(), "cursor", "s-1") {
            SessionWatermark::Present(watermark) => assert_eq!(watermark.records, 1),
            other => panic!("frontier recovered: {other:?}"),
        }
        // No segment at all: Absent — and recovery invents nothing.
        assert!(recover_session_frontier(dir.path(), "cursor", "never")
            .expect("ok")
            .is_none());
        assert!(matches!(
            session_watermark(dir.path(), "cursor", "never"),
            SessionWatermark::Absent
        ));
    }

    #[test]
    fn a_segment_read_failure_propagates_never_an_empty_dedup_history() {
        let dir = project();
        let mut req = request("original");
        req.event_identity = Some(("tool_use_id".into(), "tu-io".into()));
        let out = capture(dir.path(), req.clone()).expect("capture");
        // Invalid UTF-8 in the segment: read_to_string fails with InvalidData.
        let mut bytes = std::fs::read(&out.segment).expect("read raw");
        bytes.extend_from_slice(b"\xff\xfe");
        std::fs::write(&out.segment, bytes).expect("corrupt");
        let err = capture(dir.path(), req).expect_err("read failure must propagate");
        assert!(matches!(err, CaptureError::Io(_)), "{err}");
        // Health exposes the unreadable segment instead of treating it empty.
        let health = health(dir.path());
        assert!(
            health
                .read_failures
                .iter()
                .any(|f| f.file.contains("cursor__s-1.jsonl")),
            "{:?}",
            health.read_failures
        );
    }

    #[test]
    fn a_segment_path_that_is_a_directory_propagates_the_read_failure() {
        let dir = project();
        // Pathological layout: a directory where the segment file should be.
        let segment = segments_dir(dir.path()).join("cursor__s-1.jsonl");
        std::fs::create_dir_all(&segment).expect("segment as directory");
        let err = capture(dir.path(), request("boom")).expect_err("must propagate");
        assert!(matches!(err, CaptureError::Io(_)), "{err}");
        let health = health(dir.path());
        assert!(
            health
                .read_failures
                .iter()
                .any(|f| f.file.contains("cursor__s-1.jsonl")),
            "{:?}",
            health.read_failures
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_segment_permission_failure_propagates_and_is_diagnosed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = project();
        let out = capture(dir.path(), request("seed")).expect("capture");
        let mut perms = out.segment.metadata().expect("meta").permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&out.segment, perms).expect("chmod 000");
        let err = capture(dir.path(), request("next")).expect_err("EACCES must propagate");
        assert!(matches!(err, CaptureError::Io(_)), "{err}");
        let health = health(dir.path());
        assert!(
            health
                .read_failures
                .iter()
                .any(|f| f.file.contains("cursor__s-1.jsonl")),
            "{:?}",
            health.read_failures
        );
        let mut perms = out.segment.metadata().expect("meta").permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&out.segment, perms).expect("restore");
    }
}
