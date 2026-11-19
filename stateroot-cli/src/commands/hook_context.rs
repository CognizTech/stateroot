//! Keep the rich hook digest when it fits. For a constrained receiver, carry
//! working context first and pin any remaining text to this exact delivery.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::Ctx;
use stateroot_core::{digest_delivery, local_store, roots, safe_io};

const CLAUDE_FIELD_LIMIT: usize = 10_000;
const CODEX_DEFAULT_TOKENS: usize = 2_500;
const SCHEMA: &str = "stateroot.hook-context.v1";

#[derive(Clone, Copy, Serialize, Deserialize)]
enum Limit {
    Utf16(usize),
    Bytes(usize),
}

impl Limit {
    fn max(self) -> usize {
        match self {
            Self::Utf16(max) | Self::Bytes(max) => max,
        }
    }

    fn size(self, text: &str) -> usize {
        match self {
            Self::Utf16(_) => text.encode_utf16().count(),
            Self::Bytes(_) => text.len(),
        }
    }

    fn fits(self, text: &str) -> bool {
        self.size(text) <= self.max()
    }
}

// A hook does not receive the native handler's config or its CLI overrides.
// Take the smallest applicable StateRoot handler limit visible on disk, so
// two registrations for the same event cannot cause a spill in the smaller
// one. Codex estimates tokens as rounded-up UTF-8 bytes divided by four.
fn codex_tokens(home: &Path, project: &Path, event: &str, default: Option<usize>) -> Option<usize> {
    let mut limits = Vec::new();
    for dir in [
        stateroot_core::harness_install::paths::codex_root(home),
        project.join(".codex"),
    ] {
        for name in ["hooks.json", "config.toml"] {
            let Ok(text) = std::fs::read_to_string(dir.join(name)) else {
                continue;
            };
            let doc: Option<Value> = if name == "hooks.json" {
                serde_json::from_str(&text).ok()
            } else {
                toml::from_str::<toml::Value>(&text)
                    .ok()
                    .and_then(|doc| serde_json::to_value(doc).ok())
            };
            let Some(doc) = doc else { continue };
            let Some(groups) = doc["hooks"][event].as_array() else {
                continue;
            };
            for handler in groups
                .iter()
                .filter_map(|group| group["hooks"].as_array())
                .flatten()
            {
                let Some(command) = handler["command"].as_str() else {
                    continue;
                };
                if !stateroot_core::harness_install::health::is_stateroot_hook_command(command)
                    || !command.contains("--harness codex")
                {
                    continue;
                }
                let limit = handler["additionalContextLimit"]
                    .as_u64()
                    .or_else(|| handler["additional_context_limit"].as_u64())
                    .and_then(|limit| usize::try_from(limit).ok())
                    .or(default);
                limits.push(limit);
            }
        }
    }
    if limits.is_empty() {
        default
    } else {
        limits
            .into_iter()
            .flatten()
            .filter(|limit| *limit > 0)
            .min()
    }
}

// This executable was observed passing the full fixture. An old rollout
// reopened by a newer executable cannot prove that the new receiver has
// those capabilities. Use the header only on a new Codex CLI startup.
fn codex_start_version(project: &Path, event: &str, payload: &Value) -> Option<String> {
    if event != "SessionStart" || payload["source"].as_str() != Some("startup") {
        return None;
    }
    let path = Path::new(payload["transcript_path"].as_str()?);
    if !path.is_file() {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file.take(64 * 1024))
        .read_line(&mut line)
        .ok()?;
    let doc: Value = serde_json::from_str(&line).ok()?;
    let native = &doc["payload"];
    let session = digest_delivery::session_id_from_payload(payload)?;
    if doc["type"] == "session_meta"
        && native["id"].as_str() == Some(session.as_str())
        && native["originator"] == "codex_exec"
        && stateroot_core::transcripts::same_worktree(Path::new(native["cwd"].as_str()?), project)
    {
        native["cli_version"].as_str().map(str::to_owned)
    } else {
        None
    }
}

