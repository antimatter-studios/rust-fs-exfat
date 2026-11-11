//! The check counts the directory entries it does not interpret, by kind,
//! without failing on them or changing any other result. The entries follow
//! Microsoft's exFAT specification: Vendor Extension and Vendor Allocation
//! layouts (sections 7.8 and 7.9), unknown types by EntryType range (section
//! 6.2.1) and the skip rules of section 8.2.

use super::interface_support;
use super::support;
use fs_exfat::check::{check, CheckReport};
use fs_exfat::primitives::UninterpretedCounts;
use fs_exfat::volume::Volume;

/// The `Sub` directory's only cluster, which holds `Child.bin`.
const SUB: usize = 38 * 512;

fn single(entry_type: u8) -> Vec<u8> {
    let mut entry = vec![0u8; 32];
    entry[0] = entry_type;
    entry
}

/// Table 36: EntryType E0h, AllocationPossible clear, a VendorGuid.
fn vendor_extension() -> Vec<u8> {
    let mut entry = single(0xE0);
    entry[2..18]
        .copy_from_slice(b"\x6d\x2f\x4c\x3a\x91\x0e\x47\x52\xb3\x5c\x08\x1d\xe4\x27\x9a\x61");
    entry
}

/// Table 37: EntryType E1h, AllocationPossible set, with no allocation
/// (FirstCluster and DataLength zero, section 6.2.2).
fn vendor_allocation() -> Vec<u8> {
    let mut entry = vendor_extension();
    entry[0] = 0xE1;
    entry[1] = 0x01;
    entry
}

/// `set` with `extra` secondaries appended and its checksum resealed.
fn with_secondaries(mut set: Vec<u8>, extra: &[Vec<u8>]) -> Vec<u8> {
    set[1] += extra.len() as u8;
    for entry in extra {
        set.extend_from_slice(entry);
    }
    support::seal_set(&mut set);
    set
}

/// The interface image with `Sub` holding `entries` instead of `Child.bin`.
fn image_with(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = interface_support::image();
    let entries = entries.concat();
    bytes[SUB..SUB + 512].fill(0);
    bytes[SUB..SUB + entries.len()].copy_from_slice(&entries);
    bytes
}

fn child(extra: &[Vec<u8>]) -> Vec<u8> {
    with_secondaries(support::file_set("Child.bin", 0x20, 9, 7, true), extra)
}

fn checked(bytes: Vec<u8>) -> CheckReport {
    check(&Volume::open(support::Bytes(bytes)).unwrap()).unwrap()
}

#[test]
fn a_clean_volume_counts_nothing() {
    let report = checked(interface_support::image());
    assert_eq!(report.uninterpreted, UninterpretedCounts::default());
    assert_eq!(report.uninterpreted.total(), 0);
}

#[test]
fn vendor_and_unknown_entries_are_counted_apart_from_the_verdict() {
    let clean = checked(interface_support::image());
    let bytes = image_with(&[
        child(&[vendor_extension(), vendor_allocation()]),
        // An unknown benign primary's set, which section 6.3.1.2.2 makes
        // optional, with an unknown benign secondary.
        with_secondaries(single(0xA5), &[single(0xE7)]),
        // Unused entries: a deleted vendor entry, and deleted entries of an
        // unknown critical primary and an unknown critical secondary.
        single(0x60),
        single(0x04),
        single(0x42),
    ]);
    let volume = Volume::open(support::Bytes(bytes.clone())).unwrap();
    let names: Vec<_> = volume
        .read_directory_path("/Sub")
        .unwrap()
        .iter()
        .map(|entry| entry.name().to_owned())
        .collect();
    assert_eq!(names, ["Child.bin"]);

    let report = checked(bytes);
    assert!(report.healthy());
    assert_eq!(
        report.uninterpreted,
        UninterpretedCounts {
            vendor_extension: 2,
            vendor_allocation: 1,
            unknown_benign_primary: 1,
            unknown_critical_primary: 1,
            unknown_critical_secondary: 1,
            unknown_benign_secondary: 1,
        }
    );
    assert_eq!(
        (
            report.files,
            report.directories,
            report.allocated_clusters,
            report.referenced_clusters,
            report.unreferenced_clusters,
            report.dirty,
            report.media_failure,
        ),
        (
            clean.files,
            clean.directories,
            clean.allocated_clusters,
            clean.referenced_clusters,
            clean.unreferenced_clusters,
            clean.dirty,
            clean.media_failure,
        )
    );
}

#[test]
fn an_unknown_critical_primary_in_use_still_fails_the_check() {
    // Section 8.2: the strict check refuses an unrecognized critical primary
    // as it did before counting; counting changes no verdict.
    let volume = Volume::open(support::Bytes(image_with(&[child(&[]), single(0x84)]))).unwrap();
    let error = check(&volume).unwrap_err().to_string();
    assert!(error.contains("critical directory entry"), "{error}");
}
