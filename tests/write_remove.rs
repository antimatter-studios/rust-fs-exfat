mod write_support;

use fs_core::BlockRead;
use fs_exfat::path::Node;
use fs_exfat::volume::Volume;
use fs_exfat::write::WriteVolume;
use fs_exfat::Error;
use write_support::{format, Layout, Memory};

fn mounted() -> (WriteVolume<Memory>, Memory) {
    let (image, _) = format(&Layout::default());
    let device = Memory::new(image);
    (WriteVolume::mount(device.clone()).unwrap(), device)
}

fn read<Device: BlockRead>(volume: &Volume<Device>, path: &str) -> Vec<u8> {
    let Node::Entry(entry) = volume.lookup(path).unwrap() else {
        panic!("{path} is the root")
    };
    let file = volume.open_file(&entry).unwrap();
    let mut output = vec![0xa5; file.len() as usize];
    file.read_at(0, &mut output).unwrap();
    output
}

fn stream<Device: BlockRead>(volume: &Volume<Device>, path: &str) -> (u64, u64, bool) {
    let entry = volume.lookup(path).unwrap().entry().unwrap().clone();
    let stream = entry.stream();
    (
        stream.data_length(),
        stream.valid_data_length(),
        stream.no_fat_chain(),
    )
}

#[test]
fn truncation_shrinks_and_frees_then_grows_with_zeros() {
    let (mut volume, device) = mounted();
    let free = volume.free_clusters();
    let data: Vec<u8> = (0..20_000u32).map(|index| (index % 251) as u8).collect();
    volume.write_file("/t.bin", &data).unwrap();
    assert_eq!(volume.free_clusters(), free - 5);
    volume.truncate("/t.bin", 4097).unwrap();
    assert_eq!(volume.free_clusters(), free - 2);
    assert_eq!(read(volume.reader(), "/t.bin"), &data[..4097]);
    assert_eq!(stream(volume.reader(), "/t.bin"), (4097, 4097, true));
    volume.truncate("/t.bin", 12_000).unwrap();
    assert_eq!(volume.free_clusters(), free - 3);
    let mut expected = data[..4097].to_vec();
    expected.resize(12_000, 0);
    assert_eq!(read(volume.reader(), "/t.bin"), expected);
    assert_eq!(stream(volume.reader(), "/t.bin"), (12_000, 4097, true));
    volume.pwrite("/t.bin", 6000, b"mid").unwrap();
    expected[6000..6003].copy_from_slice(b"mid");
    assert_eq!(read(volume.reader(), "/t.bin"), expected);
    volume.truncate("/t.bin", 0).unwrap();
    assert_eq!(volume.free_clusters(), free);
    let entry = volume
        .reader()
        .lookup("/t.bin")
        .unwrap()
        .entry()
        .unwrap()
        .clone();
    assert_eq!(entry.stream().first_cluster(), 0);
    assert!(matches!(
        volume.truncate("/absent", 1),
        Err(Error::NotFound)
    ));
    volume.mkdir("/d").unwrap();
    assert!(matches!(volume.truncate("/d", 1), Err(Error::IsDirectory)));
    volume.unmount().unwrap();
    let reread = Volume::open(Memory::new(device.snapshot())).unwrap();
    assert!(read(&reread, "/t.bin").is_empty());
}

#[test]
fn truncation_turns_a_fragmented_file_back_into_a_run() {
    let (mut volume, _) = mounted();
    volume.write_file("/a", &[1; 4096]).unwrap();
    volume.write_file("/b", &[2; 4096]).unwrap();
    volume.pwrite("/a", 4096, &[3; 8192]).unwrap();
    assert!(!stream(volume.reader(), "/a").2);
    let free = volume.free_clusters();
    volume.truncate("/a", 100).unwrap();
    assert_eq!(stream(volume.reader(), "/a"), (100, 100, true));
    assert_eq!(volume.free_clusters(), free + 2);
    assert_eq!(read(volume.reader(), "/a"), vec![1; 100]);
    assert_eq!(read(volume.reader(), "/b"), vec![2; 4096]);
    volume.unmount().unwrap();
}

#[test]
fn unlink_and_rmdir_clear_in_use_and_free_every_cluster() {
    let (mut volume, device) = mounted();
    let free = volume.free_clusters();
    volume.mkdir("/dir").unwrap();
    volume.write_file("/dir/file", &[9; 9000]).unwrap();
    assert!(matches!(volume.rmdir("/dir"), Err(Error::NotEmpty)));
    assert!(matches!(volume.unlink("/dir"), Err(Error::IsDirectory)));
    assert!(matches!(
        volume.rmdir("/dir/file"),
        Err(Error::NotDirectory)
    ));
    assert!(matches!(volume.unlink("/dir/absent"), Err(Error::NotFound)));
    assert!(matches!(volume.rmdir("/"), Err(Error::Invalid(_))));
    volume.unlink("/dir/file").unwrap();
    assert!(matches!(
        volume.reader().lookup("/dir/file"),
        Err(Error::NotFound)
    ));
    volume.rmdir("/dir").unwrap();
    assert!(matches!(
        volume.reader().lookup("/dir"),
        Err(Error::NotFound)
    ));
    assert_eq!(volume.free_clusters(), free);
    volume.create("/dir").unwrap();
    volume.unmount().unwrap();
    let image = device.snapshot();
    let reread = Volume::open(Memory::new(image.clone())).unwrap();
    let names: Vec<_> = reread
        .root_directory()
        .unwrap()
        .iter()
        .map(|entry| entry.name().to_string())
        .collect();
    assert_eq!(names, ["dir"]);
    assert!(!reread.lookup("/dir").unwrap().is_directory());
}

#[test]
fn removed_names_free_their_slots_for_new_sets() {
    let (mut volume, _) = mounted();
    for index in 0..60 {
        volume.create(&format!("/name {index:02}")).unwrap();
    }
    let root_clusters = |volume: &WriteVolume<Memory>| {
        volume
            .reader()
            .cluster_chain(volume.reader().boot().root_cluster)
            .unwrap()
            .len()
    };
    let before = root_clusters(&volume);
    for index in 0..60 {
        volume.unlink(&format!("/name {index:02}")).unwrap();
    }
    for index in 0..60 {
        volume.create(&format!("/again {index:02}")).unwrap();
    }
    assert_eq!(root_clusters(&volume), before, "freed entries are reused");
    volume.unmount().unwrap();
}
