//! The exFAT salvage adapter, built on the permissive primitives only.
//!
//! | Verb | Primitives used |
//! |---|---|
//! | Find candidates | [`primitives::parse_set`] over streamed windows, as [`primitives::find_sets`] does over one buffer |
//! | Walk extents | [`primitives::walk_fat_chain`] through a [`primitives::FatWindow`], or [`primitives::contiguous_clusters`] |
//! | Rebuild the tree | the same walks over the root and directory candidates |
//! | Report damage | [`primitives::BootRegion`], [`primitives::boot_check`], [`primitives::VolumeState`], [`primitives::RawRootMetadata`], [`primitives::RawUpcaseTable`] |
//! | Read by extent | positioned source reads |
//! | Materialize | the trait's provided method over read by extent |
//! | Stale FAT chains (extension) | [`primitives::stale_chains`] over [`primitives::RawAllocationBitmap`] and the adapter's FAT words |
//! | Uninterpreted entries (extension) | [`primitives::uninterpreted_entries`] over the root and directory-candidate claims |
//! | Up-case Table for name lookups (extension) | [`primitives::RawRootMetadata`] and [`primitives::RawUpcaseTable`], compressed or not |
//! | Both TexFAT FAT and bitmap copies compared (extension) | [`primitives::FatLayout`], [`primitives::fat_differences`] and [`primitives::bitmap_differences`] |
//!
//! Geometry comes from the first usable boot copy: a checksum-valid main copy,
//! then a checksum-valid backup, then either copy with the exFAT name. No
//! strict boot, metadata or allocation policy is applied, and no strict mount
//! is needed. Claims are mapped through the active FAT and its Allocation
//! Bitmap; [`ExfatSalvage::with_fat`] maps them through the other pair of a
//! two-FAT volume instead.

use super::adapter::{
    Candidate, CandidateKind, CandidateState, Capabilities, Damage, DamageKind, DamageReport,
    Extent, ExtentWalk, Integrity, Parent, SalvageAdapter, SalvageError, ScanReport, Severity,
    Sink, Tree, TreeNode, WalkEnd,
};
use crate::device::AlignedDevice;
use crate::primitives::{
    self, BootRegion, BootSector, EntrySet, FatChainStep, FatCopy, FatLayout, FatValue, FatWindow,
    Limits, RawRootMetadata, RawUpcaseTable, RootEntryKind, SystemStream, UpcaseIssue, VolumeState,
    BOOT_REGION_SECTORS, ENTRY_SIZE, FIRST_CLUSTER,
};
use crate::Error;
use fs_core::BlockRead;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::ControlFlow;

#[path = "salvage_stale.rs"]
mod stale;

pub use stale::{StaleChain, StaleChainReport, StaleEntry};

#[path = "salvage_uninterpreted.rs"]
mod uninterpreted;

pub use uninterpreted::{HoldingSet, UninterpretedEntry, UninterpretedReport};

#[path = "salvage_upcase.rs"]
mod upcase;

pub use upcase::UpcaseReport;

#[path = "salvage_copies.rs"]
mod copies;

pub use copies::{CopyDifference, CopyKind, CopyPair, CopyReport};

const SECTOR: u64 = 512;
/// A file entry set holds at most one primary and 255 secondaries (section
/// 6.3.2 of the specification): 8 KiB, read into the scan window and never
/// allocated per set, whatever a damaged SecondaryCount claims.
const MAX_SET_BYTES: u64 = (primitives::MAX_SECONDARY_COUNT as u64 + 1) * ENTRY_SIZE as u64;
/// Bytes read per candidate-scan window.
const SCAN_WINDOW: u64 = 1 << 20;
const _: () = assert!(MAX_SET_BYTES <= SCAN_WINDOW);
const MAX_CLUSTER_COUNT: u32 = 0xFFFF_FFF5;
const MAX_CLUSTER_SHIFT: u8 = 25;
/// The specification's largest directory.
const DIRECTORY_LIMIT: u64 = 256 << 20;
/// The specification's largest upcase table.
const UPCASE_LIMIT: u64 = 128 << 10;
const BOOT_VOLUME_FLAGS: u64 = 106;
const BOOT_ROOT_CLUSTER: u64 = 96;

/// Plausibility limits for scans when no boot copy gives geometry.
const UNKNOWN_LIMITS: Limits = Limits {
    cluster_count: MAX_CLUSTER_COUNT,
    cluster_size: 1 << MAX_CLUSTER_SHIFT,
};

