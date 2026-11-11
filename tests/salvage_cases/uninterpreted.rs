//! Directory entries the driver does not interpret, over volumes built from
//! Microsoft's exFAT specification: EntryType ranges (section 6.2.1), Vendor
//! Extension and Vendor Allocation layouts (sections 7.8 and 7.9) and
//! SetChecksum (section 6.3.3). macOS drops such entries silently, so these
//! images come from the specification rather than from macOS output.

use super::support::Bytes;
use super::{named, position, scan, Failing};
use fs_core::BlockRead;
use fs_exfat::fixtures::{
    bitmap_entry, entry_set, seal_entry_set, Builder, BITMAP_CLUSTER, CLUSTER, CLUSTER_COUNT,
    ROOT_CLUSTER, SECTOR,
};
use fs_exfat::primitives::{ENTRY_SIZE, FAT_END};
use fs_exfat::salvage::{
    Candidate, EntrySetDetail, ExfatSalvage, Extent, HoldingSet, Integrity, Parent, SalvageError,
    UninterpretedCounts, UninterpretedEntry, UninterpretedKind, UninterpretedReport,
};
use std::ops::ControlFlow;

const BITMAP_BYTES: u64 = CLUSTER_COUNT.div_ceil(8) as u64;
/// The subdirectory's only cluster, claimed contiguously.
const SUB: u32 = 20;
/// Root slots before the caller's entries: the bitmap entry and the
/// subdirectory's three-entry set.
const ROOT_FIXED: usize = 4;

/// Table 36: EntryType E0h, AllocationPossible clear, a VendorGuid.
fn vendor_extension() -> Vec<u8> {
    let mut entry = vec![0u8; ENTRY_SIZE];
    entry[0] = 0xE0;
    entry[2..18]
        .copy_from_slice(b"\x6d\x2f\x4c\x3a\x91\x0e\x47\x52\xb3\x5c\x08\x1d\xe4\x27\x9a\x61");
    entry
}

/// Table 37: EntryType E1h, AllocationPossible set, a VendorGuid and a
/// one-cluster allocation.
fn vendor_allocation() -> Vec<u8> {
    let mut entry = vendor_extension();
    entry[0] = 0xE1;
    entry[1] = 0x01;
    entry[20..24].copy_from_slice(&40u32.to_le_bytes());
    entry[24..32].copy_from_slice(&(CLUSTER as u64).to_le_bytes());
    entry
}

fn single(entry_type: u8) -> Vec<u8> {
    let mut entry = vec![0u8; ENTRY_SIZE];
    entry[0] = entry_type;
    entry
}

/// `set` with `extra` secondaries appended and its checksum resealed.
fn with_secondaries(mut set: Vec<u8>, extra: &[Vec<u8>]) -> Vec<u8> {
    set[1] += extra.len() as u8;
    for entry in extra {
        set.extend_from_slice(entry);
    }
    seal_entry_set(&mut set);
    set
}

/// Clears InUse on every entry, as deletion does.
fn deleted(mut set: Vec<u8>) -> Vec<u8> {
    for slot in (0..set.len()).step_by(ENTRY_SIZE) {
        set[slot] &= 0x7f;
    }
    set
}

/// A bitmap at cluster 2, the root at 3 holding the bitmap entry, a
/// subdirectory at cluster 20 and `root`; the subdirectory holds `sub`.
fn volume(root: &[Vec<u8>], sub: &[Vec<u8>]) -> Builder {
    let mut builder = Builder::new();
    for cluster in [BITMAP_CLUSTER, ROOT_CLUSTER, SUB] {
        builder.allocate(cluster);
    }
    builder.fat(BITMAP_CLUSTER, FAT_END);
    builder.fat(ROOT_CLUSTER, FAT_END);
    let mut entries = vec![
        bitmap_entry(BITMAP_CLUSTER, BITMAP_BYTES),
        entry_set("sub", true, SUB, CLUSTER as u64, true, true),
    ];
    entries.extend_from_slice(root);
    builder.directory(ROOT_CLUSTER, &entries);
    builder.directory(SUB, sub);
    builder
}

fn slot(cluster: u32, index: usize) -> Extent {
    Extent::new(
        position(cluster) + (index * ENTRY_SIZE) as u64,
        ENTRY_SIZE as u64,
    )
}

