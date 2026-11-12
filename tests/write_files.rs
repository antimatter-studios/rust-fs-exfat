mod write_support;

use fs_core::BlockRead;
use fs_exfat::directory::DirectoryEntry;
use fs_exfat::path::Node;
use fs_exfat::timestamp::{Timestamp, Timestamps};
use fs_exfat::volume::Volume;
use fs_exfat::write::{NewEntry, WriteVolume};
use fs_exfat::Error;
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use write_support::{format, Layout, Memory};

fn mounted(layout: &Layout) -> (WriteVolume<Memory>, Memory) {
    let (image, _) = format(layout);
    let device = Memory::new(image);
    (WriteVolume::mount(device.clone()).unwrap(), device)
}

fn entry<Device: BlockRead>(volume: &Volume<Device>, path: &str) -> DirectoryEntry {
    match volume.lookup(path).unwrap() {
        Node::Entry(entry) => entry,
        Node::Root => panic!("{path} is the root"),
    }
}

fn contents<Device: BlockRead>(volume: &Volume<Device>, path: &str) -> Vec<u8> {
    let entry = entry(volume, path);
    let file = volume.open_file(&entry).unwrap();
    let mut output = vec![0xa5; file.len() as usize];
    assert_eq!(file.read_at(0, &mut output).unwrap(), output.len());
    output
}

/// Every path and its bytes, or None for a directory, as the strict reader
/// sees the volume after it has been unmounted and opened afresh.
fn tree(image: Vec<u8>) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk<Device: BlockRead>(
        volume: &Volume<Device>,
        prefix: &str,
        entries: Vec<DirectoryEntry>,
        out: &mut BTreeMap<String, Option<Vec<u8>>>,
    ) {
        for entry in entries {
            let path = format!("{prefix}/{}", entry.name());
            if entry.is_directory() {
                out.insert(path.clone(), None);
                walk(volume, &path, volume.read_directory(&entry).unwrap(), out);
            } else {
                out.insert(path.clone(), Some(contents(volume, &path)));
            }
        }
    }
    let volume = Volume::open(Memory::new(image)).unwrap();
    let mut out = BTreeMap::new();
    walk(&volume, "", volume.root_directory().unwrap(), &mut out);
    out
}

fn time(milliseconds: i64, offset: i16) -> Timestamp {
    Timestamp::from_unix_milliseconds(milliseconds, offset).unwrap()
}

#[test]
fn a_created_file_has_a_valid_set_name_hash_and_current_times() {
    let (mut volume, device) = mounted(&Layout::default());
    let before = Timestamp::now().unwrap().unix_milliseconds().unwrap();
    volume.create("/Notes.txt").unwrap();
    let found = entry(volume.reader(), "/notes.TXT");
    assert_eq!(found.name(), "Notes.txt");
    assert_eq!(found.stream().data_length(), 0);
    assert_eq!(found.stream().first_cluster(), 0);
    assert_eq!(found.attributes(), 0x20);
    let created = found.timestamps().created.unix_milliseconds().unwrap();
    assert!(created >= before / 10 * 10 && created <= before + 5000);
    assert_eq!(found.timestamps().created.utc_offset_minutes, Some(0));
    volume.unmount().unwrap();
    assert_eq!(
        tree(device.snapshot()).get("/Notes.txt"),
        Some(&Some(Vec::new()))
    );
}