fn delivery_limit(project: &Path, harness: &str, event: &str, payload: &Value) -> Option<Limit> {
    match harness {
        "claude-code" => Some(Limit::Utf16(CLAUDE_FIELD_LIMIT)),
        "codex" => {
            let default = (codex_start_version(project, event, payload).as_deref()
                != Some("0.149.1"))
            .then_some(CODEX_DEFAULT_TOKENS);
            let tokens = stateroot_core::harness_install::home_dir()
                .map(|home| codex_tokens(&home, project, event, default))
                .unwrap_or(default)?;
            Some(Limit::Bytes(tokens.saturating_mul(4)))
        }
        _ => None,
    }
}

pub(super) struct Delivery {
    pub text: String,
    /// All working sections were included in this emission. The hook itself
    /// has no acknowledgement of the model's effective input.
    pub working_inline: bool,
    pub retry: bool,
}

#[derive(Serialize, Deserialize)]
struct Continuation {
    schema: String,
    project: String,
    lineage: String,
    harness: String,
    session: Option<String>,
    limit: Limit,
    required: usize,
    pages: Vec<String>,
}

fn capture_path(project: &Path, id: &str) -> PathBuf {
    local_store::root(project)
        .join("local/hook-context")
        .join(format!("{id}.json"))
}

fn read_command(id: &str, page: usize) -> String {
    format!("stateroot resume --continuation {id} --page {page}")
}

fn instruction(id: &str, required: usize, total: usize) -> String {
    if required == 0 {
        format!(
            "\n## Supporting context\nRead pages 1–{total} as needed. Start with `{}`.\n",
            read_command(id, 1)
        )
    } else {
        let optional = if required < total {
            format!(" Pages {}–{total} are supporting context.", required + 1)
        } else {
            String::new()
        };
        format!(
            "\n## Working context continues\nBefore acting, read required pages 1–{required} for this delivery. Start with `{}` and follow the required page links.{optional}\n",
            read_command(id, 1)
        )
    }
}

fn pages(text: &str, limit: Limit, room: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if limit.size(rest) <= room {
            out.push(rest.to_string());
            break;
        }
        let mut size = 0;
        let mut end = 0;
        let mut boundary = 0;
        for (offset, ch) in rest.char_indices() {
            size += match limit {
                Limit::Utf16(_) => ch.len_utf16(),
                Limit::Bytes(_) => ch.len_utf8(),
            };
            if size > room {
                break;
            }
            end = offset + ch.len_utf8();
            if ch == '\n' {
                boundary = end;
            }
        }
        let cut = if boundary > 0 { boundary } else { end };
        debug_assert!(cut > 0);
        out.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    out
}

pub(super) fn render(
    ctx: &Ctx,
    harness: &str,
    event: &str,
    payload: &Value,
    full: String,
    working: &[&str],
    supporting: &[&str],
) -> Delivery {
    let Some(limit) = delivery_limit(&ctx.cwd, harness, event, payload) else {
        return Delivery {
            text: full,
            working_inline: true,
            retry: false,
        };
    };
    if limit.fits(&full) {
        return Delivery {
            text: full,
            working_inline: true,
            retry: false,
        };
    }
    match capture(ctx, harness, payload, limit, working, supporting) {
        Ok(delivery) => delivery,
        Err(err) => {
            super::note!("context continuation unavailable: {err:#}");
            Delivery {
                text: full,
                working_inline: false,
                retry: true,
            }
        }
    }
}

