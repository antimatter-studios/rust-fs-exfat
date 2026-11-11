mod write_support;

use fs_exfat::write::{Allocation, WriteVolume};
use fs_exfat::Error;
use std::sync::atomic::Ordering;
use write_support::{format, Geometry, Layout, Memory};

fn mounted(layout: &Layout) -> (WriteVolume<Memory>, Memory, Geometry, Vec<u8>) {
    let (image, geometry) = format(layout);
    let device = Memory::new(image.clone());
    let volume = WriteVolume::mount(device.clone()).unwrap();
    (volume, device, geometry, image)
}

fn flags(image: &[u8], sector: usize, backup: bool) -> u16 {
    let base = if backup { sector * 12 } else { 0 };
    u16::from_le_bytes([image[base + 106], image[base + 107]])
}

fn checksum_matches(image: &[u8], sector: usize) -> bool {
    let region = &image[..sector * 12];
    let stored = u32::from_le_bytes(region[sector * 11..sector * 11 + 4].try_into().unwrap());
    fs_exfat::primitives::boot_checksum(region, sector) == stored
}

#[test]
fn a_write_mount_marks_the_main_boot_region_dirty_until_a_clean_unmount() {
    for sector_shift in [9, 12] {
        let layout = Layout {
            sector_shift,
            cluster_shift: if sector_shift == 9 { 3 } else { 0 },
            ..Layout::default()
        };
        let (volume, device, geometry, original) = mounted(&layout);
        let sector = geometry.sector;
        let image = device.snapshot();
        assert_eq!(
            flags(&image, sector, false),
            2,
            "VolumeDirty is set at mount"
        );
        assert_eq!(
            flags(&image, sector, true),
            0,
            "the backup boot region is left alone"
        );
        assert_eq!(
            &image[sector * 12..sector * 24],
            &original[sector * 12..sector * 24]
        );
        assert!(checksum_matches(&image, sector));
        volume.unmount().unwrap();
        let image = device.snapshot();
        assert_eq!(
            flags(&image, sector, false),
            0,
            "a clean unmount clears VolumeDirty"
        );
        assert!(checksum_matches(&image, sector));
        assert_eq!(
            image, original,
            "a mount that changed nothing leaves the bytes as they were"
        );
    }
}

#[test]
fn dirty_media_failure_and_read_only_volumes_are_refused() {
    let (mut image, geometry) = format(&Layout::default());
    image[106] = 2;
    let refused = WriteVolume::mount(Memory::new(image.clone()));
    assert!(matches!(refused, Err(Error::Unsupported(reason)) if reason.contains("dirty")));
    image[106] = 4;
    let refused = WriteVolume::mount(Memory::new(image.clone()));
    assert!(matches!(refused, Err(Error::Unsupported(reason)) if reason.contains("media")));
    image[106] = 0;
    let mut device = Memory::new(image.clone());
    device.writable = false;
    assert!(matches!(
        WriteVolume::mount(device.clone()),
        Err(Error::Device(fs_core::Error::ReadOnly))
    ));
    assert_eq!(device.snapshot(), image);
    let _ = geometry;
}

#[test]
fn contiguous_allocations_are_no_fat_chain_and_free_back_to_the_original_bytes() {
    let (mut volume, device, geometry, original) = mounted(&Layout::default());
    let free = volume.free_clusters();
    let allocation = volume.allocate(10).unwrap();
    assert!(allocation.no_fat_chain());
    assert_eq!(allocation.count(), 10);
    assert_eq!(allocation.last_cluster(), allocation.first_cluster() + 9);
    assert_eq!(volume.free_clusters(), free - 10);
    let image = device.snapshot();
    for cluster in allocation.first_cluster()..=allocation.last_cluster() {
        assert!(geometry.allocated(&image, 0, cluster));
        assert_eq!(
            geometry.fat(&image, 0, cluster),
            0,
            "NoFatChain leaves the FAT alone"
        );
    }
    assert_eq!(
        volume.clusters(&allocation).unwrap(),
        (allocation.first_cluster()..=allocation.last_cluster()).collect::<Vec<_>>()
    );
    volume.free(allocation).unwrap();
    assert_eq!(volume.free_clusters(), free);
    volume.unmount().unwrap();
    assert_eq!(device.snapshot(), original);
}