#[test]
fn names_that_collide_after_up_casing_or_break_the_rules_are_refused() {
    let (mut volume, _) = mounted(&Layout::default());
    volume.create("/README.txt").unwrap();
    assert!(matches!(volume.create("/readme.TXT"), Err(Error::Exists)));
    assert!(matches!(volume.mkdir("/ReadMe.Txt"), Err(Error::Exists)));
    for name in [
        "/a:b", "/a*b", "/a\u{1}b", "/a\\b", "/a|b", "/a<b", "/a\"b", "/a?b", "/a>b",
    ] {
        assert!(
            matches!(volume.create(name), Err(Error::Invalid(_))),
            "{name}"
        );
    }
    assert!(matches!(
        volume.create(&format!("/{}", "x".repeat(256))),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(volume.create("/"), Err(Error::Invalid(_))));
    assert!(matches!(volume.create("/./a"), Err(Error::Invalid(_))));
    assert!(matches!(volume.create("/missing/a"), Err(Error::NotFound)));
    assert!(matches!(
        volume.create("/README.txt/a"),
        Err(Error::NotDirectory)
    ));
    assert!(matches!(
        volume.create_with(
            "/x",
            NewEntry {
                attributes: 0x10,
                times: None
            }
        ),
        Err(Error::Invalid(_))
    ));
    let long = "é".repeat(255);
    volume.create(&format!("/{long}")).unwrap();
    assert_eq!(entry(volume.reader(), &format!("/{long}")).name(), long);
    volume.unmount().unwrap();
}

#[test]
fn directories_nest_and_grow_past_one_cluster() {
    for layout in [
        Layout::default(),
        Layout {
            sector_shift: 12,
            cluster_shift: 0,
            ..Layout::default()
        },
        Layout {
            cluster_shift: 0,
            ..Layout::default()
        },
    ] {
        let (mut volume, device) = mounted(&layout);
        volume.mkdir("/Photos").unwrap();
        volume.mkdir("/Photos/2026").unwrap();
        let mut expected = BTreeMap::new();
        expected.insert("/Photos".to_string(), None);
        expected.insert("/Photos/2026".to_string(), None);
        for index in 0..150 {
            let root = format!("/root file number {index:03}.bin");
            let nested = format!("/Photos/2026/nested file number {index:03}.jpg");
            volume.create(&root).unwrap();
            volume.write_file(&nested, &[index as u8; 3]).unwrap();
            expected.insert(root, Some(Vec::new()));
            expected.insert(nested, Some(vec![index as u8; 3]));
        }
        let nested = entry(volume.reader(), "/Photos/2026");
        assert!(nested.stream().data_length() > 4096);
        assert_eq!(
            nested.stream().valid_data_length(),
            nested.stream().data_length()
        );
        assert!(volume.reader().root_directory().unwrap().len() > 100);
        volume.unmount().unwrap();
        assert_eq!(tree(device.snapshot()), expected);
    }
}

#[test]
fn file_data_grows_with_zeros_in_gaps_and_overwrites_in_place() {
    let (mut volume, device) = mounted(&Layout::default());
    volume.create("/data.bin").unwrap();
    volume.pwrite("/data.bin", 0, b"hello").unwrap();
    volume.pwrite("/data.bin", 10_000, b"tail").unwrap();
    volume.pwrite("/data.bin", 1, b"EL").unwrap();
    let mut expected = vec![0u8; 10_004];
    expected[..5].copy_from_slice(b"hELlo");
    expected[10_000..].copy_from_slice(b"tail");
    assert_eq!(contents(volume.reader(), "/data.bin"), expected);
    let found = entry(volume.reader(), "/data.bin");
    assert_eq!(found.stream().data_length(), 10_004);
    assert_eq!(found.stream().valid_data_length(), 10_004);
    assert!(found.stream().no_fat_chain());
    volume.pwrite("/data.bin", 10_004, &[]).unwrap();
    assert!(matches!(
        volume.pwrite("/missing", 0, b"x"),
        Err(Error::NotFound)
    ));
    volume.mkdir("/dir").unwrap();
    assert!(matches!(
        volume.pwrite("/dir", 0, b"x"),
        Err(Error::IsDirectory)
    ));
    assert!(matches!(
        volume.write_file("/dir", b"x"),
        Err(Error::IsDirectory)
    ));
    volume.unmount().unwrap();
    assert_eq!(tree(device.snapshot())["/data.bin"], Some(expected));
}

#[test]
fn a_file_blocked_from_growing_in_place_becomes_a_fat_chain() {
    let (mut volume, device) = mounted(&Layout::default());
    let first: Vec<u8> = (0..5000u32).map(|index| index as u8).collect();
    volume.write_file("/a.bin", &first).unwrap();
    volume.write_file("/blocker.bin", b"block").unwrap();
    let more: Vec<u8> = (0..20_000u32).map(|index| (index * 7) as u8).collect();
    volume.pwrite("/a.bin", first.len() as u64, &more).unwrap();
    let found = entry(volume.reader(), "/a.bin");
    assert!(!found.stream().no_fat_chain());
    let mut expected = first.clone();
    expected.extend_from_slice(&more);
    assert_eq!(contents(volume.reader(), "/a.bin"), expected);
    let free = volume.free_clusters();
    volume.write_file("/a.bin", b"short").unwrap();
    assert_eq!(
        volume.free_clusters(),
        free + 6,
        "replacing frees the old clusters"
    );
    assert!(entry(volume.reader(), "/a.bin").stream().no_fat_chain());
    volume.unmount().unwrap();
    let tree = tree(device.snapshot());
    assert_eq!(tree["/a.bin"], Some(b"short".to_vec()));
    assert_eq!(tree["/blocker.bin"], Some(b"block".to_vec()));
}

#[test]
fn given_times_and_attributes_are_recorded_with_hundredths_and_offsets() {
    let (mut volume, _) = mounted(&Layout::default());
    let times = Timestamps {
        created: time(1_700_000_001_230, 60),
        modified: time(1_700_000_002_990, -330),
        accessed: time(1_700_000_004_000, 0),
    };
    volume
        .create_with(
            "/kept.txt",
            NewEntry {
                attributes: 0x01 | 0x02 | 0x04,
                times: Some(times),
            },
        )
        .unwrap();
    volume
        .mkdir_with(
            "/hidden",
            NewEntry {
                attributes: 0x02,
                times: Some(times),
            },
        )
        .unwrap();
    let file = entry(volume.reader(), "/kept.txt");
    assert_eq!(file.attributes(), 0x07);
    assert_eq!(file.timestamps().created, times.created);
    assert_eq!(file.timestamps().modified, times.modified);
    assert_eq!(file.timestamps().created.millisecond, 230);
    assert_eq!(file.timestamps().modified.utc_offset_minutes, Some(-330));
    let accessed = file.timestamps().accessed;
    assert_eq!(accessed.unix_milliseconds(), Some(1_700_000_004_000));
    let directory = entry(volume.reader(), "/hidden");
    assert_eq!(directory.attributes(), 0x12);
    assert_eq!(directory.timestamps().created, times.created);
    volume.unmount().unwrap();
}

#[test]
fn a_full_volume_reports_no_space_and_stays_consistent() {
    let (mut volume, device) = mounted(&Layout::default());
    let free = volume.free_clusters() as usize;
    volume.create("/big").unwrap();
    let data = vec![0x5a; (free + 1) * 4096];
    assert!(matches!(
        volume.pwrite("/big", 0, &data),
        Err(Error::NoSpace)
    ));
    volume
        .pwrite("/big", 0, &data[..(free - 1) * 4096])
        .unwrap();
    assert_eq!(volume.free_clusters(), 1);
    volume.unmount().unwrap();
    assert_eq!(
        tree(device.snapshot())["/big"].as_ref().unwrap().len(),
        (free - 1) * 4096
    );
}

struct Random(u64);

impl Random {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }
}

