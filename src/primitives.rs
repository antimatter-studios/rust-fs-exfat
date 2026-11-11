//! Permissive on-disk decoders over raw bytes, including structures the strict
//! reader would reject. Read-only by type: nothing here takes a writable device.
//!
//! Field layout and checksum rules follow Microsoft's published exFAT
//! specification.
//!
//! # Chains and allocation
//!
//! Volume-level format operations are exposed as pure functions over borrowed
//! geometry and slices: FAT bytes map to [`decode_fat`], the root directory to
//! [`root_chain`], and stream claims to [`clusters_for`] and [`StreamClusters`].
//! Geometry and bitmap operations map to [`valid_cluster`], [`cluster_offset`],
//! [`is_allocated`] and [`allocation_class`]. The existing [`Limits`] carries the
//! entry-set geometry without a volume wrapper.
//! [`compute_predecessors`] builds the index consumed by [`predecessor`] and
//! [`chain_head`]; [`chain_islands`] and [`compute_stats`] expose topology and
//! allocation without requiring a device or a reporting layer. [`boot_check`]
//! compares already-read boot copies using the checksum primitives above.
//! [`stale_chains`] groups the non-zero FAT entries of free clusters into the
//! chains they link and classifies each chain's end, over caller-supplied
//! [`ClusterRecord`] lookups rather than whole FAT or bitmap slices.
//! [`uninterpreted_entries`] walks directory bytes for the entries this driver
//! does not interpret, vendor entries and unknown types, with each one's InUse
//! bit and the validity of the entry set it lies in.
//!
//! Chain, island and allocation-class names describe general concepts. FAT and
//! cluster-heap geometry retain their exFAT-defined names. These helpers are not
//! strict-reader validators: broken chains yield partial walks, all nonzero FAT
//! values affect allocation classes, and headless cycles are not islands. Raw
//! FAT, bitmap and predecessor slices must cover every indexed cluster; malformed
//! geometry and undersized slices retain unchecked arithmetic/index preconditions.
//!
//! [`read_structures`] loads raw fixed-512-byte-sector snapshots through a
//! read-only callback. [`inspect_structures`] decodes saved copies and reports
//! boot checksums and signatures without enforcing strict-reader validity.
//! [`bitmap_location`] checks only the declared bitmap's minimum byte length.
//! [`checked_fat_entry`] and [`checked_bitmap_bit`] accept incomplete snapshots
//! without imposing volume geometry or interpreting reserved FAT values.
//!
//! [`RawAllocationBitmap`], [`RawUpcaseTable`] and [`RawRootMetadata`] retain
//! damaged metadata and report length, checksum, mapping and presence validity.
//! The strict metadata API alone decides which observations require rejection.
//!
//! [`FatWindow`] and [`StreamCursor`] read in fixed 64 KiB chunks, and
//! [`walk_allocation`] leaves cycle detection to a caller-owned bitmap rather
//! than allocating per-cluster state. [`directory_record`] frames streamed entry
//! sets; bitmap claims, population and coverage operate on borrowed slices.
//! The allocation audit retains its budget, rejection and completeness policy.

#[path = "primitive_chains.rs"]
mod chains;

#[path = "primitive_structures.rs"]
mod structures;

#[path = "primitive_streaming.rs"]
mod streaming;

#[path = "primitive_stale.rs"]
mod stale;

pub use stale::{stale_chains, ClusterRecord, StaleChainWalk, StaleEnd, StaleSummary};

#[path = "primitive_uninterpreted.rs"]
mod uninterpreted;

pub use uninterpreted::{
    uninterpreted_entries, DirectoryWalk, SetMembership, UninterpretedCounts, UninterpretedKind,
    UninterpretedRecord,
};

pub use streaming::{
    bitmap_contains_all, bitmap_count, claim_bitmap_cluster, directory_record, walk_allocation,
    DirectoryRecord, FatWindow, StreamCursor, StreamReadError, VolumeState,
};

