//! The First and Second FAT and Allocation Bitmap (#40), located and compared
//! as the specification lays them out. Expected values are written from the
//! specification's arithmetic, not from the helpers under test.

use fs_exfat::primitives::{
    bitmap_differences, fat_differences, BootSector, Compared, FatCopy, FatLayout,
};
use std::ops::ControlFlow;

fn boot(number_of_fats: u8, volume_flags: u16) -> BootSector {
    BootSector {
        file_system_name: "EXFAT   ".into(),
        partition_offset: 0,
        volume_length: 1 << 24,
        fat_offset: 128,
        fat_length: 1000,
        cluster_heap_offset: 2128,
        cluster_count: 250_000,
        root_cluster: 4,
        volume_serial: 0,
        revision: 0x0100,
        volume_flags,
        bytes_per_sector_shift: 12,
        sectors_per_cluster_shift: 0,
        number_of_fats,
        percent_in_use: 0,
        signature_ok: true,
    }
}

fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

#[test]
fn copies_are_located_and_compared_as_the_specification_lays_them_out() {
    // Table 3: the Second FAT follows the First, FatLength sectors on, and
    // ActiveFat (section 3.1.13.1) names the Second only on a two-FAT volume.
    let two = FatLayout::from_boot(&boot(2, 1));
    assert_eq!(two.offset(FatCopy::First), Some(128 * 4096));
    assert_eq!(two.offset(FatCopy::Second), Some((128 + 1000) * 4096));
    assert_eq!((two.length, two.active), (1000 * 4096, FatCopy::Second));
    assert_eq!(
        two.copies().collect::<Vec<_>>(),
        [FatCopy::First, FatCopy::Second]
    );
    let one = FatLayout::from_boot(&boot(1, 1));
    assert_eq!(
        (one.offset(FatCopy::Second), one.active),
        (None, FatCopy::First)
    );
    assert_eq!(one.copies().count(), 1);
    assert_eq!(
        FatCopy::from_index(1).map(FatCopy::other),
        Some(FatCopy::First)
    );

    // Entries 0 and 1 and a partial word are not compared (sections 4.1.1
    // and 4.1.2); nor is an entry past FatEntry[ClusterCount+1] (3.1.7).
    let first = words(&[0xFFFF_FFF8, 0xFFFF_FFFF, 3, 0xFFFF_FFFF, 0, 9]);
    let second = words(&[0xFFFF_FFF0, 0, 3, 7, 0xFFFF_FFF7, 0]);
    let differing = [(3, 0xFFFF_FFFF, 7), (4, 0, 0xFFFF_FFF7)];
    let mut found = Vec::new();
    let mut collect = |cluster, old, new| {
        found.push((cluster, old, new));
        ControlFlow::Continue(())
    };
    let compared = fat_differences(&first, &second[..23], 0, 4, &mut collect);
    let expected = Compared {
        clusters: 3,
        differences: 2,
        stopped: false,
    };
    assert_eq!(compared, expected);
    // Ranges that start at entry 3 are matched entry for entry.
    let compared = fat_differences(&first[12..], &second[12..], 3, 3, &mut collect);
    assert_eq!((compared.clusters, compared.differences), (2, 2));
    assert_eq!(found, [differing, differing].concat());

    // Thirteen clusters: byte 1's bits 5 to 7 are Reserved (Table 22).
    let mut bits = Vec::new();
    let compared = bitmap_differences(&[0x08, 0x00], &[0x00, 0x50], 0, 13, |cluster, old, new| {
        bits.push((cluster, old, new));
        ControlFlow::Continue(())
    });
    assert_eq!((compared.clusters, compared.differences), (13, 2));
    assert_eq!(bits, [(5, true, false), (14, false, true)]);
    let stopped = bitmap_differences(&[0xFF], &[0xF7], 2, 100, |_, _, _| ControlFlow::Break(()));
    let expected = Compared {
        clusters: 4,
        differences: 1,
        stopped: true,
    };
    assert_eq!(stopped, expected);
}
