//! Marked-block helpers — re-exported from `stateroot_core::harness_install`
//! (kept as a module so existing `commands::blocks::…` paths keep working).

#[allow(unused_imports)]
pub use stateroot_core::harness_install::{
    ensure_marked_block, remove_marked_block, BLOCK_BEGIN, BLOCK_END,
};
