//! Directory entries the driver does not interpret, over entries built from
//! Microsoft's exFAT specification: EntryType ranges (section 6.2.1), the
//! revision 1.00 types (section 7), Vendor Extension and Vendor Allocation
//! layouts (sections 7.8 and 7.9) and SetChecksum (section 6.3.3).

use super::{entry_set, set_checksum};
use fs_exfat::primitives::{
    uninterpreted_entries, DirectoryWalk, SetMembership, UninterpretedCounts, UninterpretedKind,
    UninterpretedRecord, ENTRY_SIZE,
};
use std::ops::ControlFlow;

const VENDOR_GUID: [u8; 16] = [
    0x6d, 0x2f, 0x4c, 0x3a, 0x91, 0x0e, 0x47, 0x52, 0xb3, 0x5c, 0x08, 0x1d, 0xe4, 0x27, 0x9a, 0x61,
];

/// Table 36: EntryType E0h, GeneralSecondaryFlags with AllocationPossible
/// clear, a VendorGuid and fourteen vendor-defined bytes.
fn vendor_extension() -> Vec<u8> {
    let mut entry = vec![0u8; ENTRY_SIZE];
    entry[0] = 0xE0;
    entry[2..18].copy_from_slice(&VENDOR_GUID);
    entry[18..32].copy_from_slice(b"vendor-defined");
    entry
}

/// Table 37: EntryType E1h, AllocationPossible set, a VendorGuid and an
/// allocation of one cluster.
fn vendor_allocation() -> Vec<u8> {
    let mut entry = vec![0u8; ENTRY_SIZE];
    entry[0] = 0xE1;
    entry[1] = 0x01;
    entry[2..18].copy_from_slice(&VENDOR_GUID);
    entry[20..24].copy_from_slice(&40u32.to_le_bytes());
    entry[24..32].copy_from_slice(&4096u64.to_le_bytes());
    entry
}

/// A primary of `entry_type` with `secondaries` after it, SetChecksum sealed.
fn primary_set(entry_type: u8, secondaries: &[Vec<u8>]) -> Vec<u8> {
    let mut set = vec![0u8; ENTRY_SIZE];
    set[0] = entry_type;
    set[1] = secondaries.len() as u8;
    for secondary in secondaries {
        set.extend_from_slice(secondary);
    }
    seal(&mut set);
    set
}

/// A one-entry record of `entry_type`, other fields zero.
fn single(entry_type: u8) -> Vec<u8> {
    let mut entry = vec![0u8; ENTRY_SIZE];
    entry[0] = entry_type;
    entry
}

/// A live file set named `name` with `extra` secondaries after its names.
fn file_with(name: &str, extra: &[Vec<u8>]) -> Vec<u8> {
    let mut set = entry_set(name, 9, 10, true);
    set[1] += extra.len() as u8;
    for entry in extra {
        set.extend_from_slice(entry);
    }
    seal(&mut set);
    set
}

fn seal(set: &mut [u8]) {
    let checksum = set_checksum(set);
    set[2..4].copy_from_slice(&checksum.to_le_bytes());
}

/// Clears InUse on every entry, as deletion does.
fn deleted(mut set: Vec<u8>) -> Vec<u8> {
    for slot in (0..set.len()).step_by(ENTRY_SIZE) {
        set[slot] &= 0x7f;
    }
    set
}

/// The entries, then zeros: an end-of-directory marker and a spare cluster.
fn directory(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = entries.concat();
    bytes.resize(bytes.len() + 4096, 0);
    bytes
}

fn walk(directory: &[u8]) -> (Vec<UninterpretedRecord>, DirectoryWalk) {
    let mut found = Vec::new();
    let walk = uninterpreted_entries(directory, |record| {
        found.push(record);
        ControlFlow::Continue(())
    });
    (found, walk)
}

fn member(primary: usize, primary_type: u8, valid: bool, checksum_ok: bool) -> SetMembership {
    SetMembership {
        primary,
        primary_type,
        valid,
        checksum_ok,
    }
}

