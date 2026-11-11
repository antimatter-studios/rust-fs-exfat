//! The spec deviation scan behind `CheckReport::deviations` (#44).
//!
//! It walks what the strict audit walks, file data chains aside, through the
//! permissive primitives, and reports every rule of `docs/spec-rules.md` the
//! bytes break. It never fails and never decides anything: a structure it
//! cannot read or hold is skipped and named, and the strict verdict is the
//! audit's alone.

use super::MAX_AUDIT_MEMORY_BYTES;
use crate::device::Reader;
use crate::primitives::{
    backup_boot_deviations, bitmap_count, bitmap_reserved_byte, boot_region_deviations,
    checked_bitmap_bit, claim_bitmap_cluster, fat_head_deviations, mandatory_upcase,
    percent_in_use_deviation, system_allocation_deviations, BootSectorHeader, DeviationReport,
    DirectoryRules, EntryAllocation, FatValue, FatWindow, RawUpcaseTable, RootStreams, Rule,
    UpcaseIssue, VolumeRules, ENTRY_SIZE,
};
use fs_core::BlockRead;

const SECTOR_SIZES: [usize; 4] = [512, 1024, 2048, 4096];
const MAX_DIRECTORY_BYTES: u64 = 256 * 1024 * 1024;
/// Bitmap bytes past ClusterCount's that are read to check the Reserved bits.
const BITMAP_RESERVED_TAIL: u64 = 64 * 1024;

/// Lists every deviation from the specification found on `device`, whether
/// or not the strict reader accepts the volume. It never fails: what cannot
/// be checked is named in [`DeviationReport::skipped`].
pub fn spec_deviations<Device: BlockRead>(device: Device) -> DeviationReport {
    scan(&Reader::new(device), MAX_AUDIT_MEMORY_BYTES)
}

pub(super) fn scan<Device: BlockRead>(
    reader: &Reader<Device>,
    max_memory_bytes: usize,
) -> DeviationReport {
    let mut out = DeviationReport::default();
    let Some(geometry) = boot(reader, &mut out) else {
        return out;
    };
    let fat_start = geometry.fat_start();
    let mut scan = Scan {
        reader,
        fat: FatWindow::new(fat_start, geometry.fat_bytes, geometry.cluster_count),
        geometry,
        out,
        memory: max_memory_bytes,
        bitmap: None,
        visited: None,
        buffer: Vec::new(),
    };
    scan.run();
    scan.out
}

/// The geometry the scan walks with, from a boot sector usable enough to find
/// the FAT and the cluster heap.
struct Geometry {
    cluster_size: u64,
    fat_offset: u64,
    fat_bytes: u64,
    heap: u64,
    cluster_count: u32,
    root: u32,
    number_of_fats: u8,
    revision: u16,
    /// From the main boot sector, the only current copy (section 3.1.13).
    active_fat: u64,
    /// From the main boot sector (section 3.1.18).
    percent_in_use: u8,
}

impl Geometry {
    fn read(region: &[u8], sector: usize, device: u64) -> Option<Self> {
        let fields = BootSectorHeader::parse(region).ok()?.sector;
        let shift = fields.bytes_per_sector_shift;
        if 1usize.checked_shl(u32::from(shift)) != Some(sector)
            || fields.sectors_per_cluster_shift > 25 - shift
            || !matches!(fields.number_of_fats, 1 | 2)
            || !(1..=0xFFFF_FFF5).contains(&fields.cluster_count)
            || !(2..u64::from(fields.cluster_count) + 2).contains(&u64::from(fields.root_cluster))
        {
            return None;
        }
        let sector = sector as u64;
        let fat_bytes = u64::from(fields.fat_length) * sector;
        let fats_end = (u64::from(fields.fat_offset)
            + u64::from(fields.fat_length) * u64::from(fields.number_of_fats))
            * sector;
        if fat_bytes < (u64::from(fields.cluster_count) + 2) * 4 || fats_end > device {
            return None;
        }
        Some(Self {
            cluster_size: sector << fields.sectors_per_cluster_shift,
            fat_offset: u64::from(fields.fat_offset) * sector,
            fat_bytes,
            heap: u64::from(fields.cluster_heap_offset) * sector,
            cluster_count: fields.cluster_count,
            root: fields.root_cluster,
            number_of_fats: fields.number_of_fats,
            revision: fields.revision,
            active_fat: 0,
            percent_in_use: 0,
        })
    }

