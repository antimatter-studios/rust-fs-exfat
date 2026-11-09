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

#[path = "primitive_chains.rs"]
mod chains;

#[path = "primitive_structures.rs"]
mod structures;

pub use structures::{
    bitmap_location, checked_bitmap_bit, checked_fat_entry, inspect_structures, read_structures,
    BitmapLocation, StructureCopy, StructureError, StructureGeometry,
};

pub use chains::{
    allocation_class, boot_check, chain_head, chain_islands, cluster_offset, clusters_for,
    compute_predecessors, compute_stats, decode_fat, is_allocated, predecessor, root_chain,
    valid_cluster, AllocationStats, BootCheck, ChainGroup, Island, StreamClusters, ALLOC_ALLOCATED,
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

impl BootSector {
    pub fn parse(sector: &[u8]) -> Result<Self> {
        if sector.len() < 512 {
            return Err(Error::Invalid("boot sector is shorter than 512 bytes"));
        }
        if &sector[3..11] != b"EXFAT   " {
            return Err(Error::Invalid(
                "no exFAT signature at offset 3: this is not an exFAT volume",
            ));
        }
        Ok(Self {
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
        })
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

/// SetChecksum over a whole set. Deleted entries clear InUse, so the bit is
/// restored on each entry's first byte before summing; that lets a deleted
/// set's stored checksum validate.
pub fn set_checksum(set: &[u8]) -> u16 {
    let mut checksum = 0u16;
    for (index, &byte) in set.iter().enumerate() {
        if index == 2 || index == 3 {
            continue;
        }
        let value = if index % ENTRY_SIZE == 0 {
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
    let header = data.get(position..position + ENTRY_SIZE * 2)?;
    if header[0] != TYPE_FILE && header[0] != TYPE_FILE_DELETED {
        return None;
    }
    if header[ENTRY_SIZE] != TYPE_STREAM && header[ENTRY_SIZE] != TYPE_STREAM_DELETED {
        return None;
    }
    let secondary_count = usize::from(header[1]);
    if !(MIN_SECONDARY_COUNT..=MAX_SECONDARY_COUNT).contains(&secondary_count) {
        return None;
    }
    let slots = secondary_count + 1;
    let set = data.get(position..position + ENTRY_SIZE * slots)?;
    let stream = &set[ENTRY_SIZE..ENTRY_SIZE * 2];
    let name_length = usize::from(stream[3]);
    if name_length == 0 {
        return None;
    }
    let name_entries = name_length.div_ceil(NAME_CHARS_PER_ENTRY);
    if name_entries + 1 > secondary_count {
        return None;
    }
    let mut units = Vec::with_capacity(name_length);
    for index in 0..name_entries {
        let entry = &set[ENTRY_SIZE * (2 + index)..ENTRY_SIZE * (3 + index)];
        if entry[0] != TYPE_NAME && entry[0] != TYPE_NAME_DELETED {
            return None;
        }
        units.extend(
            entry[2..ENTRY_SIZE]
                .chunks_exact(2)
                .map(|pair| u16_at(pair, 0)),
        );
    }
    units.truncate(name_length);

    let checksum_ok = set_checksum(set) == u16_at(set, 2);
    let attributes = u16_at(set, 4);
    let first_cluster = u32_at(stream, 20);
    let length = u64_at(stream, 24);
    if !checksum_ok {
        // Without a checksum, accept only values that could plausibly be real.
        let capacity = u64::from(limits.cluster_count) * limits.cluster_size;
        let name_ok = units.iter().all(|&unit| unit >= 0x20);
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
        name: String::from_utf16_lossy(&units),
        in_use: set[0] == TYPE_FILE,
        checksum_ok,
        attributes,
        is_dir: attributes & ATTRIBUTE_DIRECTORY != 0,
        first_cluster,
        length,
        valid_length: u64_at(stream, 8),
        no_fat_chain: stream[1] & STREAM_FLAG_NO_FAT_CHAIN != 0,
        created: decode_timestamp_with_offset(u32_at(set, 8), set[22]),
        modified: decode_timestamp_with_offset(u32_at(set, 12), set[23]),
        accessed: decode_timestamp_with_offset(u32_at(set, 16), set[24]),
        slots,
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
        .map(|entry| (u32_at(entry, 20), u64_at(entry, 24)))
}
