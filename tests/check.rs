#[path = "check/deviations.rs"]
mod deviations;
mod interface_support;
#[path = "check/scale.rs"]
mod scale;
#[path = "check/uninterpreted.rs"]
mod uninterpreted;
use interface_support::base as support;

use fs_exfat::check::check;
use fs_exfat::interface;
use fs_exfat::volume::{ReadLimits, Volume};

#[test]
fn complete_tree_allocation_audit_accepts_only_block_read() {
    let volume = Volume::open(support::Bytes(interface_support::image())).unwrap();
    let report = check(&volume).unwrap();
    assert!(report.healthy());
    assert_eq!((report.files, report.directories), (3, 2));
    assert_eq!(
        (report.allocated_clusters, report.referenced_clusters),
        (8, 8)
    );
    assert!(report.boot_differences.is_empty());
}

#[test]
fn cross_linked_files_are_rejected() {
    let mut bytes = interface_support::image();
    let root = 32 * 512 + 96;
    let duplicate = support::file_set("Other.bin", 0x20, 5, 1030, true);
    bytes[root + 96..root + 192].copy_from_slice(&duplicate);
    let volume = Volume::open(support::Bytes(bytes)).unwrap();
    assert!(check(&volume)
        .unwrap_err()
        .to_string()
        .contains("cross-linked"));
}

#[test]
fn lost_allocations_and_dirty_flags_are_reported_without_writes() {
    let mut bytes = interface_support::image();
    bytes[33 * 512 + 1] |= 1;
    support::put16(&mut bytes, 106, 2);
    let volume = Volume::open(support::Bytes(bytes)).unwrap();
    let report = check(&volume).unwrap();
    assert!(!report.healthy());
    assert!(report.dirty);
    assert_eq!(report.unreferenced_clusters, 1);
    assert!(!report.boot_differences.is_empty());
}

#[test]
fn allocated_clusters_the_fat_marks_bad_are_not_unreferenced() {
    // Section 7.1.5.1: a set bitmap bit may mean the active FAT describes the
    // cluster as bad (FFFFFFF7h, section 4.1.3) rather than that an
    // allocation consumes it. macOS fsck_exfat accepts such a volume.
    let mut bytes = interface_support::image();
    bytes[33 * 512 + 1] |= 1;
    bytes[24 * 512 + 10 * 4..24 * 512 + 11 * 4].copy_from_slice(&[0xf7, 0xff, 0xff, 0xff]);
    let volume = Volume::open(support::Bytes(bytes)).unwrap();
    let report = check(&volume).unwrap();
    assert_eq!(report.unreferenced_clusters, 0);
    assert_eq!(report.bad_clusters, 1);
    assert!(report.healthy());
}

#[test]
fn clusters_a_vendor_allocation_owns_are_not_unreferenced() {
    // Section 7.9: a Vendor Allocation (E1h) secondary has a cluster
    // allocation of its own, which section 8.2 says implementations keep.
    // macOS fsck_exfat accepts such a volume.
    let mut bytes = interface_support::image();
    let mut file = support::file_set("Example.txt", 0x20, 5, 1030, true);
    support::put64(&mut file, 40, 513);
    file[1] += 1;
    let mut vendor = [0u8; 32];
    vendor[0] = 0xe1;
    vendor[1] = 1;
    support::put32(&mut vendor, 20, 10);
    support::put64(&mut vendor, 24, 512);
    file.extend_from_slice(&vendor);
    support::seal_set(&mut file);
    let empty = support::file_set("Empty.bin", 0x20, 0, 0, false);
    let directory = support::file_set("Sub", 0x10, 8, 512, true);
    let root = 32 * 512 + 96;
    let entries = [file, empty, directory].concat();
    bytes[root..root + entries.len()].copy_from_slice(&entries);
    bytes[33 * 512 + 1] |= 1;
    support::put32(&mut bytes, 24 * 512 + 10 * 4, 0xffff_ffff);
    let volume = Volume::open(support::Bytes(bytes)).unwrap();
    let report = check(&volume).unwrap();
    assert_eq!(report.unreferenced_clusters, 0);
    assert_eq!(report.referenced_clusters, 9);
    assert!(report.healthy());
}