    fn fat_start(&self) -> u64 {
        self.fat_offset + self.active_fat * self.fat_bytes
    }

    fn in_heap(&self, cluster: u32) -> bool {
        (2..u64::from(self.cluster_count) + 2).contains(&u64::from(cluster))
    }

    fn cluster_offset(&self, cluster: u32) -> u64 {
        self.heap + u64::from(cluster - 2) * self.cluster_size
    }

    fn rules<'a>(&self, upcase: Option<&'a [u16]>) -> VolumeRules<'a> {
        VolumeRules {
            cluster_count: self.cluster_count,
            cluster_size: self.cluster_size,
            number_of_fats: self.number_of_fats,
            revision: self.revision,
            upcase,
        }
    }
}

/// Checks both boot regions and returns the geometry to walk with.
fn boot<Device: BlockRead>(reader: &Reader<Device>, out: &mut DeviationReport) -> Option<Geometry> {
    let mut prefix = [0; 512];
    if reader.read_at(0, &mut prefix).is_err() {
        out.skip("the volume: the main boot sector cannot be read");
        return None;
    }
    let named = |shift: u8| {
        1usize
            .checked_shl(u32::from(shift))
            .filter(|size| SECTOR_SIZES.contains(size))
    };
    // A main boot sector without a valid BytesPerSectorShift leaves the backup
    // to name the sector size; with neither, the main one is checked as 512.
    let sector = named(prefix[108]).or_else(|| {
        SECTOR_SIZES.into_iter().find(|size| {
            let mut backup = [0; 512];
            reader.read_at(12 * *size as u64, &mut backup).is_ok()
                && named(backup[108]) == Some(*size)
        })
    });
    let size = sector.unwrap_or(512);
    let mut data = vec![0; size * 24];
    if reader.read_at(0, &mut data).is_err() {
        out.skip("the volume: the boot regions cannot be read");
        return None;
    }
    let (main, backup) = data.split_at(size * 12);
    let backup_base = (size * 12) as u64;
    boot_region_deviations(main, size, 0, true, out);
    boot_region_deviations(backup, size, backup_base, false, out);
    backup_boot_deviations(main, backup, backup_base, out);
    let device = reader.size_bytes();
    let geometry = sector.and_then(|sector| {
        Geometry::read(main, sector, device).or_else(|| Geometry::read(backup, sector, device))
    });
    let Some(mut geometry) = geometry else {
        out.skip("the volume: neither boot sector describes a usable geometry");
        return None;
    };
    // Only the main boot sector's VolumeFlags and PercentInUse are current.
    let current = BootSectorHeader::parse(main).ok()?.sector;
    if geometry.number_of_fats == 2 {
        geometry.active_fat = u64::from(current.volume_flags & 1);
    }
    geometry.percent_in_use = current.percent_in_use;
    Some(geometry)
}

/// How a walk of an allocation ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    /// FFFFFFFFh, or the last cluster of a contiguous allocation.
    Complete,
    /// The walk reached the caller's limit before the chain ended.
    Limit,
    /// A fault already reported, or a FAT entry that could not be read.
    Broken,
}

struct Walk {
    clusters: Vec<u32>,
    end: End,
}

/// The active Allocation Bitmap as far as it could be read.
struct Bitmap {
    bytes: Vec<u8>,
    clusters: Vec<u32>,
}

struct Scan<'reader, Device: BlockRead> {
    reader: &'reader Reader<Device>,
    geometry: Geometry,
    fat: FatWindow,
    out: DeviationReport,
    memory: usize,
    bitmap: Option<Bitmap>,
    /// Clusters already scanned, so a loop of directories ends.
    visited: Option<Vec<u8>>,
    buffer: Vec<u8>,
}

