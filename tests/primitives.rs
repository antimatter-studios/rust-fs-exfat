use fs_exfat::primitives::{find_sets, Limits, ENTRY_SIZE};

#[path = "primitive_cases/boot.rs"]
mod boot;

#[path = "primitive_cases/boot_layering.rs"]
mod boot_layering;

#[path = "primitive_cases/parse.rs"]
mod parsing;

#[path = "primitive_cases/chains.rs"]
mod chains;

#[cfg(feature = "test-support")]
#[path = "primitive_cases/structures.rs"]
mod structures;

const TYPE_FILE: u8 = 0x85;
const TYPE_FILE_DELETED: u8 = 0x05;
const TYPE_STREAM: u8 = 0xC0;
const TYPE_STREAM_DELETED: u8 = 0x40;
const TYPE_NAME: u8 = 0xC1;
const TYPE_NAME_DELETED: u8 = 0x41;

const LIMITS: Limits = Limits {
    cluster_count: 1024,
    cluster_size: 4096,
};

/// SetChecksum as Microsoft's exFAT specification defines it: every byte of
/// the set except the two checksum bytes, with the InUse bit (0x80) counted as
/// set on each entry's first byte, so a deleted set still validates.
fn set_checksum(set: &[u8]) -> u16 {
    let mut sum = 0u16;
    for (index, &byte) in set.iter().enumerate() {
        if index == 2 || index == 3 {
            continue;
        }
        let value = if index % ENTRY_SIZE == 0 {
            byte | 0x80
        } else {
            byte
        };
        sum = sum.rotate_right(1).wrapping_add(u16::from(value));
    }
    sum
}

/// A file entry set naming `name`. `live` keeps InUse set; otherwise the
/// entries are deleted, with InUse cleared and the checksum computed as if
/// they were live.
fn entry_set(name: &str, first_cluster: u32, length: u64, live: bool) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let name_entries = units.len().div_ceil(15);
    let slots = 2 + name_entries;
    let mut set = vec![0u8; ENTRY_SIZE * slots];
    let (file, stream, name_type) = if live {
        (TYPE_FILE, TYPE_STREAM, TYPE_NAME)
    } else {
        (TYPE_FILE_DELETED, TYPE_STREAM_DELETED, TYPE_NAME_DELETED)
    };

    set[0] = file;
    set[1] = (slots - 1) as u8;
    set[ENTRY_SIZE] = stream;
    set[ENTRY_SIZE + 1] = 0x01;
    set[ENTRY_SIZE + 3] = units.len() as u8;
    set[ENTRY_SIZE + 20..ENTRY_SIZE + 24].copy_from_slice(&first_cluster.to_le_bytes());
    set[ENTRY_SIZE + 24..ENTRY_SIZE + 32].copy_from_slice(&length.to_le_bytes());
    for index in 0..name_entries {
        let entry = &mut set[ENTRY_SIZE * (2 + index)..ENTRY_SIZE * (3 + index)];
        entry[0] = name_type;
        for (slot, unit) in units[index * 15..].iter().take(15).enumerate() {
            entry[2 + slot * 2..4 + slot * 2].copy_from_slice(&unit.to_le_bytes());
        }
    }
    let checksum = set_checksum(&set);
    set[2..4].copy_from_slice(&checksum.to_le_bytes());
    set
}

#[test]
fn a_deleted_set_with_a_valid_checksum_is_found_and_marked_deleted() {
    let mut data = vec![0u8; 4096];
    let set = entry_set("report.txt", 7, 1234, false);
    data[512..512 + set.len()].copy_from_slice(&set);

    let found = find_sets(&data, data.len(), LIMITS);

    assert_eq!(found.len(), 1, "expected one set, found {found:?}");
    let found = &found[0];
    assert_eq!(found.offset, 512);
    assert_eq!(found.name, "report.txt");
    assert!(!found.in_use);
    assert!(found.checksum_ok);
    assert_eq!(found.first_cluster, 7);
    assert_eq!(found.length, 1234);
    assert_eq!(found.slots, 3);
}

#[test]
fn a_live_set_is_found_as_in_use() {
    let data = entry_set("notes", 9, 10, true);

    let found = find_sets(&data, data.len(), LIMITS);

    assert_eq!(found.len(), 1);
    assert!(found[0].in_use);
    assert!(found[0].checksum_ok);
}
