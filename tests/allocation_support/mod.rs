//! #129 only: allocate, extend, convert, shrink and free without changing
//! directory entries or writing file data (#130). Unattached allocations
//! must all be freed before the independent checker sees the volume.

use crate::fixture_support::{assert_same, strict_listing};
use fs_exfat::{interface, primitives, write, Error};
use std::path::Path;

pub const GEOMETRIES: [(u32, u32); 2] = [(512, 4096), (4096, 128 * 1024)];

/// Exercise the allocation API on a guest-made image, then return it for
/// independent checking. Every change must already be visible on disk.
pub fn allocate_and_free(path: &Path) {
    let before = std::fs::read(path).unwrap();
    let reader = interface::open_path(path, 0).unwrap();
    let tree = strict_listing(&reader);
    assert_eq!(tree.len(), 2, "kernel-created sentinel directory and file");
    let boot = reader.boot().clone();
    let sector = boot.bytes_per_sector() as usize;
    let bitmap = reader.metadata().bitmaps[0].unwrap();
    let bitmap_at = boot.cluster_offset(bitmap.first_cluster).unwrap() as usize;
    let fat_at = boot.fat_offset as usize * sector;
    let original_free = u64::from(boot.cluster_count) - reader.bitmap().allocated_count();
    drop(reader);

    let mut volume = write::open_path(path, 0).unwrap();
    assert_eq!(volume.free_clusters(), original_free);
    let check_state = |volume: &write::WriteVolume<_>| {
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(u16::from_le_bytes([bytes[106], bytes[107]]) & 2, 2);
        assert!(
            bytes[12 * sector..24 * sector] == before[12 * sector..24 * sector],
            "backup boot region changed while mounted"
        );
        let used = u64::from(boot.cluster_count) - volume.free_clusters();
        assert_eq!(
            u64::from(bytes[112]),
            used * 100 / u64::from(boot.cluster_count)
        );
        let stored = u32::from_le_bytes(bytes[11 * sector..11 * sector + 4].try_into().unwrap());
        assert_eq!(
            primitives::boot_checksum(&bytes[..12 * sector], sector),
            stored
        );
        for cluster in 2..boot.cluster_count + 2 {
            let bit = (cluster - 2) as usize;
            assert_eq!(
                bytes[bitmap_at + bit / 8] & (1 << (bit % 8)) != 0,
                volume.reader().bitmap().allocated(cluster).unwrap(),
                "cluster {cluster}: on-disk bitmap differs from the mount",
            );
        }
        bytes
    };
    check_state(&volume);

    let mut run = volume.allocate(2).unwrap();
    volume.extend(&mut run, 3).unwrap();
    assert!(run.no_fat_chain());
    assert_eq!(run.count(), 5);
    assert_eq!(volume.free_clusters(), original_free - 5);
    let bytes = check_state(&volume);
    for cluster in volume.clusters(&run).unwrap() {
        assert_eq!(&bytes[fat_at + cluster as usize * 4..][..4], &[0; 4]);
    }
    let blocker = volume.allocate(1).unwrap();
    assert_eq!(blocker.first_cluster(), run.last_cluster() + 1);
    volume.extend(&mut run, 2).unwrap();
    assert!(!run.no_fat_chain(), "blocked run becomes a FAT chain");
    let chain = volume.clusters(&run).unwrap();
    assert_eq!(chain.len(), 7);
    assert!(chain.windows(2).any(|pair| pair[1] != pair[0] + 1));
    assert!(!chain.contains(&blocker.first_cluster()));
    assert_eq!(volume.free_clusters(), original_free - 8);
    let bytes = check_state(&volume);
    for (index, cluster) in chain.iter().enumerate() {
        let at = fat_at + *cluster as usize * 4;
        let next = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert_eq!(next, chain.get(index + 1).copied().unwrap_or(0xffff_ffff));
    }
    volume.shrink(&mut run, 5).unwrap();
    assert!(
        run.no_fat_chain(),
        "a chain shrunk to its first run becomes NoFatChain"
    );
    assert_eq!(volume.clusters(&run).unwrap(), chain[..5]);
    assert_eq!(volume.free_clusters(), original_free - 6);
    check_state(&volume);
    volume.free(run).unwrap();
    volume.free(blocker).unwrap();

    // Exhaust the heap around two single-cluster holes. Allocation cannot
    // find a run, so this exercises allocate's FAT-chain fallback as well
    // as extend's conversion above, and makes PercentInUse reach 100.
    let first = volume.allocate(1).unwrap();
    let separator = volume.allocate(1).unwrap();
    let second = volume.allocate(1).unwrap();
    assert_eq!(separator.first_cluster(), first.first_cluster() + 1);
    assert_eq!(second.first_cluster(), separator.first_cluster() + 1);
    let rest = volume.allocate(volume.free_clusters() as u32).unwrap();
    assert_eq!(volume.percent_in_use(), 100);
    check_state(&volume);
    assert!(matches!(volume.allocate(1), Err(Error::NoSpace)));
    volume.free(first).unwrap();
    volume.free(second).unwrap();
    let fragmented = volume.allocate(2).unwrap();
    assert!(!fragmented.no_fat_chain());
    assert_eq!(
        volume.clusters(&fragmented).unwrap(),
        [first.first_cluster(), second.first_cluster()]
    );
    let bytes = check_state(&volume);
    let at = fat_at + first.first_cluster() as usize * 4;
    assert_eq!(
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()),
        second.first_cluster()
    );
    let at = fat_at + second.first_cluster() as usize * 4;
    assert_eq!(
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()),
        0xffff_ffff
    );
    volume.free(fragmented).unwrap();
    volume.free(separator).unwrap();
    volume.free(rest).unwrap();
    assert_eq!(volume.free_clusters(), original_free);
    check_state(&volume);
    volume.unmount().unwrap();

    let after = std::fs::read(path).unwrap();
    assert_eq!(u16::from_le_bytes([after[106], after[107]]) & 2, 0);
    assert!(
        after[12 * sector..24 * sector] == before[12 * sector..24 * sector],
        "backup boot region changed after unmount"
    );
    assert!(
        after[24 * sector..] == before[24 * sector..],
        "all allocations freed; original FAT, bitmap, directories and data preserved"
    );
    assert_eq!(
        u64::from(after[112]),
        (u64::from(boot.cluster_count) - original_free) * 100 / u64::from(boot.cluster_count)
    );
    let reader = interface::open_path(path, 0).unwrap();
    assert_same(
        "allocation/free",
        "before writing",
        &tree,
        "after unmount",
        &strict_listing(&reader),
        true,
    );
}
