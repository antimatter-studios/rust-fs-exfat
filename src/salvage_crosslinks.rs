//! An exFAT extension outside the common trait: the clusters that more than
//! one FAT link, or more than one candidate's claim, reaches (#89).
//!
//! Microsoft's exFAT specification, section 4.1.3 "FatEntry[2] ...
//! FatEntry[ClusterCount+1]", has each FAT entry describe the next cluster
//! in a cluster chain, and section 6.4.3 "FirstCluster Field" has a stream's
//! FirstCluster start the chain that holds its data. A cluster that two links
//! or two claims reach sits in two chains at once: a cross-link, which macOS
//! `fsck_exfat -n` and this crate's check-only audit both refuse. The audit
//! and [`primitives::compute_stats`] count them; this listing names each one
//! and its claimants, as evidence. A free cluster's FAT entry is legal
//! whatever it holds (section 7.1), so a link from one may meet a live chain
//! without anything being damaged, and a deleted candidate's claim may name
//! clusters a later file reused.

use super::stale::{merged, FatReader};
use super::{EntrySetDetail, ExfatSalvage, Geometry, Mapper};
use crate::primitives::{self, FatValue, FIRST_CLUSTER};
use crate::salvage::adapter::{Candidate, Extent, SalvageError, Sink};
use fs_core::BlockRead;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// One FAT entry that names a cross-linked cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FatLink {
    /// The cluster whose FAT entry names the cross-linked one.
    pub from: u32,
    /// That entry's four bytes in the FAT the listing read.
    pub location: Extent,
    /// The first cluster of the run of links that reaches `from`: a cluster
    /// no FAT entry names, which is the chain's head, or a cluster that is
    /// itself listed because chains met there first.
    pub head: u32,
}

/// One cluster that two or more FAT links, or two or more of the supplied
/// candidates' claims, reach.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossLink {
    pub cluster: u32,
    /// The cluster's bytes on the source.
    pub location: Extent,
    /// Every FAT entry that names the cluster, in cluster order, when two or
    /// more do; empty when the cluster is listed for its claims alone.
    pub links: Vec<FatLink>,
    /// The record offsets ([`Candidate::location`]) of the supplied
    /// candidates whose claims include the cluster, in ascending order,
    /// whether or not two of them do.
    pub candidates: Vec<u64>,
}

/// The outcome of a cross-link listing whose cross-links went to a sink.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossLinkReport {
    /// Cross-linked clusters passed to the sink.
    pub cross_links: u64,
    /// FAT entries of clusters on the source that could not be read, or that
    /// no FAT byte covers. Their links are unknown, so a non-zero count means
    /// the listing may be incomplete.
    pub unclassified: u64,
    /// FAT links and claimed clusters that reach clusters past the end of
    /// the source. Those clusters have no bytes to salvage and are not
    /// examined or listed.
    pub past_source: u64,
    /// FAT bytes on the source that could not be read.
    pub unreadable: Vec<Extent>,
    /// Set when the sink stopped the listing. `cross_links` then covers what
    /// was passed.
    pub stopped: bool,
}

