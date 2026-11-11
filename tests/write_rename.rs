mod write_support;

use fs_core::BlockRead;
use fs_exfat::directory::DirectoryEntry;
use fs_exfat::timestamp::Timestamp;
use fs_exfat::volume::Volume;
use fs_exfat::write::{SetTimes, WriteVolume};
use fs_exfat::Error;
use std::collections::BTreeMap;
use write_support::{format, Layout, Memory};

fn mounted() -> (WriteVolume<Memory>, Memory) {
    let (image, _) = format(&Layout::default());
    let device = Memory::new(image);
    (WriteVolume::mount(device.clone()).unwrap(), device)
}

fn entry<Device: BlockRead>(volume: &Volume<Device>, path: &str) -> DirectoryEntry {
    volume.lookup(path).unwrap().entry().unwrap().clone()
}

fn read<Device: BlockRead>(volume: &Volume<Device>, path: &str) -> Vec<u8> {
    let entry = entry(volume, path);
    let file = volume.open_file(&entry).unwrap();
    let mut output = vec![0; file.len() as usize];
    file.read_at(0, &mut output).unwrap();
    output
}

fn missing<Device: BlockRead>(volume: &Volume<Device>, path: &str) -> bool {
    matches!(volume.lookup(path), Err(Error::NotFound))
}

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
                out.insert(path.clone(), Some(read(volume, &path)));
            }
        }
    }
    let volume = Volume::open(Memory::new(image)).unwrap();
    let mut out = BTreeMap::new();
    walk(&volume, "", volume.root_directory().unwrap(), &mut out);
    out
}

#[test]
fn renames_move_within_and_across_directories() {
    let (mut volume, device) = mounted();
    volume.mkdir("/a").unwrap();
    volume.mkdir("/b").unwrap();
    volume.write_file("/a/x.txt", b"x data").unwrap();
    let before = entry(volume.reader(), "/a/x.txt");
    volume
        .rename("/a/x.txt", "/a/a much longer name than before.txt")
        .unwrap();
    assert!(missing(volume.reader(), "/a/x.txt"));
    volume
        .rename("/a/a much longer name than before.txt", "/b/y")
        .unwrap();
    let after = entry(volume.reader(), "/b/y");
    assert_eq!(
        after.stream(),
        before.stream(),
        "the stream moves unchanged"
    );
    assert_eq!(
        after.timestamps(),
        before.timestamps(),
        "a rename keeps the times"
    );
    assert_eq!(read(volume.reader(), "/b/y"), b"x data");
    volume.mkdir("/a/inner").unwrap();
    volume.write_file("/a/inner/deep.bin", &[5; 5000]).unwrap();
    volume.rename("/a", "/b/moved").unwrap();
    assert_eq!(
        read(volume.reader(), "/b/moved/inner/deep.bin"),
        vec![5; 5000]
    );
    volume.rename("/b/moved", "/b/MOVED").unwrap();
    assert_eq!(entry(volume.reader(), "/b/moved").name(), "MOVED");
    volume.rename("/b/MOVED", "/b/MOVED").unwrap();
    volume.unmount().unwrap();
    let tree = tree(device.snapshot());
    assert_eq!(
        tree.keys().collect::<Vec<_>>(),
        [
            "/b",
            "/b/MOVED",
            "/b/MOVED/inner",
            "/b/MOVED/inner/deep.bin",
            "/b/y"
        ]
    );
}