#[test]
fn a_random_mix_of_creates_and_writes_matches_a_model_after_remount() {
    let (mut volume, device) = mounted(&Layout {
        cluster_shift: 1,
        ..Layout::default()
    });
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    let directories = ["", "/one", "/one/two", "/three"];
    let mut model: BTreeMap<String, Option<Vec<u8>>> = BTreeMap::new();
    for directory in &directories[1..] {
        volume.mkdir(directory).unwrap();
        model.insert(directory.to_string(), None);
    }
    for step in 0..400 {
        let directory = directories[random.next(4) as usize];
        let path = format!("{directory}/file-{}", random.next(40));
        match random.next(3) {
            0 => {
                let length = random.next(5000) as usize;
                let data: Vec<u8> = (0..length).map(|index| (index + step) as u8).collect();
                volume.write_file(&path, &data).unwrap();
                model.insert(path, Some(data));
            }
            1 => {
                let Some(Some(current)) = model.get_mut(&path) else {
                    continue;
                };
                let offset = random.next(current.len() as u64 + 3000) as usize;
                let data: Vec<u8> = (0..random.next(3000))
                    .map(|index| (index as usize * step) as u8)
                    .collect();
                volume.pwrite(&path, offset as u64, &data).unwrap();
                if offset + data.len() > current.len() {
                    current.resize(offset + data.len(), 0);
                }
                current[offset..offset + data.len()].copy_from_slice(&data);
            }
            _ => {
                let result = volume.create(&path);
                match model.entry(path) {
                    Entry::Occupied(_) => assert!(matches!(result, Err(Error::Exists))),
                    Entry::Vacant(slot) => {
                        result.unwrap();
                        slot.insert(Some(Vec::new()));
                    }
                }
            }
        }
    }
    volume.unmount().unwrap();
    assert_eq!(tree(device.snapshot()), model);
}