pub use structures::{
    bitmap_location, checked_bitmap_bit, checked_fat_entry, inspect_structures, read_structures,
    BitmapLocation, StructureCopy, StructureError, StructureGeometry,
};

pub use chains::{
    allocation_class, boot_check, chain_head, chain_islands, cluster_offset, cluster_reads,
    clusters_for, compute_predecessors, compute_stats, contiguous_clusters, decode_fat,
    decode_fat_entry, is_allocated, predecessor, read_fat_entry, root_chain, valid_cluster,
    walk_fat_chain, AllocationStats, BootCheck, ChainGroup, ChainPosition, ClusterRead,
    FatChainStep, FatEntryReport, FatValue, Island, StreamClusters, ALLOC_ALLOCATED,
    ALLOC_FREE_FAT, ALLOC_FREE_ZERO, ISLAND_END_BROKEN, ISLAND_END_EOF, ISLAND_END_JOINS_ANOTHER,
};

use crate::{Error, Result};
use serde::Serialize;

/// Size of one directory entry on disk.
pub const ENTRY_SIZE: usize = 32;
pub const FAT_END: u32 = 0xFFFF_FFFF;
pub const FAT_BAD: u32 = 0xFFFF_FFF7;
pub const FIRST_CLUSTER: u32 = 2;
pub const BOOT_REGION_SECTORS: u64 = 12;

const VOLUME_DIRTY: u16 = 0x0002;
const ATTRIBUTE_DIRECTORY: u16 = 0x10;
const ATTRIBUTE_VALID_MASK: u16 = 0x37;
const TYPE_FILE: u8 = 0x85;
const TYPE_FILE_DELETED: u8 = 0x05;
const TYPE_STREAM: u8 = 0xC0;
const TYPE_STREAM_DELETED: u8 = 0x40;
const TYPE_NAME: u8 = 0xC1;
const TYPE_NAME_DELETED: u8 = 0x41;
const TYPE_BITMAP: u8 = 0x81;
const NAME_CHARS_PER_ENTRY: usize = 15;
const MIN_SECONDARY_COUNT: usize = 2;
const MAX_SECONDARY_COUNT: usize = 18;
const STREAM_FLAG_NO_FAT_CHAIN: u8 = 0x02;
const UTC_OFFSET_VALID: u8 = 0x80;
const MINUTES_PER_OFFSET_STEP: i32 = 15;

/// A permissively decoded boot sector, separate from the strict reader's boot
/// sector. Only the minimum length and filesystem name are required; callers
/// must assess geometry and `signature_ok` before using the raw accessors.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BootSector {
    pub file_system_name: String,
    pub partition_offset: u64,
    pub volume_length: u64,
    pub fat_offset: u32,
    pub fat_length: u32,
    pub cluster_heap_offset: u32,
    pub cluster_count: u32,
    pub root_cluster: u32,
    pub volume_serial: u32,
    pub revision: u16,
    pub volume_flags: u16,
    pub bytes_per_sector_shift: u8,
    pub sectors_per_cluster_shift: u8,
    pub number_of_fats: u8,
    pub percent_in_use: u8,
    pub signature_ok: bool,
}

/// Raw boot fields and identity diagnostics, including unrecognized filesystem
/// names. Only a buffer shorter than 512 bytes is rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootSectorHeader {
    pub sector: BootSector,
    pub jump_valid: bool,
    pub file_system_name_valid: bool,
    pub must_be_zero_valid: bool,
}

