use fs_exfat::primitives::{
    boot_checksum, decode_timestamp, decode_timestamp_with_offset, decode_utc_offset,
    find_bitmap_entry, find_sets, parse_set, set_checksum, stored_boot_checksum, u16_at, u32_at,
    u64_at, BootSector, Limits, BOOT_REGION_SECTORS, ENTRY_SIZE, FAT_BAD, FAT_END, FIRST_CLUSTER,
};

const LIMITS: Limits = Limits {
    cluster_count: 8,
    cluster_size: 4096,
};

// Self-built fixtures below exercise permissive/damaged structures only; healthy
// format evidence comes from newfs_exfat, fsck_exfat -n and the mounted macOS driver.
fn damaged_boot() -> Vec<u8> {
    let mut bytes = vec![0xff; 512];
    bytes[3..11].copy_from_slice(b"EXFAT   ");
    bytes[88..92].copy_from_slice(&123u32.to_le_bytes());
    bytes[92..96].copy_from_slice(&8u32.to_le_bytes());
    bytes[106..108].copy_from_slice(&2u16.to_le_bytes());
    bytes[108] = 9;
    bytes[109] = 3;
    bytes
}

fn seal_permissive_set(bytes: &mut [u8]) {
    let mut sum = 0u32;
    for (index, &byte) in bytes.iter().enumerate() {
        if !matches!(index, 2 | 3) {
            let value = if index % 32 == 0 { byte | 128 } else { byte };
            sum = ((sum / 2 + (sum % 2) * 32768) + u32::from(value)) % 65536;
        }
    }
    bytes[2..4].copy_from_slice(&(sum as u16).to_le_bytes());
}

fn permissive_set() -> Vec<u8> {
    let mut bytes = vec![0; 96];
    bytes[0] = 0x05;
    bytes[1] = 2;
    bytes[4..6].copy_from_slice(&0x10u16.to_le_bytes());
    bytes[8..12].copy_from_slice(&((44u32 << 25) | (2 << 21) | (30 << 16)).to_le_bytes());
    bytes[22] = 0x88;
    bytes[32] = 0x40;
    bytes[33] = 3;
    bytes[35] = 1;
    bytes[40..48].copy_from_slice(&999u64.to_le_bytes());
    bytes[52..56].copy_from_slice(&2u32.to_le_bytes());
    bytes[56..64].copy_from_slice(&5u64.to_le_bytes());
    bytes[64] = 0x41;
    bytes[66..68].copy_from_slice(&0xd800u16.to_le_bytes());
    seal_permissive_set(&mut bytes);
    bytes
}

#[test]
fn boot_parse_keeps_damaged_geometry_and_reports_signature_instead_of_rejecting_it() {
    let bytes = damaged_boot();
    assert!(fs_exfat::boot::BootSector::parse(&bytes).is_err());
    let boot = BootSector::parse(&bytes).unwrap();
    assert_eq!(boot.file_system_name, "EXFAT   ");
    assert!(!boot.signature_ok);
    assert_eq!(boot.bytes_per_sector(), 512);
    assert_eq!(boot.cluster_size(), 4096);
    assert_eq!(boot.heap_offset(), 123 * 512);
    assert_eq!(boot.last_cluster_exclusive(), 10);
    assert!(boot.volume_dirty());
    let mut clean_flag = boot.clone();
    clean_flag.volume_flags = 0xfffd;
    assert!(!clean_flag.volume_dirty());
    for length in [0, 1, 511] {
        assert!(BootSector::parse(&bytes[..length]).is_err());
    }
    let mut wrong_identity = bytes;
    wrong_identity[3] = 0;
    assert!(BootSector::parse(&wrong_identity).is_err());
}

#[test]
fn boot_checksums_ignore_mutable_flags_and_read_only_the_first_stored_word() {
    let mut region = vec![0; 12 * 512];
    region[0] = 1;
    region[11 * 512..11 * 512 + 4].copy_from_slice(&0x12345678u32.to_le_bytes());
    assert_eq!(boot_checksum(&region, 512), 0x10);
    assert_eq!(stored_boot_checksum(&region, 512), 0x12345678);
    for offset in [106, 107, 112] {
        region[offset] = 0xff;
        assert_eq!(boot_checksum(&region, 512), 0x10);
    }
    region[113] = 1;
    assert_ne!(boot_checksum(&region, 512), 0x10);
    assert_eq!(stored_boot_checksum(&region, 512), 0x12345678);
    assert!(std::panic::catch_unwind(|| boot_checksum(&region[..10], 512)).is_err());
    assert!(std::panic::catch_unwind(|| stored_boot_checksum(&region[..512], 512)).is_err());
}

