//! Read-only salvage: the common adapter trait and its exFAT implementation.
//!
//! Salvage is a peer of the strict volume and file APIs, not a mode of them.
//! It sits on [`crate::primitives`], needs no successful strict mount, and
//! returns evidence rather than trusted files. [`ExfatSalvage`] implements
//! [`fs_core::layers::salvage::SalvageAdapter`] for exFAT. The old [`adapter`]
//! API remains available for one release.
//!
//! ```
//! use fs_core::layers::salvage::{Candidate, Extent, SalvageAdapter, SalvageError};
//! use fs_exfat::salvage::{EntrySetDetail, ExfatSalvage};
//!
//! # fn scan(source: &dyn fs_core::BlockRead) -> Result<(), SalvageError> {
//! let adapter = ExfatSalvage::probe(source);
//! let mut candidates: Vec<Candidate<EntrySetDetail>> = Vec::new();
//! let whole = Extent::new(0, source.size_bytes());
//! adapter.find_candidates(source, whole, &mut candidates)?;
//! let tree = adapter.rebuild_tree(source, &candidates)?;
//! for orphan in tree.orphans() {
//!     println!("orphan record at byte {}", orphan.candidate);
//! }
//! # Ok(())
//! # }
//! ```

#![allow(deprecated)]

#[path = "salvage_adapter.rs"]
pub mod adapter;

#[path = "salvage_exfat.rs"]
mod exfat;

pub use crate::primitives::{FatCopy, StaleEnd, UninterpretedCounts, UninterpretedKind};
pub use adapter::{
    Candidate, CandidateKind, CandidateState, Capabilities, ClusterLayout, Damage, DamageKind,
    DamageReport, Extent, ExtentWalk, Feature, Integrity, MaterializeReport, MaterializedRange,
    Parent, Plan, PlanCheck, PlanFault, PlanMetadata, PlanOverlap, PlanRead, PlanReader, PlanStep,
    SalvageAdapter, SalvageError, ScanReport, Severity, Sink, Tree, TreeNode, WalkEnd,
};
pub use exfat::{
    CopyDifference, CopyKind, CopyPair, CopyReport, CrossLink, CrossLinkReport, EntrySetDetail,
    ExfatSalvage, FatLink, HoldingSet, StaleChain, StaleChainReport, StaleEntry,
    UninterpretedEntry, UninterpretedReport, UpcaseReport,
};
