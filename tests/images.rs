//! `chore test:images`: every kernel-made fixture read without a VM.
//!
//! The images were formatted by exfatprogs and written by the Linux kernel's
//! exfat driver in the harness guest (`chore fixtures`), which also captured
//! the kernel's own listing of each one. Here the strict reader, the
//! allocation audit, the salvage verbs and rust-fs-core's shared layers read
//! the same images and must agree with that listing. Nothing here needs the VM or exfatprogs, so the tier
//! runs on any host that has the fixtures, arm64 runners included.

#![allow(deprecated)]

#[path = "fixture_support/mod.rs"]
mod fixture_support;

use fixture_support::{
    assert_same, fixture, images, kernel_listing, manifest, read_listing, salvage, strict_listing,
    Listing, Row,
};
use fs_core::layers::conformance::{self, Limits};
use fs_core::layers::{FileAPI, NodeKind, OpenVolume, Timestamp};
use fs_core::{BlockRead, FileDevice};
use fs_exfat::check::{check, Rule};
use fs_exfat::interface::open_path;
use fs_exfat::layers::ExfatVolume;
use fs_exfat::salvage::Severity;
use sha2::{Digest, Sha256};
use std::sync::Arc;

fn undamaged() -> Vec<String> {
    images()
        .into_iter()
        .filter(|image| !manifest(image).damaged())
        .collect()
}

fn source(image: &str) -> Arc<dyn BlockRead> {
    Arc::new(FileDevice::open(fixture(image)).unwrap())
}

#[test]
fn every_fixture_has_a_manifest_and_listing_and_a_recipe_this_tier_reads() {
    for image in images() {
        let manifest = manifest(&image);
        assert_eq!(manifest.get("image"), image);
        let recipe = manifest.get("recipe");
        assert!(
            [
                "empty",
                "tree",
                "small",
                "fragmented",
                "largedir",
                "deleted",
                "full"
            ]
            .contains(&recipe),
            "{image}: recipe {recipe} is read by no test here"
        );
        if let Some(damage) = manifest.find("damage") {
            assert!(
                ["boot", "bitmap", "upcase", "data"].contains(&damage),
                "{image}: damage {damage} is read by no test here"
            );
            fixture(&format!("{image}.damage.tsv"));
        }
        kernel_listing(&image);
    }
}

#[test]
fn the_strict_reader_lists_every_undamaged_image_as_the_kernel_did() {
    for image in undamaged() {
        let volume = open_path(&fixture(&image), 0)
            .unwrap_or_else(|error| panic!("{image}: the strict reader refuses it: {error}"));
        let ours = strict_listing(&volume);
        assert_same(
            &image,
            "the kernel",
            &kernel_listing(&image),
            "the strict reader",
            &ours,
            true,
        );
    }
}

#[test]
fn the_boot_sector_and_label_are_what_mkfs_exfat_was_asked_for() {
    for image in undamaged() {
        let manifest = manifest(&image);
        let volume = open_path(&fixture(&image), 0).unwrap();
        let boot = volume.boot();
        assert_eq!(
            boot.bytes_per_sector(),
            manifest.bytes("sector-size"),
            "{image}: sector size"
        );
        assert_eq!(
            boot.cluster_size(),
            manifest.bytes("cluster-size"),
            "{image}: cluster size"
        );
        assert_eq!(
            boot.bytes_per_sector() * boot.volume_length,
            manifest.bytes("size"),
            "{image}: volume length"
        );
        assert_eq!(
            volume.metadata().label.as_deref(),
            Some(manifest.get("label")),
            "{image}: volume label"
        );
    }
}

#[test]
fn the_allocation_audit_and_deviation_report_are_clean_on_every_undamaged_image() {
    for image in undamaged() {
        let volume = open_path(&fixture(&image), 0).unwrap();
        let report = check(&volume).unwrap();
        assert!(
            report.healthy(),
            "{image}: the audit is not clean: {report:?}"
        );
        assert!(
            report.boot_differences.is_empty(),
            "{image}: boot copies differ: {:?}",
            report.boot_differences
        );
        // mkfs.exfat 1.2.0 fills the whole OEM Parameters sector (the tenth
        // of each boot region) with FFh, its 32 reserved bytes at 480
        // included, which section 3.3 (Table 7) reserves. That one notice
        // is mkfs.exfat's and is expected on every image, at exactly those
        // two places. The PercentInUse warning below is the kernel's. Any
        // other deviation fails.
        let sector = volume.boot().bytes_per_sector();
        let expected: Vec<u64> = vec![9 * sector + 480, 21 * sector + 480];
        let unexpected: Vec<_> = report
            .deviations
            .found
            .iter()
            .filter(|deviation| {
                let oem = deviation.rule == Rule::OemReserved
                    && deviation.severity == Severity::Notice
                    && expected.contains(&deviation.offset);
                // The kernel never updates PercentInUse (section 3.1.18), so a
                // volume it wrote to keeps the 0 mkfs.exfat put there.
                let in_use = deviation.rule == Rule::PercentInUse
                    && deviation.severity == Severity::Warning
                    && deviation.offset == 112
                    && deviation.detail.starts_with("0%, but ");
                !(oem || in_use)
            })
            .collect();
        assert!(
            unexpected.is_empty() && report.deviations.omitted.is_empty(),
            "{image}: deviations from the specification: {unexpected:?} {:?}",
            report.deviations.omitted
        );
        assert!(
            report.deviations.complete,
            "{image}: the deviation scan was incomplete: {:?}",
            report.deviations.skipped
        );
        let listing = kernel_listing(&image);
        let files = listing.values().filter(|row| !row.directory).count() as u64;
        let directories = listing.values().filter(|row| row.directory).count() as u64;
        assert_eq!(report.files, files, "{image}: files counted");
        // The audit counts the root directory; the listing starts below it.
        assert_eq!(
            report.directories,
            directories + 1,
            "{image}: directories counted"
        );
    }
}

