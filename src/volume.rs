use crate::boot::{BootRegions, BootSector};
use crate::device::Reader;
use crate::directory::{self, DirectoryEntry, Stream};
use crate::file::File;
use crate::metadata::{AllocationBitmap, FatEntry, RootMetadata, SystemStream, UpcaseTable};
use crate::primitives::{self, FatChainStep, FatEntryReport};
use crate::{Error, Result};
use fs_core::BlockRead;
use std::collections::HashSet;
use std::mem::size_of;
use std::ops::Range;

#[derive(Clone, Copy, Debug)]
pub struct ReadLimits {
    pub max_metadata_bytes: usize,
    pub max_chain_clusters: usize,
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            max_metadata_bytes: 64 * 1024 * 1024,
            max_chain_clusters: usize::MAX,
        }
    }
}

pub(crate) enum StreamClusters {
    Contiguous(Range<u32>),
    Fat(Vec<u32>),
}

impl StreamClusters {
    fn len(&self) -> usize {
        match self {
            Self::Contiguous(range) => (range.end - range.start) as usize,
            Self::Fat(clusters) => clusters.len(),
        }
    }

    pub(crate) fn get(&self, index: usize) -> Option<u32> {
        match self {
            Self::Contiguous(range) => u32::try_from(index)
                .ok()
                .and_then(|index| range.start.checked_add(index))
                .filter(|cluster| *cluster < range.end),
            Self::Fat(clusters) => clusters.get(index).copied(),
        }
    }

    fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        let (range, clusters) = match self {
            Self::Contiguous(range) => (range.clone(), [].as_slice()),
            Self::Fat(clusters) => (0..0, clusters.as_slice()),
        };
        range.chain(clusters.iter().copied())
    }
}

pub struct Volume<Device: BlockRead> {
    pub(crate) reader: Reader<Device>,
    pub(crate) boot: BootSector,
    pub(crate) root: Vec<u8>,
    pub(crate) limits: ReadLimits,
    pub(crate) metadata: RootMetadata,
    pub(crate) bitmap: AllocationBitmap,
    pub(crate) upcase: UpcaseTable,
    system_clusters: HashSet<u32>,
}

impl<Device: BlockRead> Volume<Device> {
    pub fn open(device: Device) -> Result<Self> {
        Self::open_with_limits(device, ReadLimits::default())
    }

    pub fn open_with_limits(device: Device, mut limits: ReadLimits) -> Result<Self> {
        let reader = Reader::new(device);
        let report = BootRegions::read(&reader)?;
        let boot = report.strict()?.clone();
        limits.max_chain_clusters = limits.max_chain_clusters.min(boot.cluster_count as usize);
        let root_chain = chain(&reader, &boot, boot.root_cluster, limits)?;
        let root_length = (root_chain.len() as u64)
            .checked_mul(boot.cluster_size())
            .ok_or(Error::Bounds)?;
        let root = read_chain(
            &reader,
            &boot,
            root_chain.iter().copied(),
            root_length,
            limits,
        )?;
        let metadata = RootMetadata::parse(&root, boot.number_of_fats)?;
        let active = usize::from((boot.volume_flags & 1) as u8);
        let bitmap_stream =
            metadata.bitmaps[active].ok_or(Error::Invalid("active bitmap missing"))?;
        // Section 7.1.5, Table 22: the bitmap holds one bit per cluster of the
        // heap, and the bits past ClusterCount are Reserved. A DataLength
        // longer than ClusterCount needs is valid; only the bits the heap uses
        // are read, but the chain must still cover the whole DataLength.
        let bitmap_length = u64::from(boot.cluster_count).div_ceil(8);
        if bitmap_stream.length < bitmap_length {
            return Err(Error::Invalid("bitmap stream length"));
        }
        let (bitmap_bytes, bitmap_chain) =
            system_stream(&reader, &boot, bitmap_stream, bitmap_length, limits)?;
        let bitmap = AllocationBitmap::parse(bitmap_bytes, boot.cluster_count)?;
        // Sections 7.2.4, 7.2.5 and 7.2.5.1 set no maximum DataLength for the
        // Up-case Table (#76): the read is bounded by the chain, which must
        // hold exactly DataLength, and by max_metadata_bytes.
        if metadata.upcase.length == 0 || metadata.upcase.length % 2 != 0 {
            return Err(Error::Invalid("upcase stream length"));
        }
        let (upcase_bytes, upcase_chain) = system_stream(
            &reader,
            &boot,
            metadata.upcase,
            metadata.upcase.length,
            limits,
        )?;
        let upcase = UpcaseTable::parse(&upcase_bytes, metadata.upcase_checksum)?;
        let mut occupied = HashSet::new();
        for cluster in root_chain.iter().chain(&bitmap_chain).chain(&upcase_chain) {
            if !bitmap.allocated(*cluster)? || !occupied.insert(*cluster) {
                return Err(Error::Invalid("system stream unallocated or cross-linked"));
            }
        }
        let volume = Self {
            reader,
            boot,
            root,
            limits,
            metadata,
            bitmap,
            upcase,
            system_clusters: occupied,
        };
        volume.root_directory()?;
        Ok(volume)
    }

