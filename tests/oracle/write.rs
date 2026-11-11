//! Producer for the write oracle. It copies a volume macOS formatted and
//! populated, changes it through `fs_exfat::write`, and records the tree
//! this crate's reader then sees. `scripts/test-oracle.sh` checks the result
//! with `fsck_exfat -n` and compares the record with what the read-only
//! mounted macOS driver lists.

use fs_exfat::interface;
use fs_exfat::volume::Volume;
use fs_exfat::write::{self, Allocation};
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
            records.push(format!(
                "f\t{}\t{:x}\t{path}\n",
                file.len(),
                hash.finalize()
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

/// Allocations of many shapes on a volume macOS populated: single clusters,
/// runs that grow in place, runs that must become FAT chains, and chains
/// that shrink back to runs. None of them may touch a cluster macOS
/// allocated, and freeing them all must leave macOS's tree as it was.
#[test]
fn the_writer_allocates_and_frees_around_a_volume_macos_populated() {
    let (directory, sector) = paths();
    let source = directory.join(format!("populated-{sector}.dmg"));
    let target = directory.join(format!("written-{sector}.dmg"));
    std::fs::copy(&source, &target).unwrap();
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
    std::fs::write(
        directory.join(format!("written-expected-{sector}.txt")),
        reread,
    )
    .unwrap();
}
