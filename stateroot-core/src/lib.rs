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
pub mod error;
pub mod harness_identity;
pub mod harness_install;
pub mod local_store;
pub mod mcp_federation;
pub mod openclaw_identity;
pub mod presentation;
pub mod skill_federation;
pub mod sync_engine;
pub mod transcripts;
