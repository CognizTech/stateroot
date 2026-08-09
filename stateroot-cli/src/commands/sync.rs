//! `stateroot sync [--push|--pull]` — git2 remote operations on
//! `refs/stateroot/*` (Phase 1). Roots are commit-trees, so state AND files
//! travel inside the commits — no second channel.
//!
//! Rules (binding): divergence = fork (both tips kept; never force, never
//! delete remote refs); `.stateroot/local/` never enters roots (enforced at
//! snapshot time in `roots::build_tree`), so sync state and machine-local
//! notes never travel.

use serde_json::{json, Value};
use stateroot_core::local_store;
use stateroot_core::roots as engine;

use super::{auth as gh, note, repo as repo_cmd, Ctx};

/// Fetch staging namespace (fetched remote tips before reconciliation).
const FETCHED_PREFIX: &str = "refs/stateroot/fetched/";

fn sync_state_path(ctx: &Ctx) -> std::path::PathBuf {
    local_store::root(&ctx.cwd).join("local/sync-state.json")
}

fn read_sync_state(ctx: &Ctx) -> Value {
    std::fs::read_to_string(sync_state_path(ctx))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(json!({}))
}

fn write_sync_state(ctx: &Ctx, state: &Value) -> anyhow::Result<()> {
    let path = sync_state_path(ctx);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(state)?))?;
    Ok(())
}

fn credential_callbacks(token: Option<&str>) -> git2::RemoteCallbacks<'static> {
    let mut callbacks = git2::RemoteCallbacks::new();
    let token = token.map(str::to_string);
    callbacks.credentials(move |_url, _user, _allowed| {
        match &token {
            Some(token) => git2::Cred::userpass_plaintext("x-access-token", token),
            // Local remotes (file://, ssh agent): let git try its defaults.
            None => git2::Cred::default(),
        }
    });
    callbacks
}

fn resolve_remote(ctx: &Ctx) -> anyhow::Result<String> {
    let Some(binding) = repo_cmd::binding(ctx) else {
        anyhow::bail!("project not linked — `stateroot repo link <owner/repo>` first");
    };
    Ok(repo_cmd::remote_url(ctx, &binding))
}

/// `stateroot sync [--push|--pull]` (both when neither flag is given).
pub fn run(ctx: &Ctx, push: bool, pull: bool) -> anyhow::Result<()> {
    ctx.require_project()?;
    let url = resolve_remote(ctx)?;
    let repo = engine::ensure_repo(&ctx.cwd).map_err(|e| anyhow::anyhow!(e))?;
    let token = gh::github_token(ctx);
    if token.is_none() && url.starts_with("https://") {
        note!("warning: no github credential — only public/anonymous remotes will work");
    }
    let do_push = push || !pull;
    let do_pull = pull || !push;
    let mut state = read_sync_state(ctx);
    let mut synced = 0usize;

    if do_pull {
        synced += fetch_and_reconcile(ctx, &repo, &url, token.as_deref())?;
    }
    if do_push {
        synced += push_refs(&repo, &url, token.as_deref())?;
    }
    state["last_sync_at"] = json!(local_store::now_rfc3339());
    state["remote"] = json!(url);
    write_sync_state(ctx, &state)?;
    println!("sync: {synced} ref update(s) against {url}");
    Ok(())
}

