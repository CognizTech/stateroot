//! Persona cache (local file only). The full soul service lands in M3; until
//! then the one-agent block and resume render whatever the user has placed in
//! the config-dir cache (or a neutral placeholder).

use std::path::PathBuf;

/// Cache file for the rendered persona inside the config dir.
pub const PERSONA_CACHE_FILE: &str = "persona.md";

/// Path of the persona cache.
pub fn cache_path(config_dir: &std::path::Path) -> PathBuf {
    config_dir.join(PERSONA_CACHE_FILE)
}

/// Read the cached persona, if any.
pub fn read_cache(config_dir: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(cache_path(config_dir)).ok()?;
    let text = text.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// "Sync" in the local variant is a cache read: there is no server prompt
/// profile to fetch (M3 adds the local soul service). Name kept from the
/// monorepo so install/init call sites stay stable.
pub async fn sync_best_effort(ctx: &super::Ctx) -> Option<String> {
    read_cache(&ctx.config_dir)
}

/// Persona text for one harness's integration block. The fork has no
/// per-harness projection service yet (M3) — the shared cache is used for
/// every harness, honestly the same content.
pub async fn for_harness(
    ctx: &super::Ctx,
    _harness_id: &str,
    fallback: Option<&str>,
) -> Option<String> {
    fallback
        .map(str::to_string)
        .or_else(|| read_cache(&ctx.config_dir))
}
