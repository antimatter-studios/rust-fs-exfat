//! Primitives over raw bytes: finds entry sets in data that the strict reader
//! would not walk. Read-only by type: nothing here takes a writable device.
//!
//! Field layout and checksum rules follow Microsoft's published exFAT
//! specification.

/// Size of one directory entry on disk.
pub const ENTRY_SIZE: usize = 32;

const ATTRIBUTE_VALID_MASK: u16 = 0x37;
const TYPE_FILE: u8 = 0x85;
const TYPE_FILE_DELETED: u8 = 0x05;
const TYPE_STREAM: u8 = 0xC0;
const TYPE_STREAM_DELETED: u8 = 0x40;
const TYPE_NAME: u8 = 0xC1;
const TYPE_NAME_DELETED: u8 = 0x41;
const NAME_CHARS_PER_ENTRY: usize = 15;
const MIN_SECONDARY_COUNT: usize = 2;
const MAX_SECONDARY_COUNT: usize = 18;
const FIRST_CLUSTER: u32 = 2;

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntrySet {
    pub offset: usize,
    pub name: String,
    pub in_use: bool,
    pub checksum_ok: bool,
    pub first_cluster: u32,
    pub length: u64,
    pub slots: usize,
}

fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap_or([0; 8]))
}

/// SetChecksum over a whole set. Deleted entries clear InUse, so the bit is
/// restored on each entry's first byte before summing; that lets a deleted
/// set's stored checksum validate.
fn set_checksum(set: &[u8]) -> u16 {
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
        first_cluster,
        length,
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