impl BootSectorHeader {
    pub fn parse(sector: &[u8]) -> Result<Self> {
        if sector.len() < 512 {
            return Err(Error::Invalid("boot sector is shorter than 512 bytes"));
        }
        Ok(Self {
            sector: BootSector {
                file_system_name: String::from_utf8_lossy(&sector[3..11]).into_owned(),
                partition_offset: u64_at(sector, 64),
                volume_length: u64_at(sector, 72),
                fat_offset: u32_at(sector, 80),
                fat_length: u32_at(sector, 84),
                cluster_heap_offset: u32_at(sector, 88),
                cluster_count: u32_at(sector, 92),
                root_cluster: u32_at(sector, 96),
                volume_serial: u32_at(sector, 100),
                revision: u16_at(sector, 104),
                volume_flags: u16_at(sector, 106),
                bytes_per_sector_shift: sector[108],
                sectors_per_cluster_shift: sector[109],
                number_of_fats: sector[110],
                percent_in_use: sector[112],
                signature_ok: sector[510] == 0x55 && sector[511] == 0xAA,
            },
            jump_valid: sector[..3] == [0xeb, 0x76, 0x90],
            file_system_name_valid: &sector[3..11] == b"EXFAT   ",
            must_be_zero_valid: sector[11..64].iter().all(|byte| *byte == 0),
        })
    }
}

impl BootSector {
    pub fn parse(sector: &[u8]) -> Result<Self> {
        let header = BootSectorHeader::parse(sector)?;
        if !header.file_system_name_valid {
            return Err(Error::Invalid(
                "no exFAT signature at offset 3: this is not an exFAT volume",
            ));
        }
        Ok(header.sector)
    }

    pub fn bytes_per_sector(&self) -> u64 {
        1 << self.bytes_per_sector_shift
    }

    pub fn cluster_size(&self) -> u64 {
        self.bytes_per_sector() << self.sectors_per_cluster_shift
    }

    /// Set while a volume is mounted for writing, and cleared by a clean unmount.
    pub fn volume_dirty(&self) -> bool {
        self.volume_flags & VOLUME_DIRTY != 0
    }

    pub fn heap_offset(&self) -> u64 {
        u64::from(self.cluster_heap_offset) * self.bytes_per_sector()
    }

    pub fn last_cluster_exclusive(&self) -> u32 {
        self.cluster_count + FIRST_CLUSTER
    }
}

/// Checksum over sectors 0-10, skipping VolumeFlags and PercentInUse.
///
/// Panics when the complete first eleven sectors are not present.
pub fn boot_checksum(region: &[u8], bytes_per_sector: usize) -> u32 {
    let mut checksum = 0u32;
    for (index, &byte) in region[..bytes_per_sector * 11].iter().enumerate() {
        if matches!(index, 106 | 107 | 112) {
            continue;
        }
        checksum = checksum.rotate_right(1).wrapping_add(u32::from(byte));
    }
    checksum
}

/// The first stored checksum word in sector twelve, without checking repeats.
///
/// Panics when that word is not present.
pub fn stored_boot_checksum(region: &[u8], bytes_per_sector: usize) -> u32 {
    u32_at(region, bytes_per_sector * 11)
}

/// A complete boot copy with raw fields and checksum/signature diagnostics.
/// Damaged identities, geometry, checksum repetitions and extended signatures
/// are retained, not rejected. Sector size and complete buffers are required.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootRegion {
    pub header: BootSectorHeader,
    pub computed_checksum: u32,
    pub stored_checksum: u32,
    pub checksum_valid: bool,
    pub extended_signatures_valid: bool,
}

impl BootRegion {
    pub fn parse(region: &[u8], bytes_per_sector: usize) -> Result<Self> {
        if !matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096) {
            return Err(Error::Invalid("checksum sector size"));
        }
        if region.len() < bytes_per_sector * 11 {
            return Err(Error::Invalid("short boot region"));
        }
        let stored = region
            .get(bytes_per_sector * 11..bytes_per_sector * 12)
            .ok_or(Error::Invalid("short checksum sector"))?;
        let computed_checksum = boot_checksum(region, bytes_per_sector);
        Ok(Self {
            header: BootSectorHeader::parse(region)?,
            computed_checksum,
            stored_checksum: stored_boot_checksum(region, bytes_per_sector),
            checksum_valid: stored
                .chunks_exact(4)
                .all(|entry| u32_at(entry, 0) == computed_checksum),
            extended_signatures_valid: (1..=8)
                .all(|index| u32_at(region, (index + 1) * bytes_per_sector - 4) == 0xaa55_0000),
        })
    }
}

