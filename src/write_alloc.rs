//! Cluster allocation: the Allocation Bitmap, FAT chains and contiguous
//! (NoFatChain) streams.
//!
//! Section 7.1 makes the Allocation Bitmap the record of which clusters are
//! in use, and Section 4 the FAT the record of how a fragmented stream is
//! linked. A stream whose clusters are consecutive may set NoFatChain
//! (Section 7.6.2) and leave its FAT entries uninterpreted. This module
//! keeps a stream contiguous when it can, converts it to a FAT chain when it
//! cannot grow in place, and recognises a chain that has become contiguous
//! again after it shrank.
//!
//! Every FAT update is written to each FAT the volume has, and every bitmap
//! update to each Allocation Bitmap, so a two-FAT (TexFAT) volume keeps its
//! second FAT and bitmap in step with the first.

use super::WriteVolume;
use crate::{primitives, Error, Result};
use fs_core::BlockDevice;

/// The clusters of one stream: where it starts and ends, how many clusters
/// it holds, and whether they are consecutive and unlinked in the FAT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allocation {
    first: u32,
    last: u32,
    count: u32,
    no_fat_chain: bool,
}

impl Allocation {
    /// An allocation of no clusters.
    pub const EMPTY: Self = Self {
        first: 0,
        last: 0,
        count: 0,
        no_fat_chain: false,
    };

    /// The first cluster, or 0 when the allocation is empty.
    pub fn first_cluster(&self) -> u32 {
        self.first
    }

    /// The last cluster, or 0 when the allocation is empty.
    pub fn last_cluster(&self) -> u32 {
        self.last
    }

    pub fn count(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Whether the clusters are consecutive and the FAT does not link them
    /// (Section 7.6.2.2, NoFatChain).
    pub fn no_fat_chain(&self) -> bool {
        self.no_fat_chain
    }

    pub(crate) fn from_parts(first: u32, last: u32, count: u32, no_fat_chain: bool) -> Self {
        Self {
            first,
            last,
            count,
            no_fat_chain,
        }
    }
}

impl<Device: BlockDevice> WriteVolume<Device> {
    /// Allocates `count` clusters as a new stream. They are consecutive and
    /// NoFatChain when a free run that long exists, and a FAT chain
    /// otherwise. The clusters belong to no file: free them, or record them
    /// in a directory entry, before unmounting.
    pub fn allocate(&mut self, count: u32) -> Result<Allocation> {
        self.operation(|volume| volume.allocate_clusters(count, None))
    }

    /// Adds `more` clusters to the end of `allocation`.
    pub fn extend(&mut self, allocation: &mut Allocation, more: u32) -> Result<()> {
        let mut updated = *allocation;
        self.operation(|volume| volume.extend_clusters(&mut updated, more))?;
        *allocation = updated;
        Ok(())
    }

    /// Frees every cluster of `allocation` after the first `keep`.
    pub fn shrink(&mut self, allocation: &mut Allocation, keep: u32) -> Result<()> {
        let mut updated = *allocation;
        self.operation(|volume| volume.shrink_clusters(&mut updated, keep))?;
        *allocation = updated;
        Ok(())
    }

    /// Frees every cluster of `allocation`.
    pub fn free(&mut self, allocation: Allocation) -> Result<()> {
        let mut updated = allocation;
        self.operation(|volume| volume.shrink_clusters(&mut updated, 0))
    }

    /// The clusters of `allocation` in stream order, checked against the FAT
    /// and the Allocation Bitmap.
    pub fn clusters(&self, allocation: &Allocation) -> Result<Vec<u32>> {
        if allocation.count == 0 {
            return Ok(Vec::new());
        }
        let clusters = if allocation.no_fat_chain {
            (allocation.first..=allocation.last).collect()
        } else {
            self.fat_chain(allocation.first)?
        };
        if clusters.len() != allocation.count as usize || clusters.last() != Some(&allocation.last)
        {
            return Err(Error::Invalid("allocation differs from its FAT chain"));
        }
        for cluster in &clusters {
            if !self.volume.bitmap().allocated(*cluster)? {
                return Err(Error::Invalid("allocation holds a free cluster"));
            }
        }
        Ok(clusters)
    }

