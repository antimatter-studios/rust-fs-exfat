#[path = "../scale_support/mod.rs"]
mod support;

use crate::interface_support::base;
use fs_core::{BlockRead, FileDevice};
use std::os::unix::fs::FileExt;
use support::{clean, Image, MAX_CLUSTERS, OLD_CAP};

#[test]
fn a_sparse_volume_above_the_old_cluster_cap_checks_clean() {
    let image = Image::sparse(OLD_CAP + 4096, 0);
    image.high_file(0, OLD_CAP + 17);
    let report = clean(image.check());
    assert_eq!(report["files"], 1);
    assert_eq!(report["allocated_clusters"], image.allocated + 1);
}

#[test]
fn large_and_format_maximum_cluster_sizes_use_the_same_bounded_audit() {
    for (clusters, cluster_size) in [(4_506_872, 128 * 1024), (OLD_CAP + 4096, 32 * 1024 * 1024)] {
        let image = Image::with_cluster_size(clusters, 0, cluster_size);
        image.high_file(0, OLD_CAP + 17);
        let report = fs_exfat::check::check_device(&image).unwrap();
        assert!(report.healthy());
        assert_eq!(report.files, 1);
        assert_eq!(report.allocated_clusters, image.allocated + 1);
        assert_eq!(report.referenced_clusters, image.allocated + 1);
    }
}

#[test]
fn virtual_scale_reads_zero_fill_only_within_the_logical_volume() {
    let image = Image::sparse(64, 0);
    let end = image.file.metadata().unwrap().len();
    image.file.write_all_at(&[1, 2, 3, 4], end - 8).unwrap();
    image.file.set_len(end - 4).unwrap();
    let mut output = [0xff; 8];
    image.read_at(end - 8, &mut output).unwrap();
    assert_eq!(output, [1, 2, 3, 4, 0, 0, 0, 0]);
    image.read_at(end - 4, &mut output[..4]).unwrap();
    assert_eq!(&output[..4], &[0; 4]);
    assert!(image.read_at(end - 4, &mut output).is_err());
    assert!(image.read_at(u64::MAX, &mut output).is_err());
    image.read_at(end, &mut []).unwrap();
    assert!(image.read_at(end + 1, &mut []).is_err());
    assert_eq!(image.size_bytes(), end);
}

#[test]
fn cross_links_above_the_old_cluster_cap_are_not_hidden_by_limits() {
    let image = Image::sparse(OLD_CAP + 4096, 0);
    image.high_file(0, OLD_CAP + 17);
    image.high_file(1, OLD_CAP + 17);
    let output = image.check();
    assert_eq!(output.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cross-linked"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("\"healthy\":true"));
}

#[test]
fn unreferenced_allocations_above_the_old_cluster_cap_are_counted() {
    let image = Image::sparse(OLD_CAP + 4096, 0);
    image.allocate(OLD_CAP + 29);
    let output = image.check();
    assert_eq!(output.status.code(), Some(4));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["unreferenced_clusters"], 1);
    assert_eq!(report["healthy"], false);
}