impl ExfatSalvage {
    /// Lists every cluster that two or more FAT links reach, or that two or
    /// more of `candidates` claim, in cluster order, with its claimants: the
    /// FAT entries that name it and the head of each one's chain, and the
    /// candidates whose claims include it.
    ///
    /// This is an exFAT extension, not a verb of the common trait. Links come
    /// from every FAT entry, allocated or free, in the FAT the adapter maps
    /// claims through: the active one, unless [`ExfatSalvage::with_fat`]
    /// chose the other. Claims are walked as
    /// [`SalvageAdapter::walk_extents`](super::SalvageAdapter::walk_extents)
    /// walks them, so the caller chooses which candidates count: all of a
    /// scan's, or only its live or verified ones. Candidates repeated at one
    /// record offset count once.
    ///
    /// Only clusters the source holds are examined. Memory holds two bits per
    /// such cluster, the candidates' cluster runs, and every link into a
    /// cross-linked cluster. An empty listing with nothing unclassified
    /// means nothing was found.
    ///
    /// Fails only with [`SalvageError::NoGeometry`], when no boot copy gave
    /// geometry.
    pub fn cross_links(
        &self,
        source: &dyn BlockRead,
        candidates: &[Candidate<EntrySetDetail>],
        sink: &mut dyn Sink<CrossLink>,
    ) -> Result<CrossLinkReport, SalvageError> {
        let geometry = self.geometry()?;
        let on_source = source.size_bytes().saturating_sub(geometry.heap) / geometry.cluster;
        // Clusters FIRST_CLUSTER..end hold bytes on the source.
        let end = (on_source.min(u64::from(geometry.boot.cluster_count)) + u64::from(FIRST_CLUSTER))
            as u32;
        let mut report = CrossLinkReport::default();
        let mut fat = FatReader::new(geometry, source);
        let mut links = fat_links(geometry, &mut fat, end, &mut report);
        let runs = claim_runs(geometry, source, candidates, end, &mut report);
        report.unreadable = merged(std::mem::take(&mut fat.unreadable));
        links.sort_unstable();
        let mut links = links.into_iter().peekable();
        let mut sweep = Sweep::new(runs);
        let mut cluster = FIRST_CLUSTER;
        while cluster < end {
            sweep.advance(cluster);
            let mut named = Vec::new();
            while let Some((_, from, head)) = links.next_if(|link| link.0 == cluster) {
                named.push(FatLink {
                    from,
                    location: geometry.fat_word(from),
                    head,
                });
            }
            // A link a second read could not repeat leaves one: not a cross-link.
            if named.len() < 2 {
                named.clear();
            }
            if named.is_empty() && sweep.active.len() < 2 {
                let next_link = links.peek().map_or(end, |link| link.0);
                cluster = next_link.min(sweep.next_start().unwrap_or(end));
                continue;
            }
            let mut claimants: Vec<u64> = sweep.active.iter().map(|run| run.1).collect();
            claimants.sort_unstable();
            report.cross_links += 1;
            let found = CrossLink {
                cluster,
                location: Extent::new(geometry.cluster_offset(cluster), geometry.cluster),
                links: named,
                candidates: claimants,
            };
            if sink.accept(found).is_break() {
                report.stopped = true;
                break;
            }
            cluster += 1;
        }
        Ok(report)
    }
}

/// One or two bits per cluster: whether one FAT link names it, and whether
/// a second does.
struct Targets {
    once: Vec<u64>,
    twice: Vec<u64>,
}

impl Targets {
    fn new(end: u32) -> Self {
        let words = (end as usize).div_ceil(64);
        Self {
            once: vec![0; words],
            twice: vec![0; words],
        }
    }

    fn mark(&mut self, cluster: u32) {
        let (word, bit) = (cluster as usize / 64, 1u64 << (cluster % 64));
        if self.once[word] & bit != 0 {
            self.twice[word] |= bit;
        }
        self.once[word] |= bit;
    }

    fn named(&self, cluster: u32) -> bool {
        self.once[cluster as usize / 64] & (1 << (cluster % 64)) != 0
    }

    fn crossed(&self, cluster: u32) -> bool {
        self.twice[cluster as usize / 64] & (1 << (cluster % 64)) != 0
    }
}

/// The next cluster a FAT entry names, when that is a cluster on the source.
fn next_on_source(
    geometry: &Geometry,
    fat: &mut FatReader<'_>,
    cluster: u32,
    end: u32,
) -> Option<u32> {
    let word = fat.word(cluster)?;
    match primitives::decode_fat_entry(word, geometry.boot.cluster_count).kind {
        FatValue::Next(next) if next < end => Some(next),
        _ => None,
    }
}