/// exFAT fields of a file entry-set candidate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntrySetDetail {
    pub attributes: u16,
    pub first_cluster: u32,
    /// Bytes past this length read as zeros, whatever the clusters hold.
    pub valid_data_length: u64,
    pub no_fat_chain: bool,
    pub created: Option<String>,
    pub modified: Option<String>,
    pub accessed: Option<String>,
}

/// The exFAT salvage adapter. It holds geometry only, never a device.
#[derive(Clone, Debug)]
pub struct ExfatSalvage {
    geometry: Option<Geometry>,
}

impl ExfatSalvage {
    /// Reads both boot copies permissively and keeps the first usable
    /// geometry. Never fails: unreadable or damaged copies leave none, and
    /// geometry-free verbs still work.
    pub fn probe(source: &dyn BlockRead) -> Self {
        let mut copies = Vec::new();
        for shift in 9..=12u8 {
            let sector = 1u64 << shift;
            for offset in [0, sector * BOOT_REGION_SECTORS] {
                if let Some(region) = read_boot_region(source, offset, sector) {
                    if region.header.sector.bytes_per_sector_shift == shift {
                        copies.push((offset, region));
                    }
                }
            }
        }
        copies.sort_by_key(|(offset, _)| *offset != 0);
        let checksummed = copies.iter().filter(|(_, region)| region.checksum_valid);
        let named = copies
            .iter()
            .filter(|(_, region)| region.header.file_system_name_valid);
        let geometry = checksummed
            .chain(named)
            .find_map(|(offset, region)| Geometry::new(&region.header.sector, Some(*offset)));
        Self { geometry }
    }

    /// Uses caller-supplied boot fields, such as a copy found elsewhere.
    /// Unusable geometry leaves none.
    pub fn from_boot(boot: BootSector) -> Self {
        Self {
            geometry: Geometry::new(&boot, None),
        }
    }

    /// The boot fields in use, if any copy was usable.
    pub fn boot(&self) -> Option<&BootSector> {
        self.geometry.as_ref().map(|geometry| &geometry.boot)
    }

    fn geometry(&self) -> Result<&Geometry, SalvageError> {
        self.geometry.as_ref().ok_or(SalvageError::NoGeometry)
    }
}

impl SalvageAdapter for ExfatSalvage {
    type Detail = EntrySetDetail;

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn find_candidates(
        &self,
        source: &dyn BlockRead,
        range: Extent,
        sink: &mut dyn Sink<Candidate<EntrySetDetail>>,
    ) -> Result<ScanReport, SalvageError> {
        let size = source.size_bytes();
        let end = range
            .end()
            .filter(|end| *end <= size)
            .ok_or(SalvageError::Bounds)?;
        let limits = self
            .geometry
            .as_ref()
            .map_or(UNKNOWN_LIMITS, Geometry::limits);
        let mut report = ScanReport::default();
        let mut window = Window::new(range.offset, end);
        let mut position = range
            .offset
            .checked_next_multiple_of(ENTRY_SIZE as u64)
            .ok_or(SalvageError::Bounds)?;
        while position < end {
            let needed = position.saturating_add(MAX_SET_BYTES).min(size);
            if !window.covers(position, needed) {
                window.fill(source, position, size, &mut report.unreadable);
            }
            let relative = (position - window.start) as usize;
            let step = match primitives::parse_set(&window.bytes, relative, limits) {
                Some(set) if window.readable(position, (set.slots * ENTRY_SIZE) as u64) => {
                    let step = if set.checksum_ok {
                        set.slots * ENTRY_SIZE
                    } else {
                        ENTRY_SIZE
                    } as u64;
                    report.candidates += 1;
                    if sink.accept(candidate(set, position)).is_break() {
                        report.stopped_at = Some(position + step);
                        return Ok(report);
                    }
                    step
                }
                _ => ENTRY_SIZE as u64,
            };
            position += step;
        }
        Ok(report)
    }

    fn walk_extents(
        &self,
        source: &dyn BlockRead,
        candidate: &Candidate<EntrySetDetail>,
    ) -> Result<ExtentWalk, SalvageError> {
        let geometry = self.geometry()?;
        let claimed = candidate.size.unwrap_or(0);
        let chain = Mapper::new(geometry, source).chain(
            candidate.detail.first_cluster,
            claimed.div_ceil(geometry.cluster),
            candidate.detail.no_fat_chain,
        );
        Ok(ExtentWalk {
            extents: geometry.extents(&chain.runs, claimed),
            claimed,
            end: match chain.end {
                ChainEnd::Covered => WalkEnd::Complete,
                ChainEnd::Terminated | ChainEnd::Link(_) => WalkEnd::Broken,
                ChainEnd::OutOfRange => WalkEnd::OutOfRange,
                ChainEnd::Cycle => WalkEnd::Cycle,
                ChainEnd::Unreadable(extent) => WalkEnd::Unreadable(extent),
            },
        })
    }

