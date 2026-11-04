use crate::boot::{BootRegions, BootSector};
use crate::device::Reader;
use crate::directory::{self, DirectoryEntry, Stream};
use crate::file::File;
use crate::metadata::{AllocationBitmap, FatEntry, RootMetadata, SystemStream, UpcaseTable};
use crate::{Error, Result};
use fs_core::BlockRead;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug)]
pub struct ReadLimits {
    pub max_metadata_bytes: usize,
    pub max_chain_clusters: usize,
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            max_metadata_bytes: 64 * 1024 * 1024,
            max_chain_clusters: 1024 * 1024,
        }
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

    pub fn open_with_limits(device: Device, limits: ReadLimits) -> Result<Self> {
        let reader = Reader::new(device);
        let report = BootRegions::read(&reader)?;
        let boot = report.strict()?.clone();
        let root_chain = chain(&reader, &boot, boot.root_cluster, limits)?;
        let root_length = (root_chain.len() as u64)
            .checked_mul(boot.cluster_size())
            .ok_or(Error::Bounds)?;
        let root = read_chain(&reader, &boot, &root_chain, root_length, limits)?;
        let metadata = RootMetadata::parse(&root, boot.number_of_fats)?;
        let active = usize::from((boot.volume_flags & 1) as u8);
        let bitmap_stream =
            metadata.bitmaps[active].ok_or(Error::Invalid("active bitmap missing"))?;
        if bitmap_stream.length != u64::from(boot.cluster_count).div_ceil(8) {
            return Err(Error::Invalid("bitmap stream length"));
        }
        let (bitmap_bytes, bitmap_chain) = system_stream(&reader, &boot, bitmap_stream, limits)?;
        let bitmap = AllocationBitmap::parse(bitmap_bytes, boot.cluster_count)?;
        if metadata.upcase.length == 0 || metadata.upcase.length > 131072 {
            return Err(Error::Invalid("upcase stream length"));
        }
        let (upcase_bytes, upcase_chain) = system_stream(&reader, &boot, metadata.upcase, limits)?;
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
            &clusters,
            entry.stream().data_length,
            self.limits,
        )?;
        directory::parse(&data, &self.boot, &self.upcase, false)
    }

    pub(crate) fn stream_clusters(&self, stream: &Stream) -> Result<Vec<u32>> {
        let count = usize::try_from(stream.data_length.div_ceil(self.boot.cluster_size()))
            .map_err(|_| Error::Bounds)?;
        if count > self.limits.max_chain_clusters {
            return Err(Error::Unsupported("stream exceeds chain read limits"));
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        let clusters = if stream.no_fat_chain {
            let end = u64::from(stream.first_cluster) + count as u64;
            if end > u64::from(self.boot.cluster_count) + 2 {
                return Err(Error::Invalid("contiguous stream exceeds heap"));
            }
            (stream.first_cluster..end as u32).collect()
        } else {
            self.cluster_chain(stream.first_cluster)?
        };
        if clusters.len() != count {
            return Err(Error::Invalid(
                "stream chain length differs from allocation",
            ));
        }
        for cluster in &clusters {
            if !self.bitmap.allocated(*cluster)? || self.system_clusters.contains(cluster) {
                return Err(Error::Invalid("stream uses a free or system cluster"));
            }
        }
        Ok(clusters)
    }

    pub fn fat_entry(&self, cluster: u32) -> Result<FatEntry> {
        fat_entry(&self.reader, &self.boot, cluster)
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
) -> Result<FatEntry> {
    boot.cluster_offset(cluster)?;
    let active = u64::from(boot.volume_flags & 1);
    let offset = (u64::from(boot.fat_offset) + active * u64::from(boot.fat_length))
        * boot.bytes_per_sector()
        + u64::from(cluster) * 4;
    let mut bytes = [0; 4];
    reader.read_at(offset, &mut bytes)?;
    FatEntry::parse(u32::from_le_bytes(bytes), boot.cluster_count)
}

fn chain<Device: BlockRead>(
    reader: &Reader<Device>,
    boot: &BootSector,
    first: u32,
    limits: ReadLimits,
) -> Result<Vec<u32>> {
    let mut clusters = Vec::new();
    let mut seen = HashSet::new();
    let mut current = first;
    loop {
        boot.cluster_offset(current)?;
        if clusters.len() >= limits.max_chain_clusters {
            return Err(Error::Unsupported("chain exceeds read limits"));
        }
        if !seen.insert(current) {
            return Err(Error::Invalid("FAT chain cycle"));
        }
        clusters.push(current);
        match fat_entry(reader, boot, current)? {
            FatEntry::End => return Ok(clusters),
            FatEntry::Next(next) => current = next,
            FatEntry::Free | FatEntry::Bad => {
                return Err(Error::Invalid("free or bad cluster in FAT chain"))
            }
        }
    }
}

fn system_stream<Device: BlockRead>(
    reader: &Reader<Device>,
    boot: &BootSector,
    stream: SystemStream,
    limits: ReadLimits,
) -> Result<(Vec<u8>, Vec<u32>)> {
    let clusters = chain(reader, boot, stream.first_cluster, limits)?;
    if clusters.len() as u64 != stream.length.div_ceil(boot.cluster_size()) {
        return Err(Error::Invalid("system stream chain length"));
    }
    let bytes = read_chain(reader, boot, &clusters, stream.length, limits)?;
    Ok((bytes, clusters))
}

pub(crate) fn read_chain<Device: BlockRead>(
    reader: &Reader<Device>,
    boot: &BootSector,
    clusters: &[u32],
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
    let mut copied = 0;
    for cluster in clusters {
        if copied == length {
            break;
        }
        let count = usize::try_from(boot.cluster_size())
            .map_err(|_| Error::Bounds)?
            .min(length - copied);
        reader.read_at(
            boot.cluster_offset(*cluster)?,
            &mut output[copied..copied + count],
        )?;
        copied += count;
    }
    if copied != length {
        return Err(Error::Invalid("short metadata chain"));
    }
    Ok(output)
}
