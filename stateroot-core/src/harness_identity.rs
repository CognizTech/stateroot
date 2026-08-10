//! Canonical harness id ↔ display name / alias helpers.
//!
//! Storage/API use the canonical internal id (`statesmith` — there is NO
//! legacy alias, per owner directive). User-visible surfaces
//! (resume digests, install instructions) show product names (`StateSmith`).
//! Aliases come from the shared `stateroot_harness_registry.v1.json` contract.

/// Canonical native harness id used on the wire and in storage.
/// (Owner directive: no legacy aliases — `statesmith`, period.)
pub const NATIVE_HARNESS_ID: &str = "statesmith";

/// Map an alias or id to the canonical storage id. Empty → native id.
pub fn normalize(raw: &str) -> String {
    let key = raw.trim().to_ascii_lowercase();
    if key.is_empty() {
        return NATIVE_HARNESS_ID.to_string();
    }
    crate::skill_federation::normalize_harness(&key)
}

/// Display name for a canonical harness id. Unknown ids pass through trimmed.
pub fn display_name(id: &str) -> String {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return "StateSmith".to_string();
    }
    crate::skill_federation::display_name(trimmed)
}

/// Resume command line for a harness integration surface.
pub fn resume_command(harness_id: &str) -> String {
    let id = normalize(harness_id);
    format!("stateroot resume --harness {id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statesmith_displays_as_statesmith() {
        assert_eq!(display_name("statesmith"), "StateSmith");
        // Unknown ids pass through unchanged (no silent remapping).
        assert_eq!(display_name("unknown-harness"), "unknown-harness");
    }

    #[test]
    fn normalize_aliases() {
        assert_eq!(normalize("statesmith"), "statesmith");
        assert_eq!(normalize("claude-code"), "claude");
        assert_eq!(normalize(""), "statesmith");
        // Unregistered ids pass through rather than resolving to native.
        assert_eq!(normalize("unknown-harness"), "unknown-harness");
    }

    #[test]
    fn resume_command_is_harness_specific() {
        assert_eq!(resume_command("codex"), "stateroot resume --harness codex");
        assert_eq!(
            resume_command("statesmith"),
            "stateroot resume --harness statesmith"
        );
    }
}
