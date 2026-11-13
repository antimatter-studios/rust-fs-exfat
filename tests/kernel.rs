//! `chore test:kernel`: THE LINUX KERNEL is the judge.
//!
//! Each undamaged fixture is loop-mounted read-only in the harness guest and
//! walked there now (`fixture-tool.py listing`): every path, size, mode,
//! modified and accessed time and SHA-256 the kernel's exfat driver reports.
//! This crate's strict reader must accept the same volume and agree on every
//! field, and salvage must rebuild every file the kernel reads. A walk is
//! one guest call per image, tagged `[kernel vm]`.
//!
//! `kernel_walk` (tests/walk_support) takes any image under the repository, so the write
//! direction uses it as it stands: #129 checks allocation/free and volume
//! state; #130 checks creation and file-data writes. Crash recovery remains #133.
//!
//! The kernel driver is GPL: it runs in the guest as an oracle, never read.

mod allocation_support;
#[path = "fixture_support/mod.rs"]
mod fixture_support;
#[path = "guest_support/mod.rs"]
mod guest_support;
#[path = "walk_support/mod.rs"]
mod walk_support;
#[path = "write_support/created.rs"]
mod write_support;

use fixture_support::{
    assert_same, differences, fixture, images, kernel_listing, manifest, salvage, strict_listing,
};
use fs_core::{BlockRead, FileDevice};
use fs_exfat::interface::open_path;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use walk_support::kernel_walk;

#[test]
fn the_kernel_mounts_and_preserves_the_tree_after_allocation_and_free() {
    for (sector, cluster) in allocation_support::GEOMETRIES {
        let image = guest_support::write_image::WriteImage::new("kernel", sector, cluster);
        let before = kernel_walk(&image.path);
        allocation_support::allocate_and_free(&image.path);
        image.fsck("kernel");
        let after = kernel_walk(&image.path);
        assert_same(
            "allocation/free",
            "the kernel before",
            &before,
            "the kernel after unmount",
            &after,
            true,
        );
        let volume = open_path(&image.path, 0).unwrap();
        assert_same(
            "allocation/free",
            "the kernel",
            &after,
            "the strict reader",
            &strict_listing(&volume),
            true,
        );
    }
}

#[test]
fn the_kernel_reads_files_and_directories_created_by_this_crate() {
    for (sector, cluster) in allocation_support::GEOMETRIES {
        let image = guest_support::write_image::WriteImage::new("kernel", sector, cluster);
        write_support::files_and_directories(&image.path);
        image.fsck("kernel");
        let source = format!("crate-created s{sector}/c{cluster}");
        let volume = open_path(&image.path, 0).unwrap();
        let expected = strict_listing(&volume);
        let observed = kernel_walk(&image.path);
        assert_same(
            &source,
            "the writer's strict reader",
            &expected,
            "the kernel",
            &observed,
            true,
        );
        let expected_birth_times = expected
            .keys()
            .map(|path| {
                let found = volume.lookup(&format!("/{path}")).unwrap();
                let created = found.entry().unwrap().timestamps().created;
                (path.clone(), fixture_support::milliseconds(&created))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let birth_times = walk_support::kernel_birth_times(&image.path);
        assert_eq!(birth_times.len(), expected_birth_times.len(), "{source}");
        for (path, expected) in expected_birth_times {
            assert_eq!(
                birth_times.get(&path),
                Some(&expected),
                "{source}: {path}: the kernel's creation time must include hundredths and UTC offsets"
            );
        }
    }
}

fn undamaged() -> Vec<String> {
    images()
        .into_iter()
        .filter(|image| !manifest(image).damaged())
        .collect()
}

#[test]
fn the_kernel_and_the_strict_reader_agree_on_every_undamaged_fixture() {
    let mut entries = 0;
    for image in undamaged() {
        let kernel = kernel_walk(&fixture(&image));
        let volume = open_path(&fixture(&image), 0)
            .unwrap_or_else(|error| panic!("{image}: the strict reader refuses it: {error}"));
        assert_same(
            &image,
            "the kernel",
            &kernel,
            "the strict reader",
            &strict_listing(&volume),
            true,
        );
        // The walk made now and the one captured when the image was built
        // are the same kernel reading the same bytes.
        assert_same(
            &image,
            "the kernel at build time",
            &kernel_listing(&image),
            "the kernel now",
            &kernel,
            true,
        );
        entries += kernel.len();
    }
    println!("[kernel vm] the strict reader agreed with the kernel on {entries} entries");
}

#[test]
fn salvage_rebuilds_every_file_the_kernel_reads_from_every_undamaged_fixture() {
    let mut files = 0;
    for image in undamaged() {
        let kernel = kernel_walk(&fixture(&image));
        let source: Arc<dyn BlockRead> = Arc::new(FileDevice::open(fixture(&image)).unwrap());
        let found = salvage(&*source);
        assert!(
            found.problems.is_empty(),
            "{image}: salvage problems:\n  {}",
            found.problems.join("\n  ")
        );
        assert_same(&image, "the kernel", &kernel, "salvage", &found.live, false);
        files += kernel.values().filter(|row| !row.directory).count();
    }
    println!("[kernel vm] salvage rebuilt {files} files the kernel read");
}

/// The tier's comparison catches a misreading: the strict reader's listing,
/// with one file read as if its last cluster had been dropped, must
/// disagree with the kernel on exactly that file's hash.
#[test]
fn a_file_read_without_its_last_cluster_is_caught() {
    let image = "exfat-tree-s512-c4k.img";
    let victim = "large.bin";
    let kernel = kernel_walk(&fixture(image));
    let volume = open_path(&fixture(image), 0).unwrap();
    let mut misread = strict_listing(&volume);

    let entry = volume
        .root_directory()
        .unwrap()
        .into_iter()
        .find(|entry| entry.name() == victim)
        .unwrap();
    let file = volume.open_file(&entry).unwrap();
    let cluster = volume.boot().cluster_size();
    let kept = (file.len() - 1) / cluster * cluster;
    let mut content = vec![0; file.len() as usize];
    let mut offset = 0;
    while offset < file.len() {
        offset += file
            .read_at(offset, &mut content[offset as usize..])
            .unwrap() as u64;
    }
    content[kept as usize..].fill(0);
    misread.get_mut(victim).unwrap().sha256 = Some(format!("{:x}", Sha256::digest(&content)));

    let found = differences("the kernel", &kernel, "a misreading", &misread, true);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with(&format!("{victim}: sha256 ")),
        "the difference names the file and the field: {}",
        found[0]
    );
    println!(
        "[kernel vm] a dropped last cluster of {victim} is caught: {}",
        found[0]
    );
}
