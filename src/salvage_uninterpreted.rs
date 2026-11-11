//! An exFAT extension outside the common trait: the directory entries this
//! driver does not interpret, vendor entries and unknown types.
//!
//! Microsoft's exFAT specification lets an implementation skip them. Benign
//! entries are optional (sections 6.3.1.2.2 and 6.4.1.2.2), and a Vendor
//! Extension or Vendor Allocation entry whose VendorGuid is not recognized is
//! an unrecognized benign secondary (sections 7.8, 7.9 and 8.2);
//! [`primitives::uninterpreted_entries`] cites the type codes and skip rules.
//! A writer the reader does not know, or damage, leaves such entries behind.
//! This listing is evidence, not damage: it never feeds
//! [`SalvageAdapter::report_damage`](super::SalvageAdapter) and never changes
//! which candidates a scan finds.

use super::stale::{merged, read_tolerant};
use super::{EntrySetDetail, ExfatSalvage, Mapper, DIRECTORY_LIMIT};
use crate::primitives::{
    self, UninterpretedCounts, UninterpretedKind, UninterpretedRecord, ENTRY_SIZE,
};
use crate::salvage::adapter::{Candidate, CandidateKind, Extent, Parent, SalvageError, Sink};
use fs_core::BlockRead;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ops::ControlFlow;

/// One directory entry the driver does not interpret.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UninterpretedEntry {
    /// The entry's 32 bytes on the source.
    pub location: Extent,
    /// The directory whose bytes hold it.
    pub directory: Parent,
    /// The raw EntryType, InUse included.
    pub entry_type: u8,
    pub kind: UninterpretedKind,
    /// The InUse bit: clear on an unused entry, such as one of a deleted set.
    pub in_use: bool,
    /// The entry set it lies in, or `None` when no File or benign primary's
    /// set reaches it, as for an orphan secondary or an unknown critical
    /// primary.
    pub set: Option<HoldingSet>,
}

impl UninterpretedEntry {
    /// It lies in an entry set that is valid as it stands.
    pub fn in_valid_set(&self) -> bool {
        self.set.is_some_and(|set| set.valid)
    }
}

/// The entry set an uninterpreted entry lies in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldingSet {
    /// The set's primary entry on the source: the entry itself for a primary.
    pub primary: Extent,
    /// The primary's raw EntryType.
    pub primary_type: u8,
    /// Every entry present and in use, secondaries after the primary, the
    /// stored SetChecksum matching, and a File set opening with its Stream
    /// Extension and File Name entries, as
    /// [`primitives::SetMembership::valid`] defines it.
    pub valid: bool,
    /// SetChecksum matches with InUse restored on every entry, as it does for
    /// a set deleted by clearing InUse alone.
    pub checksum_ok: bool,
}

/// The outcome of a listing whose entries went to a sink.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UninterpretedReport {
    /// Entries passed to the sink, by kind.
    pub counts: UninterpretedCounts,
    /// Directories walked: the root, then each directory candidate whose
    /// first cluster no earlier directory started at.
    pub directories: u64,
    /// Directory bytes that could not be read, including claims past the
    /// source's end. No entry there was listed, so a non-empty list means the
    /// listing may be incomplete.
    pub unreadable: Vec<Extent>,
    /// Set when the sink stopped the listing. `counts` then covers what was
    /// passed.
    pub stopped: bool,
}

/// A directory's claim on the heap.
struct Claim {
    directory: Parent,
    first: u32,
    bytes: u64,
    contiguous: bool,
}