    pub fn boot(&self) -> &BootSector {
        &self.boot
    }

    pub fn metadata(&self) -> &RootMetadata {
        &self.metadata
    }

    pub fn bitmap(&self) -> &AllocationBitmap {
        &self.bitmap
    }

    pub fn upcase(&self) -> &UpcaseTable {
        &self.upcase
    }

    pub fn root_bytes(&self) -> &[u8] {
        &self.root
    }

    pub fn limits(&self) -> ReadLimits {
        self.limits
    }

    pub fn root_directory(&self) -> Result<Vec<DirectoryEntry>> {
        directory::parse(&self.root, &self.boot, &self.upcase, true)
    }

    pub fn open_file(&self, entry: &DirectoryEntry) -> Result<File<'_, Device>> {
        File::open(self, entry)
    }

    pub fn read_directory(&self, entry: &DirectoryEntry) -> Result<Vec<DirectoryEntry>> {
        if !entry.is_directory() {
            return Err(Error::Invalid("entry is not a directory"));
        }
        let clusters = self.stream_clusters(entry.stream())?;
        let data = read_chain(
            &self.reader,
            &self.boot,
            clusters.iter(),
            entry.stream().data_length,
            self.limits,
        )?;
        directory::parse(&data, &self.boot, &self.upcase, false)
    }

    pub(crate) fn stream_clusters(&self, stream: &Stream) -> Result<StreamClusters> {
        let count = usize::try_from(stream.data_length.div_ceil(self.boot.cluster_size()))
            .map_err(|_| Error::Bounds)?;
        if count > self.limits.max_chain_clusters {
            return Err(Error::Unsupported("stream exceeds chain read limits"));
        }
        if count == 0 {
            return Ok(StreamClusters::Contiguous(0..0));
        }
        let clusters = if stream.no_fat_chain {
            let range = primitives::contiguous_clusters(stream.first_cluster, count as u64);
            if range.end > u64::from(self.boot.cluster_count) + 2 {
                return Err(Error::Invalid("contiguous stream exceeds heap"));
            }
            self.boot.cluster_offset(stream.first_cluster)?;
            StreamClusters::Contiguous(stream.first_cluster..range.end as u32)
        } else {
            if count > self.limits.max_metadata_bytes / size_of::<u32>() {
                return Err(Error::Unsupported("chain exceeds memory budget"));
            }
            StreamClusters::Fat(self.cluster_chain(stream.first_cluster)?)
        };
        if clusters.len() != count {
            return Err(Error::Invalid(
                "stream chain length differs from allocation",
            ));
        }
        for cluster in clusters.iter() {
            if !self.bitmap.allocated(cluster)? || self.system_clusters.contains(&cluster) {
                return Err(Error::Invalid("stream uses a free or system cluster"));
            }
        }
        Ok(clusters)
    }

    pub fn fat_entry(&self, cluster: u32) -> Result<FatEntry> {
        FatEntry::from_report(fat_entry(&self.reader, &self.boot, cluster)?)
    }

    pub(crate) fn bitmap_mut(&mut self) -> &mut AllocationBitmap {
        &mut self.bitmap
    }

    pub(crate) fn boot_mut(&mut self) -> &mut BootSector {
        &mut self.boot
    }

    pub(crate) fn is_system_cluster(&self, cluster: u32) -> bool {
        self.system_clusters.contains(&cluster)
    }

    pub fn cluster_chain(&self, first: u32) -> Result<Vec<u32>> {
        let clusters = chain(&self.reader, &self.boot, first, self.limits)?;
        for cluster in &clusters {
            if !self.bitmap.allocated(*cluster)? {
                return Err(Error::Invalid("chain uses a free cluster"));
            }
        }
        Ok(clusters)
    }
}