/// Decodes a two-second-resolution timestamp, without calendar-day validation.
/// Zero and out-of-range individual fields return `None`.
pub fn decode_timestamp(value: u32) -> Option<String> {
    if value == 0 {
        return None;
    }
    let year = 1980 + (value >> 25);
    let month = (value >> 21) & 0xF;
    let day = (value >> 16) & 0x1F;
    let hour = (value >> 11) & 0x1F;
    let minute = (value >> 5) & 0x3F;
    let second = (value & 0x1F) * 2;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"
    ))
}

/// Formats a valid signed seven-bit count of quarter-hour UTC offset steps.
/// Without OffsetValid, the time is local to an unknown zone.
pub fn decode_utc_offset(byte: u8) -> Option<String> {
    if byte & UTC_OFFSET_VALID == 0 {
        return None;
    }
    let steps = i32::from(((byte & 0x7F) << 1) as i8 >> 1);
    let minutes = steps * MINUTES_PER_OFFSET_STEP;
    let sign = if minutes < 0 { '-' } else { '+' };
    let minutes = minutes.abs();
    Some(format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60))
}

/// A timestamp followed by its UTC offset only when the entry recorded one.
pub fn decode_timestamp_with_offset(value: u32, offset: u8) -> Option<String> {
    let time = decode_timestamp(value)?;
    Some(match decode_utc_offset(offset) {
        Some(offset) => time + offset.as_str(),
        None => time,
    })
}

/// Limits that a candidate entry set must respect when its checksum fails.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub cluster_count: u32,
    pub cluster_size: u64,
}

impl Limits {
    fn valid_cluster(&self, cluster: u32) -> bool {
        (FIRST_CLUSTER..self.cluster_count + FIRST_CLUSTER).contains(&cluster)
    }
}

/// A file entry set found in raw bytes.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EntrySet {
    pub offset: usize,
    pub name: String,
    pub in_use: bool,
    pub checksum_ok: bool,
    pub attributes: u16,
    pub is_dir: bool,
    pub first_cluster: u32,
    pub length: u64,
    pub valid_length: u64,
    pub no_fat_chain: bool,
    pub created: Option<String>,
    pub modified: Option<String>,
    pub accessed: Option<String>,
    pub slots: usize,
}

/// Reads a little-endian word, panicking if the bytes are absent.
pub fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

/// Reads a little-endian double word, panicking if the bytes are absent.
pub fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().expect("four bytes"))
}

/// Reads a little-endian quad word, panicking if the bytes are absent.
pub fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().expect("eight bytes"))
}

/// Raw entry type and its format-defined flags, without rejecting unknown types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryType {
    pub value: u8,
    pub in_use: bool,
    pub secondary: bool,
    pub benign: bool,
}

impl EntryType {
    pub fn parse(data: &[u8]) -> Option<Self> {
        let value = *data.first()?;
        Some(Self {
            value,
            in_use: value & 0x80 != 0,
            secondary: value & 0x40 != 0,
            benign: value & 0x20 != 0,
        })
    }
}

/// Primary entry fields, also available for non-file primary types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePrimary {
    pub raw: [u8; ENTRY_SIZE],
    pub entry_type: EntryType,
    pub secondary_count: u8,
    pub stored_checksum: u16,
    pub attributes: u16,
}

/// Stream fields at the first secondary slot, regardless of its type or flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamExtension {
    pub entry_type: EntryType,
    pub flags: u8,
    pub allocation_possible: bool,
    pub no_fat_chain: bool,
    pub name_length: u8,
    pub name_hash: u16,
    pub valid_data_length: u64,
    pub first_cluster: u32,
    pub data_length: u64,
}

/// Name-slot fields without imposing entry order, flags or UTF-16 validity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileNameEntry {
    pub entry_type: EntryType,
    pub flags: u8,
    pub utf16: [u16; NAME_CHARS_PER_ENTRY],
}

/// A decoded name retains its original code units even when rendering is lossy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntrySetName {
    pub utf16: Vec<u16>,
    pub text: String,
    pub utf16_ok: bool,
}

