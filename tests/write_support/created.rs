#![allow(dead_code)]

use crate::fixture_support::{repo, strict_listing};
use fs_exfat::interface;
use fs_exfat::timestamp::{Timestamp, Timestamps};
use fs_exfat::write::{self, NewEntry};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch_image(name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let scratch = repo().join("tmp");
    std::fs::create_dir_all(&scratch).unwrap();
    let directory = scratch.join(format!(
        "written-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&directory).unwrap();
    directory.join(name)
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

pub fn files_and_directories(target: &Path) {
    let mut volume = write::open_path(target, 0).unwrap();
    let original = strict_listing(volume.reader());
    let cluster = volume.reader().boot().cluster_size() as usize;
    for path in [
        "/Written",
        "/Written/Deep",
        "/Written/Deep/Er",
        "/Written/Many",
        "/Written/Ünïcödé 目录",
    ] {
        volume.mkdir(path).unwrap();
    }
    volume
        .write_file("/Written/ascii.txt", b"plain ASCII contents\n")
        .unwrap();
    volume.create("/Written/empty").unwrap();
    for name in ["grüße-😀.txt", "日本語のファイル名.txt", "ÀÉÎÕÜ àéîõü.bin"]
    {
        volume
            .write_file(&format!("/Written/Ünïcödé 目录/{name}"), name.as_bytes())
            .unwrap();
    }
    let long = format!(
        "/Written/{}",
        "a long name that needs many File Name entries "
            .repeat(5)
            .trim_end()
    );
    volume.write_file(&long, b"long").unwrap();
    let entry_bytes = (2 + "entry 000 with a name of some length.dat"
        .encode_utf16()
        .count()
        .div_ceil(15))
        * 32;
    let entries = 200.max(cluster / entry_bytes + 1);
    for index in 0..entries {
        let length = if index < 200 { index * 37 } else { 0 };
        volume
            .write_file(
                &format!("/Written/Many/entry {index:03} with a name of some length.dat"),
                &pattern(index as u32, length),
            )
            .unwrap();
    }
    assert!(
        volume
            .reader()
            .lookup("/Written/Many")
            .unwrap()
            .entry()
            .unwrap()
            .stream()
            .data_length()
            > cluster as u64,
        "the directory must grow beyond its first cluster"
    );
    volume
        .write_file(
            "/Written/Deep/Er/large.bin",
            &pattern(7, 3 * 1024 * 1024 + 123),
        )
        .unwrap();
    let fragments = ["/Written/fragment-a.bin", "/Written/fragment-b.bin"];
    for path in fragments {
        volume.create(path).unwrap();
    }
    let chunk_length = 5000.max(cluster + 1);
    for round in 0..24u32 {
        for (index, path) in fragments.iter().enumerate() {
            let offset = u64::from(round) * chunk_length as u64;
            let chunk = pattern(round * 2 + index as u32, chunk_length);
            volume.pwrite(path, offset, &chunk).unwrap();
        }
    }
    for path in fragments {
        assert!(
            !volume
                .reader()
                .lookup(path)
                .unwrap()
                .entry()
                .unwrap()
                .stream()
                .no_fat_chain(),
            "{path} must be a FAT chain at every tested cluster size"
        );
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
                attributes: 0x21,
                times: Some(given),
            },
        )
        .unwrap();
    volume
        .mkdir_with(
            "/Written/dated directory",
            NewEntry {
                attributes: 0,
                times: Some(given),
            },
        )
        .unwrap();
    volume.unmount().unwrap();
    let volume = interface::open_path(target, 0).unwrap();
    let after = strict_listing(&volume);
    for (path, row) in original {
        assert_eq!(
            after.get(&path),
            Some(&row),
            "the kernel's seed changed: {path}"
        );
    }
    let gap = volume.lookup("/Written/gap.bin").unwrap();
    let file = volume.open_file(gap.entry().unwrap()).unwrap();
    let mut bytes = vec![0xa5; 300_004];
    assert_eq!(file.read_at(0, &mut bytes).unwrap(), bytes.len());
    assert_eq!(&bytes[..4], b"head");
    assert!(bytes[4..300_000].iter().all(|byte| *byte == 0));
    assert_eq!(&bytes[300_000..], b"tail");
}

#[path = "mod.rs"]
mod synthetic;

#[test]
fn the_guest_tree_producer_grows_directories_and_fragments_both_geometries() {
    for (sector_shift, cluster_shift) in [(9, 3), (12, 5)] {
        let (bytes, _) = synthetic::format(&synthetic::Layout {
            sector_shift,
            cluster_shift,
            bytes: 64 * 1024 * 1024,
            fats: 1,
        });
        let target = scratch_image("producer.img");
        std::fs::write(&target, bytes).unwrap();
        files_and_directories(&target);
        let volume = interface::open_path(&target, 0).unwrap();
        let found = volume.lookup("/Written/Deep/Er/large.bin").unwrap();
        let file = volume.open_file(found.entry().unwrap()).unwrap();
        let mut bytes = vec![0; file.len() as usize];
        assert_eq!(file.read_at(0, &mut bytes).unwrap(), bytes.len());
        assert_eq!(bytes, pattern(7, 3 * 1024 * 1024 + 123));
        let found = volume.lookup("/Written/dated.txt").unwrap();
        let entry = found.entry().unwrap();
        assert_eq!(entry.attributes() & 0x21, 0x21);
        assert_eq!(
            entry.timestamps().created.unix_milliseconds(),
            Some(981_173_106_780)
        );
    }
}