    /// The FAT chain from `first`, read a window of the active FAT at a
    /// time. Every link must stay in the heap and the chain must end within
    /// ClusterCount links, so a cycle is refused.
    pub(crate) fn fat_chain(&self, first: u32) -> Result<Vec<u32>> {
        const WINDOW: u64 = 64 * 1024;
        let boot = self.volume.boot();
        let fat = self.fats[usize::from((boot.volume_flags & 1) as u8)];
        let fat_bytes = (u64::from(boot.cluster_count) + 2) * 4;
        let mut window = Vec::new();
        let mut window_start = u64::MAX;
        let mut clusters = Vec::new();
        let mut cluster = first;
        loop {
            boot.cluster_offset(cluster)?;
            if clusters.len() as u64 >= u64::from(boot.cluster_count) {
                return Err(Error::Invalid("FAT chain cycle"));
            }
            clusters.push(cluster);
            let position = u64::from(cluster) * 4;
            if !(window_start..window_start.saturating_add(window.len() as u64)).contains(&position)
            {
                window_start = position / WINDOW * WINDOW;
                window = vec![0; WINDOW.min(fat_bytes - window_start) as usize];
                self.cache.read(fat + window_start, &mut window)?;
            }
            let at = (position - window_start) as usize;
            let value = u32::from_le_bytes(window[at..at + 4].try_into().expect("four bytes"));
            if value == primitives::FAT_END {
                return Ok(clusters);
            }
            if !(2..u64::from(boot.cluster_count) + 2).contains(&u64::from(value)) {
                return Err(Error::Invalid("free or bad cluster in FAT chain"));
            }
            cluster = value;
        }
    }

    pub(crate) fn allocate_clusters(
        &mut self,
        count: u32,
        near: Option<u32>,
    ) -> Result<Allocation> {
        if count == 0 {
            return Ok(Allocation::EMPTY);
        }
        if u64::from(count) > self.free_clusters() {
            return Err(Error::NoSpace);
        }
        let start = near.unwrap_or(self.next_free);
        if let Some(first) = self.free_run(count, start)? {
            let last = first + (count - 1);
            for cluster in first..=last {
                self.mark(cluster, true)?;
            }
            self.next_free = self.wrap(last + 1);
            return Ok(Allocation::from_parts(first, last, count, true));
        }
        let clusters = self.free_list(count, start)?;
        for cluster in &clusters {
            self.mark(*cluster, true)?;
        }
        self.link(&clusters)?;
        let last = *clusters.last().expect("count is not zero");
        self.next_free = self.wrap(last + 1);
        Ok(Allocation::from_parts(clusters[0], last, count, false))
    }

    pub(crate) fn extend_clusters(&mut self, allocation: &mut Allocation, more: u32) -> Result<()> {
        if more == 0 {
            return Ok(());
        }
        if allocation.count == 0 {
            *allocation = self.allocate_clusters(more, None)?;
            return Ok(());
        }
        let total = allocation
            .count
            .checked_add(more)
            .filter(|total| *total <= self.volume.boot().cluster_count)
            .ok_or(Error::NoSpace)?;
        if u64::from(more) > self.free_clusters() {
            return Err(Error::NoSpace);
        }
        let next = allocation.last.checked_add(1).ok_or(Error::Bounds)?;
        if allocation.no_fat_chain {
            if self.run_is_free(next, more)? {
                for cluster in next..=allocation.last + more {
                    self.mark(cluster, true)?;
                }
                allocation.last += more;
                allocation.count = total;
                self.next_free = self.wrap(allocation.last + 1);
                return Ok(());
            }
            // The stream cannot stay contiguous, so its existing clusters are
            // linked in the FAT before the new ones are added to the chain.
            let existing: Vec<u32> = (allocation.first..=allocation.last).collect();
            self.link(&existing)?;
            allocation.no_fat_chain = false;
        }
        let start = self.wrap(next);
        let clusters = match self.free_run(more, start)? {
            Some(first) => (first..first + more).collect(),
            None => self.free_list(more, start)?,
        };
        for cluster in &clusters {
            self.mark(*cluster, true)?;
        }
        self.link(&clusters)?;
        self.set_fat(allocation.last, clusters[0])?;
        allocation.last = *clusters.last().expect("more is not zero");
        allocation.count = total;
        self.next_free = self.wrap(allocation.last + 1);
        Ok(())
    }

    pub(crate) fn shrink_clusters(&mut self, allocation: &mut Allocation, keep: u32) -> Result<()> {
        if keep >= allocation.count {
            return Ok(());
        }
        let clusters = self.clusters(allocation)?;
        let (kept, freed) = clusters.split_at(keep as usize);
        if let Some(last) = kept.last() {
            if !allocation.no_fat_chain {
                self.set_fat(*last, primitives::FAT_END)?;
            }
        }
        for cluster in freed {
            self.set_fat(*cluster, 0)?;
            self.mark(*cluster, false)?;
        }
        *allocation = match kept {
            [] => Allocation::EMPTY,
            [first, ..] => {
                let last = *kept.last().expect("not empty");
                // A chain that is now consecutive may be recorded as
                // NoFatChain. Its FAT entries stay as they are: they still
                // describe the same clusters, so the volume is consistent
                // whether or not the directory entry is updated.
                let contiguous =
                    allocation.no_fat_chain || kept.windows(2).all(|pair| pair[1] == pair[0] + 1);
                Allocation::from_parts(*first, last, keep, contiguous)
            }
        };
        Ok(())
    }

    fn wrap(&self, cluster: u32) -> u32 {
        if u64::from(cluster) >= u64::from(self.volume.boot().cluster_count) + 2 {
            2
        } else {
            cluster.max(2)
        }
    }

    fn end(&self) -> u32 {
        // cluster_count is at most 0xFFFFFFF5, so this does not overflow.
        self.volume.boot().cluster_count + 2
    }