#[test]
fn replacing_follows_posix_rename() {
    let (mut volume, _) = mounted();
    volume.write_file("/old", &[1; 9000]).unwrap();
    volume.write_file("/new", b"new").unwrap();
    let free = volume.free_clusters();
    volume.rename("/new", "/OLD").unwrap();
    assert_eq!(read(volume.reader(), "/old"), b"new");
    assert_eq!(entry(volume.reader(), "/old").name(), "OLD");
    assert_eq!(
        volume.free_clusters(),
        free + 3,
        "the replaced file's clusters are freed"
    );
    assert!(missing(volume.reader(), "/new"));
    volume.mkdir("/d1").unwrap();
    volume.mkdir("/d2").unwrap();
    volume.mkdir("/full").unwrap();
    volume.create("/full/f").unwrap();
    assert!(matches!(
        volume.rename("/OLD", "/d1"),
        Err(Error::IsDirectory)
    ));
    assert!(matches!(
        volume.rename("/d1", "/OLD"),
        Err(Error::NotDirectory)
    ));
    assert!(matches!(
        volume.rename("/d1", "/full"),
        Err(Error::NotEmpty)
    ));
    assert!(matches!(
        volume.rename("/d1", "/d1/below"),
        Err(Error::Invalid(_))
    ));
    volume.mkdir("/d1/sub").unwrap();
    assert!(matches!(
        volume.rename("/d1", "/d1/sub/below"),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        volume.rename("/absent", "/x"),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        volume.rename("/OLD", "/absent/x"),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        volume.rename("/OLD", "/a:b"),
        Err(Error::Invalid(_))
    ));
    volume.rename("/d1", "/d2").unwrap();
    assert!(volume.reader().lookup("/d2/sub").unwrap().is_directory());
    assert!(missing(volume.reader(), "/d1"));
    volume.unmount().unwrap();
}

