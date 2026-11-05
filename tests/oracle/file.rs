#[path = "../support/mod.rs"]
mod support;

use fs_core::BlockRead;
use fs_exfat::directory::DirectoryEntry;
use fs_exfat::volume::Volume;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use support::{put64, seal_set, Bytes};
use unicode_normalization::UnicodeNormalization;

fn walk<Device: BlockRead>(
    volume: &Volume<Device>,
    entries: Vec<DirectoryEntry>,
    prefix: &str,
    records: &mut BTreeMap<String, String>,
) {
    assert!(
        prefix.split('/').count() < 64,
        "directory cycle or excessive depth"
    );
    for entry in entries {
        let path: String = format!("{prefix}{}", entry.name()).nfc().collect();
        if entry.is_directory() {
            walk(
                volume,
                volume.read_directory(&entry).unwrap(),
                &format!("{path}/"),
                records,
            );
            continue;
        }
        let file = volume.open_file(&entry).unwrap();
        let mut hash = Sha256::new();
        let mut offset = 0;
        let mut buffer = [0; 8191];
        loop {
            let count = file.read_at(offset, &mut buffer).unwrap();
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            offset += count as u64;
        }
        assert_eq!(offset, file.len());
        if path == "valid-tail.bin" {
            assert_eq!(file.valid_data_length(), 4096);
            assert_eq!(file.len(), 12288);
            let mut tail = [0xff; 37];
            assert_eq!(file.read_at(4096, &mut tail).unwrap(), 37);
            assert_eq!(tail, [0; 37]);
        }
        let times = entry.timestamps();
        let modified = times
            .modified
            .unix_milliseconds()
            .expect("macOS must record a valid modified UTC offset");
        let accessed = times
            .accessed
            .unix_milliseconds()
            .expect("macOS must record a valid accessed UTC offset");
        let created = times
            .created
            .unix_milliseconds()
            .expect("macOS must record a valid created UTC offset");
        let value = format!(
            "{}\t{:x}\t{modified}\t{accessed}\t{created}",
            file.len(),
            hash.finalize()
        );
        assert!(records.insert(path, value).is_none());
    }
}

#[test]
fn every_file_hash_length_and_timestamp_match_the_read_only_macos_driver() {
    let directory =
        PathBuf::from(std::env::var("EXFAT_ORACLE_DIR").expect("run chore test:oracle on macOS"));
    for sector in [512, 4096] {
        let image = directory.join(format!("reference-{sector}.dmg"));
        assert!(std::fs::symlink_metadata(&image)
            .unwrap()
            .file_type()
            .is_file());
        let image_bytes = std::fs::read(image).unwrap();
        let volume = Volume::open(Bytes(image_bytes.clone())).unwrap();
        let entries = volume.root_directory().unwrap();
        let tail = entries
            .iter()
            .find(|entry| entry.name() == "valid-tail.bin")
            .unwrap();
        let tail_cluster = if tail.stream().no_fat_chain() {
            tail.stream().first_cluster() + 1
        } else {
            volume.cluster_chain(tail.stream().first_cluster()).unwrap()[1]
        };
        assert_eq!(volume.boot().cluster_size(), 4096);
        let physical = volume.boot().cluster_offset(tail_cluster).unwrap() as usize;
        assert!(image_bytes[physical..physical + 4096]
            .iter()
            .all(|byte| *byte == b'U'));
        let mut observed = BTreeMap::new();
        walk(&volume, entries, "", &mut observed);
        let expected =
            std::fs::read_to_string(directory.join(format!("files-{sector}.txt"))).unwrap();
        let expected: BTreeMap<String, String> = expected
            .lines()
            .map(|line| {
                let fields: Vec<_> = line.splitn(6, '\t').collect();
                assert_eq!(fields.len(), 6);
                (fields[5].nfc().collect(), fields[..5].join("\t"))
            })
            .collect();
        assert_eq!(observed.len(), expected.len());
        assert!(observed.len() > 185);
        for (path, value) in expected {
            assert_eq!(
                observed.get(&path),
                Some(&value),
                "{path} on {sector}-byte image"
            );
        }
    }
}

/// The specification zero-fills from ValidDataLength without rounding, so this reader
/// must, whatever the macOS driver does. That driver has been seen to keep original
/// bytes up to the next sector boundary (#10). Anything but that or an exact answer
/// fails here, which tells a change in the platform from a regression in the reader.
#[test]
fn an_unaligned_valid_length_is_exact_here_and_the_macos_driver_rounds_it_or_agrees() {
    let directory =
        PathBuf::from(std::env::var("EXFAT_ORACLE_DIR").expect("run chore test:oracle on macOS"));
    for sector in [512u64, 4096] {
        let mut image = std::fs::read(directory.join(format!("reference-{sector}.dmg"))).unwrap();
        let volume = Volume::open(Bytes(image.clone())).unwrap();
        let boot = volume.boot();
        let root = volume.root_bytes();
        let chain = volume.cluster_chain(boot.root_cluster).unwrap();
        let cluster_size = boot.cluster_size() as usize;
        let entries = volume.root_directory().unwrap();
        let entry = entries
            .iter()
            .find(|entry| entry.name() == "valid-tail.bin")
            .unwrap();
        let start = root
            .chunks_exact(32)
            .position(|candidate| candidate == entry.raw_primary())
            .unwrap()
            * 32;
        let mut set = root[start..start + (usize::from(root[start + 1]) + 1) * 32].to_vec();
        put64(&mut set, 40, 513);
        seal_set(&mut set);
        for (index, byte) in set.iter().enumerate() {
            let at = start + index;
            let physical =
                boot.cluster_offset(chain[at / cluster_size]).unwrap() as usize + at % cluster_size;
            image[physical] = *byte;
        }

        let volume = Volume::open(Bytes(image)).unwrap();
        let entries = volume.root_directory().unwrap();
        let entry = entries
            .iter()
            .find(|entry| entry.name() == "valid-tail.bin")
            .unwrap();
        let file = volume.open_file(entry).unwrap();
        assert_eq!(file.valid_data_length(), 513);
        let mut contents = vec![0xff; file.len() as usize];
        assert_eq!(file.read_at(0, &mut contents).unwrap(), contents.len());
        assert!(contents[..513].iter().all(|byte| *byte == b'U'));
        assert!(contents[513..].iter().all(|byte| *byte == 0));

        let kept: u64 = std::fs::read_to_string(directory.join(format!("unaligned-{sector}.txt")))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let rounded = 513u64.next_multiple_of(sector);
        assert!(
            kept == 513 || kept == rounded,
            "the {sector}-byte driver kept {kept} original bytes, neither 513 nor {rounded}"
        );
        println!("{sector}-byte driver kept {kept} original bytes of a 513-byte valid length");
    }
}
