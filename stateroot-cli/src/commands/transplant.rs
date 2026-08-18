//! `stateroot transplant` — append-only adoption of session evidence between
//! initialized projects with immutable receipts on both sides.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use stateroot_core::local_store;

use super::{note, Ctx};

const RECEIPT_SCHEMA: &str = "stateroot.transplant.receipt.v1";

fn require_initialized(path: &Path, label: &str) -> anyhow::Result<PathBuf> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !local_store::is_stateroot_dir(&path) {
        anyhow::bail!(
            "{label} is not an initialized StateRoot project: {}",
            path.display()
        );
    }
    Ok(path)
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn project_id(path: &Path) -> String {
    local_store::read_manifest(path)
        .ok()
        .flatten()
        .and_then(|m| {
            m.get("project_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .into()
        })
}

fn read_spool(path: &Path) -> Vec<String> {
    let spool = local_store::root(path).join("spool/observations.jsonl");
    std::fs::read_to_string(spool)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn filter_spool_lines(lines: &[String], harness: Option<&str>) -> Vec<String> {
    let Some(harness) = harness else {
        return lines.to_vec();
    };
    lines
        .iter()
        .filter(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|v| {
                    v.get("harness")
                        .and_then(|h| h.as_str())
                        .map(str::to_string)
                })
                .map(|h| h.eq_ignore_ascii_case(harness))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

fn write_receipt(project_dir: &Path, receipt: &Value) -> anyhow::Result<PathBuf> {
    let dir = local_store::root(project_dir).join("transplants");
    std::fs::create_dir_all(&dir)?;
    let id = receipt
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("receipt");
    let path = dir.join(format!("{id}.json"));
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(receipt)?),
    )?;
    Ok(path)
}

/// Run transplant between two initialized projects.
pub fn run(
    _ctx: &Ctx,
    from: &str,
    to: &str,
    dry_run: bool,
    confirm: bool,
    harness: Option<&str>,
    reason: Option<&str>,
) -> anyhow::Result<()> {
    let from_dir = require_initialized(Path::new(from), "source")?;
    let to_dir = require_initialized(Path::new(to), "destination")?;
    if from_dir == to_dir {
        anyhow::bail!("source and destination must be different projects");
    }

    let spool_lines = filter_spool_lines(&read_spool(&from_dir), harness);
    let handoff_src = local_store::root(&from_dir).join(local_store::HANDOFF_CURRENT_PATH);
    let handoff_exists = handoff_src.is_file();
    let spool_bytes = spool_lines.join("\n");
    let spool_hash = sha256_bytes(spool_bytes.as_bytes());
    let handoff_hash = if handoff_exists {
        sha256_bytes(&std::fs::read(&handoff_src).unwrap_or_default())
    } else {
        String::new()
    };

    println!("Transplant plan:");
    println!("  from: {} ({})", from_dir.display(), project_id(&from_dir));
    println!("  to:   {} ({})", to_dir.display(), project_id(&to_dir));
    println!("  spool rows: {}", spool_lines.len());
    println!("  handoff: {}", if handoff_exists { "yes" } else { "no" });
    if let Some(harness) = harness {
        println!("  harness filter: {harness}");
    }
    if dry_run {
        note!("dry-run — no files written");
        return Ok(());
    }
    if !confirm {
        anyhow::bail!("refusing without --confirm (use --dry-run to preview)");
    }

    let receipt_id = uuid::Uuid::now_v7().to_string();
    let created_at = stateroot_core::local_store::now_rfc3339();
    let reason = reason.unwrap_or("session evidence adoption");

    if !spool_lines.is_empty() {
        let dest_spool = local_store::root(&to_dir).join("spool/observations.jsonl");
        if let Some(parent) = dest_spool.parent() {
            std::fs::create_dir_all(parent)?;
        }
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&dest_spool)?;
        for line in &spool_lines {
            writeln!(file, "{line}")?;
        }
    }

    if handoff_exists {
        let text = std::fs::read_to_string(&handoff_src)?;
        let packet: Value = serde_json::from_str(&text).unwrap_or(json!({}));
        let harness_tag = packet
            .get("created_by_harness")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let history_name = format!("{created_at}-transplant-from-{}", project_id(&from_dir));
        let history_path = local_store::root(&to_dir)
            .join(local_store::HANDOFF_HISTORY_DIR)
            .join(format!("{history_name}-{harness_tag}.json"));
        if let Some(parent) = history_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&history_path, &text)?;
    }

    let base = json!({
        "schema_version": RECEIPT_SCHEMA,
        "id": receipt_id,
        "created_at": created_at,
        "reason": reason,
        "harness_filter": harness,
        "hashes": {
            "spool": spool_hash,
            "handoff": handoff_hash,
        },
        "counts": {
            "spool_rows": spool_lines.len(),
            "handoff_copied": handoff_exists,
        },
    });

    let out_receipt = json!({
        "direction": "out",
        "peer_project_id": project_id(&to_dir),
        "peer_path": to_dir.to_string_lossy(),
    });
    let mut out = base.clone();
    if let Some(obj) = out.as_object_mut() {
        for (k, v) in out_receipt.as_object().unwrap() {
            obj.insert(k.clone(), v.clone());
        }
    }
    let in_receipt = json!({
        "direction": "in",
        "peer_project_id": project_id(&from_dir),
        "peer_path": from_dir.to_string_lossy(),
    });
    let mut inbound = base;
    if let Some(obj) = inbound.as_object_mut() {
        for (k, v) in in_receipt.as_object().unwrap() {
            obj.insert(k.clone(), v.clone());
        }
    }

    let out_path = write_receipt(&from_dir, &out)?;
    let in_path = write_receipt(&to_dir, &inbound)?;
    println!("receipts:");
    println!("  source: {}", out_path.display());
    println!("  destination: {}", in_path.display());
    Ok(())
}