/// A complete declared set, including damaged files and unknown benign sets.
/// Only missing bytes prevent decoding; strict and recovery policies remain
/// separate. `checksum_ok` checks the actual bytes, without restoring InUse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawEntrySet<'a> {
    pub bytes: &'a [u8],
    pub primary: FilePrimary,
    pub stream: Option<StreamExtension>,
    pub secondaries: Vec<EntryType>,
    pub names: Vec<FileNameEntry>,
    pub name_complete: bool,
    pub checksum_ok: bool,
}

impl<'a> RawEntrySet<'a> {
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let primary = data.get(..ENTRY_SIZE)?;
        let secondary_count = primary[1];
        let bytes = data.get(..(usize::from(secondary_count) + 1) * ENTRY_SIZE)?;
        let primary = FilePrimary {
            raw: primary.try_into().unwrap(),
            entry_type: EntryType::parse(primary)?,
            secondary_count,
            stored_checksum: u16_at(primary, 2),
            attributes: u16_at(primary, 4),
        };
        let stream = bytes
            .get(ENTRY_SIZE..ENTRY_SIZE * 2)
            .map(|entry| StreamExtension {
                entry_type: EntryType::parse(entry).unwrap(),
                flags: entry[1],
                allocation_possible: entry[1] & 1 != 0,
                no_fat_chain: entry[1] & STREAM_FLAG_NO_FAT_CHAIN != 0,
                name_length: entry[3],
                name_hash: u16_at(entry, 4),
                valid_data_length: u64_at(entry, 8),
                first_cluster: u32_at(entry, 20),
                data_length: u64_at(entry, 24),
            });
        let name_entries = stream.as_ref().map_or(0, |stream| {
            usize::from(stream.name_length).div_ceil(NAME_CHARS_PER_ENTRY)
        });
        let names: Vec<_> = bytes[ENTRY_SIZE..]
            .chunks_exact(ENTRY_SIZE)
            .skip(1)
            .take(name_entries)
            .map(|entry| FileNameEntry {
                entry_type: EntryType::parse(entry).unwrap(),
                flags: entry[1],
                utf16: std::array::from_fn(|index| u16_at(entry, 2 + index * 2)),
            })
            .collect();
        let name_complete = stream.is_some() && names.len() == name_entries;
        Some(Self {
            bytes,
            checksum_ok: entry_set_checksum(bytes) == primary.stored_checksum,
            primary,
            stream,
            secondaries: bytes[ENTRY_SIZE..]
                .chunks_exact(ENTRY_SIZE)
                .map(|entry| EntryType::parse(entry).unwrap())
                .collect(),
            names,
            name_complete,
        })
    }

    pub fn name(&self) -> EntrySetName {
        let length = self
            .stream
            .as_ref()
            .map_or(0, |stream| usize::from(stream.name_length));
        let utf16: Vec<_> = self
            .names
            .iter()
            .flat_map(|entry| entry.utf16)
            .take(length)
            .collect();
        let decoded = String::from_utf16(&utf16);
        EntrySetName {
            utf16_ok: decoded.is_ok(),
            text: decoded.unwrap_or_else(|_| String::from_utf16_lossy(&utf16)),
            utf16,
        }
    }
}

/// NameHash using the caller's volume-specific upcase mapping.
pub fn name_hash(name: &[u16], upcase: impl Fn(u16) -> u16) -> u16 {
    name.iter()
        .flat_map(|unit| upcase(*unit).to_le_bytes())
        .fold(0u16, |sum, byte| {
            sum.rotate_right(1).wrapping_add(u16::from(byte))
        })
}

/// SetChecksum over the actual bytes, excluding the stored checksum itself.
pub fn entry_set_checksum(set: &[u8]) -> u16 {
    checksum_entries(set, false)
}

/// SetChecksum over a whole set. Deleted entries clear InUse, so the bit is
/// restored on each entry's first byte before summing; that lets a deleted
/// set's stored checksum validate.
pub fn set_checksum(set: &[u8]) -> u16 {
    checksum_entries(set, true)
}