#[test]
fn salvage_finds_every_entry_of_every_image_that_is_still_there() {
    for image in images() {
        let found = salvage(&*source(&image));
        assert!(
            found.problems.is_empty(),
            "{image}: salvage problems:\n  {}",
            found.problems.join("\n  ")
        );
        assert_same(
            &image,
            "the damage record's survivors",
            &kernel_listing(&image),
            "salvage",
            &found.live,
            false,
        );
    }
}

#[test]
fn salvage_finds_the_files_the_kernel_deleted() {
    let image = "exfat-deleted-s512-c4k.img";
    let gone = read_listing(&fixture(&format!("{image}.deleted.tsv")));
    assert_eq!(gone.len(), 8);
    let found = salvage(&*source(image));
    assert_same(
        image,
        "the deleted listing",
        &gone,
        "salvage's deleted sets",
        &found.deleted,
        false,
    );
}

/// What the strict reader makes of each damaged copy. It reads the copies
/// whose damage leaves the strict structures valid, and refuses the others.
#[test]
fn the_strict_reader_reads_or_refuses_each_damaged_copy_as_its_damage_requires() {
    for image in images() {
        let manifest = manifest(&image);
        let Some(damage) = manifest.find("damage") else {
            continue;
        };
        let opened = open_path(&fixture(&image), 0);
        match damage {
            // The Main Boot Region's checksum fails, the Up-case Table's
            // checksum fails, or the Allocation Bitmap no longer allocates the
            // system streams' clusters: a strict reader refuses each.
            "boot" | "upcase" | "bitmap" => assert!(
                opened.is_err(),
                "{image}: the strict reader accepted a volume whose {damage} is damaged"
            ),
            // Every structure the strict reader validates is intact: it reads
            // the volume as the kernel read the damaged copy.
            "data" => {
                let volume = opened.unwrap_or_else(|error| panic!("{image}: refused: {error}"));
                assert_same(
                    &image,
                    "the damage record's survivors",
                    &kernel_listing(&image),
                    "the strict reader",
                    &strict_listing(&volume),
                    true,
                );
            }
            other => panic!("{image}: no expectation for damage {other}"),
        }
    }
}

/// The shared FileAPI's listing of a volume, in the kernel's terms.
fn layers_listing(volume: &ExfatVolume) -> Listing {
    fn walk(
        volume: &ExfatVolume,
        node: &<ExfatVolume as FileAPI>::Node,
        prefix: &str,
        rows: &mut Listing,
    ) {
        assert!(prefix.matches('/').count() < 64, "excessive depth");
        let milliseconds = |time: Option<Timestamp>| {
            let time = time.expect("exFAT records every time");
            time.seconds * 1000 + i64::from(time.nanoseconds / 1_000_000)
        };
        for entry in volume.read_dir(node).unwrap() {
            let path = format!("{prefix}{}", entry.name);
            let metadata = volume.metadata(&entry.node).unwrap();
            let directory = metadata.kind == NodeKind::Directory;
            let sha256 = (!directory).then(|| {
                let mut bytes = Vec::new();
                assert_eq!(
                    volume.copy_to(&entry.node, &mut bytes).unwrap(),
                    metadata.size
                );
                format!("{:x}", Sha256::digest(&bytes))
            });
            let row = Row {
                directory,
                size: metadata.size,
                read_only: metadata.attributes.unwrap() & 1 != 0,
                modified_ms: milliseconds(metadata.modified),
                accessed_ms: milliseconds(metadata.accessed),
                sha256,
            };
            assert!(rows.insert(path.clone(), row).is_none(), "{path} twice");
            if directory {
                walk(volume, &entry.node, &format!("{path}/"), rows);
            }
        }
    }
    let mut rows = Listing::new();
    walk(volume, &volume.root().unwrap(), "", &mut rows);
    rows
}

/// Large enough for the largest fixture, so no tree is checked in part.
/// Every path is still listed, read and compared with the kernel; 256 of
/// them, spread over the tree, are also resolved by lookup, which reads every
/// directory on the path, so one per entry of the large-directory image
/// alone takes minutes.
fn limits() -> Limits {
    Limits {
        max_nodes: 1 << 16,
        max_walks: 1 << 16,
        max_lookups: 256,
        ..Limits::default()
    }
}

#[test]
fn the_shared_layers_conform_and_list_every_image_as_the_kernel_did() {
    for image in images() {
        if manifest(&image).damaged() {
            let volume = ExfatVolume::open_for_salvage(source(&image))
                .unwrap_or_else(|error| panic!("{image}: no salvage open: {error}"));
            let report = conformance::check_damaged(&volume, &limits());
            assert!(report.passed(), "{image}:\n{report}");
            continue;
        }
        let volume = ExfatVolume::open(source(&image))
            .unwrap_or_else(|error| panic!("{image}: the shared layers refuse it: {error}"));
        let report = conformance::check_healthy(&volume, &[], &limits());
        assert!(report.passed(), "{image}:\n{report}");
        let report = conformance::check_files_against_candidates(&volume, &limits());
        assert!(report.passed(), "{image}:\n{report}");
        assert_same(
            &image,
            "the kernel",
            &kernel_listing(&image),
            "the shared FileAPI",
            &layers_listing(&volume),
            true,
        );
    }
}
