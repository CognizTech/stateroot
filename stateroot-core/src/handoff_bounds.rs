//! Shared handoff field limits (local + transcript enrichment).

/// Maximum length for `context_summary`: detailed continuity narrative.
/// Matches transcript progress-summary capacity.
pub const CONTEXT_SUMMARY_MAX: usize = 6000;
