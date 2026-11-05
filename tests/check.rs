mod interface_support;
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
fn bounded_audits_fail_instead_of_skipping() {
    let volume = Volume::open_with_limits(
        support::Bytes(interface_support::image()),
        ReadLimits {
            max_metadata_bytes: 65536,
            max_chain_clusters: 10,
        },
    )
    .unwrap();
    assert!(check(&volume)
        .unwrap_err()
        .to_string()
        .contains("audit limit"));
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
