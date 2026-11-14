#[path = "../support/mod.rs"]
mod support;

#[path = "../support/image.rs"]
mod image_support;

use fs_core::BlockRead;
use fs_exfat::directory::DirectoryEntry;
use fs_exfat::volume::Volume;
use std::collections::BTreeSet;
use std::path::PathBuf;
use unicode_normalization::UnicodeNormalization;

fn walk<Device: BlockRead>(
    volume: &Volume<Device>,
    entries: Vec<DirectoryEntry>,
    prefix: &str,
    records: &mut BTreeSet<String>,
) {
    assert!(
        prefix.split('/').count() < 64,
        "directory cycle or excessive depth"
    );
    for entry in entries {
        let path: String = format!("{prefix}{}", entry.name()).nfc().collect();
        let kind = if entry.is_directory() { 'd' } else { 'f' };
        let length = if entry.is_directory() {
            0
        } else {
            entry.stream().data_length()
        };
        assert!(records.insert(format!("{kind}\t{length}\t{path}\n")));
        if entry.is_directory() {
            walk(
                volume,
                volume.read_directory(&entry).unwrap(),
                &format!("{path}/"),
                records,
            );
        }
    }
}

#[test]
fn every_directory_path_and_file_size_match_the_read_only_macos_driver() {
    let directory = PathBuf::from(
        std::env::var("EXFAT_ORACLE_DIR").expect("run chore test:oracle-macos on macOS"),
    );
    for sector in [512, 4096] {
        let image = directory.join(format!("reference-{sector}.dmg"));
        assert!(std::fs::symlink_metadata(&image)
            .unwrap()
            .file_type()
            .is_file());
        let volume = Volume::open(image_support::Image::open(image)).unwrap();
        let mut records = BTreeSet::new();
        walk(&volume, volume.root_directory().unwrap(), "", &mut records);
        assert!(records.len() > 185);
        let expected =
            std::fs::read_to_string(directory.join(format!("listing-{sector}.txt"))).unwrap();
        let expected: BTreeSet<String> = expected
            .lines()
            .map(|line| format!("{line}\n").nfc().collect())
            .collect();
        assert!(
            records == expected,
            "directory listing differs from the macOS driver on the {sector}-byte image"
        );
    }
}
