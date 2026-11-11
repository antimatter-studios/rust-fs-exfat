//! Chains of non-zero FAT entries on free clusters.
//!
//! Microsoft's exFAT specification, section 7.1 "Allocation Bitmap Directory
//! Entry": "In the exFAT file system, a FAT does not describe the allocation
//! state of clusters; rather, an Allocation Bitmap does." Section 4.1.3
//! "FatEntry\[2\] ... FatEntry\[ClusterCount+1\] Fields" defines no value that
//! marks a cluster free, and section 8.1 "Recommended Write Ordering" updates
//! the FAT only "if necessary" when cluster allocations are freed. A cluster
//! the bitmap marks free may therefore hold any FAT entry; a non-zero one is
//! legal, not damage. Such entries often still link the clusters of a deleted
//! or truncated file. Whether an implementation clears them is not specified;
//! macOS's driver clears them, others need not.

use super::{decode_fat_entry, FatValue, FIRST_CLUSTER};
use serde::{Deserialize, Serialize};
use std::ops::ControlFlow;

/// What the allocation bitmap and the FAT record for one heap cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClusterRecord {
    /// The bitmap marks the cluster as not available for allocation.
    Allocated,
    /// The bitmap marks the cluster free; this is its raw FAT entry.
    Free(u32),
    /// The bitmap bit or the FAT entry could not be read or does not exist.
    Unknown,
}

/// Where the last entry of a free-cluster chain points.
///
/// [`End`](Self::End) is an end. [`FreeTarget`](Self::FreeTarget),
/// [`BadMarker`](Self::BadMarker) and [`OutOfHeap`](Self::OutOfHeap) are
/// breaks. [`JoinsAllocated`](Self::JoinsAllocated) and
/// [`JoinsChain`](Self::JoinsChain) are joins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StaleEnd {
    /// The entry holds the end-of-chain marker, FFFFFFFFh.
    End,
    /// The entry names a free cluster whose own FAT entry is zero.
    FreeTarget(u32),
    /// The entry holds the bad-cluster marker, FFFFFFF7h.
    BadMarker,
    /// The entry, given raw, names no cluster of the heap.
    OutOfHeap(u32),
    /// The entry names a cluster the allocation bitmap marks in use: a live
    /// file's or a system structure's claim.
    JoinsAllocated(u32),
    /// The entry names a cluster that an earlier chain of the same listing
    /// already holds.
    JoinsChain(u32),
    /// The entry names a cluster earlier in its own chain.
    Cycle(u32),
    /// The named cluster's bitmap bit or FAT entry could not be read or is
    /// missing.
    Unknown(u32),
}

impl StaleEnd {
    /// A free, bad or out-of-heap target.
    pub fn is_break(self) -> bool {
        matches!(
            self,
            Self::FreeTarget(_) | Self::BadMarker | Self::OutOfHeap(_)
        )
    }

    /// A target that a live allocation or another chain already claims.
    pub fn is_join(self) -> bool {
        matches!(self, Self::JoinsAllocated(_) | Self::JoinsChain(_))
    }
}

/// Free clusters linked by their non-zero FAT entries, as the entries say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaleChainWalk {
    /// Each cluster with its raw FAT entry, in link order. The first is the
    /// head: no other listed entry names it, or, for a chain that only loops,
    /// it is the loop's lowest cluster.
    pub links: Vec<(u32, u32)>,
    pub end: StaleEnd,
}

/// The counts of a listing, complete even when the caller stopped it early.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StaleSummary {
    /// Chains passed to the caller.
    pub chains: u64,
    /// Free clusters with non-zero FAT entries in those chains.
    pub entries: u64,
    /// Heap clusters whose record was [`ClusterRecord::Unknown`].
    pub unknown: u64,
    /// The caller stopped the listing.
    pub stopped: bool,
}

/// One bit per heap cluster, from cluster two.
struct ClusterSet(Vec<u64>);

impl ClusterSet {
    fn new(clusters: u32) -> Self {
        Self(vec![0; (clusters as usize).div_ceil(64)])
    }

    fn index(cluster: u32) -> usize {
        (cluster - FIRST_CLUSTER) as usize
    }

    fn contains(&self, cluster: u32) -> bool {
        let index = Self::index(cluster);
        self.0
            .get(index / 64)
            .is_some_and(|word| word >> (index % 64) & 1 == 1)
    }

    /// Ignores clusters past the set; no record exists for them.
    fn insert(&mut self, cluster: u32) {
        let index = Self::index(cluster);
        if let Some(word) = self.0.get_mut(index / 64) {
            *word |= 1 << (index % 64);
        }
    }