/// Fetch remote refs/stateroot/* into a staging namespace, then reconcile:
/// new tips are adopted, diverged tips become forks (both kept).
fn fetch_and_reconcile(
    ctx: &Ctx,
    repo: &git2::Repository,
    url: &str,
    token: Option<&str>,
) -> anyhow::Result<usize> {
    let mut remote = repo.remote_anonymous(url)?;
    let refspecs = [
        format!("{}*:{}roots/*", engine::ROOTS_REF_PREFIX, FETCHED_PREFIX),
        format!("{}*:{}forks/*", engine::FORKS_REF_PREFIX, FETCHED_PREFIX),
        format!("{}:{}latest", engine::LATEST_REF, FETCHED_PREFIX),
    ];
    let mut options = git2::FetchOptions::new();
    options.remote_callbacks(credential_callbacks(token));
    remote.fetch(&refspecs, Some(&mut options), None)?;

    let mut updates = 0usize;
    let mut forks = 0usize;
    // Reconcile every staged ref.
    let staged: Vec<(String, git2::Oid)> = repo
        .references_glob(&format!("{FETCHED_PREFIX}*"))?
        .flatten()
        .filter_map(|reference| {
            reference
                .name()
                .map(|name| (name.to_string(), reference.target()))
                .map(|(name, target)| (name, target.unwrap_or(git2::Oid::zero())))
        })
        .collect();
    for (staged_name, target) in staged {
        let suffix = staged_name
            .strip_prefix(FETCHED_PREFIX)
            .unwrap_or("")
            .to_string();
        let (local_name, is_head_pointer) = if suffix == "latest" {
            (engine::LATEST_REF.to_string(), true)
        } else if let Some(rest) = suffix.strip_prefix("roots/") {
            (format!("{}roots/{rest}", engine::ROOTS_REF_PREFIX), false)
        } else if let Some(rest) = suffix.strip_prefix("forks/") {
            (format!("{}forks/{rest}", engine::FORKS_REF_PREFIX), false)
        } else {
            continue;
        };
        {
            // Drop the staged ref (reference_delete isn't on Repository).
            if let Ok(mut staged_ref) = repo.find_reference(&staged_name) {
                let _ = staged_ref.delete();
            }
        }
        match repo.refname_to_id(&local_name) {
            Err(_) => {
                repo.reference(&local_name, target, true, "sync fetch")?;
                updates += 1;
            }
            Ok(existing) if existing == target => {}
            Ok(existing) => {
                // Divergence: keep both. Head pointer fast-forwards only;
                // everything else gets a fork ref for the remote tip.
                let local_is_ancestor = repo.graph_descendant_of(target, existing).unwrap_or(false);
                if is_head_pointer && local_is_ancestor {
                    repo.reference(&local_name, target, true, "sync fast-forward")?;
                    updates += 1;
                } else if !is_head_pointer
                    && repo.graph_descendant_of(existing, target).unwrap_or(false)
                {
                    // remote is strictly behind — nothing to keep
                } else {
                    let fork_name = format!(
                        "{}forks/sync-diverged-{}",
                        engine::FORKS_REF_PREFIX,
                        &target.to_string()[..8]
                    );
                    repo.reference(&fork_name, target, true, "sync divergence fork")?;
                    forks += 1;
                    note!(
                        "divergence at {local_name}: remote tip kept as fork `{}` (never force, never delete)",
                        fork_name.trim_start_matches(engine::FORKS_REF_PREFIX)
                    );
                }
            }
        }
    }
    if forks > 0 {
        println!("sync: {forks} divergence fork(s) recorded — both tips kept");
    }
    let _ = ctx;
    Ok(updates + forks)
}

/// Push all refs/stateroot/* to the remote — never forced, never deleted.
fn push_refs(repo: &git2::Repository, url: &str, token: Option<&str>) -> anyhow::Result<usize> {
    let mut remote = repo.remote_anonymous(url)?;
    // libgit2's push refspec parser rejects wildcards — enumerate locally.
    let mut refspecs: Vec<String> = Vec::new();
    for prefix in [engine::ROOTS_REF_PREFIX, engine::FORKS_REF_PREFIX] {
        for reference in repo.references_glob(&format!("{prefix}*"))?.flatten() {
            if let Some(name) = reference.name() {
                refspecs.push(format!("{name}:{name}"));
            }
        }
    }
    if repo.refname_to_id(engine::LATEST_REF).is_ok() {
        refspecs.push(format!("{0}:{0}", engine::LATEST_REF));
    }
    let count = refspecs.len();
    let mut options = git2::PushOptions::new();
    options.remote_callbacks(credential_callbacks(token));
    // The remote may reject non-fast-forwards — that failure surfaces
    // honestly (never force, never delete).
    remote.push(&refspecs, Some(&mut options))?;
    Ok(count)
}