impl<Device: BlockRead> Scan<'_, Device> {
    fn run(&mut self) {
        let mut head = [0; 8];
        let fat_start = self.geometry.fat_start();
        match self.reader.read_at(fat_start, &mut head) {
            Ok(()) => fat_head_deviations(&head, fat_start, &mut self.out),
            Err(_) => self.out.skip("FatEntry[0] and FatEntry[1]: unreadable"),
        }
        let root_limit = MAX_DIRECTORY_BYTES / self.geometry.cluster_size + 1;
        let root = self.walk(self.geometry.root, None, root_limit, "the root directory");
        if root.end == End::Limit {
            self.out.skip("the root directory past 256 MB");
        }
        let bytes = u64::from(self.geometry.cluster_count).div_ceil(8) as usize;
        let mut visited = Vec::new();
        if bytes <= self.memory && visited.try_reserve_exact(bytes).is_ok() {
            visited.resize(bytes, 0);
            self.visited = Some(visited);
            self.memory -= bytes;
        } else {
            self.out
                .skip("subdirectories: the visited-cluster map exceeds the memory budget");
        }
        let streams = self.root_streams(&root.clusters);
        let basic = self.geometry.rules(None);
        for allocation in streams.bitmaps.iter().flatten() {
            system_allocation_deviations(&basic, allocation, &mut self.out);
        }
        if let Some((allocation, _)) = &streams.upcase {
            system_allocation_deviations(&basic, allocation, &mut self.out);
        }
        self.load_bitmap(&streams);
        let upcase = self.load_upcase(&streams);
        self.claim(&root.clusters, "the root directory");
        let rules = self.geometry.rules(upcase.as_deref());
        let mut pending = self.directory(&root.clusters, None, rules, true);
        if self.visited.is_none() {
            return;
        }
        while let Some(directory) = pending.pop() {
            pending.extend(self.subdirectory(directory, rules));
        }
    }

    /// Walks an allocation from `first` for at most `limit` clusters,
    /// reporting faults of FAT chains (section 4.1.3) on the way.
    fn walk(&mut self, first: u32, contiguous: Option<u64>, limit: u64, owner: &str) -> Walk {
        let mut clusters = Vec::new();
        if !self.geometry.in_heap(first) {
            return Walk {
                clusters,
                end: End::Broken,
            };
        }
        if let Some(count) = contiguous {
            let last = (u64::from(first) + count.min(limit))
                .min(u64::from(self.geometry.cluster_count) + 2);
            clusters.extend(first..last as u32);
            let end = if count > limit {
                End::Limit
            } else {
                End::Complete
            };
            return Walk { clusters, end };
        }
        let fat_start = self.geometry.fat_start();
        let (mut checkpoint, mut power, mut distance) = (first, 1u64, 0u64);
        let mut current = first;
        loop {
            clusters.push(current);
            if clusters.len() as u64 >= limit {
                return Walk {
                    clusters,
                    end: End::Limit,
                };
            }
            let reader = self.reader;
            let Ok(entry) = self
                .fat
                .entry(current, |offset, output| reader.read_at(offset, output))
            else {
                self.out.skip(format!(
                    "{owner}: the FAT entry of cluster {current} cannot be read"
                ));
                return Walk {
                    clusters,
                    end: End::Broken,
                };
            };
            let at = fat_start + u64::from(current) * 4;
            match entry.kind {
                FatValue::End => {
                    return Walk {
                        clusters,
                        end: End::Complete,
                    }
                }
                FatValue::Next(next) => {
                    distance += 1;
                    if next == checkpoint {
                        self.out.report(Rule::FatChainCycle, at, 4, || {
                            format!("cluster {current} of {owner} links back to cluster {next}")
                        });
                        // The loop may have been walked more than once before
                        // it was found; each cluster counts once.
                        let mut seen = std::collections::HashSet::new();
                        let first_repeat =
                            clusters.iter().position(|cluster| !seen.insert(*cluster));
                        clusters.truncate(first_repeat.unwrap_or(clusters.len()));
                        return Walk {
                            clusters,
                            end: End::Broken,
                        };
                    }
                    if distance == power {
                        checkpoint = next;
                        power = power.saturating_mul(2);
                        distance = 0;
                    }
                    current = next;
                }
                FatValue::Bad => {
                    self.out.report(Rule::FatChainEnd, at, 4, || {
                        format!("cluster {current} of {owner} is marked bad inside its chain")
                    });
                    return Walk {
                        clusters,
                        end: End::Broken,
                    };
                }
                FatValue::Free | FatValue::Reserved => {
                    let value = entry.value;
                    self.out.report(Rule::FatChainValue, at, 4, || {
                        format!("cluster {current} of {owner} links to {value:08X}h")
                    });
                    return Walk {
                        clusters,
                        end: End::Broken,
                    };
                }
            }
        }
    }

    /// Reads `cluster` into the scan's buffer, or names it as skipped.
    fn read_cluster(&mut self, cluster: u32, owner: &str) -> bool {
        let size = self.geometry.cluster_size as usize;
        if self.buffer.len() != size {
            self.buffer = vec![0; size];
        }
        let offset = self.geometry.cluster_offset(cluster);
        if self.reader.read_at(offset, &mut self.buffer).is_err() {
            self.out
                .skip(format!("{owner}: cluster {cluster} cannot be read"));
            return false;
        }
        true
    }

    /// The root's first Allocation Bitmap and Up-case Table entries, before its
    /// end-of-directory marker.
    fn root_streams(&mut self, clusters: &[u32]) -> RootStreams {
        let mut streams = RootStreams::default();
        for cluster in clusters {
            if !self.read_cluster(*cluster, "the root directory") {
                break;
            }
            let base = self.geometry.cluster_offset(*cluster);
            for (index, entry) in self.buffer.chunks_exact(ENTRY_SIZE).enumerate() {
                let entry: &[u8; ENTRY_SIZE] = entry.try_into().unwrap();
                let offset = base + (index * ENTRY_SIZE) as u64;
                if streams.observe(offset, entry).is_break() {
                    return streams;
                }
            }
        }
        streams
    }

    /// Reads a system stream of `length` bytes. A chain that ends early is a
    /// `data-length` deviation; the bytes read so far are returned.
    fn system_stream(
        &mut self,
        allocation: &EntryAllocation,
        length: u64,
        owner: &str,
    ) -> Option<(Vec<u8>, Vec<u32>, bool)> {
        let cluster_size = self.geometry.cluster_size;
        let walk = self.walk(
            allocation.first_cluster,
            None,
            length.div_ceil(cluster_size),
            owner,
        );
        let held = walk.clusters.len() as u64 * cluster_size;
        if walk.end == End::Complete && held < allocation.data_length.min(length) {
            let data_length = allocation.data_length;
            self.out
                .report(Rule::DataLength, allocation.entry + 24, 8, || {
                    format!("{owner}'s chain holds {held} bytes of DataLength {data_length}")
                });
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length as usize).ok()?;
        for cluster in &walk.clusters {
            if !self.read_cluster(*cluster, owner) {
                return Some((bytes, walk.clusters, false));
            }
            let take = (length - bytes.len() as u64).min(cluster_size) as usize;
            bytes.extend_from_slice(&self.buffer[..take]);
        }
        let complete = bytes.len() as u64 == length;
        self.claim(&walk.clusters, owner);
        Some((bytes, walk.clusters, complete))
    }

    fn load_bitmap(&mut self, streams: &RootStreams) {
        let active = self.geometry.active_fat as usize;
        let Some(allocation) = streams.bitmaps[active] else {
            self.out
                .skip("the Allocation Bitmap: the root has no entry for the active bitmap");
            return;
        };
        let needed = u64::from(self.geometry.cluster_count).div_ceil(8);
        if allocation.data_length < needed {
            let length = allocation.data_length;
            self.out
                .report(Rule::BitmapLength, allocation.entry + 24, 8, || {
                    format!("DataLength {length}, fewer than the {needed} bytes ClusterCount needs")
                });
        }
        let length = allocation.data_length.min(needed + BITMAP_RESERVED_TAIL);
        if length > self.memory as u64 {
            self.out
                .skip("the Allocation Bitmap: it exceeds the memory budget");
            return;
        }
        let Some((bytes, clusters, _)) =
            self.system_stream(&allocation, length, "the Allocation Bitmap")
        else {
            self.out
                .skip("the Allocation Bitmap: it exceeds the memory budget");
            return;
        };
        self.memory -= length as usize;
        if let Some(position) = bitmap_reserved_byte(&bytes, self.geometry.cluster_count) {
            let at = self.stream_offset(&clusters, position as u64);
            let value = bytes[position];
            self.out.report(Rule::BitmapReserved, at, 1, || {
                format!("bitmap byte {position} is {value:02X}h past ClusterCount's bits")
            });
        }
        let whole = bytes.len() as u64 >= needed;
        if whole {
            let allocated = bitmap_count(&bytes, self.geometry.cluster_count);
            percent_in_use_deviation(
                self.geometry.percent_in_use,
                allocated,
                self.geometry.cluster_count,
                112,
                &mut self.out,
            );
        } else {
            self.out
                .skip("PercentInUse: the Allocation Bitmap could not be read whole");
        }
        let bitmap_clusters = clusters.clone();
        self.bitmap = Some(Bitmap { bytes, clusters });
        self.claim(&bitmap_clusters, "the Allocation Bitmap");
    }

    fn load_upcase(&mut self, streams: &RootStreams) -> Option<Vec<u16>> {
        let skip = |out: &mut DeviationReport| {
            out.skip("name hashes and duplicate names: no usable Up-case Table");
        };
        let Some((allocation, checksum)) = streams.upcase else {
            skip(&mut self.out);
            return None;
        };
        let length = allocation.data_length;
        // Sections 7.2.4, 7.2.5 and 7.2.5.1 set no maximum DataLength (#76):
        // a compressed table may be longer than an uncompressed one. Only an
        // empty stream or a partial word cannot hold the mappings; a longer
        // read is bounded by the chain and the memory budget.
        if length == 0 || length % 2 != 0 {
            self.out
                .report(Rule::UpcaseCoverage, allocation.entry + 24, 8, || {
                    format!("DataLength {length} cannot hold mappings for exactly 0000h to FFFFh")
                });
            skip(&mut self.out);
            return None;
        }
        if length > self.memory as u64 {
            self.out.skip(
                "name hashes and duplicate names: the Up-case Table exceeds the memory budget",
            );
            return None;
        }
        let Some((bytes, clusters, true)) =
            self.system_stream(&allocation, length, "the Up-case Table")
        else {
            skip(&mut self.out);
            return None;
        };
        let at = self.stream_offset(&clusters, 0);
        let table = RawUpcaseTable::parse(&bytes, checksum);
        if !table.checksum_ok {
            let computed = table.checksum;
            self.out
                .report(Rule::UpcaseChecksum, allocation.entry + 4, 4, || {
                    format!("TableChecksum {checksum:08X}h, computed {computed:08X}h")
                });
        }
        let mapped = table.mappings.len();
        let cluster_size = self.geometry.cluster_size;
        match (table.issue, table.issue_at) {
            (Some(UpcaseIssue::ExtraMappings), Some(position)) => {
                let extra = bytes.len() - position;
                let offset = self.stream_offset(&clusters, position as u64);
                self.out.report(Rule::UpcaseCoverage, offset, 2, || {
                    format!("{extra} bytes follow the mapping of FFFFh")
                });
            }
            // A compressed table's identity run that cannot be expanded
            // (section 7.2.5): its FFFFh, with the count after it when the
            // stream holds one in the same cluster. Coverage past it is
            // unknown, so it is not reported as well.
            (Some(issue), Some(position)) => {
                let offset = self.stream_offset(&clusters, position as u64);
                let (span, detail) = if issue == UpcaseIssue::TruncatedRun {
                    (2, "ends the table with no count of identity mappings")
                } else {
                    let room = cluster_size - position as u64 % cluster_size;
                    (4.min(room), "has a count that runs past FFFFh")
                };
                self.out.report(Rule::UpcaseRun, offset, span, || {
                    format!("the identity run at character {mapped:04X}h {detail}")
                });
            }
            _ if !table.complete => {
                let first = length.min(cluster_size);
                self.out.report(Rule::UpcaseCoverage, at, first, || {
                    format!("the {length}-byte table maps {mapped} characters, not 65536")
                });
            }
            _ => {}
        }
        // Table 24 is broken by a decoded mapping of the first 128 that
        // differs, or by a table that ends before them. A faulty run before
        // them leaves the rest unknown, not wrong.
        let stopped = matches!(
            table.issue,
            Some(UpcaseIssue::TruncatedRun | UpcaseIssue::RunOutsideRange)
        );
        let differs = (0u16..128)
            .zip(&table.mappings)
            .any(|(unit, mapped)| *mapped != mandatory_upcase(unit));
        if differs || (!table.ascii_ok && !stopped) {
            // The words that encode the first 128 mappings, fewer than 256
            // bytes when identity runs cover some of them.
            let span = (table.mandatory_bytes as u64).min(cluster_size);
            self.out.report(Rule::UpcaseMandatory, at, span, || {
                "a mapping of the first 128 characters differs from Table 24".into()
            });
        }
        if table.checksum_ok && table.complete && table.issue.is_none() {
            Some(table.mappings)
        } else {
            skip(&mut self.out);
            None
        }
    }

    /// The volume offset of byte `position` of a stream stored in `clusters`.
    fn stream_offset(&self, clusters: &[u32], position: u64) -> u64 {
        let size = self.geometry.cluster_size;
        clusters
            .get((position / size) as usize)
            .map_or(0, |cluster| {
                self.geometry.cluster_offset(*cluster) + position % size
            })
    }

    /// Checks that `owner`'s clusters are allocated in the bitmap (section
    /// 7.1.5.1) and marks them scanned.
    fn claim(&mut self, clusters: &[u32], owner: &str) {
        for cluster in clusters {
            if let Some(visited) = &mut self.visited {
                claim_bitmap_cluster(visited, *cluster);
            }
            let Some(bitmap) = &self.bitmap else {
                continue;
            };
            let index = u64::from(cluster - 2) / 8;
            if index >= bitmap.bytes.len() as u64 || checked_bitmap_bit(&bitmap.bytes, *cluster) {
                continue;
            }
            let at = self.stream_offset(&bitmap.clusters, index);
            self.out.report(Rule::BitmapFreeCluster, at, 1, || {
                format!("cluster {cluster} of {owner} is marked free")
            });
        }
    }

    /// Feeds a directory's entries to the rules, up to `length` bytes, or the
    /// whole chain for the root.
    fn directory(
        &mut self,
        clusters: &[u32],
        length: Option<u64>,
        rules: VolumeRules<'_>,
        root: bool,
    ) -> Vec<EntryAllocation> {
        let owner = if root {
            "the root directory"
        } else {
            "a directory"
        };
        let mut checker = DirectoryRules::new(rules, root);
        let mut remaining = length.unwrap_or(u64::MAX);
        for cluster in clusters {
            if remaining == 0 || !self.read_cluster(*cluster, owner) {
                break;
            }
            let base = self.geometry.cluster_offset(*cluster);
            let take = remaining.min(self.geometry.cluster_size) as usize;
            remaining -= take as u64;
            for (index, entry) in self.buffer[..take].chunks_exact(ENTRY_SIZE).enumerate() {
                let entry: &[u8; ENTRY_SIZE] = entry.try_into().unwrap();
                checker.entry(base + (index * ENTRY_SIZE) as u64, entry, &mut self.out);
            }
        }
        let outcome = checker.finish(&mut self.out);
        if outcome.names_skipped {
            self.out
                .skip("duplicate names: a directory holds more names than the budget");
        }
        outcome.subdirectories
    }

    /// Walks and checks one subdirectory, returning the directories it holds.
    fn subdirectory(
        &mut self,
        stream: EntryAllocation,
        rules: VolumeRules<'_>,
    ) -> Vec<EntryAllocation> {
        let visited = self
            .visited
            .as_ref()
            .expect("subdirectories need the visited map");
        if stream.data_length == 0 || checked_bitmap_bit(visited, stream.first_cluster) {
            return Vec::new();
        }
        let cluster_size = self.geometry.cluster_size;
        let length = stream.data_length.min(MAX_DIRECTORY_BYTES);
        let wanted = length.div_ceil(cluster_size);
        let walk = if stream.no_fat_chain {
            self.walk(stream.first_cluster, Some(wanted), wanted, "a directory")
        } else {
            self.walk(stream.first_cluster, None, wanted + 1, "a directory")
        };
        let mut clusters = walk.clusters;
        let declared = stream.data_length;
        let whole = declared.is_multiple_of(cluster_size) && declared <= MAX_DIRECTORY_BYTES;
        let held = clusters.len() as u64;
        if whole
            && !stream.no_fat_chain
            && (walk.end == End::Limit || walk.end == End::Complete && held != wanted)
        {
            self.out
                .report(Rule::DirectoryLength, stream.entry + 24, 8, || {
                    let chain = if walk.end == End::Limit {
                        "more than".to_string()
                    } else {
                        held.to_string()
                    };
                    format!("DataLength {declared} covers {wanted} clusters; the chain has {chain}")
                });
        }
        clusters.truncate(wanted as usize);
        let visited = self
            .visited
            .as_ref()
            .expect("subdirectories need the visited map");
        if let Some(shared) = clusters
            .iter()
            .position(|cluster| checked_bitmap_bit(visited, *cluster))
        {
            clusters.truncate(shared);
        }
        self.claim(&clusters, "a directory");
        let readable = (clusters.len() as u64 * cluster_size).min(length);
        self.directory(&clusters, Some(readable), rules, false)
    }
}