fn capture(
    ctx: &Ctx,
    harness: &str,
    payload: &Value,
    limit: Limit,
    working: &[&str],
    supporting: &[&str],
) -> Result<Delivery> {
    // Reserve space for the exact command and page counts. Very small custom
    // Codex limits can leave too little room for even a continuation link.
    let room = limit.max().saturating_sub(380);
    let page_room = 8_000;
    let mut inline = String::new();
    let mut required_tail = String::new();
    for section in working
        .iter()
        .copied()
        .filter(|text| !text.trim().is_empty())
    {
        let prefix = section;
        let used = limit.size(&inline);
        if required_tail.is_empty() && used + limit.size(prefix) + 2 <= room {
            inline.push_str(prefix);
            inline.push_str("\n\n");
        } else if required_tail.is_empty() {
            // Preserve whole lines whenever possible. Large single items are
            // preserved verbatim in the continuation, possibly across pages.
            let mut used = used.saturating_add(2);
            let cut: usize = prefix
                .split_inclusive('\n')
                .take_while(|line| {
                    used = used.saturating_add(limit.size(line));
                    used <= room
                })
                .map(str::len)
                .sum();
            inline.push_str(&prefix[..cut]);
            required_tail.push_str(&prefix[cut..]);
            required_tail.push_str("\n\n");
        } else {
            required_tail.push_str(prefix);
            required_tail.push_str("\n\n");
        }
    }
    let supporting = supporting
        .iter()
        .copied()
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut remainder = pages(&required_tail, Limit::Bytes(page_room), page_room);
    let required = remainder.len();
    remainder.extend(pages(&supporting, Limit::Bytes(page_room), page_room));
    if remainder.is_empty() {
        return Ok(Delivery {
            text: inline,
            working_inline: true,
            retry: false,
        });
    }
    let record = Continuation {
        schema: SCHEMA.into(),
        project: ctx.require_project()?.project_id,
        lineage: roots::lineage_refname_checked(&ctx.cwd)?,
        harness: harness.into(),
        session: digest_delivery::session_id_from_payload(payload),
        limit,
        required,
        pages: remainder,
    };
    let bytes = serde_json::to_vec(&record)?;
    let id = format!("{:x}", Sha256::digest(&bytes));
    let path = capture_path(&ctx.cwd, &id);
    std::fs::create_dir_all(
        path.parent()
            .context("continuation directory unavailable")?,
    )?;
    safe_io::atomic_replace(&path, &bytes)?;
    let notice = instruction(&id, required, record.pages.len());
    let mut text = format!("{notice}\n{inline}");
    let retry = !limit.fits(&text);
    if retry {
        let command = read_command(&id, 1);
        super::note!("hook limit too small for the context notice. Read {command}");
        text = if limit.fits(&command) {
            command
        } else {
            String::new()
        };
    }
    Ok(Delivery {
        text,
        working_inline: required == 0 && !retry,
        retry,
    })
}

pub(crate) fn read_page(ctx: &Ctx, id: &str, page: usize) -> Result<()> {
    ensure!(
        id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid context continuation ID"
    );
    let bytes = std::fs::read(capture_path(&ctx.cwd, id))
        .context("context continuation unavailable in this checkout")?;
    ensure!(
        format!("{:x}", Sha256::digest(&bytes)) == id,
        "context continuation changed on disk"
    );
    let record: Continuation = serde_json::from_slice(&bytes)?;
    ensure!(
        record.schema == SCHEMA
            && record.project == ctx.require_project()?.project_id
            && record.lineage == roots::lineage_refname_checked(&ctx.cwd)?,
        "context continuation belongs to another checkout"
    );
    let part = page
        .checked_sub(1)
        .and_then(|index| record.pages.get(index))
        .context("context continuation page out of range")?;
    println!(
        "Captured context {page}/{} for {} ({}).\n",
        record.pages.len(),
        record.harness,
        if page <= record.required {
            "required"
        } else {
            "supporting"
        }
    );
    println!("{part}");
    if page < record.pages.len() {
        let next = page + 1;
        let label = if next <= record.required {
            "Required next page"
        } else {
            "Optional next page"
        };
        println!(
            "\n{label} ({next}/{}). Run `{}`.",
            record.pages.len(),
            read_command(id, next)
        );
    }
    Ok(())
}
