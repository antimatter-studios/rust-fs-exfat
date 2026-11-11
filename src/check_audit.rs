use crate::boot::{BootRegions, BootSector};
use crate::check::CheckReport;
use crate::device::Reader;
use crate::directory::{self, Stream};
use crate::metadata::{RootMetadata, SystemStream, UpcaseTable};
use crate::primitives::{self, FatChainStep, FatEntryReport, FatValue, StreamReadError};
use crate::{Error, Result};
use fs_core::BlockRead;
use std::mem::size_of;

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
    window: primitives::FatWindow,
}

impl<'reader, Device: BlockRead> Fat<'reader, Device> {
    fn new(reader: &'reader Reader<Device>, boot: &'reader BootSector) -> Self {
        Self {
            reader,
            boot,
            window: primitives::FatWindow::new(
                (u64::from(boot.fat_offset)
                    + primitives::VolumeState::from_flags(boot.volume_flags).active_fat as u64
                        * u64::from(boot.fat_length))
                    * boot.bytes_per_sector(),
                u64::from(boot.fat_length) * boot.bytes_per_sector(),
                boot.cluster_count,
            ),
        }
    }

    fn entry(&mut self, cluster: u32) -> Result<FatEntryReport> {
        self.boot.cluster_offset(cluster)?;
        self.window.entry(cluster, |offset, output| {
            self.reader.read_at(offset, output)
        })
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

    /// Cluster allocations of a File set's benign secondaries after its
    /// File Name entries.
    ///
    /// exFAT specification section 7.9 (Vendor Allocation Directory Entry,
    /// EntryType E1h): the entry has a cluster allocation of its own, with
    /// AllocationPossible 1 and FirstCluster and DataLength as defined in
    /// sections 6.4.3 and 6.4.4. Section 8.2: implementations keep the
    /// cluster allocations of unrecognized benign secondaries. Any benign
    /// secondary whose GeneralSecondaryFlags (section 6.4.2) set
    /// AllocationPossible therefore owns its clusters. A Vendor Extension
    /// (E0h, section 7.8) has no allocation, so its fields are not read as
    /// one even when it sets the flag.
    fn benign_secondaries(set: &[u8]) -> impl Iterator<Item = Self> + '_ {
        let names = primitives::RawEntrySet::parse(set).map_or(0, |raw| raw.names.len());
        set.chunks_exact(primitives::ENTRY_SIZE)
            .skip(2 + names)
            .filter(|entry| entry[0] & 0x20 != 0 && entry[0] != 0xe0 && entry[1] & 1 != 0)
            .map(|entry| Self {
                first: primitives::u32_at(entry, 20),
                length: primitives::u64_at(entry, 24),
                contiguous: entry[1] & 2 != 0,
                root: false,
            })
    }
}

struct Ownership {
    bytes: Vec<u8>,
    count: u64,
    /// Set while [`Self::claim_if_clean`] walks: claims are checked, not made.
    trial: bool,
}

impl Ownership {
    fn claim(&mut self, cluster: u32, boot: &BootSector, bitmap: Option<&[u8]>) -> Result<()> {
        boot.cluster_offset(cluster)?;
        if bitmap.is_some_and(|bytes| !primitives::checked_bitmap_bit(bytes, cluster)) {
            return Err(Error::Invalid("stream uses a free cluster"));
        }
        if self.trial {
            if primitives::checked_bitmap_bit(&self.bytes, cluster) {
                return Err(Error::Invalid("cross-linked allocation or FAT chain cycle"));
            }
            return Ok(());
        }
        if primitives::claim_bitmap_cluster(&mut self.bytes, cluster).ok_or(Error::Bounds)? {
            return Err(Error::Invalid("cross-linked allocation or FAT chain cycle"));
        }
        self.count += 1;
        Ok(())
    }

