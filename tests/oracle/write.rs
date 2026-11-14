//! Producer for the write oracle. It copies a volume macOS formatted and
//! populated, changes it through `fs_exfat::write`, and records the tree
//! this crate's reader then sees. `scripts/test-oracle.sh` checks the result
//! with `fsck_exfat -n` and compares the record with what the read-only
//! mounted macOS driver lists.

#[path = "../support/image.rs"]
mod image_support;

mod listing;

use fs_exfat::interface;
use fs_exfat::timestamp::{Timestamp, Timestamps};
use fs_exfat::write::{self, Allocation, NewEntry, SetTimes};
use listing::listing;
use std::path::{Path, PathBuf};

fn paths() -> (PathBuf, String) {
    let directory = PathBuf::from(
        std::env::var("EXFAT_ORACLE_DIR").expect("run chore test:oracle-macos on macOS"),
    );
    let sector =
        std::env::var("EXFAT_ORACLE_SECTOR").expect("run chore test:oracle-macos on macOS");
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

#[path = "../write_support/kinds.rs"]
mod kinds;

/// #185: one clean, unmounted checkpoint after each kind of write, on a
/// fresh copy of the volume macOS formatted and populated. Each checkpoint
/// is saved named for its kind, and `scripts/test-oracle.sh` gives every one
/// listed in `checkpoints-<sector>.txt` to `fsck_exfat -n` on its own.
#[test]
fn every_kind_of_write_leaves_a_clean_checkpoint() {
    let (directory, sector) = paths();
    let image = directory.join(format!("kinds-{sector}.dmg"));
    std::fs::copy(directory.join(format!("populated-{sector}.dmg")), &image).unwrap();
    let mut listed = String::new();
    kinds::run(&image, |_, kind| {
        let checkpoint = directory.join(format!("checkpoint-{sector}-{kind}.dmg"));
        kinds::save_checkpoint(&image, &checkpoint);
        listed.push_str(kind);
        listed.push('\n');
    });
    std::fs::write(directory.join(format!("checkpoints-{sector}.txt")), listed).unwrap();
}

/// One test, because every step changes the same image in turn.
#[test]
fn the_writer_changes_a_volume_macos_populated() {
    let (directory, sector) = paths();
    let source = directory.join(format!("populated-{sector}.dmg"));
    let target = directory.join(format!("written-{sector}.dmg"));
    std::fs::copy(&source, &target).unwrap();
    allocations_leave_the_tree_alone(&target);
    files_and_directories(&target);
    truncations_and_removals(&target);
    let label = "Écrit 2026";
    let tree = renames_times_attributes_and_label(&target, label);
    std::fs::write(directory.join(format!("written-label-{sector}.txt")), label).unwrap();
    std::fs::write(
        directory.join(format!("written-expected-{sector}.txt")),
        tree,
    )
    .unwrap();
}

/// Truncation and removal of files macOS made and files this crate made:
/// shrinking to an unaligned length, growing past an unaligned valid
/// length, a fragmented file shrunk back to a run, and removed files and
/// directories, named through the volume's case mapping.
fn truncations_and_removals(target: &Path) {
    let mut volume = write::open_path(target, 0).unwrap();
    let free = volume.free_clusters();
    volume.truncate("/contiguous.bin", 100_001).unwrap();
    volume.truncate("/item-005.txt", 50_000).unwrap();
    volume.truncate("/valid-tail.bin", 20_000).unwrap();
    volume.truncate("/Written/fragment-a.bin", 7000).unwrap();
    volume.truncate("/Written/gap.bin", 0).unwrap();
    let entry = volume
        .reader()
        .lookup("/Written/fragment-a.bin")
        .unwrap()
        .entry()
        .unwrap()
        .clone();
    assert!(
        entry.stream().no_fat_chain(),
        "the first two clusters are a run"
    );
    for index in 10..20 {
        volume.unlink(&format!("/ITEM-{index:03}.TXT")).unwrap();
    }
    volume.unlink("/blocker.bin").unwrap();
    volume.unlink("/GRÜßE-😀.TXT").unwrap();
    volume.unlink("/Written/replaced.bin").unwrap();
    assert!(matches!(
        volume.rmdir("/Written/Deep/Er"),
        Err(fs_exfat::Error::NotEmpty)
    ));
    volume.unlink("/Written/Deep/Er/large.bin").unwrap();
    volume.rmdir("/Written/Deep/Er").unwrap();
    volume.rmdir("/Empty").unwrap();
    assert!(volume.free_clusters() > free);
    volume.unmount().unwrap();
    let volume = interface::open_path(target, 0).unwrap();
    for (path, length) in [("/item-005.txt", 50_000), ("/valid-tail.bin", 20_000)] {
        let entry = volume.lookup(path).unwrap().entry().unwrap().clone();
        let valid = entry.stream().valid_data_length() as usize;
        let mut bytes = vec![0xa5; length];
        volume
            .open_file(&entry)
            .unwrap()
            .read_at(0, &mut bytes)
            .unwrap();
        assert!(
            bytes[valid..].iter().all(|byte| *byte == 0),
            "{path} reads zeros past its valid length"
        );
    }
}

/// Renames and moves between macOS's directories and this crate's, a case
/// change in place, replacements, times with UTC offsets, attributes on files
/// and a directory, and a non-ASCII label. Every directory whose entries
/// change records it in its own times (#151), so the listing compares them
/// with the driver's too. Returns the tree as this crate's reader sees it
/// after the unmount.
fn renames_times_attributes_and_label(target: &Path, label: &str) -> String {
    let mut volume = write::open_path(target, 0).unwrap();
    let modified = |volume: &write::WriteVolume<_>, path: &str| {
        volume
            .reader()
            .lookup(path)
            .unwrap()
            .entry()
            .unwrap()
            .timestamps()
            .modified
    };
    let nested = modified(&volume, "/Nested");
    volume.rename("/MixedCase.txt", "/MIXEDCASE.TXT").unwrap();
    volume
        .rename("/ascii.txt", "/Written/moved from macOS.txt")
        .unwrap();
    volume
        .rename("/Written/Deep", "/Nested/Deep moved here")
        .unwrap();
    volume
        .rename("/Written/ascii.txt", "/timestamp.bin")
        .unwrap();
    volume
        .rename(
            "/item-020.txt",
            "/Written/Many/entry 000 with a name of some length.dat",
        )
        .unwrap();
    volume
        .rename("/Written/Ünïcödé 目录", "/Written/Ünïcödé 目录 renamed")
        .unwrap();
    let time =
        |milliseconds, offset| Timestamp::from_unix_milliseconds(milliseconds, offset).unwrap();
    volume
        .set_times(
            "/item-030.txt",
            SetTimes {
                created: Some(time(1_111_111_111_110, 600)),
                modified: Some(time(1_222_222_222_220, -210)),
                accessed: Some(time(1_333_333_334_000, 0)),
            },
        )
        .unwrap();
    volume
        .set_times(
            "/Written/dated.txt",
            SetTimes {
                modified: Some(time(1_444_444_444_440, 45)),
                ..SetTimes::default()
            },
        )
        .unwrap();
    volume.set_attributes("/item-031.txt", 0x22).unwrap();
    volume.set_attributes("/item-032.txt", 0x21).unwrap();
    volume.set_attributes("/item-033.txt", 0).unwrap();
    volume.set_attributes("/Written/empty", 0x03).unwrap();
    assert_ne!(
        modified(&volume, "/Nested"),
        nested,
        "a directory macOS made records the entry moved into it"
    );
    volume.set_attributes("/Nested", 0x02).unwrap();
    volume.set_label(label).unwrap();
    volume.unmount().unwrap();
    let volume = interface::open_path(target, 0).unwrap();
    assert_eq!(volume.metadata().label.as_deref(), Some(label));
    listing(&volume)
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
/// given with hundredths and UTC offsets.
fn files_and_directories(target: &Path) {
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
    let image = image_support::Image::open(&target);
    assert_eq!(
        u16::from_le_bytes([image.byte(106), image.byte(107)]) & 2,
        0
    );
    let used = before.iter().filter(|bit| **bit).count() as u64;
    assert_eq!(
        u64::from(image.byte(112)),
        used * 100 / u64::from(cluster_count)
    );
    let reread = listing(&interface::open_path(&target, 0).unwrap());
    assert_eq!(reread, tree);
}
