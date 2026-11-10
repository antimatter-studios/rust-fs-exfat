use super::{
    boot_checksum, checked_fat_entry, stored_boot_checksum, u32_at, BootSector, FAT_BAD, FAT_END,
    FIRST_CLUSTER,
};
use crate::{Error, Result};
use serde::Serialize;
use std::collections::HashSet;

pub const ALLOC_ALLOCATED: u8 = 1;
pub const ALLOC_FREE_FAT: u8 = 2;
pub const ALLOC_FREE_ZERO: u8 = 3;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainGroup {
    pub chains: u64,
    pub clusters: u64,
    pub longest: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AllocationStats {
    pub allocated: u64,
    pub free_with_fat: u64,
    pub free_zero_fat: u64,
    pub cross_links: u64,
    pub free_chains: ChainGroup,
    pub allocated_chains: ChainGroup,
    pub mixed_chains: ChainGroup,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootCheck {
    pub main_stored: u32,
    pub main_computed: u32,
    pub backup_stored: u32,
    pub backup_computed: u32,
    pub regions_identical: bool,
}

pub struct StreamClusters {
    pub clusters: Vec<u32>,
    pub needed: u64,
}

/// The meaning of a FAT word, without rejecting free, damaged or reserved links.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FatValue {
    Free,
    Next(u32),
    Bad,
    End,
    Reserved,
}

/// A raw FAT word and its classification against the supplied cluster count.
/// Free and bad markers are valid words, not promises of a usable chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FatEntryReport {
    pub value: u32,
    pub kind: FatValue,
    pub valid: bool,
}

pub fn decode_fat_entry(value: u32, cluster_count: u32) -> FatEntryReport {
    let kind = match value {
        0 => FatValue::Free,
        FAT_BAD => FatValue::Bad,
        FAT_END => FatValue::End,
        next if (u64::from(FIRST_CLUSTER)..u64::from(cluster_count) + 2)
            .contains(&u64::from(next)) =>
        {
            FatValue::Next(next)
        }
        _ => FatValue::Reserved,
    };
    FatEntryReport {
        value,
        kind,
        valid: kind != FatValue::Reserved,
    }
}

