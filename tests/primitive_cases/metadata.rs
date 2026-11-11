use fs_exfat::metadata::{AllocationBitmap, RootMetadata, UpcaseTable};
use fs_exfat::primitives::{
    checksum32, RawAllocationBitmap, RawRootMetadata, RawUpcaseTable, RootEntryKind, UpcaseIssue,
};

fn upcase_bytes() -> Vec<u8> {
    (0u32..65536)
        .flat_map(|unit| {
            let mapped = if (97..=122).contains(&unit) {
                unit - 32
            } else {
                unit
            };
            (mapped as u16).to_le_bytes()
        })
        .collect()
}

fn system_entry(kind: u8, first_cluster: u32, length: u64) -> [u8; 32] {
    let mut entry = [0; 32];
    entry[0] = kind;
    entry[20..24].copy_from_slice(&first_cluster.to_le_bytes());
    entry[24..32].copy_from_slice(&length.to_le_bytes());
    entry
}

#[test]
fn wrong_bitmap_lengths_preserve_available_bits() {
    for bytes in [&[0x81][..], &[0x81, 0x02, 0xff][..]] {
        let raw = RawAllocationBitmap::parse(bytes, 10);
        assert_eq!(raw.bytes, bytes);
        assert_eq!(raw.cluster_count, 10);
        assert!(!raw.length_ok);
        assert_eq!(raw.allocated(2), Some(true));
        assert_eq!(raw.allocated(3), Some(false));
        assert_eq!(raw.allocated(9), Some(true));
        assert_eq!(raw.allocated(11), bytes.get(1).map(|byte| byte & 2 != 0));
        assert_eq!(raw.allocated(1), None);
        assert_eq!(raw.allocated(12), None);
        assert!(AllocationBitmap::parse(bytes.to_vec(), 10).is_err());
    }
    assert!(RawAllocationBitmap::parse(&[], 0).length_ok);
    assert_eq!(RawAllocationBitmap::parse(&[], 1).allocated(2), None);
}

#[test]
fn bad_upcase_checksum_preserves_complete_mappings() {
    let bytes = upcase_bytes();
    let checksum = checksum32(&bytes);
    let raw = RawUpcaseTable::parse(&bytes, checksum ^ 1);
    assert_eq!(raw.data, bytes);
    assert_eq!(raw.checksum, checksum);
    assert!(raw.length_ok);
    assert!(!raw.checksum_ok);
    assert!(raw.complete);
    assert!(raw.ascii_ok);
    assert_eq!(raw.issue, None);
    assert_eq!(raw.mappings[97], 65);
    assert_eq!(raw.mappings[65535], 65535);
    assert!(UpcaseTable::parse(&bytes, checksum ^ 1).is_err());
}

#[test]
fn upcase_damage_reports_partial_tables_without_rejection() {
    for (bytes, issue, mappings) in [
        (
            vec![0x41, 0, 0xff, 0xff],
            Some(UpcaseIssue::TruncatedRun),
            vec![65],
        ),
        // A run of 0 is read as no mappings (#75); this one runs past FFFFh.
        (
            vec![0x41, 0, 0x42, 0, 0xff, 0xff, 0xff, 0xff],
            Some(UpcaseIssue::RunOutsideRange),
            vec![65, 66],
        ),
        (vec![0x41, 0, 0xff], None, vec![65]),
    ] {
        let raw = RawUpcaseTable::parse(&bytes, checksum32(&bytes));
        assert_eq!(raw.data, bytes);
        assert!(raw.checksum_ok);
        assert_eq!(raw.length_ok, bytes.len().is_multiple_of(2));
        assert_eq!(raw.issue, issue);
        assert_eq!(raw.mappings, mappings);
        assert!(!raw.complete);
        assert!(!raw.ascii_ok);
    }
    let mut bytes = upcase_bytes();
    bytes.extend_from_slice(&[0, 0]);
    let raw = RawUpcaseTable::parse(&bytes, checksum32(&bytes));
    assert!(!raw.length_ok);
    assert!(raw.complete);
    assert_eq!(raw.issue, Some(UpcaseIssue::ExtraMappings));
    bytes.truncate(131072);
    bytes[0] = 1;
    let raw = RawUpcaseTable::parse(&bytes, checksum32(&bytes));
    assert!(raw.complete);
    assert!(!raw.ascii_ok);
    let bytes = [0xff, 0xff, 0xfe, 0xff, 0xff, 0xff, 3, 0];
    let raw = RawUpcaseTable::parse(&bytes, checksum32(&bytes));
    assert_eq!(raw.issue, Some(UpcaseIssue::RunOutsideRange));
    assert_eq!(raw.mappings.len(), 65534);
}