/// Every link into a cross-linked cluster, as (target, from, head).
///
/// One pass counts the links into each cluster. Then a walk starts at each
/// head, a cluster that a FAT entry names nothing into, and at each
/// cross-linked cluster, and follows single links until it reaches a
/// cross-linked cluster or stops. Every cluster with one link into it lies
/// on exactly one such walk, so each link into a cross-linked cluster is
/// found once, with the start of its walk as its head.
fn fat_links(
    geometry: &Geometry,
    fat: &mut FatReader<'_>,
    end: u32,
    report: &mut CrossLinkReport,
) -> Vec<(u32, u32, u32)> {
    let cluster_count = geometry.boot.cluster_count;
    let mut targets = Targets::new(end);
    for cluster in FIRST_CLUSTER..end {
        let Some(word) = fat.word(cluster) else {
            report.unclassified += 1;
            continue;
        };
        if let FatValue::Next(next) = primitives::decode_fat_entry(word, cluster_count).kind {
            if next < end {
                targets.mark(next);
            } else {
                report.past_source += 1;
            }
        }
    }
    let mut links = Vec::new();
    // Each walk passes clusters with one link into them, so none is longer
    // than the clusters on the source; the bound only guards against reads
    // that differ between the passes.
    let longest = u64::from(end - FIRST_CLUSTER);
    for head in FIRST_CLUSTER..end {
        if targets.named(head) && !targets.crossed(head) {
            continue;
        }
        let mut current = head;
        let mut steps = 0;
        while let Some(next) = next_on_source(geometry, fat, current, end) {
            if targets.crossed(next) {
                links.push((next, current, head));
                break;
            }
            steps += 1;
            if steps > longest {
                break;
            }
            current = next;
        }
    }
    links
}

/// Each distinct candidate's claimed clusters on the source, as
/// (first, end, record offset) runs.
fn claim_runs(
    geometry: &Geometry,
    source: &dyn BlockRead,
    candidates: &[Candidate<EntrySetDetail>],
    end: u32,
    report: &mut CrossLinkReport,
) -> Vec<(u64, u64, u64)> {
    let mut mapper = Mapper::new(geometry, source);
    let mut seen = HashSet::new();
    let mut runs = Vec::new();
    for candidate in candidates {
        let record = candidate.location.offset;
        if !seen.insert(record) {
            continue;
        }
        let claimed = candidate.size.unwrap_or(0);
        let chain = mapper.chain(
            candidate.detail.first_cluster,
            claimed.div_ceil(geometry.cluster),
            candidate.detail.no_fat_chain,
        );
        for &(first, count) in &chain.runs {
            let start = u64::from(first);
            let stop = (start + count).min(u64::from(end));
            report.past_source += start + count - stop.max(start);
            if start < stop {
                runs.push((start, stop, record));
            }
        }
    }
    runs.sort_unstable();
    runs
}

/// The candidate runs that cover each cluster, swept in cluster order.
struct Sweep {
    /// Sorted by first cluster.
    runs: Vec<(u64, u64, u64)>,
    next: usize,
    /// (end, record offset) of the runs covering the current cluster.
    active: Vec<(u64, u64)>,
}

impl Sweep {
    fn new(runs: Vec<(u64, u64, u64)>) -> Self {
        Self {
            runs,
            next: 0,
            active: Vec::new(),
        }
    }

    /// Moves to `cluster`, which never decreases between calls.
    fn advance(&mut self, cluster: u32) {
        let cluster = u64::from(cluster);
        self.active.retain(|&(end, _)| end > cluster);
        while let Some(&(start, end, record)) = self.runs.get(self.next) {
            if start > cluster {
                break;
            }
            if end > cluster {
                self.active.push((end, record));
            }
            self.next += 1;
        }
    }

    /// The first cluster of the next run not yet covering anything.
    fn next_start(&self) -> Option<u32> {
        self.runs.get(self.next).map(|run| run.0 as u32)
    }
}
