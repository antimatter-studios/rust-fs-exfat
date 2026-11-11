//! Producer for the write oracle. It copies a volume macOS formatted and
//! populated, changes it through `fs_exfat::write`, and records the tree
//! this crate's reader then sees. `scripts/test-oracle.sh` checks the result
//! with `fsck_exfat -n` and compares the record with what the read-only
//! mounted macOS driver lists.

use fs_exfat::interface;
use fs_exfat::timestamp::{Timestamp, Timestamps};
use fs_exfat::volume::Volume;
use fs_exfat::write::{self, Allocation, NewEntry};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

fn listing<Device: fs_core::BlockRead>(volume: &Volume<Device>) -> String {
    fn walk<Device: fs_core::BlockRead>(
        volume: &Volume<Device>,
        prefix: &str,
        entries: Vec<fs_exfat::directory::DirectoryEntry>,
        records: &mut Vec<String>,
    ) {
        for entry in entries {
            let path: String = format!("{prefix}{}", entry.name()).nfc().collect();
            if entry.is_directory() {
                records.push(format!("d\t0\t-\t{path}\n"));
                walk(
                    volume,
                    &format!("{path}/"),
                    volume.read_directory(&entry).unwrap(),
                    records,
                );
                continue;
            }
            let file = volume.open_file(&entry).unwrap();
            let mut hash = Sha256::new();
            let mut buffer = vec![0; 1 << 20];
            let mut offset = 0;
            loop {
                let count = file.read_at(offset, &mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
                offset += count as u64;
            }
            let times = entry.timestamps();
            let milliseconds = |time: &Timestamp| {
                time.unix_milliseconds()
                    .expect("every file on the written image has a UTC offset")
            };
            records.push(format!(
                "f\t{}\t{:x}\t{}\t{}\t{}\t{path}\n",
                file.len(),
                hash.finalize(),
                milliseconds(&times.modified),
                milliseconds(&times.accessed),
                milliseconds(&times.created),
            ));
        }
    }
    let mut records = Vec::new();
    walk(volume, "", volume.root_directory().unwrap(), &mut records);
    records.sort();
    records.concat()
}

fn paths() -> (PathBuf, String) {
    let directory =
        PathBuf::from(std::env::var("EXFAT_ORACLE_DIR").expect("run chore test:oracle on macOS"));
    let sector = std::env::var("EXFAT_ORACLE_SECTOR").expect("run chore test:oracle on macOS");
    (directory, sector)
}

fn bitmap(image: &Path) -> (Vec<bool>, u32) {
    let volume = interface::open_path(image, 0).unwrap();
    let count = volume.boot().cluster_count;
    let bits = (2..count + 2)
        .map(|cluster| volume.bitmap().allocated(cluster).unwrap())
        .collect();
    (bits, count)
}

/// One test, because every step changes the same image in turn.
#[test]
fn the_writer_changes_a_volume_macos_populated() {
    let (directory, sector) = paths();
    let source = directory.join(format!("populated-{sector}.dmg"));
    let target = directory.join(format!("written-{sector}.dmg"));
    std::fs::copy(&source, &target).unwrap();
    allocations_leave_the_tree_alone(&target);
    let tree = files_and_directories(&target);
    std::fs::write(
        directory.join(format!("written-expected-{sector}.txt")),
        tree,
    )
    .unwrap();
}

fn pattern(seed: u32, length: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2).wrapping_add(1);
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect()
}

