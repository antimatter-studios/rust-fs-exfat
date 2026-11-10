use crate::boot::{BootRegions, BootSector};
use crate::check::CheckReport;
use crate::device::Reader;
use crate::directory::{self, Stream};
use crate::metadata::{FatEntry, RootMetadata, SystemStream, UpcaseTable};
use crate::{Error, Result};
use fs_core::BlockRead;
use std::mem::size_of;

const CHUNK_BYTES: usize = 64 * 1024;
const FIXED_BYTES: usize = 1024 * 1024;
const WORKSPACE_BYTES: usize = 512 * 1024 * 1024;
pub const MAX_AUDIT_MEMORY_BYTES: usize = 1537 * 1024 * 1024;

struct Budget {
    remaining: usize,
}

impl Budget {
    fn take(&mut self, bytes: usize, reason: &'static str) -> Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or(Error::Unsupported(reason))?;
        Ok(())
    }

    fn grow<Value>(&mut self, values: &mut Vec<Value>, additional: usize) -> Result<()> {
        let required = values.len().checked_add(additional).ok_or(Error::Bounds)?;
        if required <= values.capacity() {
            return Ok(());
        }
        let maximum = values.capacity() + self.remaining / size_of::<Value>();
        let capacity = required.max(values.capacity().saturating_mul(2).max(64));
        let capacity = capacity.min(maximum).max(required);
        let bytes = (capacity - values.capacity())
            .checked_mul(size_of::<Value>())
            .ok_or(Error::Bounds)?;
        self.take(
            bytes,
            "allocation audit directory workspace exceeds memory budget",
        )?;
        values
            .try_reserve_exact(capacity - values.len())
            .map_err(|_| {
                Error::Unsupported("allocation audit directory workspace allocation failed")
            })
    }

    fn bytes(&mut self, length: usize, reason: &'static str) -> Result<Vec<u8>> {
        self.take(length, reason)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| Error::Unsupported("allocation audit bitmap allocation failed"))?;
        bytes.resize(length, 0);
        Ok(bytes)
    }
}

struct Fat<'reader, Device: BlockRead> {
    reader: &'reader Reader<Device>,
    boot: &'reader BootSector,
    start: u64,
    length: u64,
    cached: u64,
    bytes: [u8; CHUNK_BYTES],
}

impl<'reader, Device: BlockRead> Fat<'reader, Device> {
    fn new(reader: &'reader Reader<Device>, boot: &'reader BootSector) -> Self {
        Self {
            reader,
            boot,
            start: (u64::from(boot.fat_offset)
                + u64::from(boot.volume_flags & 1) * u64::from(boot.fat_length))
                * boot.bytes_per_sector(),
            length: u64::from(boot.fat_length) * boot.bytes_per_sector(),
            cached: u64::MAX,
            bytes: [0; CHUNK_BYTES],
        }
    }

    fn entry(&mut self, cluster: u32) -> Result<FatEntry> {
        self.boot.cluster_offset(cluster)?;
        let offset = u64::from(cluster) * 4;
        let chunk = offset / CHUNK_BYTES as u64 * CHUNK_BYTES as u64;
        if chunk != self.cached {
            let length = (self.length - chunk).min(CHUNK_BYTES as u64) as usize;
            self.reader
                .read_at(self.start + chunk, &mut self.bytes[..length])?;
            self.cached = chunk;
        }
        let position = (offset - chunk) as usize;
        FatEntry::parse(
            u32::from_le_bytes(self.bytes[position..position + 4].try_into().unwrap()),
            self.boot.cluster_count,
        )
    }
}

#[derive(Clone, Copy)]
struct Allocation {
    first: u32,
    length: u64,
    contiguous: bool,
    root: bool,
}

impl Allocation {
    fn system(stream: SystemStream) -> Self {
        Self {
            first: stream.first_cluster,
            length: stream.length,
            contiguous: false,
            root: false,
        }
    }

    fn stream(stream: &Stream) -> Self {
        Self {
            first: stream.first_cluster(),
            length: stream.data_length(),
            contiguous: stream.no_fat_chain(),
            root: false,
        }
    }
}

