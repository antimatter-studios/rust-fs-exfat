#[path = "../support/mod.rs"]
mod support;

mod primitives;
mod salvage;
mod scale;
mod windows;

#[cfg(feature = "test-support")]
mod fixtures;

use fs_exfat::{boot::BootRegions, device::Reader};
use std::path::PathBuf;
use support::Bytes;

/// Volumes that newfs_exfat formatted and the macOS driver wrote break none
/// of the rules in docs/spec-rules.md: an independent writer's structures
/// agree with how this crate reads the specification.
#[test]
fn macos_formatted_and_written_volumes_break_no_specification_rule() {
    let directory = PathBuf::from(
        std::env::var("EXFAT_ORACLE_DIR")
            .expect("run chore test:oracle on macOS to create independent reference images"),
    );
    for sector in [512, 4096] {
        let image = std::fs::read(directory.join(format!("reference-{sector}.dmg"))).unwrap();
        let report = fs_exfat::check::spec_deviations(Bytes(image));
        // macOS 26 writers leave PercentInUse stale after writing files (spec
        // 3.1.18 asks for an update or FFh). That is a Warning the runner shows
        // and this test records; every other rule must hold.
        let unexpected: Vec<_> = report
            .found
            .iter()
            .filter(|d| {
                !(d.rule == fs_exfat::check::Rule::PercentInUse
                    && d.severity == fs_exfat::salvage::Severity::Warning)
            })
            .collect();
        assert!(unexpected.is_empty(), "{sector}: {unexpected:?}");
        assert!(report.complete, "{sector}: {:?}", report.skipped);
    }
}

#[test]
fn macos_formatter_and_checker_agree_with_both_boot_regions() {
    let directory = PathBuf::from(
        std::env::var("EXFAT_ORACLE_DIR")
            .expect("run chore test:oracle on macOS to create independent reference images"),
    );
    for sector in [512, 4096] {
        let image = directory.join(format!("reference-{sector}.dmg"));
        assert!(std::fs::symlink_metadata(&image)
            .unwrap()
            .file_type()
            .is_file());
        let report = BootRegions::read(&Reader::new(Bytes(std::fs::read(image).unwrap()))).unwrap();
        let boot = report.strict().unwrap();
        assert_eq!(boot.bytes_per_sector(), sector);
        assert_eq!(boot.cluster_size(), 4096);
        assert_eq!(boot.volume_length * sector, 64 * 1024 * 1024);
        let formatter =
            std::fs::read_to_string(directory.join(format!("format-{sector}.txt"))).unwrap();
        for (label, expected) in [
            ("FAT offset", u64::from(boot.fat_offset)),
            ("# FAT sectors", u64::from(boot.fat_length)),
            ("Cluster offset", u64::from(boot.cluster_heap_offset)),
            ("# Clusters", u64::from(boot.cluster_count)),
            ("Root start", u64::from(boot.root_cluster)),
        ] {
            let line = formatter
                .lines()
                .find(|line| line.split(':').next().unwrap().trim() == label)
                .unwrap();
            let observed: u64 = line
                .split(':')
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(expected, observed, "{label}");
        }
        assert!(report
            .differences
            .iter()
            .all(|difference| difference.is_mutable()));
    }
}
