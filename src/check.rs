#[path = "check_audit.rs"]
mod audit;

pub use audit::{check_device, check_device_with_budget, MAX_AUDIT_MEMORY_BYTES};

use crate::boot::BootDifference;
use crate::primitives::UninterpretedCounts;
use crate::volume::Volume;
use crate::Result;
use fs_core::BlockRead;

#[derive(Debug)]
pub struct CheckReport {
    pub files: u64,
    pub directories: u64,
    pub allocated_clusters: u64,
    pub referenced_clusters: u64,
    pub unreferenced_clusters: u64,
    pub dirty: bool,
    pub media_failure: bool,
    pub boot_differences: Vec<BootDifference>,
    /// Entries of the checked directories that the reader does not interpret,
    /// by kind: vendor entries and unknown types the specification lets a
    /// reader skip. Counted, never rejected, and not part of [`Self::healthy`].
    pub uninterpreted: UninterpretedCounts,
}

impl CheckReport {
    /// No inconsistency was found. Uninterpreted entries do not count.
    pub fn healthy(&self) -> bool {
        !self.dirty && !self.media_failure && self.unreferenced_clusters == 0
    }
}

pub fn check<Device: BlockRead>(volume: &Volume<Device>) -> Result<CheckReport> {
    audit::check_reader(&volume.reader, MAX_AUDIT_MEMORY_BYTES)
}