struct Ownership {
    bytes: Vec<u8>,
    count: u64,
}

impl Ownership {
    fn claim(
        &mut self,
        cluster: u32,
        fat: &Fat<'_, impl BlockRead>,
        bitmap: Option<&[u8]>,
    ) -> Result<()> {
        fat.boot.cluster_offset(cluster)?;
        let index = (cluster - 2) as usize;
        let mask = 1 << (index % 8);
        if bitmap.is_some_and(|bytes| bytes[index / 8] & mask == 0) {
            return Err(Error::Invalid("stream uses a free cluster"));
        }
        if self.bytes[index / 8] & mask != 0 {
            return Err(Error::Invalid("cross-linked allocation or FAT chain cycle"));
        }
        self.bytes[index / 8] |= mask;
        self.count += 1;
        Ok(())
    }

    fn walk(
        &mut self,
        fat: &mut Fat<'_, impl BlockRead>,
        bitmap: Option<&[u8]>,
        allocation: &mut Allocation,
    ) -> Result<()> {
        let expected = allocation.length.div_ceil(fat.boot.cluster_size());
        if !allocation.root && expected == 0 {
            return Ok(());
        }
        let mut current = allocation.first;
        let mut count = 0u64;
        loop {
            self.claim(current, fat, bitmap)?;
            count += 1;
            if allocation.contiguous {
                if count == expected {
                    break;
                }
                current = current.checked_add(1).ok_or(Error::Bounds)?;
            } else {
                match fat.entry(current)? {
                    FatEntry::End => break,
                    FatEntry::Next(next) => current = next,
                    FatEntry::Free | FatEntry::Bad => {
                        return Err(Error::Invalid("free or bad cluster in FAT chain"));
                    }
                }
                if !allocation.root && count >= expected {
                    return Err(Error::Invalid(
                        "stream chain length differs from allocation",
                    ));
                }
            }
        }
        if allocation.root {
            allocation.length = count * fat.boot.cluster_size();
            if allocation.length > 256 * 1024 * 1024 {
                return Err(Error::Invalid("root directory exceeds format length limit"));
            }
        } else if count != expected {
            return Err(Error::Invalid(
                "stream chain length differs from allocation",
            ));
        }
        Ok(())
    }
}

struct Cursor {
    allocation: Allocation,
    cluster: u32,
    within: u64,
    position: u64,
    bytes: [u8; CHUNK_BYTES],
    start: usize,
    end: usize,
}

impl Cursor {
    fn new(allocation: Allocation) -> Self {
        Self {
            allocation,
            cluster: allocation.first,
            within: 0,
            position: 0,
            bytes: [0; CHUNK_BYTES],
            start: 0,
            end: 0,
        }
    }

    fn read(&mut self, fat: &mut Fat<'_, impl BlockRead>, mut output: &mut [u8]) -> Result<()> {
        while !output.is_empty() {
            if self.start == self.end {
                if self.position == self.allocation.length {
                    return Err(Error::Invalid("short allocation audit stream"));
                }
                if self.within == fat.boot.cluster_size() {
                    self.cluster = if self.allocation.contiguous {
                        self.cluster.checked_add(1).ok_or(Error::Bounds)?
                    } else if let FatEntry::Next(next) = fat.entry(self.cluster)? {
                        next
                    } else {
                        return Err(Error::Invalid("short allocation audit chain"));
                    };
                    self.within = 0;
                }
                let length = (CHUNK_BYTES as u64)
                    .min(fat.boot.cluster_size() - self.within)
                    .min(self.allocation.length - self.position)
                    as usize;
                fat.reader.read_at(
                    fat.boot.cluster_offset(self.cluster)? + self.within,
                    &mut self.bytes[..length],
                )?;
                self.within += length as u64;
                self.position += length as u64;
                self.start = 0;
                self.end = length;
            }
            let count = output.len().min(self.end - self.start);
            output[..count].copy_from_slice(&self.bytes[self.start..self.start + count]);
            self.start += count;
            output = &mut output[count..];
        }
        Ok(())
    }