/// Reads one raw FAT word from an already selected FAT's byte offset. Only
/// arithmetic and I/O failures are errors; damaged words remain reports.
pub fn read_fat_entry(
    fat_offset: u64,
    cluster: u32,
    cluster_count: u32,
    mut read: impl FnMut(u64, &mut [u8]) -> Result<()>,
) -> Result<FatEntryReport> {
    let offset = fat_offset
        .checked_add(u64::from(cluster) * 4)
        .ok_or(Error::Bounds)?;
    let mut bytes = [0; 4];
    read(offset, &mut bytes)?;
    Ok(decode_fat_entry(
        checked_fat_entry(&bytes, 0).unwrap(),
        cluster_count,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainPosition {
    pub cluster: u32,
    pub valid: bool,
    pub cycle: bool,
}

/// Positions precede their FAT reads, allowing a caller to enforce its own
/// budget and validity policy before any allocation or I/O for that position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FatChainStep {
    Cluster(ChainPosition),
    Entry { cluster: u32, entry: FatEntryReport },
}

/// Walks a FAT lazily, retaining raw words and reporting the first repeated or
/// out-of-range position. Free, bad, reserved and end words finish the walk.
/// No FAT snapshot or caller-owned cluster list is allocated here.
pub fn walk_fat_chain(
    first: u32,
    cluster_count: u32,
    mut read: impl FnMut(u32) -> Result<FatEntryReport>,
) -> impl Iterator<Item = Result<FatChainStep>> {
    let mut current = Some(first);
    let mut pending = None;
    let mut seen = HashSet::new();
    std::iter::from_fn(move || {
        if let Some(cluster) = pending.take() {
            seen.insert(cluster);
            return Some(match read(cluster) {
                Ok(entry) => {
                    if let FatValue::Next(next) = entry.kind {
                        current = Some(next);
                    }
                    Ok(FatChainStep::Entry { cluster, entry })
                }
                Err(error) => Err(error),
            });
        }
        let cluster = current.take()?;
        let valid =
            (u64::from(FIRST_CLUSTER)..u64::from(cluster_count) + 2).contains(&u64::from(cluster));
        let cycle = seen.contains(&cluster);
        if valid && !cycle {
            pending = Some(cluster);
        }
        Some(Ok(FatChainStep::Cluster(ChainPosition {
            cluster,
            valid,
            cycle,
        })))
    })
}

/// A stream's raw contiguous claim, without clipping it to the cluster heap.
/// The count is bounded by the on-disk length divided by the cluster size.
pub fn contiguous_clusters(first: u32, count: u64) -> std::ops::Range<u64> {
    u64::from(first)..u64::from(first) + count
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClusterRead {
    pub cluster_index: u64,
    pub within: u64,
    pub output: std::ops::Range<usize>,
}

/// Maps an already bounded stream read into cluster-sized chunks, without
/// looking up clusters or applying allocation, length or ValidDataLength policy.
/// A zero cluster size yields no chunks; callers assess geometry themselves.
pub fn cluster_reads(
    offset: u64,
    length: usize,
    cluster_size: u64,
) -> impl Iterator<Item = ClusterRead> {
    let mut copied = 0;
    std::iter::from_fn(move || {
        if copied == length || cluster_size == 0 {
            return None;
        }
        let position = offset + copied as u64;
        let within = position % cluster_size;
        let count = (cluster_size - within).min((length - copied) as u64) as usize;
        let output = copied..copied + count;
        copied += count;
        Some(ClusterRead {
            cluster_index: position / cluster_size,
            within,
            output,
        })
    })
}

/// The chain ends with the FAT's end marker.
pub const ISLAND_END_EOF: &str = "eof";
/// The chain stops at a zero, bad or out-of-range FAT entry.
pub const ISLAND_END_BROKEN: &str = "broken";
/// The chain runs into a cluster already reached from another head.
pub const ISLAND_END_JOINS_ANOTHER: &str = "joins-another";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Island {
    pub head: u32,
    pub tail: u32,
    pub length: u64,
    pub allocated: u64,
    pub end: &'static str,
}

/// Compares complete boot-region copies without rejecting damaged checksums or
/// ignoring mutable bytes when deciding whether the copies are identical.
pub fn boot_check(main: &[u8], backup: &[u8], bytes_per_sector: usize) -> BootCheck {
    BootCheck {
        main_stored: stored_boot_checksum(main, bytes_per_sector),
        main_computed: boot_checksum(main, bytes_per_sector),
        backup_stored: stored_boot_checksum(backup, bytes_per_sector),
        backup_computed: boot_checksum(backup, bytes_per_sector),
        regions_identical: main == backup,
    }
}

/// The FAT's entries, padded to cover every cluster of the volume. Trailing
/// partial words are ignored; extra entries and damaged values are retained.
pub fn decode_fat(raw: &[u8], boot: &BootSector) -> Vec<u32> {
    let mut fat: Vec<u32> = raw.chunks_exact(4).map(|entry| u32_at(entry, 0)).collect();
    let last = boot.last_cluster_exclusive();
    if fat.len() < last as usize {
        fat.resize(last as usize, 0);
    }
    fat
}

/// The root directory's clusters, stopping before an invalid or repeated cluster.
pub fn root_chain(boot: &BootSector, fat: &[u32]) -> Vec<u32> {
    let last = boot.last_cluster_exclusive();
    let mut clusters = Vec::new();
    let mut current = boot.root_cluster;
    let mut seen = HashSet::new();
    while (FIRST_CLUSTER..last).contains(&current) && seen.insert(current) {
        clusters.push(current);
        current = fat[current as usize];
    }
    clusters
}

pub fn valid_cluster(boot: &BootSector, cluster: u32) -> bool {
    (FIRST_CLUSTER..boot.last_cluster_exclusive()).contains(&cluster)
}

/// Reads the raw allocation bit, without validating the cluster or bitmap length.
pub fn is_allocated(bitmap: &[u8], cluster: u32) -> bool {
    let index = (cluster - FIRST_CLUSTER) as usize;
    bitmap[index / 8] >> (index % 8) & 1 == 1
}

fn fat_in_use(boot: &BootSector, fat: &[u32], cluster: u32) -> bool {
    let value = fat[cluster as usize];
    value == FAT_END || valid_cluster(boot, value)
}

/// Non-zero FAT entries count here, including values outside the valid range.
pub fn allocation_class(fat: &[u32], bitmap: &[u8], cluster: u32) -> u8 {
    if is_allocated(bitmap, cluster) {
        ALLOC_ALLOCATED
    } else if fat[cluster as usize] != 0 {
        ALLOC_FREE_FAT
    } else {
        ALLOC_FREE_ZERO
    }
}

/// The byte offset in the cluster heap, without validating the cluster number.
pub fn cluster_offset(boot: &BootSector, cluster: u32) -> u64 {
    boot.heap_offset() + u64::from(cluster - FIRST_CLUSTER) * boot.cluster_size()
}

/// Clusters a stream claims. `clusters` is shorter than `needed` when the
/// chain ends early or the claim runs off the end of the volume.
pub fn clusters_for(
    boot: &BootSector,
    fat: &[u32],
    first: u32,
    length: u64,
    no_fat_chain: bool,
) -> StreamClusters {
    let needed = length.div_ceil(boot.cluster_size());
    if needed == 0 || !valid_cluster(boot, first) || needed > u64::from(boot.cluster_count) {
        return StreamClusters {
            clusters: Vec::new(),
            needed,
        };
    }
    if no_fat_chain {
        let end = (u64::from(first) + needed).min(u64::from(boot.last_cluster_exclusive()));
        return StreamClusters {
            clusters: (first..end as u32).collect(),
            needed,
        };
    }
    let mut clusters = Vec::with_capacity(needed.min(1024) as usize);
    let mut seen = HashSet::new();
    let mut current = first;
    while valid_cluster(boot, current) && (clusters.len() as u64) < needed && seen.insert(current) {
        clusters.push(current);
        current = fat[current as usize];
    }
    StreamClusters { clusters, needed }
}

/// One FAT entry that points at this cluster, if any, from a precomputed index.
pub fn predecessor(predecessors: &[u32], cluster: u32) -> Option<u32> {
    match predecessors[cluster as usize] {
        0 => None,
        value => Some(value),
    }
}

/// Walks the FAT backward to a head or the first repeated cluster in a cycle.
pub fn chain_head(predecessors: &[u32], cluster: u32) -> u32 {
    let mut seen = HashSet::new();
    let mut current = cluster;
    while seen.insert(current) {
        match predecessor(predecessors, current) {
            Some(previous) => current = previous,
            None => break,
        }
    }
    current
}

/// Builds a predecessor index, choosing the lowest source when links converge.
pub fn compute_predecessors(boot: &BootSector, fat: &[u32]) -> Vec<u32> {
    let last = boot.last_cluster_exclusive();
    let mut predecessors = vec![0u32; last as usize];
    for cluster in FIRST_CLUSTER..last {
        let value = fat[cluster as usize];
        if valid_cluster(boot, value) && predecessors[value as usize] == 0 {
            predecessors[value as usize] = cluster;
        }
    }
    predecessors
}

/// Counts allocation classes and cross-link targets, grouping only headed islands.
pub fn compute_stats(boot: &BootSector, fat: &[u32], bitmap: &[u8]) -> AllocationStats {
    let last = boot.last_cluster_exclusive();
    let mut stats = AllocationStats::default();
    let mut targeted = vec![0u8; last as usize];
    for cluster in FIRST_CLUSTER..last {
        match allocation_class(fat, bitmap, cluster) {
            ALLOC_ALLOCATED => stats.allocated += 1,
            ALLOC_FREE_FAT => stats.free_with_fat += 1,
            _ => stats.free_zero_fat += 1,
        }
        let value = fat[cluster as usize];
        if valid_cluster(boot, value) {
            targeted[value as usize] = targeted[value as usize].saturating_add(1);
        }
    }
    stats.cross_links = targeted.iter().filter(|&&count| count > 1).count() as u64;
    for island in walk_islands(boot, fat, bitmap, &targeted) {
        let group = if island.allocated == 0 {
            &mut stats.free_chains
        } else if island.allocated == island.length {
            &mut stats.allocated_chains
        } else {
            &mut stats.mixed_chains
        };
        group.chains += 1;
        group.clusters += island.length;
        group.longest = group.longest.max(island.length);
    }
    stats
}

/// Splits the FAT into islands: runs that start at an entry nothing points
/// to, and end where the chain stops or meets a cluster already walked.
pub fn chain_islands(boot: &BootSector, fat: &[u32], bitmap: &[u8]) -> Vec<Island> {
    let last = boot.last_cluster_exclusive();
    let mut targeted = vec![0u8; last as usize];
    for cluster in FIRST_CLUSTER..last {
        let value = fat[cluster as usize];
        if valid_cluster(boot, value) {
            targeted[value as usize] = targeted[value as usize].saturating_add(1);
        }
    }
    walk_islands(boot, fat, bitmap, &targeted)
}

fn walk_islands(boot: &BootSector, fat: &[u32], bitmap: &[u8], targeted: &[u8]) -> Vec<Island> {
    let last = boot.last_cluster_exclusive();
    let mut visited = vec![false; last as usize];
    let mut islands = Vec::new();
    for head in FIRST_CLUSTER..last {
        if !fat_in_use(boot, fat, head) || targeted[head as usize] != 0 {
            continue;
        }
        let mut island = Island {
            head,
            tail: head,
            length: 0,
            allocated: 0,
            end: ISLAND_END_JOINS_ANOTHER,
        };
        let mut current = head;
        while valid_cluster(boot, current) && !visited[current as usize] {
            visited[current as usize] = true;
            island.length += 1;
            island.tail = current;
            if is_allocated(bitmap, current) {
                island.allocated += 1;
            }
            let value = fat[current as usize];
            if value == FAT_END {
                island.end = ISLAND_END_EOF;
                break;
            }
            if value == 0 || value == FAT_BAD {
                island.end = ISLAND_END_BROKEN;
                break;
            }
            if !valid_cluster(boot, value) {
                island.end = ISLAND_END_BROKEN;
                break;
            }
            current = value;
        }
        islands.push(island);
    }
    islands
}
