//! An exFAT extension outside the common trait: both FAT and Allocation
//! Bitmap pairs of a TexFAT volume, compared cluster by cluster, and the
//! adapter over either pair (#40).
//!
//! Microsoft's exFAT specification, section 3.1.16 "NumberOfFats Field",
//! gives a TexFAT volume a Second FAT and a Second Allocation Bitmap, and
//! section 7.1.2.1 "BitmapIdentifier Field" uses each bitmap with the FAT of
//! the same number. Section 3.1.13.1 "ActiveFat Field" says implementations
//! shall use the active pair and consider the other stale, and section 4
//! "File Allocation Table Region" leaves the use of the inactive FAT
//! implementation specific. A stale copy is not a damaged one, so where the
//! copies differ is evidence of what each pair recorded, not damage:
//! [`SalvageAdapter::report_damage`](super::SalvageAdapter) and the strict
//! reader use the active pair alone. macOS does not implement TexFAT, so no
//! platform oracle covers this extension.

use super::stale::{merged, read_tolerant, Bitmap};
use super::{ExfatSalvage, Geometry};
use crate::primitives::{self, FatCopy, FatLayout, FIRST_CLUSTER};
use crate::salvage::adapter::{Extent, SalvageError, Sink};
use fs_core::BlockRead;
use serde::{Deserialize, Serialize};

/// FAT bytes read from each copy per comparison step.
const FAT_CHUNK: u64 = 64 * 1024;

/// Which structure two copies record differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CopyKind {
    /// The cluster's FAT entries differ.
    Fat,
    /// The cluster's Allocation Bitmap bits differ.
    Bitmap,
}

/// One heap cluster that the First and the Second copy record differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyDifference {
    pub kind: CopyKind,
    pub cluster: u32,
    /// The cluster's raw FAT entry, or its bitmap bit as 0 or 1, in the
    /// First copy.
    pub first: u32,
    /// The same in the Second copy.
    pub second: u32,
    /// The entry's four bytes, or the bitmap byte that holds the bit, on the
    /// source, in the First copy.
    pub first_location: Extent,
    /// The same in the Second copy.
    pub second_location: Extent,
}

/// One FAT and Allocation Bitmap pair as the comparison found it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyPair {
    pub copy: FatCopy,
    /// The FAT's bytes on the source: FatLength sectors from its offset.
    pub fat: Extent,
    /// The root directory, as this pair's FAT chains it, holds an in-use
    /// Allocation Bitmap entry for this pair.
    pub bitmap_entry: bool,
    /// That bitmap's bytes on the source, in stream order, as far as its
    /// chain and the source reach.
    pub bitmap: Vec<Extent>,
}

/// The outcome of a comparison whose differences went to a sink.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyReport {
    /// The pair ActiveFat names in the boot copy the geometry came from.
    pub active: FatCopy,
    /// One per FAT the boot fields describe, First first. With one FAT
    /// nothing is compared, and the counts stay zero.
    pub pairs: Vec<CopyPair>,
    /// Heap clusters whose entries both FATs held readable.
    pub fat_compared: u64,
    /// Those whose entries differ, each passed to the sink.
    pub fat_differences: u64,
    /// Heap clusters whose bits both bitmaps held readable.
    pub bitmap_compared: u64,
    /// Those whose bits differ, each passed to the sink.
    pub bitmap_differences: u64,
    /// FAT and bitmap bytes on the source that could not be read.
    pub unreadable: Vec<Extent>,
    /// The sink stopped the comparison; the counts cover what came before.
    pub stopped: bool,
}

impl ExfatSalvage {
    /// The adapter over `copy`'s FAT and Allocation Bitmap: every verb and
    /// extension maps FAT claims through that FAT, and
    /// [`stale_chains`](Self::stale_chains) classifies clusters by that
    /// pair's bitmap. Over the inactive pair of a TexFAT volume, its walks
    /// and stale chains are chain candidates as that pair recorded them,
    /// evidence that [`fat_copies`](Self::fat_copies) shows where it differs.
    /// Boot checks and candidate scans do not depend on the FAT.
    ///
    /// `None` when no boot copy gave geometry, or for [`FatCopy::Second`]
    /// when the boot fields describe one FAT.
    pub fn with_fat(&self, copy: FatCopy) -> Option<Self> {
        let geometry = self.geometry.as_ref()?.with_copy(copy)?;
        Some(Self {
            geometry: Some(geometry),
        })
    }

    /// The pair this adapter maps claims through: the active one, unless
    /// [`with_fat`](Self::with_fat) chose the other. `None` without
    /// geometry.
    pub fn fat(&self) -> Option<FatCopy> {
        self.geometry.as_ref().map(|geometry| geometry.copy)
    }