    fn set(&mut self, fat: &mut Fat<'_, impl BlockRead>) -> Result<Option<Vec<u8>>> {
        if self.position == self.allocation.length && self.start == self.end {
            return Ok(None);
        }
        let mut primary = [0; 32];
        self.read(fat, &mut primary)?;
        if primary[0] == 0 {
            return Ok(None);
        }
        let kind = primary[0];
        let secondaries = if kind & 0x80 != 0 && (kind == 0x85 || kind & 0xe0 == 0xa0) {
            usize::from(primary[1])
        } else {
            0
        };
        let mut bytes = vec![0; (secondaries + 1) * 32];
        bytes[..32].copy_from_slice(&primary);
        self.read(fat, &mut bytes[32..])?;
        Ok(Some(bytes))
    }
}

fn root_metadata(fat: &mut Fat<'_, impl BlockRead>, root: Allocation) -> Result<RootMetadata> {
    let mut cursor = Cursor::new(root);
    let mut system = [0; 5 * 32];
    let mut length = 0;
    while let Some(set) = cursor.set(fat)? {
        let kind = set[0];
        if matches!(kind, 0x81..=0x83 | 0xa0) {
            if length == system.len() {
                return Err(Error::Invalid("duplicate root system metadata"));
            }
            system[length..length + 32].copy_from_slice(&set[..32]);
            length += 32;
        } else if kind & 0x80 != 0 && kind != 0x85 && kind & 0xe0 != 0xa0 {
            return Err(Error::Unsupported("critical root directory entry"));
        }
    }
    RootMetadata::parse(&system[..length], fat.boot.number_of_fats)
}

fn load(
    fat: &mut Fat<'_, impl BlockRead>,
    allocation: Allocation,
    output: &mut [u8],
) -> Result<()> {
    Cursor::new(allocation).read(fat, output)
}

struct Names {
    units: Vec<u16>,
    records: Vec<(u32, u16)>,
}

impl Names {
    fn add(&mut self, name: &[u16], upcase: &UpcaseTable, budget: &mut Budget) -> Result<()> {
        budget.grow(&mut self.units, name.len())?;
        budget.grow(&mut self.records, 1)?;
        let start = u32::try_from(self.units.len()).map_err(|_| Error::Bounds)?;
        self.records.push((start, name.len() as u16));
        self.units.extend(name.iter().map(|unit| upcase.map(*unit)));
        Ok(())
    }

    fn finish(mut self, budget: &mut Budget) -> Result<()> {
        let units = &self.units;
        let name = |record: &(u32, u16)| {
            let start = record.0 as usize;
            &units[start..start + usize::from(record.1)]
        };
        self.records
            .sort_unstable_by(|first, second| name(first).cmp(name(second)));
        if self
            .records
            .windows(2)
            .any(|pair| name(&pair[0]) == name(&pair[1]))
        {
            return Err(Error::Invalid("duplicate case-insensitive directory name"));
        }
        budget.remaining += self.units.capacity() * size_of::<u16>()
            + self.records.capacity() * size_of::<(u32, u16)>();
        Ok(())
    }
}

pub fn check_device<Device: BlockRead>(device: Device) -> Result<CheckReport> {
    check_device_with_budget(device, MAX_AUDIT_MEMORY_BYTES)
}

pub fn check_device_with_budget<Device: BlockRead>(
    device: Device,
    max_memory_bytes: usize,
) -> Result<CheckReport> {
    check_reader(&Reader::new(device), max_memory_bytes)
}