    /// Claims an allocation only when the whole of it is valid and unowned.
    ///
    /// The strict layer does not interpret benign secondaries (see the
    /// Vendor Allocation cases in `tests/check/deviations.rs`), so one that
    /// is out of range, free, cross-linked or the wrong length is left
    /// unclaimed rather than rejected, as before #71.
    fn claim_if_clean(
        &mut self,
        fat: &mut Fat<'_, impl BlockRead>,
        bitmap: &[u8],
        mut allocation: Allocation,
    ) -> Result<()> {
        self.trial = true;
        let trial = self.walk(fat, Some(bitmap), &mut allocation);
        self.trial = false;
        match trial {
            Ok(()) => self.walk(fat, Some(bitmap), &mut allocation),
            Err(Error::Device(error)) => Err(Error::Device(error)),
            Err(_) => Ok(()),
        }
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
        let boot = fat.boot;
        let mut count = 0u64;
        for step in primitives::walk_allocation(
            allocation.first,
            boot.cluster_count,
            allocation.contiguous.then_some(expected),
            |cluster| fat.entry(cluster),
        ) {
            match step? {
                FatChainStep::Cluster(position) => {
                    self.claim(position.cluster, boot, bitmap)?;
                    count += 1;
                }
                FatChainStep::Entry { entry, .. } => match entry.kind {
                    FatValue::End => break,
                    FatValue::Free | FatValue::Bad => {
                        return Err(Error::Invalid("free or bad cluster in FAT chain"));
                    }
                    FatValue::Reserved => {
                        return Err(Error::Invalid("reserved or out-of-range FAT entry"));
                    }
                    FatValue::Next(_) => {
                        if !allocation.root && count >= expected {
                            return Err(Error::Invalid(
                                "stream chain length differs from allocation",
                            ));
                        }
                    }
                },
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
    stream: primitives::StreamCursor,
}

impl Cursor {
    fn new(allocation: Allocation) -> Self {
        Self {
            stream: primitives::StreamCursor::new(
                allocation.first,
                allocation.length,
                allocation.contiguous,
            ),
        }
    }

    fn read(&mut self, fat: &mut Fat<'_, impl BlockRead>, output: &mut [u8]) -> Result<()> {
        let boot = fat.boot;
        let reader = fat.reader;
        self.stream
            .read(
                boot.cluster_size(),
                |cluster| fat.entry(cluster),
                |cluster, within, bytes| {
                    reader.read_at(boot.cluster_offset(cluster)? + within, bytes)
                },
                output,
            )
            .map_err(|error| match error {
                StreamReadError::End => Error::Invalid("short allocation audit stream"),
                StreamReadError::Link(entry) if entry.kind == FatValue::Reserved => {
                    Error::Invalid("reserved or out-of-range FAT entry")
                }
                StreamReadError::Link(_) => Error::Invalid("short allocation audit chain"),
                StreamReadError::Failure(error) => error,
            })
    }

    fn set(&mut self, fat: &mut Fat<'_, impl BlockRead>) -> Result<Option<Vec<u8>>> {
        if self.stream.finished() {
            return Ok(None);
        }
        let mut primary = [0; 32];
        self.read(fat, &mut primary)?;
        let record = primitives::directory_record(&primary).unwrap();
        if record.entry_type.value == 0 {
            return Ok(None);
        }
        let mut bytes = vec![0; record.length];
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
        let kind = primitives::EntryType::parse(&set).unwrap();
        if matches!(kind.value, 0x81..=0x83 | 0xa0) {
            if length == system.len() {
                return Err(Error::Invalid("duplicate root system metadata"));
            }
            system[length..length + 32].copy_from_slice(&set[..32]);
            length += 32;
        } else if kind.in_use && kind.value != 0x85 && (!kind.benign || kind.secondary) {
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
        trial: false,
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
    let state = primitives::VolumeState::from_flags(boot.volume_flags);
    let active = state.active_fat;
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
    if !primitives::bitmap_contains_all(&bitmap, &ownership.bytes) {
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
    let mut uninterpreted = primitives::UninterpretedCounts::default();
    // Claimed after every stream, so a benign secondary never turns a later
    // stream's claim into a cross-link.
    let mut secondaries = Vec::new();
    while let Some(allocation) = pending.pop() {
        let mut cursor = Cursor::new(allocation);
        let mut names = Names {
            units: Vec::new(),
            records: Vec::new(),
        };
        while let Some(set) = cursor.set(&mut fat)? {
            uninterpreted.tally(&set);
            let mut entries = directory::parse(&set, boot, &upcase, allocation.root)?;
            if let Some(entry) = entries.pop() {
                names.add(entry.name_utf16(), &upcase, &mut budget)?;
                let mut stream = Allocation::stream(entry.stream());
                ownership.walk(&mut fat, Some(&bitmap), &mut stream)?;
                for secondary in Allocation::benign_secondaries(&set) {
                    budget.grow(&mut secondaries, 1)?;
                    secondaries.push(secondary);
                }
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
    for secondary in secondaries.drain(..) {
        ownership.claim_if_clean(&mut fat, &bitmap, secondary)?;
    }
    let allocated_clusters = primitives::bitmap_count(&bitmap, boot.cluster_count);
    let referenced_clusters = ownership.count;
    let bad_clusters = unowned_bad_clusters(&mut fat, &bitmap, &ownership.bytes)?;
    // The audit's buffers go before the deviation scan takes its own.
    drop((bitmap, ownership, pending, secondaries, upcase));
    Ok(CheckReport {
        files,
        directories,
        allocated_clusters,
        referenced_clusters,
        bad_clusters,
        unreferenced_clusters: allocated_clusters - referenced_clusters - bad_clusters,
        dirty: state.dirty,
        media_failure: state.media_failure,
        boot_differences: regions.differences,
        uninterpreted,
        // Scanned only once the audit has accepted the volume, so deviations
        // can never turn a verdict into an error or the reverse.
        deviations: super::deviations::scan(reader, max_memory_bytes),
    })
}

/// Counts clusters the bitmap marks allocated, no allocation consumes, and the
/// active FAT marks bad.
///
/// exFAT specification section 4.1 (FAT region) and 4.1.3 (FatEntry[2] ..
/// FatEntry[ClusterCount+1]): an entry is a next cluster (2 ..
/// ClusterCount + 1), FFFFFFF7h for a bad cluster, or FFFFFFFFh for the end
/// of a chain. Section 7.1.5.1: a set bitmap bit means the cluster is not
/// available because "a cluster allocation may already consume the
/// corresponding cluster or the active FAT may describe the corresponding
/// cluster as bad". Such a cluster is therefore not lost.
fn unowned_bad_clusters(
    fat: &mut Fat<'_, impl BlockRead>,
    bitmap: &[u8],
    owned: &[u8],
) -> Result<u64> {
    let mut bad = 0u64;
    for (index, allocated) in bitmap.iter().enumerate() {
        let mut unowned = allocated & !owned.get(index).copied().unwrap_or(0);
        while unowned != 0 {
            let bit = unowned.trailing_zeros();
            unowned &= unowned - 1;
            let Some(cluster) = u32::try_from(index * 8)
                .ok()
                .and_then(|offset| offset.checked_add(bit + primitives::FIRST_CLUSTER))
            else {
                break;
            };
            if cluster - primitives::FIRST_CLUSTER >= fat.boot.cluster_count {
                break;
            }
            if matches!(fat.entry(cluster)?.kind, FatValue::Bad) {
                bad += 1;
            }
        }
    }
    Ok(bad)
}