    /// Compares the First and Second FAT entry by entry, then the First and
    /// Second Allocation Bitmap bit by bit, over every heap cluster, and
    /// passes each cluster they record differently to `sink`, in cluster
    /// order, with its byte offset in each copy.
    ///
    /// This is an exFAT extension, not a verb of the common trait. Each
    /// bitmap is found through the root directory as its own FAT chains it.
    /// Bytes either copy could not read, and bitmap bits either bitmap does
    /// not hold, are not compared; the counts say how many clusters were.
    /// An empty listing with every cluster compared means the copies agree.
    ///
    /// Fails only with [`SalvageError::NoGeometry`], when no boot copy gave
    /// geometry.
    pub fn fat_copies(
        &self,
        source: &dyn BlockRead,
        sink: &mut dyn Sink<CopyDifference>,
    ) -> Result<CopyReport, SalvageError> {
        let geometry = self.geometry()?;
        let layout = FatLayout::from_boot(&geometry.boot);
        let mut unreadable = Vec::new();
        let mut copies = Vec::new();
        for copy in layout.copies() {
            let Some(paired) = geometry.with_copy(copy) else {
                continue;
            };
            let bitmap = Bitmap::read(&paired, source, &mut unreadable);
            copies.push((paired, bitmap));
        }
        let mut report = CopyReport {
            active: layout.active,
            pairs: copies
                .iter()
                .map(|(paired, bitmap)| CopyPair {
                    copy: paired.copy,
                    fat: Extent::new(paired.fat, paired.fat_bytes),
                    bitmap_entry: bitmap.entry,
                    bitmap: bitmap.extents.clone(),
                })
                .collect(),
            fat_compared: 0,
            fat_differences: 0,
            bitmap_compared: 0,
            bitmap_differences: 0,
            unreadable: Vec::new(),
            stopped: false,
        };
        if let [first, second] = copies.as_slice() {
            compare_fats(
                &first.0,
                &second.0,
                source,
                sink,
                &mut report,
                &mut unreadable,
            );
            if !report.stopped {
                compare_bitmaps(&geometry.boot, &first.1, &second.1, sink, &mut report);
            }
        }
        report.unreadable = merged(unreadable);
        Ok(report)
    }
}

/// Compares the two FATs a chunk at a time, entries 0 to ClusterCount + 1 as
/// far as FatLength and the source hold them.
fn compare_fats(
    first: &Geometry,
    second: &Geometry,
    source: &dyn BlockRead,
    sink: &mut dyn Sink<CopyDifference>,
    report: &mut CopyReport,
    unreadable: &mut Vec<Extent>,
) {
    let cluster_count = first.boot.cluster_count;
    // The Second FAT follows the First, so the source ends in it first.
    let length = first
        .fat_bytes
        .min((u64::from(cluster_count) + 2) * 4)
        .min(source.size_bytes().saturating_sub(second.fat));
    let mut offset = 0;
    while offset < length {
        let size = FAT_CHUNK.min(length - offset);
        let (mut old, mut new) = (Vec::new(), Vec::new());
        let mut lost = read_tolerant(
            source,
            Extent::new(first.fat + offset, size),
            &mut old,
            unreadable,
        );
        lost.extend(read_tolerant(
            source,
            Extent::new(second.fat + offset, size),
            &mut new,
            unreadable,
        ));
        let lost = lost
            .into_iter()
            .map(|(at, length)| (at, at + length))
            .collect();
        for (start, end) in kept(size, lost, 4) {
            let entry = ((offset + start as u64) / 4) as u32;
            let compared = primitives::fat_differences(
                &old[start..end],
                &new[start..end],
                entry,
                cluster_count,
                |cluster, old, new| {
                    let at = u64::from(cluster) * 4;
                    sink.accept(CopyDifference {
                        kind: CopyKind::Fat,
                        cluster,
                        first: old,
                        second: new,
                        first_location: Extent::new(first.fat + at, 4),
                        second_location: Extent::new(second.fat + at, 4),
                    })
                },
            );
            report.fat_compared += compared.clusters;
            report.fat_differences += compared.differences;
            if compared.stopped {
                report.stopped = true;
                return;
            }
        }
        offset += size;
    }
}

/// Compares the bytes both bitmaps hold and read.
fn compare_bitmaps(
    boot: &primitives::BootSector,
    first: &Bitmap,
    second: &Bitmap,
    sink: &mut dyn Sink<CopyDifference>,
    report: &mut CopyReport,
) {
    let length = first.bytes.len().min(second.bytes.len()) as u64;
    let lost = first.lost.iter().chain(&second.lost).copied().collect();
    for (start, end) in kept(length, lost, 1) {
        let compared = primitives::bitmap_differences(
            &first.bytes[start..end],
            &second.bytes[start..end],
            start as u64,
            boot.cluster_count,
            |cluster, old, new| {
                let byte = u64::from(cluster - FIRST_CLUSTER) / 8;
                sink.accept(CopyDifference {
                    kind: CopyKind::Bitmap,
                    cluster,
                    first: u32::from(old),
                    second: u32::from(new),
                    first_location: byte_location(&first.extents, byte),
                    second_location: byte_location(&second.extents, byte),
                })
            },
        );
        report.bitmap_compared += compared.clusters;
        report.bitmap_differences += compared.differences;
        if compared.stopped {
            report.stopped = true;
            return;
        }
    }
}

/// The parts of `0..length` outside every `lost` range, given as start and
/// end, cut to whole `unit`s.
fn kept(length: u64, mut lost: Vec<(u64, u64)>, unit: u64) -> Vec<(usize, usize)> {
    lost.sort_unstable();
    let mut kept = Vec::new();
    let mut start = 0u64;
    for (from, to) in lost.into_iter().chain([(length, length)]) {
        let (first, last) = (start.next_multiple_of(unit), from.min(length) / unit * unit);
        if first < last {
            kept.push((first as usize, last as usize));
        }
        start = start.max(to);
    }
    kept
}

/// The source byte behind byte `index` of a stream read from `extents` in
/// order. Compared bytes were read from those extents, so one holds it.
fn byte_location(extents: &[Extent], mut index: u64) -> Extent {
    for extent in extents {
        if index < extent.length {
            return Extent::new(extent.offset + index, 1);
        }
        index -= extent.length;
    }
    unreachable!("a compared bitmap byte lies in the extents it was read from")
}