fn checksum_entries(set: &[u8], restore_in_use: bool) -> u16 {
    let mut checksum = 0u16;
    for (index, &byte) in set.iter().enumerate() {
        if index == 2 || index == 3 {
            continue;
        }
        let value = if restore_in_use && index % ENTRY_SIZE == 0 {
            byte | 0x80
        } else {
            byte
        };
        checksum = checksum.rotate_right(1).wrapping_add(u16::from(value));
    }
    checksum
}

/// Parses the entry set at `position`. Returns `None` when the bytes there do
/// not have the structure of a file entry set.
pub fn parse_set(data: &[u8], position: usize, limits: Limits) -> Option<EntrySet> {
    let raw = RawEntrySet::parse(data.get(position..)?)?;
    if raw.primary.entry_type.value != TYPE_FILE
        && raw.primary.entry_type.value != TYPE_FILE_DELETED
    {
        return None;
    }
    let stream = raw.stream.as_ref()?;
    if stream.entry_type.value != TYPE_STREAM && stream.entry_type.value != TYPE_STREAM_DELETED {
        return None;
    }
    let secondary_count = usize::from(raw.primary.secondary_count);
    if !(MIN_SECONDARY_COUNT..=MAX_SECONDARY_COUNT).contains(&secondary_count) {
        return None;
    }
    let set = raw.bytes;
    if stream.name_length == 0 || !raw.name_complete {
        return None;
    }
    if raw.names.iter().any(|entry| {
        entry.entry_type.value != TYPE_NAME && entry.entry_type.value != TYPE_NAME_DELETED
    }) {
        return None;
    }
    let name = raw.name();
    let checksum_ok = set_checksum(set) == raw.primary.stored_checksum;
    let attributes = raw.primary.attributes;
    let first_cluster = stream.first_cluster;
    let length = stream.data_length;
    if !checksum_ok {
        // Without a checksum, accept only values that could plausibly be real.
        let capacity = u64::from(limits.cluster_count) * limits.cluster_size;
        let name_ok = name.utf16.iter().all(|&unit| unit >= 0x20);
        if attributes & !ATTRIBUTE_VALID_MASK != 0
            || length > capacity
            || !name_ok
            || !(first_cluster == 0 || limits.valid_cluster(first_cluster))
        {
            return None;
        }
    }
    Some(EntrySet {
        offset: position,
        name: name.text,
        in_use: raw.primary.entry_type.in_use,
        checksum_ok,
        attributes,
        is_dir: attributes & ATTRIBUTE_DIRECTORY != 0,
        first_cluster,
        length,
        valid_length: stream.valid_data_length,
        no_fat_chain: stream.no_fat_chain,
        created: decode_timestamp_with_offset(u32_at(set, 8), set[22]),
        modified: decode_timestamp_with_offset(u32_at(set, 12), set[23]),
        accessed: decode_timestamp_with_offset(u32_at(set, 16), set[24]),
        slots: secondary_count + 1,
    })
}

/// Finds every entry set starting within `data[..search_end]`. Sets may extend
/// past `search_end` into the rest of `data`.
pub fn find_sets(data: &[u8], search_end: usize, limits: Limits) -> Vec<EntrySet> {
    let mut found = Vec::new();
    let mut position = 0;
    while position < search_end {
        match parse_set(data, position, limits) {
            Some(set) => {
                position += if set.checksum_ok {
                    set.slots * ENTRY_SIZE
                } else {
                    ENTRY_SIZE
                };
                found.push(set);
            }
            None => position += ENTRY_SIZE,
        }
    }
    found
}

/// Location of the first allocation bitmap entry, without validating its flags,
/// geometry, or preceding directory end markers.
pub fn find_bitmap_entry(root: &[u8]) -> Option<(u32, u64)> {
    root.chunks_exact(ENTRY_SIZE)
        .find(|entry| entry[0] == TYPE_BITMAP)
        .map(|entry| {
            let stream = SystemStream::from_entry(entry);
            (stream.first_cluster, stream.length)
        })
}