/// Files and directories of every shape #130 names, written beside macOS's
/// own: large, fragmented, non-ASCII and long names, directories grown past
/// a cluster, a gap that must read as zeros, a replaced file, and times
/// given with hundredths and UTC offsets. Returns the tree as this crate's
/// reader sees it after the unmount.
fn files_and_directories(target: &Path) -> String {
    let mut volume = write::open_path(target, 0).unwrap();
    volume.mkdir("/Written").unwrap();
    volume.mkdir("/Written/Deep").unwrap();
    volume.mkdir("/Written/Deep/Er").unwrap();
    volume.mkdir("/Written/Many").unwrap();
    volume.mkdir("/Written/Ünïcödé 目录").unwrap();
    volume
        .write_file("/Written/ascii.txt", b"plain ASCII contents\n")
        .unwrap();
    volume.create("/Written/empty").unwrap();
    for name in ["grüße-😀.txt", "日本語のファイル名.txt", "ÀÉÎÕÜ àéîõü.bin"]
    {
        let path = format!("/Written/Ünïcödé 目录/{name}");
        volume.write_file(&path, name.as_bytes()).unwrap();
    }
    let long = format!(
        "/Written/{}",
        "a long name that needs many File Name entries "
            .repeat(5)
            .trim_end()
    );
    volume.write_file(&long, b"long").unwrap();
    for index in 0..200 {
        let path = format!("/Written/Many/entry {index:03} with a name of some length.dat");
        volume
            .write_file(&path, &pattern(index, index as usize * 37))
            .unwrap();
    }
    volume
        .write_file(
            "/Written/Deep/Er/large.bin",
            &pattern(7, 3 * 1024 * 1024 + 123),
        )
        .unwrap();
    // Two files appended to in turn cannot stay contiguous.
    volume.create("/Written/fragment-a.bin").unwrap();
    volume.create("/Written/fragment-b.bin").unwrap();
    for round in 0..24u32 {
        for (index, name) in ["/Written/fragment-a.bin", "/Written/fragment-b.bin"]
            .iter()
            .enumerate()
        {
            let chunk = pattern(round * 2 + index as u32, 5000);
            let length = volume
                .reader()
                .lookup(name)
                .unwrap()
                .entry()
                .unwrap()
                .stream()
                .data_length();
            volume.pwrite(name, length, &chunk).unwrap();
        }
    }
    for name in ["/Written/fragment-a.bin", "/Written/fragment-b.bin"] {
        let entry = volume
            .reader()
            .lookup(name)
            .unwrap()
            .entry()
            .unwrap()
            .clone();
        assert!(!entry.stream().no_fat_chain(), "{name} is a FAT chain");
    }
    volume.write_file("/Written/gap.bin", b"head").unwrap();
    volume.pwrite("/Written/gap.bin", 300_000, b"tail").unwrap();
    volume
        .write_file("/Written/replaced.bin", &pattern(9, 100_000))
        .unwrap();
    volume
        .write_file("/Written/replaced.bin", b"second contents")
        .unwrap();
    let given = Timestamps {
        created: Timestamp::from_unix_milliseconds(981_173_106_780, 570).unwrap(),
        modified: Timestamp::from_unix_milliseconds(1_234_567_890_120, -300).unwrap(),
        accessed: Timestamp::from_unix_milliseconds(1_600_000_000_000, 0).unwrap(),
    };
    volume
        .create_with(
            "/Written/dated.txt",
            NewEntry {
                attributes: 0x20,
                times: Some(given),
            },
        )
        .unwrap();
    volume.unmount().unwrap();
    let volume = interface::open_path(target, 0).unwrap();
    let gap = volume
        .lookup("/Written/gap.bin")
        .unwrap()
        .entry()
        .unwrap()
        .clone();
    let mut bytes = vec![0xa5; 300_004];
    volume
        .open_file(&gap)
        .unwrap()
        .read_at(0, &mut bytes)
        .unwrap();
    assert_eq!(&bytes[..4], b"head");
    assert!(bytes[4..300_000].iter().all(|byte| *byte == 0));
    listing(&volume)
}

/// Allocations of many shapes on a volume macOS populated: single clusters,
/// runs that grow in place, runs that must become FAT chains, and chains
/// that shrink back to runs. None of them may touch a cluster macOS
/// allocated, and freeing them all must leave macOS's tree as it was.
fn allocations_leave_the_tree_alone(target: &Path) {
    let target = target.to_path_buf();
    let (before, cluster_count) = bitmap(&target);
    let tree = listing(&interface::open_path(&target, 0).unwrap());

    let mut volume = write::open_path(&target, 0).unwrap();
    let free = volume.free_clusters();
    let mut held: Vec<Allocation> = Vec::new();
    for size in [1, 7, 2, 64, 3, 1, 300] {
        held.push(volume.allocate(size).unwrap());
    }
    for index in (0..held.len()).step_by(2) {
        let allocation = held[index];
        volume.free(allocation).unwrap();
        held[index] = Allocation::EMPTY;
    }
    let mut grown = volume.allocate(2).unwrap();
    volume.extend(&mut grown, 5).unwrap();
    assert!(grown.no_fat_chain(), "a run grows in place");
    let blocker = volume.allocate(1).unwrap();
    for _ in 0..20 {
        volume.extend(&mut grown, 9).unwrap();
    }
    assert!(!grown.no_fat_chain(), "a blocked run becomes a chain");
    volume.shrink(&mut grown, 7).unwrap();
    assert!(
        grown.no_fat_chain(),
        "a chain shrunk to its first run is a run again"
    );
    held.push(grown);
    held.push(blocker);
    for allocation in &held {
        for cluster in volume.clusters(allocation).unwrap() {
            assert!(
                !before[(cluster - 2) as usize],
                "cluster {cluster} was allocated by macOS"
            );
        }
    }
    for allocation in held {
        volume.free(allocation).unwrap();
    }
    assert_eq!(volume.free_clusters(), free);
    volume.unmount().unwrap();

    let (after, _) = bitmap(&target);
    assert_eq!(after, before, "the Allocation Bitmap is as macOS left it");
    let image = std::fs::read(&target).unwrap();
    assert_eq!(u16::from_le_bytes([image[106], image[107]]) & 2, 0);
    let used = before.iter().filter(|bit| **bit).count() as u64;
    assert_eq!(u64::from(image[112]), used * 100 / u64::from(cluster_count));
    let reread = listing(&interface::open_path(&target, 0).unwrap());
    assert_eq!(reread, tree);
}
