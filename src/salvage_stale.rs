//! An exFAT extension outside the common trait: the chains that non-zero FAT
//! entries on free clusters still link.
//!
//! Microsoft's exFAT specification, section 7.1 "Allocation Bitmap Directory
//! Entry", says a FAT does not describe the allocation state of clusters; the
//! Allocation Bitmap does. A free cluster's FAT entry is therefore legal
//! whatever it holds, and a non-zero one often still names the next cluster
//! of a deleted or truncated file. This listing is evidence, not damage: it
//! never feeds [`SalvageAdapter::report_damage`](super::SalvageAdapter).

use super::{read_source, ExfatSalvage, Geometry, Mapper, DIRECTORY_LIMIT, SECTOR};
use crate::primitives::{
    self, ClusterRecord, RawAllocationBitmap, RawRootMetadata, RootEntryKind, StaleEnd,
    SystemStream, VolumeState, FIRST_CLUSTER,
};
use crate::salvage::adapter::{Extent, SalvageError, Sink};
use fs_core::BlockRead;
use serde::{Deserialize, Serialize};

/// FAT bytes cached per read.
const FAT_CHUNK: u64 = 64 * 1024;

/// One free cluster whose FAT entry is non-zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaleEntry {
    pub cluster: u32,
    /// The raw FAT entry: the next cluster, or a marker.
    pub value: u32,
    /// The entry's four bytes in the active FAT.
    pub location: Extent,
}

/// Free clusters linked by their non-zero FAT entries, and where the last
/// entry points. Every entry's own chain is the rest of this one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaleChain {
    /// In link order. The first is the head: no other listed entry names it,
    /// or, for a chain that only loops, it is the loop's lowest cluster.
    pub entries: Vec<StaleEntry>,
    /// The source bytes of those clusters, in link order, merged where
    /// adjacent, so a plan can copy them as they stand.
    pub extents: Vec<Extent>,
    pub end: StaleEnd,
}

impl StaleChain {
    pub fn head(&self) -> Option<u32> {
        self.entries.first().map(|entry| entry.cluster)
    }
}

/// The outcome of a stale-chain listing whose chains went to a sink.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaleChainReport {
    /// Chains passed to the sink.
    pub chains: u64,
    /// Free clusters with non-zero FAT entries in those chains.
    pub entries: u64,
    /// Heap clusters whose allocation bit or FAT entry could not be read, or
    /// that no bitmap or FAT byte covers. None of them was listed, so a
    /// non-zero count means the listing may be incomplete.
    pub unclassified: u64,
    /// Bitmap and FAT bytes on the source that could not be read.
    pub unreadable: Vec<Extent>,
    /// Set when the sink stopped the listing. `chains` and `entries` then
    /// cover what was passed; `unclassified` still covers the whole heap.
    pub stopped: bool,
}

impl ExfatSalvage {
    /// Lists every free cluster with a non-zero FAT entry, grouped into the
    /// chains those entries link, with each chain checked for its end: the
    /// end-of-chain marker; a break at a free, bad or out-of-heap target; or a
    /// join with an allocated cluster or an earlier chain.
    ///
    /// This is an exFAT extension, not a verb of the common trait. Allocation
    /// comes from the root directory's in-use bitmap entry for the active FAT;
    /// links come from the active FAT. Unreadable or missing bitmap and FAT
    /// bytes leave their clusters unclassified and are counted in the report,
    /// so an empty listing with nothing unclassified means nothing was found.
    ///
    /// Fails only with [`SalvageError::NoGeometry`], when no boot copy gave
    /// geometry.
    pub fn stale_chains(
        &self,
        source: &dyn BlockRead,
        sink: &mut dyn Sink<StaleChain>,
    ) -> Result<StaleChainReport, SalvageError> {
        let geometry = self.geometry()?;
        let mut unreadable = Vec::new();
        let bitmap = Bitmap::read(geometry, source, &mut unreadable);
        let mut fat = FatReader::new(geometry, source);
        let raw = RawAllocationBitmap::parse(&bitmap.bytes, geometry.boot.cluster_count);
        let recorded = (bitmap.bytes.len() as u64 * 8).min(u64::from(geometry.boot.cluster_count));
        let record = |cluster: u32| {
            if bitmap.unknown(cluster) {
                return ClusterRecord::Unknown;
            }
            match raw.allocated(cluster) {
                Some(true) => ClusterRecord::Allocated,
                Some(false) => fat
                    .word(cluster)
                    .map_or(ClusterRecord::Unknown, ClusterRecord::Free),
                None => ClusterRecord::Unknown,
            }
        };
        let summary = primitives::stale_chains(
            geometry.boot.cluster_count,
            recorded as u32,
            record,
            |walk| {
                sink.accept(StaleChain {
                    entries: walk
                        .links
                        .iter()
                        .map(|&(cluster, value)| StaleEntry {
                            cluster,
                            value,
                            location: geometry.fat_word(cluster),
                        })
                        .collect(),
                    extents: cluster_extents(geometry, &walk.links),
                    end: walk.end,
                })
            },
        );
        unreadable.append(&mut fat.unreadable);
        Ok(StaleChainReport {
            chains: summary.chains,
            entries: summary.entries,
            unclassified: summary.unknown,
            unreadable: merged(unreadable),
            stopped: summary.stopped,
        })
    }
}