    fn is_free(&self, cluster: u32) -> Result<bool> {
        Ok(!self.volume.bitmap().allocated(cluster)? && !self.volume.is_system_cluster(cluster))
    }

    /// Whether `count` clusters from `first` are free, inside the heap and
    /// not marked bad in the FAT.
    fn run_is_free(&self, first: u32, count: u32) -> Result<bool> {
        let Some(end) = first.checked_add(count).filter(|end| *end <= self.end()) else {
            return Ok(false);
        };
        if first < 2 {
            return Ok(false);
        }
        for cluster in first..end {
            if !self.is_free(cluster)? {
                return Ok(false);
            }
        }
        Ok(self.bad_in(first, count)?.is_none())
    }

    /// The first run of `count` free clusters at or after `start`, wrapping
    /// once to the start of the heap.
    fn free_run(&self, count: u32, start: u32) -> Result<Option<u32>> {
        let end = self.end();
        let scan = |from: u32, to: u32| -> Result<Option<u32>> {
            let mut run_start = from;
            let mut length = 0u32;
            let mut cluster = from;
            while cluster < to {
                if self.is_free(cluster)? {
                    if length == 0 {
                        run_start = cluster;
                    }
                    length += 1;
                    if length == count {
                        match self.bad_in(run_start, count)? {
                            None => return Ok(Some(run_start)),
                            Some(bad) => {
                                length = 0;
                                cluster = bad + 1;
                                continue;
                            }
                        }
                    }
                } else {
                    length = 0;
                }
                cluster += 1;
            }
            Ok(None)
        };
        if let Some(first) = scan(start, end)? {
            return Ok(Some(first));
        }
        let wrap_end = start.saturating_add(count).min(end);
        scan(2, wrap_end)
    }

    /// `count` free clusters, not marked bad, in heap order from `start`,
    /// wrapping once.
    fn free_list(&self, count: u32, start: u32) -> Result<Vec<u32>> {
        let mut clusters = Vec::new();
        clusters
            .try_reserve_exact(count as usize)
            .map_err(|_| Error::Unsupported("allocation list"))?;
        for cluster in (start..self.end()).chain(2..start) {
            if clusters.len() == count as usize {
                break;
            }
            if self.is_free(cluster)? && self.bad_in(cluster, 1)?.is_none() {
                clusters.push(cluster);
            }
        }
        if clusters.len() != count as usize {
            return Err(Error::NoSpace);
        }
        Ok(clusters)
    }

    /// The first cluster from `first` the active FAT marks bad, if any.
    /// Section 4.1.4 forbids allocating one, and the Allocation Bitmap does
    /// not always record it.
    fn bad_in(&self, first: u32, count: u32) -> Result<Option<u32>> {
        let mut bytes = vec![0; count as usize * 4];
        let active = u64::from(self.volume.boot().volume_flags & 1) as usize;
        self.cache
            .read(self.fats[active] + u64::from(first) * 4, &mut bytes)?;
        Ok(bytes
            .chunks_exact(4)
            .position(|word| {
                u32::from_le_bytes(word.try_into().expect("four bytes")) == primitives::FAT_BAD
            })
            .map(|index| first + index as u32))
    }

    /// Links `clusters` into one FAT chain ending in an end-of-chain mark.
    fn link(&mut self, clusters: &[u32]) -> Result<()> {
        for pair in clusters.windows(2) {
            self.set_fat(pair[0], pair[1])?;
        }
        if let Some(last) = clusters.last() {
            self.set_fat(*last, primitives::FAT_END)?;
        }
        Ok(())
    }

    fn set_fat(&mut self, cluster: u32, value: u32) -> Result<()> {
        self.volume.boot().cluster_offset(cluster)?;
        for fat in &self.fats {
            self.cache
                .write(fat + u64::from(cluster) * 4, &value.to_le_bytes())?;
        }
        Ok(())
    }

    fn mark(&mut self, cluster: u32, allocated: bool) -> Result<()> {
        if self.volume.is_system_cluster(cluster) {
            return Err(Error::Invalid("system cluster in a stream allocation"));
        }
        let was = self.volume.bitmap().allocated(cluster)?;
        if was == allocated {
            return Err(Error::Invalid(if allocated {
                "cluster is already allocated"
            } else {
                "cluster is already free"
            }));
        }
        let (index, byte) = self.volume.bitmap_mut().set(cluster, allocated)?;
        let cluster_size = self.volume.boot().cluster_size();
        let within = index as u64 % cluster_size;
        let position = (index as u64 / cluster_size) as usize;
        for chain in &self.bitmaps {
            let holder = *chain
                .get(position)
                .ok_or(Error::Invalid("allocation bitmap chain is short"))?;
            let offset = self.volume.boot().cluster_offset(holder)? + within;
            self.cache.write(offset, &[byte])?;
        }
        if allocated {
            self.allocated += 1;
        } else {
            self.allocated -= 1;
        }
        Ok(())
    }
}