#[test]
fn media_failure_alone_is_not_reported_clean() {
    let mut bytes = interface_support::image();
    support::put16(&mut bytes, 106, 4);
    let volume = Volume::open(support::Bytes(bytes)).unwrap();
    let report = check(&volume).unwrap();
    assert!(report.media_failure);
    assert!(!report.dirty);
    assert_eq!(report.unreferenced_clusters, 0);
    assert!(!report.healthy());
}

#[test]
fn extraction_chain_limits_do_not_limit_complete_audits() {
    let volume = Volume::open_with_limits(
        support::Bytes(interface_support::image()),
        ReadLimits {
            max_metadata_bytes: 65536,
            max_chain_clusters: 10,
        },
    )
    .unwrap();
    assert!(check(&volume).unwrap().healthy());
}

#[test]
fn aligned_partition_offsets_work_and_unaligned_offsets_are_refused() {
    let bytes = interface_support::image();
    let mut whole = vec![0; 4096];
    whole.extend(bytes);
    let volume = interface::open_device(support::Bytes(whole), 4096).unwrap();
    assert!(volume.lookup("example.txt").is_ok());
    assert!(interface::open_device(support::Bytes(interface_support::image()), 1).is_err());
    assert!(interface::open_device(support::Bytes(vec![0; 512]), 512).is_err());
}

#[test]
fn streaming_audits_reject_invalid_system_streams_before_reporting() {
    let root = 32 * 512;
    let mutations: &[(usize, &[u8], &str)] = &[
        (24 * 512 + 8, &[0, 0, 0, 0], "free or bad cluster"),
        (
            24 * 512 + 8,
            &[0xf7, 0xff, 0xff, 0xff],
            "free or bad cluster",
        ),
        (24 * 512 + 8, &[2, 0, 0, 0], "FAT chain cycle"),
        (
            24 * 512 + 8,
            &[0xf8, 0xff, 0xff, 0xff],
            "reserved or out-of-range",
        ),
        (24 * 512 + 12, &[3, 0, 0, 0], "chain length differs"),
        (
            root + 24,
            &[12, 0, 0, 0, 0, 0, 0, 0],
            "bitmap stream length",
        ),
        (33 * 512, &[0xfe], "system stream uses a free cluster"),
        (33 * 512, &[0xfb], "stream uses a free cluster"),
        (root + 32, &[0x81], "bitmap identity or duplicate"),
        (root + 56, &[0, 0, 0, 0, 0, 0, 0, 0], "upcase stream length"),
        (root + 64, &[0x84], "critical root directory entry"),
        (root + 96 + 2, &[0, 0], "directory set checksum"),
    ];
    for (offset, replacement, diagnostic) in mutations {
        let mut bytes = interface_support::image();
        bytes[*offset..offset + replacement.len()].copy_from_slice(replacement);
        let failure = fs_exfat::check::check_device(support::Bytes(bytes)).unwrap_err();
        assert!(
            failure.to_string().contains(diagnostic),
            "{offset}: {failure}"
        );
    }
}

#[test]
fn streaming_audits_reject_truncated_sets_and_wrong_file_chain_lengths() {
    let mut bytes = interface_support::image();
    let root = 32 * 512;
    for position in (384..480).step_by(32) {
        bytes[root + position] = 5;
    }
    bytes[root + 480] = 0x85;
    bytes[root + 481] = 2;
    assert!(fs_exfat::check::check_device(support::Bytes(bytes))
        .unwrap_err()
        .to_string()
        .contains("short allocation audit stream"));
    let mut bytes = interface_support::image();
    let file = &mut bytes[root + 96..root + 192];
    file[33] = 1;
    support::seal_set(file);
    support::put32(&mut bytes, 24 * 512 + 5 * 4, u32::MAX);
    assert!(fs_exfat::check::check_device(support::Bytes(bytes))
        .unwrap_err()
        .to_string()
        .contains("chain length differs"));
    let mut bytes = interface_support::image();
    let label = bytes[root + 64..root + 96].to_vec();
    for position in [96, 128, 160] {
        bytes[root + position..root + position + 32].copy_from_slice(&label);
    }
    assert!(fs_exfat::check::check_device(support::Bytes(bytes))
        .unwrap_err()
        .to_string()
        .contains("duplicate root system metadata"));
}
