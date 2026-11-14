#![allow(dead_code)]

//! One clean, unmounted checkpoint after each kind of write (#185).
//!
//! Every kind runs on its own mount of a fresh image: open, change, unmount.
//! The caller then gives the unmounted image to a checker that is not this
//! crate (exfatprogs `fsck.exfat -n` in the Linux guest, `fsck_exfat -n` on a
//! macOS host), so a rejection names the one kind of write that preceded it.
//! The kinds run in order on the same image, because later kinds change what
//! earlier ones made; every checkpoint is still a whole clean volume.
//!
//! Everything this module makes lives under `/Kinds`, so whatever the image
//! held before (the kernel's sentinel, macOS's populated tree) must still
//! read back unchanged at the end, which [`run`] checks itself.

use crate::image_support;

use fs_core::OwnedRwSlice;
use fs_exfat::interface;
use fs_exfat::timestamp::Timestamp;
use fs_exfat::write::{self, Allocation, NewEntry, SetTimes, WriteVolume};
use std::path::Path;

type Writer = WriteVolume<OwnedRwSlice>;

/// A kind of write and the change that exercises it.
pub struct Kind {
    pub name: &'static str,
    change: fn(&mut Writer),
}

/// Every kind the writer supports, in the order they run. Allocation and
/// creation come first: they are checked by earlier layers too, and the
/// later kinds change what creation made.
pub const KINDS: [Kind; 15] = [
    Kind {
        name: "allocation",
        change: allocation,
    },
    Kind {
        name: "creation",
        change: creation,
    },
    Kind {
        name: "truncate-grow",
        change: truncate_grow,
    },
    Kind {
        name: "truncate-shrink",
        change: truncate_shrink,
    },
    Kind {
        name: "truncate-zero",
        change: truncate_zero,
    },
    Kind {
        name: "truncate-fragmented",
        change: truncate_fragmented,
    },
    Kind {
        name: "unlink",
        change: unlink,
    },
    Kind {
        name: "rmdir",
        change: rmdir,
    },
    Kind {
        name: "rename-case-only",
        change: rename_case_only,
    },
    Kind {
        name: "rename-cross-directory",
        change: rename_cross_directory,
    },
    Kind {
        name: "rename-replacement",
        change: rename_replacement,
    },
    Kind {
        name: "timestamps",
        change: timestamps,
    },
    Kind {
        name: "attributes",
        change: attributes,
    },
    Kind {
        name: "label-set",
        change: label_set,
    },
    Kind {
        name: "label-clear",
        change: label_clear,
    },
];

/// The label `label-set` writes: non-ASCII, the full 11 UTF-16 units.
pub const LABEL: &str = "Écrit 2026é";

/// Run every kind on `image`, one mount each, and call `checkpoint` with the
/// kind's position and name after each unmount. The tree that was on the
/// image before must be unchanged at the end.
pub fn run(image: &Path, mut checkpoint: impl FnMut(usize, &str)) {
    let before = outside_kinds(image);
    for (index, kind) in KINDS.iter().enumerate() {
        let mut volume = write::open_path(image, 0)
            .unwrap_or_else(|error| panic!("mount for {}: {error}", kind.name));
        (kind.change)(&mut volume);
        volume
            .unmount()
            .unwrap_or_else(|error| panic!("unmount after {}: {error}", kind.name));
        let flags = image_support::Image::open(image);
        assert_eq!(
            u16::from_le_bytes([flags.byte(106), flags.byte(107)]) & 2,
            0,
            "VolumeDirty is still set after {}",
            kind.name
        );
        interface::open_path(image, 0)
            .unwrap_or_else(|error| panic!("strict reader after {}: {error}", kind.name));
        checkpoint(index, kind.name);
    }
    assert_eq!(
        outside_kinds(image),
        before,
        "the tree outside /Kinds changed"
    );
    let volume = interface::open_path(image, 0).unwrap();
    assert_eq!(volume.metadata().label.as_deref().unwrap_or(""), "");
}