#[test]
fn kinds_follow_the_specification_type_ranges() {
    use UninterpretedKind::*;
    for value in 0..=255u8 {
        let restored = value | 0x80;
        let expected = match restored {
            _ if value == 0 => None,
            // Sections 7.1 to 7.7: the types this driver interprets.
            0x81 | 0x82 | 0x83 | 0x85 | 0xA0 | 0xC0 | 0xC1 => None,
            0xE0 => Some(VendorExtension),
            0xE1 => Some(VendorAllocation),
            0x80..=0x9F => Some(UnknownCriticalPrimary),
            0xA1..=0xBF => Some(UnknownBenignPrimary),
            0xC2..=0xDF => Some(UnknownCriticalSecondary),
            0xE2..=0xFF => Some(UnknownBenignSecondary),
            _ => unreachable!(),
        };
        assert_eq!(UninterpretedKind::of(value), expected, "{value:#04x}");
        if let Some(kind) = expected {
            assert_eq!(kind.is_primary(), restored & 0x40 == 0, "{value:#04x}");
        }
    }
}

#[test]
fn a_clean_directory_lists_nothing() {
    let mut guid = primary_set(0xA0, &[]);
    guid[6..22].copy_from_slice(&VENDOR_GUID);
    seal(&mut guid);
    let bytes = directory(&[
        single(0x81),
        single(0x82),
        single(0x83),
        guid,
        entry_set("kept.txt", 9, 10, true),
        entry_set("gone.txt", 12, 10, false),
    ]);
    let (found, walk) = walk(&bytes);
    assert!(found.is_empty(), "{found:?}");
    assert_eq!(walk.end_marker, Some(4 * ENTRY_SIZE + 3 * ENTRY_SIZE * 2));
    assert!(!walk.stopped);
    let mut counts = UninterpretedCounts::default();
    counts.tally(&bytes);
    assert_eq!(counts, UninterpretedCounts::default());
}

#[test]
fn vendor_entries_in_a_valid_file_set_are_listed_in_that_set() {
    let set = file_with("vendor.bin", &[vendor_extension(), vendor_allocation()]);
    let bytes = directory(&[single(0x81), set]);
    let (found, walk) = walk(&bytes);
    let holding = Some(member(ENTRY_SIZE, 0x85, true, true));
    assert_eq!(
        found,
        [
            UninterpretedRecord {
                offset: 4 * ENTRY_SIZE,
                entry_type: 0xE0,
                kind: UninterpretedKind::VendorExtension,
                in_use: true,
                set: holding,
            },
            UninterpretedRecord {
                offset: 5 * ENTRY_SIZE,
                entry_type: 0xE1,
                kind: UninterpretedKind::VendorAllocation,
                in_use: true,
                set: holding,
            },
        ]
    );
    assert_eq!(walk.end_marker, Some(6 * ENTRY_SIZE));
}

#[test]
fn a_vendor_entry_before_the_names_leaves_its_set_invalid() {
    // Section 7.8: valid only when it does not precede the Stream Extension
    // and File Name entries. The checksum still covers the set as written.
    let mut set = file_with("early.bin", &[vendor_extension()]);
    let name = set[2 * ENTRY_SIZE..3 * ENTRY_SIZE].to_vec();
    set.copy_within(3 * ENTRY_SIZE..4 * ENTRY_SIZE, 2 * ENTRY_SIZE);
    set[3 * ENTRY_SIZE..4 * ENTRY_SIZE].copy_from_slice(&name);
    seal(&mut set);
    let (found, _) = walk(&directory(&[set]));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].offset, 2 * ENTRY_SIZE);
    assert_eq!(found[0].set, Some(member(0, 0x85, false, true)));
}

#[test]
fn unknown_primaries_are_listed_with_their_sets_when_they_frame_one() {
    let benign = primary_set(0xA5, &[single(0xE7)]);
    // Section 7.10: TexFAT Padding is treated as an unknown benign primary.
    let padding = primary_set(0xA1, &[]);
    // Section 6.3.2 lets a critical primary redefine SecondaryCount, so an
    // unknown one frames no set, and a secondary after it is an orphan.
    let mut critical = single(0x86);
    critical[1] = 1;
    let bytes = directory(&[benign, padding, critical, vendor_extension()]);
    let (found, _) = walk(&bytes);
    let kinds: Vec<_> = found.iter().map(|record| record.kind).collect();
    assert_eq!(
        kinds,
        [
            UninterpretedKind::UnknownBenignPrimary,
            UninterpretedKind::UnknownBenignSecondary,
            UninterpretedKind::UnknownBenignPrimary,
            UninterpretedKind::UnknownCriticalPrimary,
            UninterpretedKind::VendorExtension,
        ]
    );
    let offsets: Vec<_> = found.iter().map(|record| record.offset).collect();
    assert_eq!(offsets, [0, 32, 64, 96, 128]);
    assert_eq!(found[0].set, Some(member(0, 0xA5, true, true)));
    assert_eq!(found[1].set, found[0].set);
    assert_eq!(found[2].set, Some(member(64, 0xA1, true, true)));
    assert_eq!(found[3].set, None);
    assert_eq!(found[4].set, None);
    assert!(found.iter().all(|record| record.in_use));
}

