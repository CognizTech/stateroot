//! `stateroot-core`: the local-first StateRoot engine.
//!
//! Seeded by lifting the proven pure-local modules from the StateSmith
//! monorepo's `agentdrive-core` (copy-and-own; the monorepo remains upstream
//! reference — drift is deliberate for open-source self-containment).
//!
//! No server anywhere: every module here works against the local filesystem
//! and the harnesses on the machine.

pub mod canonical;
pub mod config;
pub mod context_pack;
pub mod digest_delivery;
pub mod error;
pub mod extensions;
pub mod handoff_bounds;
pub mod handoff_continuity;
pub mod harness_identity;
pub mod harness_install;
pub mod hot_apex;
pub mod learnings;
pub mod local_store;
pub mod mcp_federation;
pub mod memory_index;
pub mod observations;
pub mod openclaw_identity;
pub mod persona_injection;
pub mod presentation;
pub mod proposals;
pub mod roots;
pub mod rules;
pub mod seed;
pub mod skill_federation;
pub mod snap_context;
pub mod soul;
pub mod sync_engine;
pub mod transcripts;
pub mod user_profile;
pub mod wiki;