    /// A candidate's parents are the directories whose clusters hold its entry
    /// set: the root chain, and every directory candidate's claimed clusters,
    /// deleted or damaged ones included. Directory claims are followed up to
    /// the specification's 256 MiB directory limit.
    fn rebuild_tree(
        &self,
        source: &dyn BlockRead,
        candidates: &[Candidate<EntrySetDetail>],
    ) -> Result<Tree, SalvageError> {
        let geometry = self.geometry()?;
        let mut mapper = Mapper::new(geometry, source);
        let directory_clusters = DIRECTORY_LIMIT / geometry.cluster;
        let mut owners: HashMap<u32, Vec<Parent>> = HashMap::new();
        let root = mapper.chain(geometry.boot.root_cluster, directory_clusters, false);
        claim(&mut owners, &root.runs, Parent::Root);
        for directory in candidates
            .iter()
            .filter(|candidate| candidate.kind == CandidateKind::Directory)
        {
            let clusters = directory.size.unwrap_or(0).div_ceil(geometry.cluster);
            let chain = mapper.chain(
                directory.detail.first_cluster,
                clusters.min(directory_clusters),
                directory.detail.no_fat_chain,
            );
            claim(
                &mut owners,
                &chain.runs,
                Parent::Candidate(directory.location.offset),
            );
        }
        let nodes = candidates
            .iter()
            .map(|candidate| TreeNode {
                candidate: candidate.location.offset,
                parents: geometry
                    .cluster_at(candidate.location.offset)
                    .and_then(|cluster| owners.get(&cluster))
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect();
        Ok(Tree { nodes })
    }

    /// Checks both boot copies and their agreement, the volume flags, the root
    /// directory chain, and the root's allocation-bitmap and upcase entries.
    fn report_damage(
        &self,
        source: &dyn BlockRead,
        sink: &mut dyn Sink<Damage>,
    ) -> Result<DamageReport, SalvageError> {
        let mut findings = Findings { sink, reported: 0 };
        let flow = self.audit(source, &mut findings);
        Ok(DamageReport {
            reported: findings.reported,
            stopped: flow.is_break(),
        })
    }

    fn read_extent(
        &self,
        source: &dyn BlockRead,
        extent: Extent,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, SalvageError> {
        if offset > extent.length || extent.end().is_none_or(|end| end > source.size_bytes()) {
            return Err(SalvageError::Bounds);
        }
        let count = (extent.length - offset).min(output.len() as u64) as usize;
        AlignedDevice::new(source)
            .read_at(extent.offset + offset, &mut output[..count])
            .map_err(SalvageError::Device)?;
        Ok(count)
    }
}

impl ExfatSalvage {
    fn audit(&self, source: &dyn BlockRead, out: &mut Findings<'_>) -> ControlFlow<()> {
        let sector = self
            .geometry
            .as_ref()
            .map_or(SECTOR, |geometry| geometry.sector);
        let region = sector * BOOT_REGION_SECTORS;
        let main = audit_boot_copy(source, 0, sector, "main boot region", out)?;
        let backup = audit_boot_copy(source, region, sector, "backup boot region", out)?;
        if let (Some(main), Some(backup)) = (main, backup) {
            let check = primitives::boot_check(&main, &backup, sector as usize);
            if check.main_computed != check.backup_computed {
                out.add(
                    DamageKind::Mismatch,
                    Severity::Warning,
                    Extent::new(0, region * 2),
                    "boot regions",
                    format!(
                        "main computes checksum {:#010x}, backup {:#010x}",
                        check.main_computed, check.backup_computed
                    ),
                )?;
            }
        }
        let Some(geometry) = &self.geometry else {
            return ControlFlow::Continue(());
        };
        let boot_offset = geometry.origin.unwrap_or(0);
        let flags = Extent::new(boot_offset + BOOT_VOLUME_FLAGS, 2);
        let state = VolumeState::from_flags(geometry.boot.volume_flags);
        if state.media_failure {
            out.add(
                DamageKind::Recorded,
                Severity::Warning,
                flags,
                "volume flags",
                "MediaFailure is set".into(),
            )?;
        }
        if state.dirty {
            out.add(
                DamageKind::Recorded,
                Severity::Notice,
                flags,
                "volume flags",
                "VolumeDirty is set".into(),
            )?;
        }
        audit_root(geometry, source, out)
    }
}

/// Reports damage in the root directory's chain and its system entries.
fn audit_root(
    geometry: &Geometry,
    source: &dyn BlockRead,
    out: &mut Findings<'_>,
) -> ControlFlow<()> {
    let mut mapper = Mapper::new(geometry, source);
    let limit = DIRECTORY_LIMIT / geometry.cluster;
    let root = mapper.chain(geometry.boot.root_cluster, limit + 1, false);
    let boot_offset = geometry.origin.unwrap_or(0);
    let start = Extent::new(boot_offset + BOOT_ROOT_CLUSTER, 4);
    match root.end {
        ChainEnd::Terminated => {}
        ChainEnd::Covered => out.add(
            DamageKind::Malformed,
            Severity::Error,
            start,
            "root directory chain",
            format!("longer than the {DIRECTORY_LIMIT}-byte directory limit"),
        )?,
        _ => chain_damage(geometry, &root, start, "root directory chain", out)?,
    }
    let Some(root_bytes) = read_runs(geometry, source, &root.runs, "root directory", out)? else {
        return ControlFlow::Continue(());
    };
    let complete = root.end == ChainEnd::Terminated
        && root_bytes.len() as u64 == root.clusters * geometry.cluster;
    audit_root_entries(&mut mapper, &root.clusters(), &root_bytes, complete, out)
}

/// Reports missing system entries and damage in the bitmap and upcase entries.
/// Missing entries count only when the whole root was read: an entry absent
/// from part of the root is not evidence that it is missing.
fn audit_root_entries(
    mapper: &mut Mapper<'_>,
    root: &[u32],
    root_bytes: &[u8],
    complete: bool,
    out: &mut Findings<'_>,
) -> ControlFlow<()> {
    let geometry = mapper.geometry;
    // Root bytes are whole clusters in chain order.
    let root_location = |position: usize| {
        let cluster = root[position / geometry.cluster as usize];
        Extent::new(
            geometry.cluster_offset(cluster) + position as u64 % geometry.cluster,
            ENTRY_SIZE as u64,
        )
    };
    let metadata = RawRootMetadata::parse(root_bytes);
    let first = Extent::new(geometry.cluster_offset(root[0]), geometry.cluster);
    let bitmaps = if !complete {
        0
    } else if geometry.boot.number_of_fats >= 2 {
        2
    } else {
        1
    };
    for (id, present) in metadata.bitmap_present.iter().take(bitmaps).enumerate() {
        if !present {
            out.add(
                DamageKind::Missing,
                Severity::Error,
                first,
                "root directory",
                format!("no allocation bitmap entry for FAT {id}"),
            )?;
        }
    }
    if complete && !metadata.upcase_present {
        out.add(
            DamageKind::Missing,
            Severity::Error,
            first,
            "root directory",
            "no upcase table entry".into(),
        )?;
    }
    for entry in metadata.entries.iter().filter(|entry| entry.in_use) {
        let location = root_location(entry.offset);
        match &entry.kind {
            RootEntryKind::Bitmap {
                flags,
                flags_ok,
                stream,
                ..
            } => {
                if !flags_ok {
                    out.add(
                        DamageKind::Malformed,
                        Severity::Warning,
                        location,
                        "allocation bitmap entry",
                        format!("BitmapFlags {flags:#04x} sets reserved bits"),
                    )?;
                }
                // Section 7.1.5, Table 22: the bits past ClusterCount are
                // Reserved, so only a bitmap too short for the heap is damage.
                let required = u64::from(geometry.boot.cluster_count).div_ceil(8);
                if stream.length < required {
                    out.add(
                        DamageKind::Mismatch,
                        Severity::Error,
                        location,
                        "allocation bitmap entry",
                        format!(
                            "declares {} bytes; {} clusters need {required}",
                            stream.length, geometry.boot.cluster_count
                        ),
                    )?;
                }
            }
            RootEntryKind::Upcase { stream, checksum } => {
                audit_upcase(mapper, *stream, *checksum, location, out)?;
            }
            _ => {}
        }
    }
    ControlFlow::Continue(())
}

/// The exFAT geometry the adapter maps claims with, from one boot copy.
#[derive(Clone, Debug)]
struct Geometry {
    boot: BootSector,
    /// The boot copy's offset on the source, when it came from one.
    origin: Option<u64>,
    sector: u64,
    cluster: u64,
    heap: u64,
    /// The FAT and Allocation Bitmap pair claims are mapped through: the
    /// active one, unless [`ExfatSalvage::with_fat`] chose the other.
    copy: FatCopy,
    /// That FAT's byte offset, and each FAT's length.
    fat: u64,
    fat_bytes: u64,
}

impl Geometry {
    /// Keeps boot fields whose arithmetic is safe. This is not strict
    /// validation: offsets, lengths and the root cluster are not checked.
    fn new(boot: &BootSector, origin: Option<u64>) -> Option<Self> {
        let shift = boot.bytes_per_sector_shift;
        if !(9..=12).contains(&shift)
            || boot.sectors_per_cluster_shift > MAX_CLUSTER_SHIFT - shift
            || !(1..=MAX_CLUSTER_COUNT).contains(&boot.cluster_count)
        {
            return None;
        }
        let layout = FatLayout::from_boot(boot);
        Some(Self {
            boot: boot.clone(),
            origin,
            sector: boot.bytes_per_sector(),
            cluster: boot.cluster_size(),
            heap: boot.heap_offset(),
            copy: layout.active,
            fat: layout.offset(layout.active)?,
            fat_bytes: layout.length,
        })
    }

    /// The same geometry over `copy`'s FAT and Allocation Bitmap, when the
    /// boot fields describe that FAT.
    fn with_copy(&self, copy: FatCopy) -> Option<Self> {
        let fat = FatLayout::from_boot(&self.boot).offset(copy)?;
        Some(Self {
            copy,
            fat,
            ..self.clone()
        })
    }

    fn limits(&self) -> Limits {
        Limits {
            cluster_count: self.boot.cluster_count,
            cluster_size: self.cluster,
        }
    }

    /// The byte offset of a heap cluster; callers pass clusters from walks,
    /// which never yield one below two.
    fn cluster_offset(&self, cluster: u32) -> u64 {
        primitives::cluster_offset(&self.boot, cluster)
    }

    fn cluster_at(&self, offset: u64) -> Option<u32> {
        let index = offset.checked_sub(self.heap)? / self.cluster;
        (index < u64::from(self.boot.cluster_count)).then(|| index as u32 + FIRST_CLUSTER)
    }

    fn fat_word(&self, cluster: u32) -> Extent {
        Extent::new(self.fat + u64::from(cluster) * 4, 4)
    }

    /// Byte extents of cluster runs, trimmed to the claimed length. Walks stop
    /// at the clusters a claim needs, so no run lies wholly past the claim.
    fn extents(&self, runs: &[(u32, u64)], claimed: u64) -> Vec<Extent> {
        let mut remaining = claimed;
        runs.iter()
            .map(|&(first, count)| {
                let length = (count * self.cluster).min(remaining);
                remaining -= length;
                Extent::new(self.cluster_offset(first), length)
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChainEnd {
    /// The requested number of clusters was reached.
    Covered,
    /// The FAT's end marker came first.
    Terminated,
    /// A free, bad or reserved FAT word came first.
    Link(FatValue),
    OutOfRange,
    Cycle,
    Unreadable(Extent),
}

/// Clusters a walk reached, as runs of consecutive clusters.
struct Chain {
    runs: Vec<(u32, u64)>,
    clusters: u64,
    last: Option<u32>,
    end: ChainEnd,
}

impl Chain {
    fn new() -> Self {
        Self {
            runs: Vec::new(),
            clusters: 0,
            last: None,
            end: ChainEnd::Covered,
        }
    }

    fn push(&mut self, cluster: u32) {
        match self.runs.last_mut() {
            Some((first, count)) if u64::from(*first) + *count == u64::from(cluster) => *count += 1,
            _ => self.runs.push((cluster, 1)),
        }
        self.clusters += 1;
        self.last = Some(cluster);
    }

    /// Every cluster reached, in walk order.
    fn clusters(&self) -> Vec<u32> {
        self.runs
            .iter()
            .flat_map(|&(first, count)| first..first + count as u32)
            .collect()
    }
}

/// Walks claims through one FAT window shared across walks.
struct Mapper<'a> {
    geometry: &'a Geometry,
    source: &'a dyn BlockRead,
    window: Box<FatWindow>,
}

impl<'a> Mapper<'a> {
    fn new(geometry: &'a Geometry, source: &'a dyn BlockRead) -> Self {
        Self {
            geometry,
            source,
            window: Box::new(FatWindow::new(
                geometry.fat,
                geometry.fat_bytes,
                geometry.boot.cluster_count,
            )),
        }
    }

    /// Follows up to `needed` clusters from `first`, contiguously or through
    /// the active FAT, keeping what it reached and why it stopped.
    fn chain(&mut self, first: u32, needed: u64, contiguous: bool) -> Chain {
        let mut chain = Chain::new();
        if needed == 0 {
            return chain;
        }
        let heap_end = u64::from(self.geometry.boot.cluster_count) + u64::from(FIRST_CLUSTER);
        if contiguous {
            let claim = primitives::contiguous_clusters(first, needed);
            if claim.start < u64::from(FIRST_CLUSTER) || claim.start >= heap_end {
                chain.end = ChainEnd::OutOfRange;
                return chain;
            }
            let end = claim.end.min(heap_end);
            chain.runs.push((first, end - claim.start));
            chain.clusters = end - claim.start;
            chain.last = Some((end - 1) as u32);
            if claim.end > heap_end {
                chain.end = ChainEnd::OutOfRange;
            }
            return chain;
        }
        let Self {
            geometry,
            source,
            window,
        } = self;
        let steps = primitives::walk_fat_chain(first, geometry.boot.cluster_count, |cluster| {
            window.entry(cluster, |offset, output| {
                read_source(*source, offset, output)
            })
        });
        chain.end = ChainEnd::Terminated;
        for step in steps {
            match step {
                Ok(FatChainStep::Cluster(position)) => {
                    if !position.valid {
                        chain.end = ChainEnd::OutOfRange;
                        break;
                    }
                    if position.cycle {
                        chain.end = ChainEnd::Cycle;
                        break;
                    }
                    chain.push(position.cluster);
                    if chain.clusters == needed {
                        chain.end = ChainEnd::Covered;
                        break;
                    }
                }
                Ok(FatChainStep::Entry { entry, .. }) => match entry.kind {
                    FatValue::Next(_) => {}
                    FatValue::End => break,
                    other => {
                        chain.end = ChainEnd::Link(other);
                        break;
                    }
                },
                Err(Error::Device(_)) => {
                    chain.end =
                        ChainEnd::Unreadable(geometry.fat_word(chain.last.unwrap_or(first)));
                    break;
                }
                Err(_) => {
                    chain.end = ChainEnd::OutOfRange;
                    break;
                }
            }
        }
        chain
    }
}

/// A scan window over the source, with the sectors that failed to read.
struct Window {
    start: u64,
    bytes: Vec<u8>,
    unreadable: Vec<Extent>,
    /// The scan range, outside which unreadable bytes are not reported.
    range: (u64, u64),
}

impl Window {
    fn new(start: u64, end: u64) -> Self {
        Self {
            start: 0,
            bytes: Vec::new(),
            unreadable: Vec::new(),
            range: (start, end),
        }
    }

    fn covers(&self, start: u64, end: u64) -> bool {
        self.start <= start && end <= self.start + self.bytes.len() as u64
    }

    /// Reads a window from the sector holding `position`. A failed read is
    /// retried per sector; failed sectors become zeros and are recorded.
    fn fill(&mut self, source: &dyn BlockRead, position: u64, size: u64, report: &mut Vec<Extent>) {
        self.start = position / SECTOR * SECTOR;
        let length = SCAN_WINDOW.min(size - self.start) as usize;
        self.bytes.resize(length, 0);
        self.unreadable.clear();
        if read_source(source, self.start, &mut self.bytes).is_ok() {
            return;
        }
        for (index, sector) in self.bytes.chunks_mut(SECTOR as usize).enumerate() {
            let offset = self.start + index as u64 * SECTOR;
            if read_source(source, offset, sector).is_err() {
                sector.fill(0);
                let failed = Extent::new(offset, sector.len() as u64);
                merge(&mut self.unreadable, failed);
                let reported = report.last().and_then(Extent::end).unwrap_or(0);
                let start = offset.max(self.range.0).max(reported);
                let end = (offset + failed.length).min(self.range.1);
                if start < end {
                    merge(report, Extent::new(start, end - start));
                }
            }
        }
    }

    fn readable(&self, offset: u64, length: u64) -> bool {
        self.unreadable.iter().all(|extent| {
            offset + length <= extent.offset || extent.end().is_some_and(|end| end <= offset)
        })
    }
}

/// Appends an extent, joining it to the last one when they touch.
fn merge(extents: &mut Vec<Extent>, extent: Extent) {
    match extents.last_mut() {
        Some(last) if last.end() == Some(extent.offset) => last.length += extent.length,
        _ => extents.push(extent),
    }
}

/// Damage findings streamed to the caller's sink, with a count.
struct Findings<'a> {
    sink: &'a mut dyn Sink<Damage>,
    reported: u64,
}

impl Findings<'_> {
    fn add(
        &mut self,
        kind: DamageKind,
        severity: Severity,
        location: Extent,
        structure: &str,
        detail: String,
    ) -> ControlFlow<()> {
        self.reported += 1;
        self.sink.accept(Damage {
            kind,
            severity,
            location,
            structure: structure.into(),
            detail,
        })
    }
}

fn candidate(set: EntrySet, position: u64) -> Candidate<EntrySetDetail> {
    Candidate {
        location: Extent::new(position, (set.slots * ENTRY_SIZE) as u64),
        kind: if set.is_dir {
            CandidateKind::Directory
        } else {
            CandidateKind::File
        },
        state: if set.in_use {
            CandidateState::Live
        } else {
            CandidateState::Deleted
        },
        integrity: if set.checksum_ok {
            Integrity::Verified
        } else {
            Integrity::Failed
        },
        name: Some(set.name),
        size: Some(set.length),
        detail: EntrySetDetail {
            attributes: set.attributes,
            first_cluster: set.first_cluster,
            valid_data_length: set.valid_length,
            no_fat_chain: set.no_fat_chain,
            created: set.created,
            modified: set.modified,
            accessed: set.accessed,
        },
    }
}

fn claim(owners: &mut HashMap<u32, Vec<Parent>>, runs: &[(u32, u64)], parent: Parent) {
    for &(first, count) in runs {
        for cluster in (u64::from(first)..u64::from(first) + count).map(|cluster| cluster as u32) {
            let parents = owners.entry(cluster).or_default();
            if !parents.contains(&parent) {
                parents.push(parent);
            }
        }
    }
}

/// A bounded, sector-aligned read through the device alignment adapter.
fn read_source(source: &dyn BlockRead, offset: u64, output: &mut [u8]) -> crate::Result<()> {
    let end = offset
        .checked_add(output.len() as u64)
        .ok_or(Error::Bounds)?;
    if end > source.size_bytes() {
        return Err(Error::Bounds);
    }
    AlignedDevice::new(source)
        .read_at(offset, output)
        .map_err(Error::Device)
}

fn read_boot_region(source: &dyn BlockRead, offset: u64, sector: u64) -> Option<BootRegion> {
    let mut region = vec![0; (sector * BOOT_REGION_SECTORS) as usize];
    read_source(source, offset, &mut region).ok()?;
    BootRegion::parse(&region, sector as usize).ok()
}

/// Reports a boot copy's damage and returns its bytes when they were read.
fn audit_boot_copy(
    source: &dyn BlockRead,
    offset: u64,
    sector: u64,
    structure: &str,
    out: &mut Findings<'_>,
) -> ControlFlow<(), Option<Vec<u8>>> {
    let length = sector * BOOT_REGION_SECTORS;
    let mut region = vec![0; length as usize];
    if let Err(error) = read_source(source, offset, &mut region) {
        out.add(
            DamageKind::Unreadable,
            Severity::Error,
            Extent::new(offset, length),
            structure,
            error.to_string(),
        )?;
        return ControlFlow::Continue(None);
    }
    let Ok(parsed) = BootRegion::parse(&region, sector as usize) else {
        return ControlFlow::Continue(None);
    };
    let header = &parsed.header;
    let fields = &header.sector;
    let mut checks = vec![];
    if !header.file_system_name_valid {
        checks.push((
            DamageKind::Signature,
            Severity::Error,
            Extent::new(offset + 3, 8),
            format!("filesystem name {:?}", fields.file_system_name),
        ));
    }
    if !header.jump_valid {
        checks.push((
            DamageKind::Signature,
            Severity::Warning,
            Extent::new(offset, 3),
            format!("jump instruction {:02x?}", &region[..3]),
        ));
    }
    if !fields.signature_ok {
        checks.push((
            DamageKind::Signature,
            Severity::Error,
            Extent::new(offset + 510, 2),
            format!("boot signature {:02x?}", &region[510..512]),
        ));
    }
    if !header.must_be_zero_valid {
        checks.push((
            DamageKind::Malformed,
            Severity::Warning,
            Extent::new(offset + 11, 53),
            "MustBeZero holds nonzero bytes".into(),
        ));
    }
    if Geometry::new(fields, None).is_none() {
        checks.push((
            DamageKind::Malformed,
            Severity::Error,
            Extent::new(offset + 64, 56),
            format!(
                "unusable geometry: sector shift {}, cluster shift {}, {} clusters",
                fields.bytes_per_sector_shift,
                fields.sectors_per_cluster_shift,
                fields.cluster_count
            ),
        ));
    }
    if !parsed.extended_signatures_valid {
        checks.push((
            DamageKind::Signature,
            Severity::Warning,
            Extent::new(offset + sector, sector * 8),
            "extended boot signatures missing".into(),
        ));
    }
    if !parsed.checksum_valid {
        checks.push((
            DamageKind::Checksum,
            Severity::Error,
            Extent::new(offset + sector * 11, sector),
            format!(
                "stored {:#010x}, computed {:#010x}",
                parsed.stored_checksum, parsed.computed_checksum
            ),
        ));
    }
    for (kind, severity, location, detail) in checks {
        out.add(kind, severity, location, structure, detail)?;
    }
    ControlFlow::Continue(Some(region))
}

/// Reports why a chain stopped before its structure was covered.
fn chain_damage(
    geometry: &Geometry,
    chain: &Chain,
    start: Extent,
    structure: &str,
    out: &mut Findings<'_>,
) -> ControlFlow<()> {
    // The FAT word that stopped the walk, or the field that named its start.
    let location = chain
        .last
        .map_or(start, |cluster| geometry.fat_word(cluster));
    let clusters = chain.clusters;
    let (kind, location, detail) = match chain.end {
        ChainEnd::Covered | ChainEnd::Terminated => (
            DamageKind::Broken,
            location,
            format!("ends after {clusters} clusters"),
        ),
        ChainEnd::Link(value) => (
            DamageKind::Broken,
            location,
            format!("links to {value:?} after {clusters} clusters"),
        ),
        ChainEnd::Cycle => (
            DamageKind::Broken,
            location,
            format!("returns to a used cluster after {clusters} clusters"),
        ),
        ChainEnd::OutOfRange => (
            DamageKind::Malformed,
            location,
            format!("leaves the cluster heap after {clusters} clusters"),
        ),
        ChainEnd::Unreadable(extent) => (
            DamageKind::Unreadable,
            extent,
            "the FAT could not be read".into(),
        ),
    };
    out.add(kind, Severity::Error, location, structure, detail)
}

/// Reads a walk's clusters, reporting and stopping at the first unreadable run.
fn read_runs(
    geometry: &Geometry,
    source: &dyn BlockRead,
    runs: &[(u32, u64)],
    structure: &str,
    out: &mut Findings<'_>,
) -> ControlFlow<(), Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    for extent in geometry.extents(runs, u64::MAX) {
        let start = bytes.len();
        bytes.resize(start + extent.length as usize, 0);
        if let Err(error) = read_source(source, extent.offset, &mut bytes[start..]) {
            out.add(
                DamageKind::Unreadable,
                Severity::Error,
                extent,
                structure,
                error.to_string(),
            )?;
            bytes.truncate(start);
            break;
        }
    }
    ControlFlow::Continue((!bytes.is_empty()).then_some(bytes))
}

fn audit_upcase(
    mapper: &mut Mapper<'_>,
    stream: SystemStream,
    checksum: u32,
    entry: Extent,
    out: &mut Findings<'_>,
) -> ControlFlow<()> {
    let structure = "upcase table";
    if stream.length == 0 || stream.length > UPCASE_LIMIT {
        return out.add(
            DamageKind::Malformed,
            Severity::Error,
            entry,
            structure,
            format!(
                "declares {} bytes; a table holds 1 to {UPCASE_LIMIT}",
                stream.length
            ),
        );
    }
    let geometry = mapper.geometry;
    let chain = mapper.chain(
        stream.first_cluster,
        stream.length.div_ceil(geometry.cluster),
        false,
    );
    if chain.end != ChainEnd::Covered {
        return chain_damage(geometry, &chain, entry, structure, out);
    }
    let location = geometry.extents(&chain.runs, stream.length)[0];
    let bytes = read_runs(geometry, mapper.source, &chain.runs, structure, out)?;
    let Some(bytes) = bytes
        .as_deref()
        .and_then(|bytes| bytes.get(..stream.length as usize))
    else {
        return ControlFlow::Continue(());
    };
    let table = RawUpcaseTable::parse(bytes, checksum);
    if !table.checksum_ok {
        out.add(
            DamageKind::Checksum,
            Severity::Error,
            location,
            structure,
            format!("stored {checksum:#010x}, computed {:#010x}", table.checksum),
        )?;
    }
    if let Some(issue) = table.issue {
        // Section 7.2.5: a compressed table's identity run, or the words
        // after the mapping of FFFFh.
        let at = table.issue_at.unwrap_or(0);
        let detail = match issue {
            UpcaseIssue::TruncatedRun => "an identity run with no count",
            UpcaseIssue::RunOutsideRange => "an identity run past FFFFh",
            UpcaseIssue::ExtraMappings => "a word after the mapping of FFFFh",
        };
        out.add(
            DamageKind::Malformed,
            Severity::Error,
            location,
            structure,
            format!("{detail} at byte {at} of the table"),
        )?;
    }
    ControlFlow::Continue(())
}
