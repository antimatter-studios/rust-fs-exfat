use fs_exfat::primitives::{entry_set_checksum, parse_set, RawEntrySet, ENTRY_SIZE};

fn seal(set: &mut [u8]) {
    let checksum = entry_set_checksum(set);
    set[2..4].copy_from_slice(&checksum.to_le_bytes());
}

#[test]
fn raw_sets_preserve_bad_checksums_and_implausible_stream_values() {
    let mut bytes = super::entry_set("raw", 7, 1234, true);
    bytes[4..6].copy_from_slice(&0xffffu16.to_le_bytes());
    bytes[40..48].copy_from_slice(&u64::MAX.to_le_bytes());
    bytes[52..56].copy_from_slice(&u32::MAX.to_le_bytes());
    bytes[56..64].copy_from_slice(&u64::MAX.to_le_bytes());
    let raw = RawEntrySet::parse(&bytes).unwrap();
    assert!(!raw.checksum_ok);
    assert_eq!(raw.primary.attributes, 0xffff);
    let stream = raw.stream.as_ref().unwrap();
    assert_eq!(stream.first_cluster, u32::MAX);
    assert_eq!(stream.data_length, u64::MAX);
    assert_eq!(stream.valid_data_length, u64::MAX);
    assert!(stream.allocation_possible);
    assert!(!stream.no_fat_chain);
    assert_eq!(raw.name().text, "raw");
    assert!(parse_set(&bytes, 0, super::LIMITS).is_none());
}

#[test]
fn raw_sets_report_wrong_secondary_counts_without_discarding_complete_bytes() {
    let mut bytes = super::entry_set("raw", 7, 1234, true);
    bytes[1] = 1;
    seal(&mut bytes[..ENTRY_SIZE * 2]);
    let raw = RawEntrySet::parse(&bytes).unwrap();
    assert_eq!(raw.primary.secondary_count, 1);
    assert_eq!(raw.bytes.len(), ENTRY_SIZE * 2);
    assert!(raw.checksum_ok);
    assert!(!raw.name_complete);
    assert_eq!(raw.stream.unwrap().name_length, 3);
    assert!(parse_set(&bytes, 0, super::LIMITS).is_none());
    bytes[1] = 0;
    seal(&mut bytes[..ENTRY_SIZE]);
    let raw = RawEntrySet::parse(&bytes).unwrap();
    assert!(raw.checksum_ok);
    assert!(raw.stream.is_none());
    assert!(!raw.name_complete);
    bytes[1] = 3;
    assert!(RawEntrySet::parse(&bytes).is_none());
}

#[test]
fn raw_sets_distinguish_deleted_bytes_from_recovery_checksum_normalization() {
    let bytes = super::entry_set("deleted", 7, 1234, false);
    let raw = RawEntrySet::parse(&bytes).unwrap();
    assert_eq!(raw.primary.entry_type.value, 0x05);
    assert!(!raw.primary.entry_type.in_use);
    assert!(!raw.checksum_ok);
    assert_eq!(raw.stream.as_ref().unwrap().entry_type.value, 0x40);
    assert_eq!(raw.names[0].entry_type.value, 0x41);
    assert!(!raw.secondaries[0].in_use);
    assert!(raw.secondaries[0].secondary);
    assert!(!raw.secondaries[0].benign);
    assert_eq!(raw.name().text, "deleted");
    let recovered = parse_set(&bytes, 0, super::LIMITS).unwrap();
    assert!(recovered.checksum_ok);
    assert!(!recovered.in_use);
}

#[test]
fn raw_sets_preserve_lossy_utf16_units_and_validity() {
    let mut bytes = super::entry_set("raw", 7, 1234, true);
    bytes[66..68].copy_from_slice(&0xd800u16.to_le_bytes());
    seal(&mut bytes);
    let raw = RawEntrySet::parse(&bytes).unwrap();
    assert!(raw.checksum_ok);
    assert!(raw.name_complete);
    let name = raw.name();
    assert_eq!(name.utf16, [0xd800, 0x61, 0x77]);
    assert_eq!(name.text, "�aw");
    assert!(!name.utf16_ok);
    assert_eq!(parse_set(&bytes, 0, super::LIMITS).unwrap().name, name.text);
}

#[test]
fn raw_sets_preserve_name_flags_hashes_and_benign_secondaries() {
    let mut bytes = super::entry_set("raw", 7, 1234, true);
    bytes[33] = 0xfe;
    bytes[36..38].copy_from_slice(&0x1234u16.to_le_bytes());
    bytes[65] = 0xff;
    bytes.extend([0; ENTRY_SIZE]);
    bytes[96] = 0xe0;
    bytes[1] = 3;
    seal(&mut bytes);
    let raw = RawEntrySet::parse(&bytes).unwrap();
    assert!(raw.checksum_ok);
    let stream = raw.stream.as_ref().unwrap();
    assert_eq!(stream.flags, 0xfe);
    assert!(!stream.allocation_possible);
    assert!(stream.no_fat_chain);
    assert_eq!(stream.name_hash, 0x1234);
    assert_eq!(raw.names[0].flags, 0xff);
    assert_eq!(raw.name().text, "raw");
    assert!(raw.name().utf16_ok);
    assert!(raw.secondaries[2].in_use);
    assert!(raw.secondaries[2].secondary);
    assert!(raw.secondaries[2].benign);
    assert!(parse_set(&bytes, 0, super::LIMITS).is_some());
}

#[test]
fn raw_sets_preserve_non_file_primaries_and_long_benign_sets() {
    let mut bytes = vec![0; ENTRY_SIZE * 20];
    bytes[0] = 0xa0;
    bytes[1] = 19;
    for entry in bytes[ENTRY_SIZE..].chunks_exact_mut(ENTRY_SIZE) {
        entry[0] = 0xe0;
    }
    seal(&mut bytes);
    let raw = RawEntrySet::parse(&bytes).unwrap();
    assert_eq!(raw.primary.entry_type.value, 0xa0);
    assert!(raw.primary.entry_type.benign);
    assert!(!raw.primary.entry_type.secondary);
    assert_eq!(raw.primary.secondary_count, 19);
    assert!(raw.checksum_ok);
    assert_eq!(raw.secondaries.len(), 19);
    assert!(raw.secondaries.iter().all(|entry| entry.benign));
    assert!(parse_set(&bytes, 0, super::LIMITS).is_none());
    for length in 0..ENTRY_SIZE {
        assert!(RawEntrySet::parse(&bytes[..length]).is_none());
    }
}