/// The active allocation bitmap's bytes, with the cluster ranges whose bits
/// could not be read.
struct Bitmap {
    bytes: Vec<u8>,
    /// Sorted, disjoint cluster ranges.
    unknown: Vec<(u32, u32)>,
}

impl Bitmap {
    /// Finds the in-use bitmap entry for the active FAT in the root directory
    /// and reads as much of its claim as maps onto the source. A missing
    /// entry or claim leaves no bytes, so every cluster is unclassified.
    fn read(geometry: &Geometry, source: &dyn BlockRead, unreadable: &mut Vec<Extent>) -> Self {
        let mut bitmap = Self {
            bytes: Vec::new(),
            unknown: Vec::new(),
        };
        let mut mapper = Mapper::new(geometry, source);
        let root = mapper.chain(
            geometry.boot.root_cluster,
            DIRECTORY_LIMIT / geometry.cluster,
            false,
        );
        let mut root_bytes = Vec::new();
        for extent in geometry.extents(&root.runs, u64::MAX) {
            read_tolerant(source, extent, &mut root_bytes, &mut Vec::new());
        }
        let Some(stream) = active_bitmap_stream(geometry, &root_bytes) else {
            return bitmap;
        };
        // Bits only for clusters the source holds: the rest have no bytes to
        // salvage, and a damaged count cannot make the bitmap outgrow the source.
        let on_source = source.size_bytes().saturating_sub(geometry.heap) / geometry.cluster;
        let length = stream.length.min(
            on_source
                .min(u64::from(geometry.boot.cluster_count))
                .div_ceil(8),
        );
        let claim = mapper.chain(
            stream.first_cluster,
            length.div_ceil(geometry.cluster),
            false,
        );
        for extent in geometry.extents(&claim.runs, length) {
            let start = bitmap.bytes.len() as u64;
            for (offset, length) in read_tolerant(source, extent, &mut bitmap.bytes, unreadable) {
                let first = start + offset;
                let cluster = |byte: u64| {
                    (byte * 8 + u64::from(FIRST_CLUSTER)).min(u64::from(u32::MAX)) as u32
                };
                bitmap
                    .unknown
                    .push((cluster(first), cluster(first + length)));
            }
        }
        bitmap
    }

    fn unknown(&self, cluster: u32) -> bool {
        let index = self.unknown.partition_point(|&(_, end)| end <= cluster);
        self.unknown
            .get(index)
            .is_some_and(|&(start, _)| start <= cluster)
    }
}

/// The in-use allocation bitmap entry for the active FAT: the second bitmap
/// belongs to the second FAT, which only a two-FAT volume can make active.
fn active_bitmap_stream(geometry: &Geometry, root: &[u8]) -> Option<SystemStream> {
    let active = if geometry.boot.number_of_fats >= 2 {
        VolumeState::from_flags(geometry.boot.volume_flags).active_fat
    } else {
        0
    };
    RawRootMetadata::parse(root)
        .entries
        .iter()
        .filter(|entry| entry.in_use)
        .find_map(|entry| match entry.kind {
            RootEntryKind::Bitmap { id, stream, .. } if id == active => Some(stream),
            _ => None,
        })
}

/// Words of the active FAT through a two-chunk cache, so a chain walk and the
/// scan it interrupts do not evict each other.
struct FatReader<'a> {
    source: &'a dyn BlockRead,
    start: u64,
    /// FAT bytes that exist on the source.
    length: u64,
    chunks: [Chunk; 2],
    /// Which chunk to replace next.
    oldest: usize,
    unreadable: Vec<Extent>,
}