#[test]
fn more_than_a_million_entries_and_a_large_root_are_fully_audited() {
    let image = Image::sparse(OLD_CAP + 4096, OLD_CAP + 1);
    let report = clean(image.check());
    assert_eq!(report["files"], u64::from(OLD_CAP) + 1);
    assert_eq!(report["directories"], 1);
    assert_eq!(report["unreferenced_clusters"], 0);
    image.high_file(u64::from(OLD_CAP) - 1, OLD_CAP + 17);
    image.high_file(u64::from(OLD_CAP), OLD_CAP + 17);
    let output = image.check();
    assert_eq!(output.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cross-linked"));
}

#[test]
fn the_format_maximum_and_512_mib_bitmap_fit_the_default_audit_budget() {
    let image = Image::sparse(MAX_CLUSTERS, 0);
    let report = clean(image.check());
    assert_eq!(report["allocated_clusters"], image.allocated);
    assert_eq!(report["referenced_clusters"], image.allocated);
}

#[test]
fn fat_linked_and_contiguous_files_can_exceed_the_extraction_chain_limit() {
    for contiguous in [false, true] {
        let image = Image::sparse(OLD_CAP + 4096, 0);
        image.long_file(OLD_CAP + 1, contiguous);
        let report = clean(image.check());
        assert_eq!(report["files"], 1);
        assert_eq!(
            report["referenced_clusters"],
            image.allocated + u64::from(OLD_CAP) + 1
        );
        if !contiguous {
            let last = image.allocated + 2 + u64::from(OLD_CAP);
            image
                .file
                .write_all_at(&[0; 4], 24 * 512 + last * 4)
                .unwrap();
            let output = image.check();
            assert_eq!(output.status.code(), Some(4));
            assert!(String::from_utf8_lossy(&output.stderr).contains("free or bad cluster"));
        }
    }
}

#[test]
fn duplicate_names_across_streamed_entry_sets_are_rejected() {
    let image = Image::sparse(OLD_CAP + 4096, 1000);
    let mut entry = [0; 96];
    image
        .file
        .read_exact_at(&mut entry, image.heap + 64)
        .unwrap();
    image
        .file
        .write_all_at(&entry, image.heap + 64 + 999 * 96)
        .unwrap();
    let output = image.check();
    assert_eq!(output.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate case-insensitive"));
}

struct BoundedReads {
    file: FileDevice,
    fail_at: Option<u64>,
}

impl BlockRead for BoundedReads {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        assert!(output.len() <= 64 * 1024);
        if self.fail_at.is_some_and(|position| offset == position) {
            return Err(fs_core::Error::Custom(
                "injected late audit I/O failure".into(),
            ));
        }
        self.file.read_at(offset, output)
    }

    fn size_bytes(&self) -> u64 {
        self.file.size_bytes()
    }
}

#[test]
fn exact_bitmap_budget_and_late_io_failures_never_produce_clean_reports() {
    use fs_exfat::check::{check_device, check_device_with_budget};

    let clusters = OLD_CAP + 4096;
    let image = Image::sparse(clusters, 0);
    let bitmap_budget = 1024 * 1024 + 2 * (clusters as usize).div_ceil(8);
    for (limit, diagnostic) in [
        (1024 * 1024 - 1, "fixed buffers exceed memory budget"),
        (
            1024 * 1024 + (clusters as usize).div_ceil(8) - 1,
            "ownership bitmap exceeds memory budget",
        ),
    ] {
        let refused =
            check_device_with_budget(FileDevice::open(&image.path).unwrap(), limit).unwrap_err();
        assert!(refused.to_string().contains(diagnostic));
    }
    let refused =
        check_device_with_budget(FileDevice::open(&image.path).unwrap(), bitmap_budget - 1)
            .unwrap_err();
    assert!(refused
        .to_string()
        .contains("allocation bitmap exceeds allocation audit memory budget"));
    let refused = check_device_with_budget(FileDevice::open(&image.path).unwrap(), bitmap_budget)
        .unwrap_err();
    assert!(refused
        .to_string()
        .contains("directory workspace exceeds memory budget"));
    let report =
        check_device_with_budget(FileDevice::open(&image.path).unwrap(), bitmap_budget + 4096)
            .unwrap();
    assert!(report.healthy());
    let report = check_device(BoundedReads {
        file: FileDevice::open(&image.path).unwrap(),
        fail_at: None,
    })
    .unwrap();
    assert!(report.healthy());
    let failure = check_device(BoundedReads {
        file: FileDevice::open(&image.path).unwrap(),
        fail_at: Some(image.bitmap + 512),
    })
    .unwrap_err();
    assert!(failure
        .to_string()
        .contains("injected late audit I/O failure"));
}