#[test]
fn timestamp_decoding_keeps_impossible_calendar_dates_but_rejects_invalid_fields() {
    let february_thirty = (44 << 25) | (2 << 21) | (30 << 16);
    assert_eq!(
        decode_timestamp(february_thirty).as_deref(),
        Some("2024-02-30T00:00:00")
    );
    assert!(fs_exfat::timestamp::Timestamp::decode(february_thirty, 0, 0x80).is_err());
    for raw in [
        0,
        1 << 16,
        1 << 21,
        (13 << 21) | (1 << 16),
        february_thirty | (24 << 11),
        february_thirty | (60 << 5),
        february_thirty | 30,
    ] {
        assert_eq!(decode_timestamp(raw), None, "{raw:#x}");
    }
}

#[test]
fn all_utc_offset_bytes_keep_validity_and_signed_quarter_hour_semantics() {
    for byte in 0..=255u8 {
        let expected = if byte < 128 {
            None
        } else {
            let steps = if byte < 192 {
                i32::from(byte) - 128
            } else {
                i32::from(byte) - 256
            };
            let minutes = steps.abs() * 15;
            Some(format!(
                "{}{:02}:{:02}",
                if steps < 0 { '-' } else { '+' },
                minutes / 60,
                minutes % 60
            ))
        };
        assert_eq!(decode_utc_offset(byte), expected, "{byte:#x}");
    }
}

#[test]
fn timestamp_offsets_keep_unknown_zones_and_do_not_repair_calendar_dates() {
    let raw = (44 << 25) | (2 << 21) | (30 << 16);
    for (offset, suffix) in [
        (0x01, ""),
        (0x80, "+00:00"),
        (0x96, "+05:30"),
        (0xec, "-05:00"),
    ] {
        assert_eq!(
            decode_timestamp_with_offset(raw, offset),
            Some(format!("2024-02-30T00:00:00{suffix}"))
        );
    }
    assert_eq!(decode_timestamp_with_offset(0, 0x88), None);
}

#[test]
fn little_endian_helpers_support_unaligned_values_and_panic_on_short_input() {
    let bytes = [0xff, 1, 2, 3, 4, 5, 6, 7, 8];
    assert_eq!(u16_at(&bytes, 1), 0x0201);
    assert_eq!(u32_at(&bytes, 1), 0x04030201);
    assert_eq!(u64_at(&bytes, 1), 0x0807060504030201);
    assert!(std::panic::catch_unwind(|| u16_at(&bytes, 8)).is_err());
    assert!(std::panic::catch_unwind(|| u32_at(&bytes, 6)).is_err());
    assert!(std::panic::catch_unwind(|| u64_at(&bytes, 2)).is_err());
}

#[test]
fn primitive_types_keep_every_field_and_camel_case_serialization() {
    assert_eq!(
        (ENTRY_SIZE, FIRST_CLUSTER, BOOT_REGION_SECTORS),
        (32, 2, 12)
    );
    assert_eq!((FAT_END, FAT_BAD), (0xffffffff, 0xfffffff7));
    let boot = BootSector::parse(&damaged_boot()).unwrap();
    let json = serde_json::to_value(&boot).unwrap();
    assert_eq!(json.as_object().unwrap().len(), 16);
    assert_eq!(json["signatureOk"], false);
    assert_eq!(json["fileSystemName"], "EXFAT   ");
    assert_eq!(json["clusterCount"], 8);
    let entry = parse_set(&permissive_set(), 0, LIMITS).unwrap();
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json.as_object().unwrap().len(), 14);
    assert_eq!(json["isDir"], true);
    assert_eq!(json["noFatChain"], true);
    assert_eq!(json["validLength"], 999);
    assert_eq!(json["checksumOk"], true);
}

#[test]
fn deleted_sets_keep_lossy_names_flags_unbounded_valid_length_and_optional_times() {
    let mut data = vec![0; 32];
    data.extend(permissive_set());
    let entry = parse_set(&data, 32, LIMITS).unwrap();
    assert_eq!(entry.offset, 32);
    assert_eq!(entry.name, "\u{fffd}");
    assert!(!entry.in_use);
    assert!(entry.checksum_ok);
    assert_eq!(entry.attributes, 0x10);
    assert!(entry.is_dir);
    assert_eq!(
        (entry.first_cluster, entry.length, entry.valid_length),
        (2, 5, 999)
    );
    assert!(entry.no_fat_chain);
    assert_eq!(entry.created.as_deref(), Some("2024-02-30T00:00:00+02:00"));
    assert_eq!((entry.modified, entry.accessed), (None, None));
    assert_eq!(entry.slots, 3);
}

