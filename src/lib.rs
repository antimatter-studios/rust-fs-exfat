#![deny(unsafe_code)]

//! A strict, read-only exFAT reader with permissive on-disk primitives and a
//! read-only salvage adapter ([`salvage`]) built on them.
//!
//! # Test-support images
//!
//! The optional `test-support` feature exposes `fs_exfat::fixtures`, pure in-memory
//! encoders for boot copies, FAT words, allocation bits, upcase tables and
//! live/deleted directory entry sets, plus complete damaged-volume scenarios.
//! It is off by default, adds no dependencies, and never enables device writes.
//! These deliberately incomplete fixtures are not a formatter or evidence of
//! format correctness; healthy structures are checked against macOS oracles.
//!
//! The same feature exposes `fs_exfat::conformance`, the driver convention's
//! format-neutral conformance set. A driver runs it against its own salvage
//! adapter and strict API; `tests/conformance.rs` runs it against this crate's.
//!
//! ```toml
//! [dev-dependencies]
//! fs_exfat = { package = "rust-fs-exfat", version = "0.1.0", features = ["test-support"] }
//! ```
//!
//! ```
//! # #[cfg(feature = "test-support")] {
//! use fs_exfat::fixtures::{entry_set, Builder, ROOT_CLUSTER};
//!
//! let mut image = Builder::new();
//! image.directory(ROOT_CLUSTER, &[entry_set("sample.bin", false, 10, 4, true, true)]);
//! image.put(10, 0, b"data");
//! assert_eq!(image.image.len(), 835_584);
//! # }
//! ```

pub mod boot;
#[allow(unsafe_code)]
pub mod capi;
pub mod check;
#[cfg(feature = "cli")]
pub mod cli;
#[cfg(feature = "test-support")]
pub mod conformance;
pub mod device;
pub mod directory;
pub mod error;
pub mod file;
#[cfg(feature = "test-support")]
pub mod fixtures;
pub mod interface;
pub mod metadata;
pub mod path;
pub mod primitives;
pub mod salvage;
pub mod timestamp;
pub mod volume;

pub use error::{Error, Result};
