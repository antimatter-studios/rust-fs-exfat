//! The salvage conformance set, re-exported from rust-fs-core.
//!
//! It lives in rust-fs-core, as `fs_core::layers::conformance::salvage`,
//! shared by every rust-fs driver; `fs_core::layers::conformance` adds the
//! checks for the volume, primitive and file layers. This module re-exports
//! the salvage set under its old path for one release.

pub use fs_core::layers::conformance::salvage::*;