/// Every path outside `/Kinds` with its length and content.
fn outside_kinds(image: &Path) -> Vec<(String, Vec<u8>)> {
    let volume = interface::open_path(image, 0).unwrap();
    let mut found = Vec::new();
    let mut pending = vec![(String::new(), volume.root_directory().unwrap())];
    while let Some((prefix, entries)) = pending.pop() {
        for entry in entries {
            let path = format!("{prefix}/{}", entry.name());
            if path == "/Kinds" {
                continue;
            }
            if entry.is_directory() {
                found.push((path.clone(), Vec::new()));
                pending.push((path, volume.read_directory(&entry).unwrap()));
                continue;
            }
            let file = volume.open_file(&entry).unwrap();
            let mut bytes = vec![0; file.len() as usize];
            assert_eq!(file.read_at(0, &mut bytes).unwrap(), bytes.len());
            found.push((path, bytes));
        }
    }
    found.sort();
    found
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

fn cluster(volume: &Writer) -> u64 {
    volume.reader().boot().cluster_size()
}

fn length(volume: &Writer, path: &str) -> u64 {
    stream(volume, path).0
}

/// DataLength and NoFatChain of a path's stream.
fn stream(volume: &Writer, path: &str) -> (u64, bool) {
    let found = volume
        .reader()
        .lookup(path)
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    let entry = found.entry().unwrap();
    (entry.stream().data_length(), entry.stream().no_fat_chain())
}

fn exists(volume: &Writer, path: &str) -> bool {
    volume.reader().lookup(path).is_ok()
}

fn read(volume: &Writer, path: &str) -> Vec<u8> {
    let found = volume.reader().lookup(path).unwrap();
    let entry = found.entry().unwrap();
    let file = volume.reader().open_file(entry).unwrap();
    let mut bytes = vec![0; file.len() as usize];
    assert_eq!(file.read_at(0, &mut bytes).unwrap(), bytes.len());
    bytes
}

/// Allocate, grow in place, convert to a FAT chain, shrink back to a run
/// and free: no entry changes, and the heap ends as it started.
fn allocation(volume: &mut Writer) {
    let free = volume.free_clusters();
    let mut run = volume.allocate(2).unwrap();
    volume.extend(&mut run, 3).unwrap();
    assert!(run.no_fat_chain(), "a run grows in place");
    let blocker = volume.allocate(1).unwrap();
    volume.extend(&mut run, 2).unwrap();
    assert!(!run.no_fat_chain(), "a blocked run becomes a FAT chain");
    volume.shrink(&mut run, 4).unwrap();
    assert!(
        run.no_fat_chain(),
        "a chain shrunk to its run is a run again"
    );
    let mut held: Vec<Allocation> = vec![run, blocker];
    held.push(volume.allocate(9).unwrap());
    for allocation in held {
        volume.free(allocation).unwrap();
    }
    assert_eq!(volume.free_clusters(), free);
}

/// The files and directories the later kinds change.
fn creation(volume: &mut Writer) {
    let cluster = cluster(volume) as usize;
    for path in [
        "/Kinds",
        "/Kinds/From",
        "/Kinds/From/Moved directory",
        "/Kinds/To",
        "/Kinds/Empty",
        "/Kinds/Nested",
        "/Kinds/Nested/Inner",
        "/Kinds/Replaced directory",
        "/Kinds/Replacing directory",
    ] {
        volume.mkdir(path).unwrap();
    }
    volume
        .write_file("/Kinds/grow.bin", &pattern(1, cluster / 2 + 7))
        .unwrap();
    volume
        .write_file("/Kinds/shrink.bin", &pattern(2, cluster * 5 + 11))
        .unwrap();
    volume
        .write_file("/Kinds/zero.bin", &pattern(3, cluster * 3))
        .unwrap();
    volume
        .write_file("/Kinds/unlinked.bin", &pattern(4, cluster * 2 + 1))
        .unwrap();
    volume.create("/Kinds/unlinked empty").unwrap();
    volume
        .write_file("/Kinds/Ünïcödé gone.txt", "über".as_bytes())
        .unwrap();
    volume
        .write_file("/Kinds/MixedCase.txt", b"case only\n")
        .unwrap();
    volume
        .write_file("/Kinds/From/moved.txt", b"moved across\n")
        .unwrap();
    volume
        .write_file("/Kinds/From/Moved directory/inside.bin", &pattern(5, 999))
        .unwrap();
    volume
        .write_file("/Kinds/replacing.bin", &pattern(6, cluster + 3))
        .unwrap();
    volume
        .write_file("/Kinds/replaced.bin", &pattern(7, cluster * 4))
        .unwrap();
    volume
        .create_with(
            "/Kinds/dated.txt",
            NewEntry {
                attributes: 0x20,
                times: None,
            },
        )
        .unwrap();
    volume.write_file("/Kinds/flags.txt", b"flags\n").unwrap();
    // Two files appended to in turn cannot stay contiguous.
    let fragments = ["/Kinds/fragment-a.bin", "/Kinds/fragment-b.bin"];
    for path in fragments {
        volume.create(path).unwrap();
    }
    for round in 0..8u32 {
        for (index, path) in fragments.iter().enumerate() {
            let offset = length(volume, path);
            let chunk = pattern(round * 2 + index as u32 + 10, cluster + 1);
            volume.pwrite(path, offset, &chunk).unwrap();
        }
    }
    for path in fragments {
        assert!(!stream(volume, path).1, "{path} must be a FAT chain");
    }
}

/// Unaligned growth past several cluster boundaries: the tail reads as
/// zeros while the original bytes stay.
fn truncate_grow(volume: &mut Writer) {
    let cluster = cluster(volume);
    let before = read(volume, "/Kinds/grow.bin");
    volume
        .truncate("/Kinds/grow.bin", cluster * 3 + 513)
        .unwrap();
    let after = read(volume, "/Kinds/grow.bin");
    assert_eq!(after[..before.len()], before[..]);
    assert!(after[before.len()..].iter().all(|byte| *byte == 0));
}

/// Unaligned shrinking frees the clusters past the new end.
fn truncate_shrink(volume: &mut Writer) {
    let cluster = cluster(volume);
    let free = volume.free_clusters();
    volume
        .truncate("/Kinds/shrink.bin", cluster * 2 - 5)
        .unwrap();
    assert_eq!(volume.free_clusters(), free + 4);
    assert_eq!(
        read(volume, "/Kinds/shrink.bin"),
        pattern(2, cluster as usize * 5 + 11)[..cluster as usize * 2 - 5]
    );
}

/// A file truncated to nothing keeps its entry and holds no cluster.
fn truncate_zero(volume: &mut Writer) {
    let free = volume.free_clusters();
    volume.truncate("/Kinds/zero.bin", 0).unwrap();
    assert_eq!(volume.free_clusters(), free + 3);
    assert_eq!(length(volume, "/Kinds/zero.bin"), 0);
}

/// A fragmented chain shrunk into its first run becomes NoFatChain; the
/// other is cut mid-chain and stays a chain, then grows again.
fn truncate_fragmented(volume: &mut Writer) {
    let cluster = cluster(volume);
    volume
        .truncate("/Kinds/fragment-a.bin", cluster + 100)
        .unwrap();
    assert!(stream(volume, "/Kinds/fragment-a.bin").1, "back to a run");
    let length = length(volume, "/Kinds/fragment-b.bin");
    volume
        .truncate("/Kinds/fragment-b.bin", length - cluster * 3 - 17)
        .unwrap();
    assert!(!stream(volume, "/Kinds/fragment-b.bin").1, "still a chain");
    volume
        .truncate("/Kinds/fragment-b.bin", length + cluster * 2)
        .unwrap();
}

/// Files with clusters, an empty file and a non-ASCII name looked up
/// through the volume's case mapping.
fn unlink(volume: &mut Writer) {
    let free = volume.free_clusters();
    volume.unlink("/Kinds/unlinked.bin").unwrap();
    volume.unlink("/Kinds/unlinked empty").unwrap();
    volume.unlink("/KINDS/ÜNÏCÖDÉ GONE.TXT").unwrap();
    assert_eq!(volume.free_clusters(), free + 4);
    for path in [
        "/Kinds/unlinked.bin",
        "/Kinds/unlinked empty",
        "/Kinds/Ünïcödé gone.txt",
    ] {
        assert!(!exists(volume, path), "{path} is gone");
    }
}

/// Empty directories, a nested one emptied first; a full one is refused.
fn rmdir(volume: &mut Writer) {
    assert!(matches!(
        volume.rmdir("/Kinds/Nested"),
        Err(fs_exfat::Error::NotEmpty)
    ));
    volume.rmdir("/Kinds/Nested/Inner").unwrap();
    volume.rmdir("/Kinds/Nested").unwrap();
    volume.rmdir("/Kinds/Empty").unwrap();
    assert!(!exists(volume, "/Kinds/Nested"));
    assert!(!exists(volume, "/Kinds/Empty"));
}

/// A file and a directory renamed in place to a different case.
fn rename_case_only(volume: &mut Writer) {
    volume
        .rename("/Kinds/MixedCase.txt", "/Kinds/MIXEDCASE.TXT")
        .unwrap();
    volume.rename("/Kinds/To", "/Kinds/TO").unwrap();
    let names: Vec<String> = volume
        .reader()
        .read_directory(volume.reader().lookup("/Kinds").unwrap().entry().unwrap())
        .unwrap()
        .iter()
        .map(|entry| entry.name().to_owned())
        .collect();
    assert!(names.iter().any(|name| name == "MIXEDCASE.TXT"));
    assert!(names.iter().any(|name| name == "TO"));
}

/// A file and a directory with contents moved to another directory.
fn rename_cross_directory(volume: &mut Writer) {
    volume
        .rename("/Kinds/From/moved.txt", "/Kinds/TO/moved here.txt")
        .unwrap();
    volume
        .rename("/Kinds/From/Moved directory", "/Kinds/TO/Moved directory")
        .unwrap();
    assert_eq!(read(volume, "/Kinds/TO/moved here.txt"), b"moved across\n");
    assert_eq!(
        read(volume, "/Kinds/TO/Moved directory/inside.bin"),
        pattern(5, 999)
    );
    assert!(!exists(volume, "/Kinds/From/moved.txt"));
}

/// A file replaces a file, freeing the replaced clusters, and a directory
/// replaces an empty directory.
fn rename_replacement(volume: &mut Writer) {
    let cluster = cluster(volume) as usize;
    let free = volume.free_clusters();
    volume
        .rename("/Kinds/replacing.bin", "/Kinds/replaced.bin")
        .unwrap();
    assert_eq!(volume.free_clusters(), free + 4);
    assert_eq!(read(volume, "/Kinds/replaced.bin"), pattern(6, cluster + 3));
    volume
        .rename("/Kinds/Replacing directory", "/Kinds/Replaced directory")
        .unwrap();
    assert!(!exists(volume, "/Kinds/replacing.bin"));
    assert!(!exists(volume, "/Kinds/Replacing directory"));
}

/// All three times with hundredths and UTC offsets on a file and a
/// directory, and one time alone.
fn timestamps(volume: &mut Writer) {
    let time =
        |milliseconds, offset| Timestamp::from_unix_milliseconds(milliseconds, offset).unwrap();
    let all = SetTimes {
        created: Some(time(1_111_111_111_110, 600)),
        modified: Some(time(1_222_222_222_220, -210)),
        accessed: Some(time(1_333_333_334_000, 0)),
    };
    volume.set_times("/Kinds/dated.txt", all).unwrap();
    volume.set_times("/Kinds/TO", all).unwrap();
    volume
        .set_times(
            "/Kinds/grow.bin",
            SetTimes {
                modified: Some(time(1_444_444_444_440, 45)),
                ..SetTimes::default()
            },
        )
        .unwrap();
    let found = volume.reader().lookup("/Kinds/dated.txt").unwrap();
    let times = found.entry().unwrap().timestamps();
    assert_eq!(times.created.unix_milliseconds(), Some(1_111_111_111_110));
    assert_eq!(times.modified.unix_milliseconds(), Some(1_222_222_222_220));
}

/// ReadOnly, Hidden, System and Archive set and cleared on files and a
/// directory.
fn attributes(volume: &mut Writer) {
    for (path, value) in [
        ("/Kinds/flags.txt", 0x21),
        ("/Kinds/dated.txt", 0x06),
        ("/Kinds/MIXEDCASE.TXT", 0),
        ("/Kinds/TO", 0x02),
    ] {
        volume.set_attributes(path, value).unwrap();
        let found = volume.reader().lookup(path).unwrap();
        assert_eq!(found.entry().unwrap().attributes() & 0x27, value, "{path}");
    }
}

fn label_set(volume: &mut Writer) {
    volume.set_label(LABEL).unwrap();
    assert_eq!(volume.reader().metadata().label.as_deref(), Some(LABEL));
}

/// An empty Volume Label entry, as macOS's formatter writes it.
fn label_clear(volume: &mut Writer) {
    volume.set_label("").unwrap();
    assert_eq!(
        volume.reader().metadata().label.as_deref().unwrap_or(""),
        ""
    );
}

/// Save the unmounted image as a checkpoint at `destination`, keeping only
/// the blocks that hold data so that a checkpoint per kind stays cheap.
pub fn save_checkpoint(image: &Path, destination: &Path) {
    use std::io::{Read, Seek, SeekFrom, Write};
    const BLOCK: usize = 64 * 1024;
    let mut source = std::fs::File::open(image).unwrap();
    let mut output = std::fs::File::create_new(destination)
        .unwrap_or_else(|error| panic!("{}: {error}", destination.display()));
    let length = source.metadata().unwrap().len();
    let mut buffer = vec![0; BLOCK];
    let mut offset = 0;
    while offset < length {
        let count = (length - offset).min(BLOCK as u64) as usize;
        source.read_exact(&mut buffer[..count]).unwrap();
        if buffer[..count].iter().any(|byte| *byte != 0) {
            output.seek(SeekFrom::Start(offset)).unwrap();
            output.write_all(&buffer[..count]).unwrap();
        }
        offset += count as u64;
    }
    output.set_len(length).unwrap();
    output.sync_all().unwrap();
}