fn listing(
    source: &dyn BlockRead,
    candidates: &[Candidate<EntrySetDetail>],
) -> (Vec<UninterpretedEntry>, UninterpretedReport) {
    let mut found = Vec::new();
    let report = ExfatSalvage::probe(source)
        .uninterpreted_entries(source, candidates, &mut found)
        .unwrap();
    let mut counts = UninterpretedCounts::default();
    for entry in &found {
        counts.add(entry.kind);
        assert_eq!(UninterpretedKind::of(entry.entry_type), Some(entry.kind));
    }
    assert_eq!(report.counts, counts);
    (found, report)
}

fn held(primary: Extent, primary_type: u8, valid: bool, checksum_ok: bool) -> Option<HoldingSet> {
    Some(HoldingSet {
        primary,
        primary_type,
        valid,
        checksum_ok,
    })
}

#[test]
fn a_clean_volume_lists_nothing() {
    let builder = volume(
        &[
            entry_set("kept.txt", false, 30, 100, true, true),
            entry_set("gone.txt", false, 31, 100, true, false),
        ],
        &[entry_set("child.txt", false, 32, 100, true, true)],
    );
    let source = Bytes(builder.image);
    let (_, candidates) = scan(&source);
    let (found, report) = listing(&source, &candidates);
    assert!(found.is_empty(), "{found:?}");
    assert_eq!(
        report,
        UninterpretedReport {
            directories: 2,
            ..UninterpretedReport::default()
        }
    );

    let blank = Bytes(vec![0; 64 * 1024]);
    let error = ExfatSalvage::probe(&blank)
        .uninterpreted_entries(&blank, &[], &mut Vec::new())
        .unwrap_err();
    assert!(matches!(error, SalvageError::NoGeometry));
}

#[test]
fn vendor_entries_in_a_valid_set_are_listed_with_their_set() {
    let vendor = with_secondaries(
        entry_set("vendor.bin", false, 30, 100, true, true),
        &[vendor_extension(), vendor_allocation()],
    );
    let source = Bytes(volume(&[vendor], &[]).image);
    let (_, candidates) = scan(&source);
    let (found, report) = listing(&source, &candidates);
    let set = held(slot(ROOT_CLUSTER, ROOT_FIXED), 0x85, true, true);
    assert_eq!(
        found,
        [
            UninterpretedEntry {
                location: slot(ROOT_CLUSTER, ROOT_FIXED + 3),
                directory: Parent::Root,
                entry_type: 0xE0,
                kind: UninterpretedKind::VendorExtension,
                in_use: true,
                set,
            },
            UninterpretedEntry {
                location: slot(ROOT_CLUSTER, ROOT_FIXED + 4),
                directory: Parent::Root,
                entry_type: 0xE1,
                kind: UninterpretedKind::VendorAllocation,
                in_use: true,
                set,
            },
        ]
    );
    assert!(found.iter().all(UninterpretedEntry::in_valid_set));
    assert_eq!(report.counts.vendor_extension, 1);
    assert_eq!(report.counts.vendor_allocation, 1);
    assert_eq!(report.counts.total(), 2);
    assert_eq!(report.directories, 2);

    // The scan still finds the file, whole and verified: listing its vendor
    // entries changes nothing about what is salvaged.
    let file = named(&candidates, "vendor.bin");
    assert_eq!(file.integrity, Integrity::Verified);
    assert_eq!(
        file.location,
        Extent::new(slot(ROOT_CLUSTER, ROOT_FIXED).offset, 5 * ENTRY_SIZE as u64)
    );
}

#[test]
fn unknown_primaries_and_unused_entries_name_their_directory() {
    let benign = with_secondaries(single(0xA5), &[single(0xE7)]);
    let old = deleted(with_secondaries(
        entry_set("old.bin", false, 32, 100, true, false),
        &[vendor_extension()],
    ));
    // A deleted set of the same directory under an older name: its claim
    // starts where the live one does, so the directory is walked once.
    let renamed = entry_set("sub-old", true, SUB, CLUSTER as u64, true, false);
    let source = Bytes(volume(&[renamed], &[benign, single(0x84), old]).image);
    let (_, candidates) = scan(&source);
    let (found, report) = listing(&source, &candidates);
    let sub = Parent::Candidate(slot(ROOT_CLUSTER, 1).offset);
    let summary: Vec<_> = found
        .iter()
        .map(|entry| {
            (
                entry.location,
                entry.directory,
                entry.kind,
                entry.in_use,
                entry.set,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                slot(SUB, 0),
                sub,
                UninterpretedKind::UnknownBenignPrimary,
                true,
                held(slot(SUB, 0), 0xA5, true, true)
            ),
            (
                slot(SUB, 1),
                sub,
                UninterpretedKind::UnknownBenignSecondary,
                true,
                held(slot(SUB, 0), 0xA5, true, true)
            ),
            (
                slot(SUB, 2),
                sub,
                UninterpretedKind::UnknownCriticalPrimary,
                true,
                None
            ),
            (
                slot(SUB, 6),
                sub,
                UninterpretedKind::VendorExtension,
                false,
                held(slot(SUB, 3), 0x05, false, true)
            ),
        ]
    );
    assert_eq!(found[3].entry_type, 0x60);
    assert!(!found[2].in_valid_set() && !found[3].in_valid_set());
    assert_eq!(report.directories, 2);

    // Without candidates only the root is walked.
    let (found, report) = listing(&source, &[]);
    assert!(found.is_empty());
    assert_eq!(report.directories, 1);
}