    fn remove(&mut self, cluster: u32) {
        let index = Self::index(cluster);
        if let Some(word) = self.0.get_mut(index / 64) {
            *word &= !(1 << (index % 64));
        }
    }
}

/// Lists every free cluster with a non-zero FAT entry, grouped into the chains
/// those entries form, and says where each chain ends.
///
/// `cluster_count` is the heap's declared size, which decides whether an entry
/// names a heap cluster. `recorded` clusters from cluster two have records;
/// the rest of the heap is [`ClusterRecord::Unknown`] without a call to
/// `record`. Three bitsets of `recorded` bits are the only state besides the
/// chain being walked.
///
/// Chains whose heads no listed entry names come first, in head order; chains
/// that only loop follow, from their lowest cluster. Each listed cluster
/// appears in exactly one chain. A chain that reaches a cluster an earlier
/// chain holds ends there as a join, so converging chains share no clusters.
pub fn stale_chains(
    cluster_count: u32,
    recorded: u32,
    mut record: impl FnMut(u32) -> ClusterRecord,
    mut emit: impl FnMut(StaleChainWalk) -> ControlFlow<()>,
) -> StaleSummary {
    let recorded = recorded.min(cluster_count);
    let heap_end = FIRST_CLUSTER.saturating_add(cluster_count);
    let recorded_end = FIRST_CLUSTER + recorded;
    let mut lookup = |cluster: u32| {
        if (FIRST_CLUSTER..recorded_end).contains(&cluster) {
            record(cluster)
        } else {
            ClusterRecord::Unknown
        }
    };
    let mut summary = StaleSummary {
        unknown: u64::from(heap_end - recorded_end),
        ..StaleSummary::default()
    };
    let mut named = ClusterSet::new(recorded);
    for cluster in FIRST_CLUSTER..recorded_end {
        match lookup(cluster) {
            ClusterRecord::Free(value) if value != 0 => {
                if let FatValue::Next(next) = decode_fat_entry(value, cluster_count).kind {
                    named.insert(next);
                }
            }
            ClusterRecord::Unknown => summary.unknown += 1,
            _ => {}
        }
    }
    let mut walker = Walker {
        cluster_count,
        listed: ClusterSet::new(recorded),
        current: ClusterSet::new(recorded),
    };
    // Heads first, then the clusters of chains that only loop.
    for heads in [true, false] {
        for cluster in FIRST_CLUSTER..recorded_end {
            if walker.listed.contains(cluster) || (heads && named.contains(cluster)) {
                continue;
            }
            let ClusterRecord::Free(value) = lookup(cluster) else {
                continue;
            };
            if value == 0 {
                continue;
            }
            let chain = walker.walk(cluster, value, &mut lookup);
            summary.chains += 1;
            summary.entries += chain.links.len() as u64;
            if emit(chain).is_break() {
                summary.stopped = true;
                return summary;
            }
        }
    }
    summary
}

struct Walker {
    cluster_count: u32,
    listed: ClusterSet,
    /// The clusters of the chain being walked, cleared after each walk.
    current: ClusterSet,
}

impl Walker {
    fn walk(
        &mut self,
        head: u32,
        value: u32,
        lookup: &mut impl FnMut(u32) -> ClusterRecord,
    ) -> StaleChainWalk {
        let mut links = Vec::new();
        let (mut cluster, mut value) = (head, value);
        let end = loop {
            self.listed.insert(cluster);
            self.current.insert(cluster);
            links.push((cluster, value));
            let next = match decode_fat_entry(value, self.cluster_count).kind {
                FatValue::Next(next) => next,
                FatValue::End => break StaleEnd::End,
                FatValue::Bad => break StaleEnd::BadMarker,
                // Listed entries are non-zero, so only reserved values remain.
                FatValue::Free | FatValue::Reserved => break StaleEnd::OutOfHeap(value),
            };
            if self.current.contains(next) {
                break StaleEnd::Cycle(next);
            }
            if self.listed.contains(next) {
                break StaleEnd::JoinsChain(next);
            }
            match lookup(next) {
                ClusterRecord::Allocated => break StaleEnd::JoinsAllocated(next),
                ClusterRecord::Unknown => break StaleEnd::Unknown(next),
                ClusterRecord::Free(0) => break StaleEnd::FreeTarget(next),
                ClusterRecord::Free(following) => (cluster, value) = (next, following),
            }
        };
        for &(cluster, _) in &links {
            self.current.remove(cluster);
        }
        StaleChainWalk { links, end }
    }
}