#[test]
fn a_fragmented_heap_gets_a_fat_chain_that_the_strict_reader_follows() {
    let (mut volume, device, geometry, _) = mounted(&Layout::default());
    let mut held = Vec::new();
    let mut gaps = Vec::new();
    for _ in 0..4 {
        held.push(volume.allocate(3).unwrap());
        gaps.push(volume.allocate(2).unwrap());
    }
    let tail = volume.free_clusters();
    let rest = volume.allocate(tail as u32).unwrap();
    for gap in gaps {
        volume.free(gap).unwrap();
    }
    let chain = volume.allocate(8).unwrap();
    assert!(!chain.no_fat_chain());
    let clusters = volume.clusters(&chain).unwrap();
    assert_eq!(clusters.len(), 8);
    assert!(clusters.windows(2).any(|pair| pair[1] != pair[0] + 1));
    assert_eq!(
        volume
            .reader()
            .cluster_chain(chain.first_cluster())
            .unwrap(),
        clusters
    );
    let image = device.snapshot();
    // Nothing is flushed between operations except at their ends, so the
    // device already holds the chain.
    for pair in clusters.windows(2) {
        assert_eq!(geometry.fat(&image, 0, pair[0]), pair[1]);
    }
    assert_eq!(geometry.fat(&image, 0, chain.last_cluster()), 0xffff_ffff);
    assert!(matches!(volume.allocate(1), Err(Error::NoSpace)));
    volume.free(chain).unwrap();
    volume.free(rest).unwrap();
    for allocation in held {
        volume.free(allocation).unwrap();
    }
    volume.unmount().unwrap();
}

#[test]
fn a_contiguous_stream_grows_in_place_and_becomes_a_chain_when_it_cannot() {
    let (mut volume, device, geometry, _) = mounted(&Layout::default());
    let mut stream = volume.allocate(2).unwrap();
    volume.extend(&mut stream, 3).unwrap();
    assert!(stream.no_fat_chain());
    assert_eq!(stream.count(), 5);
    let blocker = volume.allocate(1).unwrap();
    assert_eq!(blocker.first_cluster(), stream.last_cluster() + 1);
    volume.extend(&mut stream, 2).unwrap();
    assert!(!stream.no_fat_chain());
    let clusters = volume.clusters(&stream).unwrap();
    assert_eq!(clusters.len(), 7);
    assert_eq!(
        &clusters[..5],
        &(stream.first_cluster()..stream.first_cluster() + 5).collect::<Vec<_>>()[..]
    );
    assert!(!clusters.contains(&blocker.first_cluster()));
    let image = device.snapshot();
    assert_eq!(geometry.fat(&image, 0, clusters[4]), clusters[5]);
    assert_eq!(geometry.fat(&image, 0, clusters[6]), 0xffff_ffff);

    volume.shrink(&mut stream, 5).unwrap();
    assert!(
        stream.no_fat_chain(),
        "the remaining chain is consecutive again"
    );
    assert_eq!(stream.count(), 5);
    let image = device.snapshot();
    assert!(!geometry.allocated(&image, 0, clusters[5]));
    assert_eq!(
        geometry.fat(&image, 0, clusters[5]),
        0,
        "freed FAT entries are zeroed"
    );
    assert_eq!(volume.clusters(&stream).unwrap(), &clusters[..5]);
    volume.free(stream).unwrap();
    volume.free(blocker).unwrap();
    volume.unmount().unwrap();
}

