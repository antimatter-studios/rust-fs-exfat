//! Read-only salvage: the common adapter trait and its exFAT implementation.
//!
//! Salvage is a peer of the strict volume and file APIs, not a mode of them.
//! It sits on [`crate::primitives`], needs no successful strict mount, and
//! returns evidence rather than trusted files. [`adapter`] defines the
//! format-neutral trait; [`ExfatSalvage`] implements it for exFAT.
//!
//! ```
//! use fs_exfat::salvage::{Candidate, EntrySetDetail, Extent, ExfatSalvage, SalvageAdapter};
//!
//! # fn scan(source: &dyn fs_core::BlockRead) -> Result<(), fs_exfat::salvage::SalvageError> {
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

#[path = "salvage_adapter.rs"]
pub mod adapter;

#[path = "salvage_exfat.rs"]
mod exfat;

pub use crate::primitives::{FatCopy, StaleEnd, UninterpretedCounts, UninterpretedKind};
pub use adapter::{
    Candidate, CandidateKind, CandidateState, Capabilities, Damage, DamageKind, DamageReport,
    Extent, ExtentWalk, Feature, Integrity, MaterializeReport, MaterializedRange, Parent, Plan,
    PlanStep, SalvageAdapter, SalvageError, ScanReport, Severity, Sink, Tree, TreeNode, WalkEnd,
};
pub use exfat::{
    CopyDifference, CopyKind, CopyPair, CopyReport, EntrySetDetail, ExfatSalvage, HoldingSet,
    StaleChain, StaleChainReport, StaleEntry, UninterpretedEntry, UninterpretedReport,
    UpcaseReport,
};
