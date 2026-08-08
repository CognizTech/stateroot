//! Canonical harness id ↔ user-facing display name.
//!
//! Thin re-export of [`stateroot_core::harness_identity`] so CLI commands
//! keep a stable import path.

#[allow(unused_imports)]
pub use stateroot_core::harness_identity::{
    display_name, normalize, resume_command, NATIVE_HARNESS_ID,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skillsagent_displays_as_statesmith() {
        assert_eq!(display_name("skillsagent"), "StateSmith");
        assert_eq!(display_name("SKILLSAGENT"), "StateSmith");
    }

    #[test]
    fn canonical_harnesses_have_titles() {
        assert_eq!(display_name("cursor"), "Cursor");
        assert_eq!(display_name("codex"), "Codex");
    }

    #[test]
    fn resume_command_is_harness_specific() {
        assert_eq!(resume_command("codex"), "stateroot resume --harness codex");
        assert_eq!(
            resume_command("cursor"),
            "stateroot resume --harness cursor"
        );
    }

    #[test]
    fn normalize_statesmith_alias() {
        assert_eq!(normalize("statesmith"), "skillsagent");
        assert_eq!(NATIVE_HARNESS_ID, "skillsagent");
    }
}