#[test]
fn percent_in_use_is_kept_current_and_rounded_down() {
    let (mut volume, device, geometry, _) = mounted(&Layout::default());
    let count = u64::from(geometry.cluster_count);
    let used = count - volume.free_clusters();
    let target = count / 2 - used;
    let allocation = volume.allocate(target as u32).unwrap();
    let expected = ((used + target) * 100 / count) as u8;
    assert_eq!(volume.percent_in_use(), expected);
    assert_eq!(device.snapshot()[112], expected);
    assert_eq!(volume.reader().boot().percent_in_use, expected);
    volume.free(allocation).unwrap();
    volume.unmount().unwrap();
    assert_eq!(device.snapshot()[112], (used * 100 / count) as u8);
}

#[test]
fn a_two_fat_volume_keeps_its_second_fat_and_bitmap_in_step() {
    let layout = Layout {
        fats: 2,
        ..Layout::default()
    };
    let (mut volume, device, geometry, _) = mounted(&layout);
    let mut kept = Vec::new();
    for _ in 0..3 {
        kept.push(volume.allocate(2).unwrap());
        let gap = volume.allocate(1).unwrap();
        kept.push(volume.allocate(1).unwrap());
        volume.free(gap).unwrap();
    }
    let rest = volume.free_clusters() as u32 - 3;
    kept.push(volume.allocate(rest).unwrap());
    let chain = volume.allocate(3).unwrap();
    assert!(!chain.no_fat_chain());
    let image = device.snapshot();
    assert_eq!(geometry.fat_bytes(&image, 0), geometry.fat_bytes(&image, 1));
    assert_eq!(
        geometry.bitmap_bytes(&image, 0),
        geometry.bitmap_bytes(&image, 1)
    );
    volume.free(chain).unwrap();
    for allocation in kept {
        volume.free(allocation).unwrap();
    }
    volume.unmount().unwrap();
    let image = device.snapshot();
    assert_eq!(geometry.fat_bytes(&image, 0), geometry.fat_bytes(&image, 1));
    assert_eq!(
        geometry.bitmap_bytes(&image, 0),
        geometry.bitmap_bytes(&image, 1)
    );
}

#[test]
fn clusters_the_fat_marks_bad_are_never_allocated() {
    let (mut image, geometry) = format(&Layout::default());
    let bad = geometry.root + 3;
    let position = geometry.fat_offset * geometry.sector + bad as usize * 4;
    image[position..position + 4].copy_from_slice(&0xffff_fff7u32.to_le_bytes());
    let device = Memory::new(image);
    let mut volume = WriteVolume::mount(device.clone()).unwrap();
    let allocation = volume.allocate(5).unwrap();
    assert!(allocation.no_fat_chain());
    assert!(allocation.first_cluster() > bad);
    let before = volume.allocate(2).unwrap();
    assert!(!(before.first_cluster()..=before.last_cluster()).contains(&bad));
    volume.free(allocation).unwrap();
    volume.free(before).unwrap();
    volume.unmount().unwrap();
}

#[test]
fn a_write_that_fails_partway_leaves_the_volume_dirty_and_the_mount_closed() {
    let (image, geometry) = format(&Layout::default());
    let device = Memory::new(image);
    let mut volume = WriteVolume::mount(device.clone()).unwrap();
    device.writes_left.store(0, Ordering::SeqCst);
    assert!(volume.allocate(4).is_err());
    device.writes_left.store(u64::MAX, Ordering::SeqCst);
    assert!(
        volume.allocate(1).is_err(),
        "a failed mount takes no more writes"
    );
    assert!(volume.unmount().is_err());
    assert_eq!(flags(&device.snapshot(), geometry.sector, false), 2);
}

#[test]
fn an_empty_allocation_frees_and_grows_like_a_new_one() {
    let (mut volume, _, _, _) = mounted(&Layout::default());
    let mut empty = Allocation::EMPTY;
    volume.free(empty).unwrap();
    volume.extend(&mut empty, 0).unwrap();
    assert!(empty.is_empty());
    volume.extend(&mut empty, 3).unwrap();
    assert_eq!(empty.count(), 3);
    volume.shrink(&mut empty, 0).unwrap();
    assert!(empty.is_empty());
    volume.unmount().unwrap();
}