pub(super) fn check_reader<Device: BlockRead>(
    reader: &Reader<Device>,
    max_memory_bytes: usize,
) -> Result<CheckReport> {
    let regions = BootRegions::read(reader)?;
    let boot = regions.strict()?;
    let bitmap_length =
        usize::try_from(u64::from(boot.cluster_count).div_ceil(8)).map_err(|_| Error::Bounds)?;
    let mut budget = Budget {
        remaining: max_memory_bytes.min(2 * bitmap_length + WORKSPACE_BYTES + FIXED_BYTES),
    };
    budget.take(
        FIXED_BYTES,
        "allocation audit fixed buffers exceed memory budget",
    )?;
    let mut ownership = Ownership {
        bytes: budget.bytes(
            bitmap_length,
            "allocation audit ownership bitmap exceeds memory budget",
        )?,
        count: 0,
    };
    let mut fat = Fat::new(reader, boot);
    let mut root = Allocation {
        first: boot.root_cluster,
        length: 0,
        contiguous: false,
        root: true,
    };
    ownership.walk(&mut fat, None, &mut root)?;
    let metadata = root_metadata(&mut fat, root)?;
    let active = usize::from((boot.volume_flags & 1) as u8);
    let mut bitmap_allocation = Allocation::system(
        metadata.bitmaps[active].ok_or(Error::Invalid("active bitmap missing"))?,
    );
    if bitmap_allocation.length != bitmap_length as u64 {
        return Err(Error::Invalid("bitmap stream length"));
    }
    ownership.walk(&mut fat, None, &mut bitmap_allocation)?;
    let mut bitmap = budget.bytes(
        bitmap_length,
        "allocation bitmap exceeds allocation audit memory budget",
    )?;
    load(&mut fat, bitmap_allocation, &mut bitmap)?;
    if ownership
        .bytes
        .iter()
        .zip(&bitmap)
        .any(|(owned, allocated)| owned & !allocated != 0)
    {
        return Err(Error::Invalid("system stream uses a free cluster"));
    }
    for (identity, stream) in metadata.bitmaps.iter().enumerate() {
        if identity != active {
            if let Some(stream) = stream {
                if stream.length != bitmap_length as u64 {
                    return Err(Error::Invalid("bitmap stream length"));
                }
                ownership.walk(&mut fat, Some(&bitmap), &mut Allocation::system(*stream))?;
            }
        }
    }
    if metadata.upcase.length == 0 || metadata.upcase.length > 131072 {
        return Err(Error::Invalid("upcase stream length"));
    }
    let mut upcase_allocation = Allocation::system(metadata.upcase);
    ownership.walk(&mut fat, Some(&bitmap), &mut upcase_allocation)?;
    let upcase = {
        let mut bytes = vec![0; metadata.upcase.length as usize];
        load(&mut fat, upcase_allocation, &mut bytes)?;
        UpcaseTable::parse(&bytes, metadata.upcase_checksum)?
    };
    let mut pending = Vec::new();
    budget.grow(&mut pending, 1)?;
    pending.push(root);
    let mut files = 0u64;
    let mut directories = 1u64;
    while let Some(allocation) = pending.pop() {
        let mut cursor = Cursor::new(allocation);
        let mut names = Names {
            units: Vec::new(),
            records: Vec::new(),
        };
        while let Some(set) = cursor.set(&mut fat)? {
            let mut entries = directory::parse(&set, boot, &upcase, allocation.root)?;
            if let Some(entry) = entries.pop() {
                names.add(entry.name_utf16(), &upcase, &mut budget)?;
                let mut stream = Allocation::stream(entry.stream());
                ownership.walk(&mut fat, Some(&bitmap), &mut stream)?;
                if entry.is_directory() {
                    directories += 1;
                    budget.grow(&mut pending, 1)?;
                    pending.push(stream);
                } else {
                    files += 1;
                }
            }
        }
        names.finish(&mut budget)?;
    }
    if !boot.cluster_count.is_multiple_of(8) {
        let last = bitmap.last_mut().unwrap();
        *last &= (1 << (boot.cluster_count % 8)) - 1;
    }
    let allocated_clusters = bitmap.iter().map(|byte| u64::from(byte.count_ones())).sum();
    Ok(CheckReport {
        files,
        directories,
        allocated_clusters,
        referenced_clusters: ownership.count,
        unreferenced_clusters: allocated_clusters - ownership.count,
        dirty: boot.volume_flags & 2 != 0,
        media_failure: boot.volume_flags & 4 != 0,
        boot_differences: regions.differences,
    })
}
