//! `chore test:kernel`: THE LINUX KERNEL is the judge.
//!
//! Each undamaged fixture is loop-mounted read-only in the harness guest and
//! walked there now (`fixture-tool.py listing`): every path, size, mode,
//! modified and accessed time and SHA-256 the kernel's exfat driver reports.
//! This crate's strict reader must accept the same volume and agree on every
//! field, and salvage must rebuild every file the kernel reads. A walk is
//! one guest call per image, tagged `[kernel vm]`.
//!
//! [`kernel_walk`] takes any image under the repository, so the write
//! direction (#133: this crate writes, the kernel reads back) uses it as it
//! stands.
//!
//! The kernel driver is GPL: it runs in the guest as an oracle, never read.

#[path = "fixture_support/mod.rs"]
mod fixture_support;
#[path = "guest_support/mod.rs"]
mod guest_support;

use fixture_support::{
    assert_same, differences, fixture, images, kernel_listing, manifest, parse_listing, salvage,
    strict_listing, Listing,
};
use fs_core::{BlockRead, FileDevice};
use fs_exfat::interface::open_path;
use guest_support::{guest, guest_path, quote, stage_image};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;

/// The kernel's walk of `image`, mounted read-only in the guest now.
fn kernel_walk(image: &Path) -> Listing {
    let tool = repo_file("test-disks/fixture-tool.py");
    let script = format!(
        "{stage}mkdir \"$scratch/mnt\"\n\
         trap 'umount \"$scratch/mnt\" 2>/dev/null || true; rm -rf \"$scratch\"' EXIT\n\
         modprobe exfat\n\
         mount -t exfat -o loop,ro,fmask=0,dmask=0,iocharset=utf8 \"$image\" \"$scratch/mnt\"\n\
         python3 {tool} listing \"$scratch/mnt\" \"$scratch/listing.tsv\" >/dev/null\n\
         umount \"$scratch/mnt\"\n\
         cat \"$scratch/listing.tsv\"\n",
        stage = stage_image(image),
        tool = quote(&tool),
    );
    let name = image.file_name().unwrap().to_string_lossy();
    let out = guest(
        "kernel",
        &format!("mount -o loop,ro and walk {name}"),
        &script,
    )
    .success("the kernel's walk");
    let listing = parse_listing(&out.stdout, &format!("the kernel's walk of {name}"));
    println!(
        "[kernel vm] the kernel listed {} entries of {name}",
        listing.len()
    );
    listing
}

fn repo_file(relative: &str) -> String {
    guest_path(&guest_support::repo().join(relative))
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