#[test]
fn a_dropped_mount_is_unmounted_cleanly() {
    let (volume, device, geometry, original) = mounted(&Layout::default());
    drop(volume);
    assert_eq!(flags(&device.snapshot(), geometry.sector, false), 0);
    assert_eq!(device.snapshot(), original);
}

#[test]
fn a_mount_dropped_by_a_panic_is_left_dirty() {
    let (image, geometry) = format(&Layout::default());
    let device = Memory::new(image);
    let held = device.clone();
    let result = std::panic::catch_unwind(move || {
        let mut volume = WriteVolume::mount(device).unwrap();
        let _leaked = volume.allocate(3).unwrap();
        panic!("stopped partway");
    });
    assert!(result.is_err());
    assert_eq!(flags(&held.snapshot(), geometry.sector, false), 2);
}

/// A 512-byte-sector volume on a raw device that accepts only 4096-byte
/// transfers mounts for writing (#152): every write is widened to the
/// device's block, with the sectors around it read first, and the tree it
/// writes reads back. Clusters of one sector put metadata, directories and
/// file data in the same device block.
#[test]
fn a_512_byte_sector_volume_mounts_for_writing_on_a_4096_byte_device() {
    for cluster_shift in [0, 3] {
        let layout = Layout {
            cluster_shift,
            ..Layout::default()
        };
        let (image, _) = format(&layout);
        let device = Memory::new(image).with_block(4096);
        let mut volume = WriteVolume::mount(device.clone()).unwrap();
        let pattern = |seed: u8, length: usize| -> Vec<u8> {
            (0..length)
                .map(|index| (index as u8).wrapping_mul(31) ^ seed)
                .collect()
        };
        volume.mkdir("/dir").unwrap();
        volume.mkdir("/dir/deeper").unwrap();
        volume.write_file("/one.bin", &pattern(1, 1)).unwrap();
        volume.write_file("/dir/odd.bin", &pattern(2, 513)).unwrap();
        volume
            .write_file("/dir/deeper/large.bin", &pattern(3, 70_000))
            .unwrap();
        volume.create("/dir/patched.bin").unwrap();
        volume
            .pwrite("/dir/patched.bin", 777, &pattern(4, 5000))
            .unwrap();
        volume.truncate("/dir/deeper/large.bin", 9_999).unwrap();
        volume.write_file("/gone.bin", &pattern(5, 3000)).unwrap();
        volume.unlink("/gone.bin").unwrap();
        volume.rename("/one.bin", "/dir/moved.bin").unwrap();
        volume.set_label("BLOCKS").unwrap();
        volume.unmount().unwrap();

        let reader = fs_exfat::volume::Volume::open(Memory::new(device.snapshot())).unwrap();
        assert_eq!(reader.boot().volume_flags & 2, 0, "unmounted clean");
        assert_eq!(reader.metadata().label.as_deref(), Some("BLOCKS"));
        let read = |path: &str| {
            let entry = reader.lookup(path).unwrap().entry().unwrap().clone();
            let file = reader.open_file(&entry).unwrap();
            let mut bytes = vec![0; file.len() as usize];
            file.read_at(0, &mut bytes).unwrap();
            bytes
        };
        let mut patched = vec![0; 777];
        patched.extend(pattern(4, 5000));
        assert_eq!(read("/dir/moved.bin"), pattern(1, 1));
        assert_eq!(read("/dir/odd.bin"), pattern(2, 513));
        assert_eq!(read("/dir/deeper/large.bin"), pattern(3, 9_999));
        assert_eq!(read("/dir/patched.bin"), patched);
        assert!(matches!(reader.lookup("/gone.bin"), Err(Error::NotFound)));
        assert!(matches!(reader.lookup("/one.bin"), Err(Error::NotFound)));
    }
}