#[test]
fn times_attributes_and_the_label_are_set() {
    let (mut volume, device) = mounted();
    volume.create("/f").unwrap();
    volume.mkdir("/d").unwrap();
    let created = Timestamp::from_unix_milliseconds(1_000_000_000_990, 345).unwrap();
    let modified = Timestamp::from_unix_milliseconds(1_100_000_001_010, -480).unwrap();
    let accessed = Timestamp::from_unix_milliseconds(1_200_000_001_500, 0).unwrap();
    volume
        .set_times(
            "/f",
            SetTimes {
                created: Some(created),
                modified: Some(modified),
                accessed: Some(accessed),
            },
        )
        .unwrap();
    let times = *entry(volume.reader(), "/f").timestamps();
    assert_eq!(times.created, created);
    assert_eq!(times.modified, modified);
    assert_eq!(
        times.accessed.unix_milliseconds(),
        Some(1_200_000_000_000),
        "LastAccessed keeps even seconds only"
    );
    let untouched = entry(volume.reader(), "/d").timestamps().created;
    volume
        .set_times(
            "/d",
            SetTimes {
                modified: Some(modified),
                ..SetTimes::default()
            },
        )
        .unwrap();
    assert_eq!(entry(volume.reader(), "/d").timestamps().created, untouched);
    assert!(matches!(
        volume.set_times("/", SetTimes::default()),
        Err(Error::Invalid(_))
    ));
    volume.set_attributes("/f", 0x07).unwrap();
    assert_eq!(entry(volume.reader(), "/f").attributes(), 0x07);
    volume.set_attributes("/d", 0x02).unwrap();
    assert_eq!(entry(volume.reader(), "/d").attributes(), 0x12);
    assert!(matches!(
        volume.set_attributes("/f", 0x10),
        Err(Error::Invalid(_))
    ));
    volume.set_attributes("/f", 0).unwrap();
    assert_eq!(entry(volume.reader(), "/f").attributes(), 0);
    assert_eq!(volume.reader().metadata().label.as_deref(), Some("WRITER"));
    volume.set_label("Ünïcödé 11").unwrap();
    assert_eq!(
        volume.reader().metadata().label.as_deref(),
        Some("Ünïcödé 11")
    );
    assert!(matches!(
        volume.set_label("twelve chars"),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(volume.set_label("a/b"), Err(Error::Invalid(_))));
    volume.set_label("").unwrap();
    assert_eq!(volume.reader().metadata().label.as_deref(), Some(""));
    volume.set_label("BACKUP").unwrap();
    volume.unmount().unwrap();
    let reread = Volume::open(Memory::new(device.snapshot())).unwrap();
    assert_eq!(reread.metadata().label.as_deref(), Some("BACKUP"));
}

#[test]
fn a_volume_without_a_label_entry_gains_one() {
    let (mut image, geometry) = format(&Layout::default());
    let root = geometry.cluster_offset(geometry.root);
    assert_eq!(image[root], 0x83);
    image[root] = 0x03;
    let device = Memory::new(image);
    let mut volume = WriteVolume::mount(device.clone()).unwrap();
    assert_eq!(volume.reader().metadata().label, None);
    volume.set_label("NEW").unwrap();
    assert_eq!(volume.reader().metadata().label.as_deref(), Some("NEW"));
    volume.unmount().unwrap();
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

/// Every operation, chosen at random, checked against a model of the tree
/// and then against the volume read afresh after unmount.
#[test]
fn a_scripted_mix_of_every_operation_matches_a_model() {
    let (mut volume, device) = mounted();
    let mut random = Random(0x2545_f491_4f6c_dd1d);
    let directories = ["/p", "/p/q", "/r"];
    let mut model: BTreeMap<String, Option<Vec<u8>>> = BTreeMap::new();
    for directory in directories {
        volume.mkdir(directory).unwrap();
        model.insert(directory.to_string(), None);
    }
    let name = |random: &mut Random| {
        let parent = ["", "/p", "/p/q", "/r"][random.next(4) as usize];
        format!("{parent}/n{}", random.next(12))
    };
    let files = |model: &BTreeMap<String, Option<Vec<u8>>>| -> Vec<String> {
        model
            .iter()
            .filter(|(_, value)| value.is_some())
            .map(|(path, _)| path.clone())
            .collect()
    };
    let mut done = [0usize; 7];
    for step in 0..500usize {
        let existing = files(&model);
        let pick =
            |random: &mut Random| existing[random.next(existing.len() as u64) as usize].clone();
        let operation = random.next(7) as usize;
        if operation != 0 && operation != 6 && existing.is_empty()
            || operation == 3 && existing.len() < 15
        {
            continue;
        }
        done[operation] += 1;
        match operation {
            0 => {
                let path = name(&mut random);
                if model.contains_key(&path) {
                    continue;
                }
                let data: Vec<u8> = (0..random.next(9000))
                    .map(|index| (index as usize ^ step) as u8)
                    .collect();
                volume.write_file(&path, &data).unwrap();
                model.insert(path, Some(data));
            }
            1 => {
                let path = pick(&mut random);
                let current = model.get_mut(&path).unwrap().as_mut().unwrap();
                let offset = random.next(current.len() as u64 + 2000) as usize;
                let data = vec![step as u8; random.next(3000) as usize];
                volume.pwrite(&path, offset as u64, &data).unwrap();
                if !data.is_empty() {
                    if offset + data.len() > current.len() {
                        current.resize(offset + data.len(), 0);
                    }
                    current[offset..offset + data.len()].copy_from_slice(&data);
                }
            }
            2 => {
                let path = pick(&mut random);
                let length = random.next(12_000) as usize;
                volume.truncate(&path, length as u64).unwrap();
                model
                    .get_mut(&path)
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .resize(length, 0);
            }
            3 => {
                let path = pick(&mut random);
                volume.unlink(&path).unwrap();
                model.remove(&path);
            }
            4 => {
                let from = pick(&mut random);
                let to = name(&mut random);
                match model.get(&to) {
                    Some(None) => {
                        assert!(matches!(volume.rename(&from, &to), Err(Error::IsDirectory)));
                    }
                    _ => {
                        volume.rename(&from, &to).unwrap();
                        let data = model.remove(&from).unwrap();
                        model.insert(to, data);
                    }
                }
            }
            5 => {
                let path = pick(&mut random);
                let time = Timestamp::from_unix_milliseconds(
                    1_500_000_000_000 + random.next(1 << 30) as i64 * 10,
                    (random.next(8) as i16 - 4) * 15,
                )
                .unwrap();
                volume
                    .set_times(
                        &path,
                        SetTimes {
                            modified: Some(time),
                            ..SetTimes::default()
                        },
                    )
                    .unwrap();
                volume
                    .set_attributes(&path, [0, 0x01, 0x02, 0x20][random.next(4) as usize])
                    .unwrap();
                assert_eq!(entry(volume.reader(), &path).timestamps().modified, time);
            }
            6 => {
                volume.set_label(&format!("MIX {}", step % 1000)).unwrap();
            }
            _ => {}
        }
    }
    assert!(
        done.iter().all(|count| *count > 20),
        "every operation ran: {done:?}"
    );
    assert!(files(&model).len() > 10);
    volume.unmount().unwrap();
    assert_eq!(tree(device.snapshot()), model);
}