#[test]
fn missing_root_system_entries_leave_raw_streams_visible() {
    for missing in [0x81, 0x82] {
        let kind = if missing == 0x81 { 0x82 } else { 0x81 };
        let mut bytes = system_entry(kind, 0x1234, 0x5678).to_vec();
        bytes.extend_from_slice(&[0; 32]);
        bytes.extend_from_slice(&system_entry(missing, 99, 100));
        let raw = RawRootMetadata::parse(&bytes);
        assert!(raw.aligned);
        assert_eq!(raw.entries.len(), 1);
        assert_eq!(raw.bitmap_present, [kind == 0x81, false]);
        assert_eq!(raw.upcase_present, kind == 0x82);
        let stream = match &raw.entries[0].kind {
            RootEntryKind::Bitmap { stream, .. } | RootEntryKind::Upcase { stream, .. } => stream,
            other => panic!("unexpected entry: {other:?}"),
        };
        assert_eq!(stream.first_cluster, 0x1234);
        assert_eq!(stream.length, 0x5678);
        assert!(RootMetadata::parse(&bytes, 1).is_err());
    }
}

#[test]
fn damaged_bitmap_entries_keep_flags_identity_and_duplicates() {
    let mut entry = system_entry(0x81, 4, 99);
    entry[1] = 0xff;
    let bytes = [entry, entry].concat();
    let raw = RawRootMetadata::parse(&bytes);
    assert_eq!(raw.bitmap_present, [false, true]);
    assert_eq!(raw.entries.len(), 2);
    for entry in raw.entries {
        let RootEntryKind::Bitmap {
            flags,
            id,
            flags_ok,
            stream,
        } = entry.kind
        else {
            panic!("bitmap expected");
        };
        assert_eq!(flags, 0xff);
        assert_eq!(id, 1);
        assert!(!flags_ok);
        assert_eq!(stream.length, 99);
    }
}

#[test]
fn damaged_labels_and_guids_retain_values_and_validity() {
    let mut label = [0; 32];
    label[0] = 0x83;
    label[1] = 12;
    label[2..4].copy_from_slice(&0xd800u16.to_le_bytes());
    let mut guid = [0; 32];
    guid[0] = 0xa0;
    guid[1] = 2;
    guid[4] = 3;
    guid[6..22].fill(0x42);
    let raw = RawRootMetadata::parse(&[label, guid].concat());
    let RootEntryKind::Label {
        length,
        units,
        value,
        length_ok,
        characters_ok,
        utf16_ok,
    } = &raw.entries[0].kind
    else {
        panic!("label expected")
    };
    assert_eq!(*length, 12);
    assert_eq!(units.len(), 11);
    assert_eq!(units[0], 0xd800);
    assert!(value.starts_with('\u{fffd}'));
    assert!(!length_ok);
    assert!(!characters_ok);
    assert!(!utf16_ok);
    let RootEntryKind::Guid {
        secondary_count,
        flags,
        value,
        checksum,
        checksum_ok,
    } = raw.entries[1].kind
    else {
        panic!("GUID expected")
    };
    assert_eq!(secondary_count, 2);
    assert_eq!(flags, 3);
    assert_eq!(value, [0x42; 16]);
    assert_eq!(checksum, 0);
    assert!(!checksum_ok);
}

#[test]
fn root_secondary_sets_report_truncation_and_hide_their_members() {
    for kind in [0x85, 0xa1] {
        let mut primary = [0; 32];
        primary[0] = kind;
        primary[1] = 2;
        let bytes = [primary, system_entry(0x81, 3, 13)].concat();
        let raw = RawRootMetadata::parse(&bytes);
        assert_eq!(raw.entries.len(), 1);
        assert_eq!(raw.bitmap_present, [false, false]);
        match raw.entries[0].kind {
            RootEntryKind::File {
                secondary_count,
                complete,
            }
            | RootEntryKind::Benign {
                secondary_count,
                complete,
            } => {
                assert_eq!(secondary_count, 2);
                assert!(!complete);
            }
            ref other => panic!("unexpected entry: {other:?}"),
        }
    }
    let mut deleted = system_entry(0x01, 3, 13);
    deleted[1] = 0xff;
    let mut bytes = [deleted, system_entry(0x82, 4, 8)].concat();
    bytes.push(0xff);
    let raw = RawRootMetadata::parse(&bytes);
    assert!(!raw.aligned);
    assert_eq!(raw.entries[0].entry_type, 1);
    assert_eq!(raw.bitmap_present, [false, false]);
    assert!(raw.upcase_present);
}

#[test]
fn strict_metadata_does_not_decode_disk_bytes() {
    let source = include_str!("../../src/metadata.rs");
    for duplicate in [
        "from_le_bytes",
        "chunks_exact",
        "rotate_right",
        "to_le_bytes",
    ] {
        assert!(
            !source.contains(duplicate),
            "metadata.rs still decodes disk bytes: {duplicate}"
        );
    }
}