struct Chunk {
    index: Option<u64>,
    bytes: Vec<u8>,
    /// Byte ranges of `bytes` that could not be read.
    failed: Vec<(u64, u64)>,
}

impl<'a> FatReader<'a> {
    fn new(geometry: &Geometry, source: &'a dyn BlockRead) -> Self {
        // Both terms are 32-bit sector counts times a sector of at most 4 KiB.
        let end = geometry.fat + geometry.fat_bytes;
        let chunk = || Chunk {
            index: None,
            bytes: Vec::new(),
            failed: Vec::new(),
        };
        Self {
            source,
            start: geometry.fat,
            length: end.min(source.size_bytes()).saturating_sub(geometry.fat),
            chunks: [chunk(), chunk()],
            oldest: 0,
            unreadable: Vec::new(),
        }
    }

    /// The raw word, or `None` when its bytes are missing or unreadable.
    fn word(&mut self, cluster: u32) -> Option<u32> {
        let offset = u64::from(cluster) * 4;
        if offset + 4 > self.length {
            return None;
        }
        let index = offset / FAT_CHUNK;
        let slot = match self
            .chunks
            .iter()
            .position(|chunk| chunk.index == Some(index))
        {
            Some(slot) => slot,
            None => {
                let slot = self.oldest;
                self.oldest = 1 - slot;
                let chunk = &mut self.chunks[slot];
                let start = index * FAT_CHUNK;
                let extent = Extent::new(self.start + start, FAT_CHUNK.min(self.length - start));
                chunk.bytes.clear();
                chunk.failed =
                    read_tolerant(self.source, extent, &mut chunk.bytes, &mut self.unreadable);
                chunk.index = Some(index);
                slot
            }
        };
        let chunk = &self.chunks[slot];
        let within = offset - index * FAT_CHUNK;
        if chunk
            .failed
            .iter()
            .any(|&(start, length)| within < start + length && start < within + 4)
        {
            return None;
        }
        primitives::checked_fat_entry(&chunk.bytes, (within / 4) as usize)
    }
}

/// Appends `extent`'s bytes to `output`, retrying a failed read per sector.
/// Failed sectors and bytes past the source's end append zeros and are
/// returned as ranges relative to the extent; only failed sectors on the
/// source are added to `unreadable`.
fn read_tolerant(
    source: &dyn BlockRead,
    extent: Extent,
    output: &mut Vec<u8>,
    unreadable: &mut Vec<Extent>,
) -> Vec<(u64, u64)> {
    let start = output.len();
    output.resize(start + extent.length as usize, 0);
    let bytes = &mut output[start..];
    let available = source
        .size_bytes()
        .saturating_sub(extent.offset)
        .min(extent.length);
    let mut failed = Vec::new();
    if available < extent.length {
        failed.push((available, extent.length - available));
    }
    let readable = &mut bytes[..available as usize];
    if read_source(source, extent.offset, readable).is_ok() {
        return failed;
    }
    let mut lost = Vec::new();
    for (index, sector) in readable.chunks_mut(SECTOR as usize).enumerate() {
        let within = index as u64 * SECTOR;
        if read_source(source, extent.offset + within, sector).is_err() {
            sector.fill(0);
            let length = sector.len() as u64;
            unreadable.push(Extent::new(extent.offset + within, length));
            match lost.last_mut() {
                Some((start, run)) if *start + *run == within => *run += length,
                _ => lost.push((within, length)),
            }
        }
    }
    lost.append(&mut failed);
    lost
}

/// Source extents of the linked clusters, whole and in link order, merged
/// where adjacent.
fn cluster_extents(geometry: &Geometry, links: &[(u32, u32)]) -> Vec<Extent> {
    let mut extents: Vec<Extent> = Vec::new();
    for &(cluster, _) in links {
        let extent = Extent::new(geometry.cluster_offset(cluster), geometry.cluster);
        match extents.last_mut() {
            Some(last) if last.end() == Some(extent.offset) => last.length += extent.length,
            _ => extents.push(extent),
        }
    }
    extents
}

/// Sorts extents and joins those that touch or overlap.
fn merged(mut extents: Vec<Extent>) -> Vec<Extent> {
    extents.sort();
    let mut result: Vec<Extent> = Vec::new();
    for extent in extents {
        match result.last_mut() {
            Some(last) if last.end().is_some_and(|end| end >= extent.offset) => {
                let end = last.end().max(extent.end()).unwrap_or(u64::MAX);
                last.length = end - last.offset;
            }
            _ => result.push(extent),
        }
    }
    result
}