#[test]
fn a_matching_set_checksum_bypasses_only_the_plausibility_filters() {
    for (offset, value) in [(4, 0xff), (52, 1), (56, 0xff), (66, 1)] {
        let mut bytes = permissive_set();
        bytes[offset] = value;
        if offset == 56 {
            bytes[57..64].fill(0xff);
        }
        if offset == 66 {
            bytes[67] = 0;
        }
        seal_permissive_set(&mut bytes);
        assert!(parse_set(&bytes, 0, LIMITS).unwrap().checksum_ok);
        bytes[2] ^= 1;
        assert!(parse_set(&bytes, 0, LIMITS).is_none(), "offset {offset}");
    }
    for cluster in [0u32, 2, 9, 10, u32::MAX] {
        let mut bytes = permissive_set();
        bytes[52..56].copy_from_slice(&cluster.to_le_bytes());
        seal_permissive_set(&mut bytes);
        bytes[2] ^= 1;
        assert_eq!(
            parse_set(&bytes, 0, LIMITS).is_some(),
            matches!(cluster, 0 | 2 | 9)
        );
    }
    let mut bytes = permissive_set();
    bytes[56..64].copy_from_slice(&(8u64 * 4096).to_le_bytes());
    seal_permissive_set(&mut bytes);
    bytes[2] ^= 1;
    assert!(!parse_set(&bytes, 0, LIMITS).unwrap().checksum_ok);
}

#[test]
fn malformed_set_structure_is_rejected_even_with_a_matching_checksum() {
    let original = permissive_set();
    for length in [0, 31, 32, 63, 64, 95] {
        assert!(parse_set(&original[..length], 0, LIMITS).is_none());
    }
    for (offset, value) in [(0, 0), (32, 0), (64, 0), (1, 1), (1, 19), (35, 0), (35, 16)] {
        let mut bytes = original.clone();
        bytes[offset] = value;
        seal_permissive_set(&mut bytes);
        assert!(
            parse_set(&bytes, 0, LIMITS).is_none(),
            "offset {offset}, value {value}"
        );
    }
    assert!(parse_set(&original, original.len(), LIMITS).is_none());
}

#[test]
fn set_checksum_restores_each_in_use_bit_and_ignores_only_its_stored_word() {
    let deleted = permissive_set();
    let stored = u16_at(&deleted, 2);
    assert_eq!(set_checksum(&deleted), stored);
    let mut mixed = deleted.clone();
    mixed[0] |= 0x80;
    mixed[64] |= 0x80;
    mixed[2..4].fill(0xff);
    assert_eq!(set_checksum(&mixed), stored);
    mixed[95] ^= 1;
    assert_ne!(set_checksum(&mixed), stored);
    assert_eq!(set_checksum(&[]), 0);
}

#[test]
fn scanning_crosses_end_markers_and_search_boundaries_and_rechecks_suspect_slots() {
    let set = permissive_set();
    let mut data = vec![0; 32];
    data.extend(&set);
    data.extend(&set);
    assert!(find_sets(&data, 32, LIMITS).is_empty());
    assert_eq!(find_sets(&data, 33, LIMITS).len(), 1);
    assert_eq!(find_sets(&data, data.len() + 32, LIMITS).len(), 2);
    assert!(find_sets(&data, 0, LIMITS).is_empty());
    let mut overlapping = set.clone();
    overlapping[1] = 5;
    overlapping.extend(&set);
    seal_permissive_set(&mut overlapping);
    assert_eq!(find_sets(&overlapping, overlapping.len(), LIMITS).len(), 1);
    overlapping[2] ^= 1;
    assert_eq!(find_sets(&overlapping, overlapping.len(), LIMITS).len(), 2);
}

#[test]
fn bitmap_lookup_ignores_end_markers_flags_and_geometry_but_requires_a_complete_slot() {
    let mut root = vec![0; 96];
    root[32] = 0x01;
    root[64] = 0x81;
    root[65] = 0xff;
    root[84..88].copy_from_slice(&u32::MAX.to_le_bytes());
    root[88..96].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(find_bitmap_entry(&root), Some((u32::MAX, u64::MAX)));
    assert_eq!(find_bitmap_entry(&root[..95]), None);
    assert_eq!(find_bitmap_entry(&[]), None);
    root[32] = 0x81;
    assert_eq!(find_bitmap_entry(&root), Some((0, 0)));
}