fn fat_entry<Device: BlockRead>(
    reader: &Reader<Device>,
    boot: &BootSector,
    cluster: u32,
) -> Result<FatEntryReport> {
    boot.cluster_offset(cluster)?;
    let active = u64::from(boot.volume_flags & 1);
    let offset = (u64::from(boot.fat_offset) + active * u64::from(boot.fat_length))
        * boot.bytes_per_sector();
    primitives::read_fat_entry(offset, cluster, boot.cluster_count, |offset, output| {
        reader.read_at(offset, output)
    })
}

fn chain<Device: BlockRead>(
    reader: &Reader<Device>,
    boot: &BootSector,
    first: u32,
    limits: ReadLimits,
) -> Result<Vec<u32>> {
    let mut clusters = Vec::new();
    let maximum = limits.max_metadata_bytes / size_of::<u32>();
    // Brent's cycle detection keeps constant space; walk_fat_chain's visited set would not.
    let mut checkpoint = first;
    let mut power = 1usize;
    let mut distance = 0usize;
    for step in primitives::walk_allocation(first, boot.cluster_count, None, |cluster| {
        fat_entry(reader, boot, cluster)
    }) {
        match step? {
            FatChainStep::Cluster(position) => {
                if !position.valid {
                    return Err(Error::Invalid("cluster outside heap"));
                }
                if clusters.len() >= limits.max_chain_clusters {
                    return Err(Error::Unsupported("chain exceeds read limits"));
                }
                if clusters.len() >= maximum {
                    return Err(Error::Unsupported("chain exceeds memory budget"));
                }
                if clusters.len() == clusters.capacity() {
                    let capacity = clusters
                        .capacity()
                        .saturating_mul(2)
                        .max(64)
                        .min(maximum)
                        .min(limits.max_chain_clusters);
                    clusters
                        .try_reserve_exact(capacity - clusters.len())
                        .map_err(|_| Error::Unsupported("chain allocation failed"))?;
                }
                clusters.push(position.cluster);
            }
            FatChainStep::Entry { entry, .. } => match FatEntry::from_report(entry)? {
                FatEntry::End => return Ok(clusters),
                FatEntry::Next(next) => {
                    distance += 1;
                    if next == checkpoint {
                        return Err(Error::Invalid("FAT chain cycle"));
                    }
                    if distance == power {
                        checkpoint = next;
                        power = power.saturating_mul(2);
                        distance = 0;
                    }
                }
                FatEntry::Free | FatEntry::Bad => {
                    return Err(Error::Invalid("free or bad cluster in FAT chain"))
                }
            },
        }
    }
    Err(Error::Invalid("free or bad cluster in FAT chain"))
}

fn system_stream<Device: BlockRead>(
    reader: &Reader<Device>,
    boot: &BootSector,
    stream: SystemStream,
    read_length: u64,
    limits: ReadLimits,
) -> Result<(Vec<u8>, Vec<u32>)> {
    let clusters = chain(reader, boot, stream.first_cluster, limits)?;
    if clusters.len() as u64 != stream.length.div_ceil(boot.cluster_size()) {
        return Err(Error::Invalid("system stream chain length"));
    }
    let bytes = read_chain(
        reader,
        boot,
        clusters.iter().copied(),
        read_length.min(stream.length),
        limits,
    )?;
    Ok((bytes, clusters))
}

pub(crate) fn read_chain<Device: BlockRead>(
    reader: &Reader<Device>,
    boot: &BootSector,
    clusters: impl IntoIterator<Item = u32>,
    length: u64,
    limits: ReadLimits,
) -> Result<Vec<u8>> {
    let length = usize::try_from(length).map_err(|_| Error::Bounds)?;
    if length > limits.max_metadata_bytes {
        return Err(Error::Unsupported("metadata exceeds read limits"));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(length)
        .map_err(|_| Error::Unsupported("metadata allocation failed"))?;
    output.resize(length, 0);
    let mut clusters = clusters.into_iter();
    let mut copied = 0;
    // Reads start at offset 0, so chunk n is exactly the chain's nth cluster.
    for chunk in primitives::cluster_reads(0, length, boot.cluster_size()) {
        let Some(cluster) = clusters.next() else {
            return Err(Error::Invalid("short metadata chain"));
        };
        reader.read_at(
            boot.cluster_offset(cluster)?,
            &mut output[chunk.output.clone()],
        )?;
        copied = chunk.output.end;
    }
    if copied != length {
        return Err(Error::Invalid("short metadata chain"));
    }
    Ok(output)
}
