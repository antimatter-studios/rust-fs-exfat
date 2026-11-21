//! The format-neutral salvage adapter trait and its evidence types.
//!
//! They live in rust-fs-core, as `fs_core::layers::salvage`, shared by every
//! rust-fs driver. This module re-exports them under their old paths for one
//! release; new code names `fs_core::layers` directly.

pub use fs_core::layers::salvage::*;