#[test]
fn in_use_and_unused_entries_are_told_apart() {
    let live = file_with("live.bin", &[vendor_extension()]);
    let gone = deleted(file_with("gone.bin", &[vendor_extension()]));
    // A live set whose vendor entry alone lost InUse is no longer valid as
    // it stands, though its checksum still matches with InUse restored.
    let mut cleared = file_with("half.bin", &[vendor_extension()]);
    cleared[3 * ENTRY_SIZE] = 0x60;
    let stray = single(0x04);
    let bytes = directory(&[live.clone(), gone.clone(), cleared.clone(), stray]);
    let (found, _) = walk(&bytes);
    let second = live.len();
    let third = second + gone.len();
    let summary: Vec<_> = found
        .iter()
        .map(|record| (record.offset, record.entry_type, record.in_use, record.set))
        .collect();
    assert_eq!(
        summary,
        [
            (3 * 32, 0xE0, true, Some(member(0, 0x85, true, true))),
            (
                second + 3 * 32,
                0x60,
                false,
                Some(member(second, 0x05, false, true))
            ),
            (
                third + 3 * 32,
                0x60,
                false,
                Some(member(third, 0x85, false, true))
            ),
            (third + cleared.len(), 0x04, false, None),
        ]
    );
    assert_eq!(found[1].kind, UninterpretedKind::VendorExtension);
    assert_eq!(found[3].kind, UninterpretedKind::UnknownCriticalPrimary);
}

#[test]
fn damaged_sets_keep_their_entries_and_report_why() {
    // A corrupted checksum, and a set whose declared secondaries run past the
    // bytes given: both still list the vendor entry, in an invalid set.
    let mut corrupt = file_with("bad.bin", &[vendor_extension()]);
    corrupt[2] ^= 0xff;
    let (found, _) = walk(&directory(&[corrupt]));
    assert_eq!(found[0].set, Some(member(0, 0x85, false, false)));

    let mut short = file_with("short.bin", &[vendor_extension()]);
    short[1] += 1;
    let (found, walk) = walk(&short);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].set, Some(member(0, 0x85, false, false)));
    assert_eq!(walk.end_marker, None);
}

#[test]
fn the_walk_ends_at_the_end_marker_or_when_the_caller_stops() {
    let mut bytes = directory(&[vendor_extension()]);
    // Section 6.2.1: every entry after an end-of-directory marker is one too.
    bytes[2 * ENTRY_SIZE..3 * ENTRY_SIZE].copy_from_slice(&vendor_extension());
    let (found, walk) = walk(&bytes);
    assert_eq!(found.len(), 1);
    assert_eq!(walk.end_marker, Some(ENTRY_SIZE));

    let both = directory(&[vendor_extension(), vendor_extension()]);
    let mut calls = 0;
    let stopped = uninterpreted_entries(&both, |_| {
        calls += 1;
        ControlFlow::Break(())
    });
    assert_eq!(calls, 1);
    assert!(stopped.stopped);
    assert_eq!(stopped.end_marker, None);
}

#[test]
fn counts_tally_every_kind_once() {
    let bytes = [
        single(0xE0),
        single(0x60),
        single(0xE1),
        single(0xA1),
        single(0x84),
        single(0xC2),
        single(0xFF),
        single(0x85),
        single(0x00),
    ]
    .concat();
    let mut counts = UninterpretedCounts::default();
    counts.tally(&bytes);
    assert_eq!(
        counts,
        UninterpretedCounts {
            vendor_extension: 2,
            vendor_allocation: 1,
            unknown_benign_primary: 1,
            unknown_critical_primary: 1,
            unknown_critical_secondary: 1,
            unknown_benign_secondary: 1,
        }
    );
    assert_eq!(counts.total(), 7);
    assert_eq!(counts.get(UninterpretedKind::VendorExtension), 2);
}