#[test]
fn an_unreadable_sector_is_reported_and_the_walk_resumes_after_it() {
    let per_sector = SECTOR / ENTRY_SIZE;
    let first = with_secondaries(
        entry_set("first.bin", false, 30, 100, true, true),
        &[vendor_extension()],
    );
    // Unused entries fill the rest of the first sector, so no
    // end-of-directory marker comes before the unreadable one.
    let mut root = vec![first];
    root.resize(per_sector - ROOT_FIXED - 3, single(0x01));
    let mut builder = volume(&root, &[]);
    builder.put(ROOT_CLUSTER, SECTOR + 4 * ENTRY_SIZE, &vendor_extension());
    let after = with_secondaries(
        entry_set("after.bin", false, 31, 100, true, true),
        &[vendor_extension()],
    );
    builder.put(ROOT_CLUSTER, 2 * SECTOR, &after);
    let bad = Extent::new(position(ROOT_CLUSTER) + SECTOR as u64, SECTOR as u64);
    let source = Failing {
        inner: Bytes(builder.image),
        bad: vec![bad],
    };
    let (found, report) = listing(&source, &[]);
    let listed: Vec<_> = found
        .iter()
        .map(|entry| (entry.location, entry.set))
        .collect();
    let resumed = 2 * per_sector;
    assert_eq!(
        listed,
        [
            (
                slot(ROOT_CLUSTER, ROOT_FIXED + 3),
                held(slot(ROOT_CLUSTER, ROOT_FIXED), 0x85, true, true)
            ),
            (
                slot(ROOT_CLUSTER, resumed + 3),
                held(slot(ROOT_CLUSTER, resumed), 0x85, true, true)
            ),
        ]
    );
    assert_eq!(report.unreadable, [bad]);
    assert_eq!(report.directories, 1);
}

#[test]
fn a_sink_can_stop_the_listing() {
    let vendor = with_secondaries(
        entry_set("vendor.bin", false, 30, 100, true, true),
        &[vendor_extension(), vendor_allocation()],
    );
    let source = Bytes(volume(&[vendor], &[]).image);
    let mut found = Vec::new();
    let mut first = |entry: UninterpretedEntry| {
        found.push(entry);
        ControlFlow::Break(())
    };
    let report = ExfatSalvage::probe(&source)
        .uninterpreted_entries(&source, &[], &mut first)
        .unwrap();
    assert!(report.stopped);
    assert_eq!(found.len(), 1);
    assert_eq!(report.counts.total(), 1);
}

#[test]
fn listings_serialize_as_plain_data() {
    let vendor = with_secondaries(
        entry_set("vendor.bin", false, 30, 100, true, true),
        &[vendor_extension()],
    );
    let source = Bytes(volume(&[vendor], &[]).image);
    let (found, report) = listing(&source, &[]);
    let json = serde_json::to_value(found[0]).unwrap();
    assert_eq!(json["kind"], "vendor-extension");
    assert_eq!(json["directory"], "root");
    assert_eq!(json["entryType"], 0xE0);
    assert_eq!(json["inUse"], true);
    assert_eq!(json["set"]["primaryType"], 0x85);
    assert_eq!(json["set"]["valid"], true);
    let back: UninterpretedEntry = serde_json::from_value(json).unwrap();
    assert_eq!(back, found[0]);
    let text = serde_json::to_string(&report).unwrap();
    assert!(text.contains("\"vendorExtension\":1"), "{text}");
    let back: UninterpretedReport = serde_json::from_str(&text).unwrap();
    assert_eq!(back, report);
}