impl ExfatSalvage {
    /// Lists every directory entry the driver does not interpret, with its
    /// location, type, InUse bit and the entry set it lies in: Vendor
    /// Extension and Vendor Allocation entries, and primary and secondary
    /// types that revision 1.00 does not define or that this driver skips.
    ///
    /// This is an exFAT extension, not a verb of the common trait. It walks
    /// the directories [`rebuild_tree`](super::SalvageAdapter::rebuild_tree)
    /// treats as parents: the root chain, then the claim of every directory
    /// candidate, deleted and damaged ones included, up to the
    /// specification's 256 MiB directory limit. A candidate that starts at an
    /// already walked directory's first cluster is not walked again. Each
    /// directory is read as far as its chain reaches and up to its
    /// end-of-directory marker; unreadable stretches are skipped and listed
    /// in the report, and an entry after one is not placed in a set that
    /// starts before it.
    ///
    /// Fails only with [`SalvageError::NoGeometry`], when no boot copy gave
    /// geometry.
    pub fn uninterpreted_entries(
        &self,
        source: &dyn BlockRead,
        candidates: &[Candidate<EntrySetDetail>],
        sink: &mut dyn Sink<UninterpretedEntry>,
    ) -> Result<UninterpretedReport, SalvageError> {
        let geometry = self.geometry()?;
        let mut mapper = Mapper::new(geometry, source);
        let mut report = UninterpretedReport::default();
        let mut unreadable = Vec::new();
        let mut started = HashSet::new();
        let root = Claim {
            directory: Parent::Root,
            first: geometry.boot.root_cluster,
            bytes: DIRECTORY_LIMIT,
            contiguous: false,
        };
        let directories = candidates
            .iter()
            .filter(|candidate| candidate.kind == CandidateKind::Directory)
            .map(|directory| Claim {
                directory: Parent::Candidate(directory.location.offset),
                first: directory.detail.first_cluster,
                bytes: directory.size.unwrap_or(0).min(DIRECTORY_LIMIT),
                contiguous: directory.detail.no_fat_chain,
            });
        for claim in std::iter::once(root).chain(directories) {
            if claim.bytes == 0 || !started.insert(claim.first) {
                continue;
            }
            let chain = mapper.chain(
                claim.first,
                claim.bytes.div_ceil(geometry.cluster),
                claim.contiguous,
            );
            let extents = geometry.extents(&chain.runs, claim.bytes);
            if extents.is_empty() {
                continue;
            }
            report.directories += 1;
            let mut listing = Listing {
                directory: claim.directory,
                counts: &mut report.counts,
                sink: &mut *sink,
            };
            if listing
                .directory(source, &extents, &mut unreadable)
                .is_break()
            {
                report.stopped = true;
                break;
            }
        }
        report.unreadable = merged(unreadable);
        Ok(report)
    }
}

/// One directory's entries on their way to the sink.
struct Listing<'a> {
    directory: Parent,
    counts: &'a mut UninterpretedCounts,
    sink: &'a mut dyn Sink<UninterpretedEntry>,
}

impl Listing<'_> {
    /// Reads the directory's extents and walks each readable stretch in turn
    /// until an end-of-directory marker.
    fn directory(
        &mut self,
        source: &dyn BlockRead,
        extents: &[Extent],
        unreadable: &mut Vec<Extent>,
    ) -> ControlFlow<()> {
        let mut bytes = Vec::new();
        // Where each extent's bytes start in `bytes`.
        let mut starts = Vec::with_capacity(extents.len());
        let mut gaps = Vec::new();
        for &extent in extents {
            let start = bytes.len();
            starts.push(start);
            for (offset, length) in read_tolerant(source, extent, &mut bytes, &mut Vec::new()) {
                gaps.push((start + offset as usize, start + (offset + length) as usize));
                unreadable.push(Extent::new(extent.offset + offset, length));
            }
        }
        gaps.sort_unstable();
        // Clusters hold whole entries, so no entry spans two extents.
        let entry = |position: usize| {
            let index = starts.partition_point(|&start| start <= position) - 1;
            let offset = extents[index].offset + (position - starts[index]) as u64;
            Extent::new(offset, ENTRY_SIZE as u64)
        };
        let mut from = 0;
        let end = bytes.len();
        for (gap, after) in gaps.into_iter().chain(std::iter::once((end, end))) {
            if gap > from {
                let walk = primitives::uninterpreted_entries(&bytes[from..gap], |record| {
                    self.accept(record, from, &entry)
                });
                if walk.stopped {
                    return ControlFlow::Break(());
                }
                if walk.end_marker.is_some() {
                    break;
                }
            }
            from = from.max(after);
        }
        ControlFlow::Continue(())
    }

    fn accept(
        &mut self,
        record: UninterpretedRecord,
        base: usize,
        entry: &impl Fn(usize) -> Extent,
    ) -> ControlFlow<()> {
        self.counts.add(record.kind);
        self.sink.accept(UninterpretedEntry {
            location: entry(base + record.offset),
            directory: self.directory,
            entry_type: record.entry_type,
            kind: record.kind,
            in_use: record.in_use,
            set: record.set.map(|set| HoldingSet {
                primary: entry(base + set.primary),
                primary_type: set.primary_type,
                valid: set.valid,
                checksum_ok: set.checksum_ok,
            }),
        })
    }
}