/// Rotating-add checksum used by the upcase stream.
pub fn checksum32(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |sum, byte| {
        sum.rotate_right(1).wrapping_add(u32::from(*byte))
    })
}

/// Borrowed bitmap bytes, including streams of the wrong declared size.
#[derive(Clone, Copy, Debug)]
pub struct RawAllocationBitmap<'a> {
    pub bytes: &'a [u8],
    pub cluster_count: u32,
    pub length_ok: bool,
}

impl<'a> RawAllocationBitmap<'a> {
    pub fn parse(bytes: &'a [u8], cluster_count: u32) -> Self {
        Self {
            bytes,
            cluster_count,
            length_ok: bytes.len() as u64 == u64::from(cluster_count).div_ceil(8),
        }
    }

    /// An available heap bit; `None` means outside the heap or missing bytes.
    pub fn allocated(&self, cluster: u32) -> Option<bool> {
        let index = cluster.checked_sub(FIRST_CLUSTER)?;
        if index >= self.cluster_count {
            return None;
        }
        self.bytes
            .get(index as usize / 8)
            .map(|byte| byte & (1 << (index % 8)) != 0)
    }
}

/// The first structural problem encountered while expanding an upcase stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpcaseIssue {
    TruncatedRun,
    RunOutsideRange,
    ExtraMappings,
}

/// Raw stream and bounded, possibly partial mappings, independent of validity.
#[derive(Debug)]
pub struct RawUpcaseTable<'a> {
    pub data: &'a [u8],
    pub mappings: Vec<u16>,
    pub checksum: u32,
    pub length_ok: bool,
    pub checksum_ok: bool,
    pub issue: Option<UpcaseIssue>,
    pub complete: bool,
    pub ascii_ok: bool,
}

impl<'a> RawUpcaseTable<'a> {
    pub fn parse(data: &'a [u8], expected_checksum: u32) -> Self {
        let checksum = checksum32(data);
        let mut mappings = Vec::with_capacity(65536);
        let mut words = data.chunks_exact(2).map(|word| u16_at(word, 0));
        let mut issue = None;
        while let Some(value) = words.next() {
            if value == 0xffff && mappings.len() < 65535 {
                let Some(count) = words.next().map(usize::from) else {
                    issue = Some(UpcaseIssue::TruncatedRun);
                    break;
                };
                let end = mappings.len() + count;
                if count == 0 || end > 65536 {
                    issue = Some(UpcaseIssue::RunOutsideRange);
                    break;
                }
                mappings.extend((mappings.len()..end).map(|unit| unit as u16));
            } else {
                if mappings.len() == 65536 {
                    issue = Some(UpcaseIssue::ExtraMappings);
                    break;
                }
                mappings.push(value);
            }
        }
        let complete = mappings.len() == 65536;
        let ascii_ok = (0u16..128).all(|unit| {
            let expected = if (97..=122).contains(&unit) {
                unit - 32
            } else {
                unit
            };
            mappings.get(usize::from(unit)) == Some(&expected)
        });
        Self {
            data,
            mappings,
            checksum,
            length_ok: !data.is_empty() && data.len().is_multiple_of(2) && data.len() <= 131072,
            checksum_ok: checksum == expected_checksum,
            issue,
            complete,
            ascii_ok,
        }
    }
}

/// Raw system-stream location, without cluster or length policy.
#[derive(Clone, Copy, Debug)]
pub struct SystemStream {
    pub first_cluster: u32,
    pub length: u64,
}

impl SystemStream {
    fn from_entry(entry: &[u8]) -> Self {
        Self {
            first_cluster: u32_at(entry, 20),
            length: u64_at(entry, 24),
        }
    }
}

/// System-entry fields and local validity, without duplicate or required-entry policy.
#[derive(Debug)]
pub enum RootEntryKind {
    Bitmap {
        flags: u8,
        id: usize,
        flags_ok: bool,
        stream: SystemStream,
    },
    Upcase {
        stream: SystemStream,
        checksum: u32,
    },
    Label {
        length: u8,
        units: Vec<u16>,
        value: String,
        length_ok: bool,
        characters_ok: bool,
        utf16_ok: bool,
    },
    Guid {
        secondary_count: u8,
        flags: u16,
        value: [u8; 16],
        checksum: u16,
        checksum_ok: bool,
    },
    File {
        secondary_count: u8,
        complete: bool,
    },
    Benign {
        secondary_count: u8,
        complete: bool,
    },
    Unknown,
}

/// An entry retains its original InUse bit even when its fields are damaged.
#[derive(Debug)]
pub struct RawRootEntry {
    /// Byte offset of the entry within the parsed data.
    pub offset: usize,
    pub entry_type: u8,
    pub in_use: bool,
    pub kind: RootEntryKind,
}

/// Root entries up to the end marker, with presence and alignment observations.
/// Secondary sets are traversed as units; deleted primaries do not hide entries.
#[derive(Debug)]
pub struct RawRootMetadata {
    pub aligned: bool,
    pub entries: Vec<RawRootEntry>,
    pub bitmap_present: [bool; 2],
    pub upcase_present: bool,
}

impl RawRootMetadata {
    pub fn parse(data: &[u8]) -> Self {
        let mut result = Self {
            aligned: data.len().is_multiple_of(ENTRY_SIZE),
            entries: Vec::new(),
            bitmap_present: [false; 2],
            upcase_present: false,
        };
        let mut position: usize = 0;
        while let Some(entry) = data.get(position..position.saturating_add(ENTRY_SIZE)) {
            if entry[0] == 0 {
                break;
            }
            let offset = position;
            position += ENTRY_SIZE;
            let in_use = entry[0] & 0x80 != 0;
            let next = position.saturating_add(usize::from(entry[1]) * ENTRY_SIZE);
            let kind = match entry[0] | 0x80 {
                0x81 => {
                    let id = usize::from(entry[1] & 1);
                    result.bitmap_present[id] |= in_use;
                    RootEntryKind::Bitmap {
                        flags: entry[1],
                        id,
                        flags_ok: entry[1] & !1 == 0,
                        stream: SystemStream::from_entry(entry),
                    }
                }
                0x82 => {
                    result.upcase_present |= in_use;
                    RootEntryKind::Upcase {
                        stream: SystemStream::from_entry(entry),
                        checksum: u32_at(entry, 4),
                    }
                }
                0x83 => {
                    let length = entry[1];
                    let units: Vec<_> = (0..usize::from(length).min(11))
                        .map(|index| u16_at(entry, 2 + index * 2))
                        .collect();
                    let characters_ok = units.iter().all(|unit| {
                        *unit >= 32 && ![34, 42, 47, 58, 60, 62, 63, 92, 124].contains(unit)
                    });
                    let decoded = String::from_utf16(&units);
                    let utf16_ok = decoded.is_ok();
                    let value = decoded.unwrap_or_else(|_| String::from_utf16_lossy(&units));
                    RootEntryKind::Label {
                        length,
                        units,
                        value,
                        length_ok: length <= 11,
                        characters_ok,
                        utf16_ok,
                    }
                }
                0xa0 => RootEntryKind::Guid {
                    secondary_count: entry[1],
                    flags: u16_at(entry, 4),
                    value: entry[6..22].try_into().expect("16 GUID bytes"),
                    checksum: u16_at(entry, 2),
                    checksum_ok: entry_set_checksum(entry) == u16_at(entry, 2),
                },
                0x85 => {
                    if in_use {
                        position = next;
                    }
                    RootEntryKind::File {
                        secondary_count: entry[1],
                        complete: next <= data.len(),
                    }
                }
                kind if kind & 0x60 == 0x20 => {
                    if in_use {
                        position = next;
                    }
                    RootEntryKind::Benign {
                        secondary_count: entry[1],
                        complete: next <= data.len(),
                    }
                }
                _ => RootEntryKind::Unknown,
            };
            result.entries.push(RawRootEntry {
                offset,
                entry_type: entry[0],
                in_use,
                kind,
            });
        }
        result
    }
}
