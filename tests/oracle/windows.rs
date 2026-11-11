use super::{primitives::chain, support::Bytes};
use fs_exfat::{
    boot::BootRegions,
    device::Reader,
    primitives::{boot_checksum, u16_at, u32_at, u64_at, BootSector},
    volume::Volume,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf};

fn directory() -> PathBuf {
    std::env::var("EXFAT_ORACLE_DIR")
        .expect("run chore test:oracle-macos on macOS")
        .into()
}

fn reference(sector: usize) -> (Vec<u8>, BootSector) {
    let image = std::fs::read(directory().join(format!("windows-{sector}.dmg"))).unwrap();
    let boot = BootSector::parse(&image[..512]).unwrap();
    (image, boot)
}

#[test]
fn windows_layout_boot_matches_the_independent_reader_and_both_checksums() {
    for sector in [512, 4096] {
        let (image, independent) = reference(sector);
        let report = BootRegions::read(&Reader::new(Bytes(image.clone()))).unwrap();
        let boot = report.strict().unwrap();
        assert_eq!(boot.bytes_per_sector(), sector as u64);
        assert_eq!(boot.cluster_size(), 32768);
        assert_eq!(boot.cluster_size(), independent.cluster_size());
        assert_eq!(boot.volume_length, independent.volume_length);
        assert_eq!(boot.fat_offset, independent.fat_offset);
        assert_eq!(boot.fat_length, independent.fat_length);
        assert_eq!(boot.cluster_heap_offset, independent.cluster_heap_offset);
        assert_eq!(boot.cluster_count, independent.cluster_count);
        assert_eq!(boot.root_cluster, independent.root_cluster);
        assert_eq!(boot.volume_serial, independent.volume_serial);
        assert_eq!(boot.partition_offset, independent.partition_offset);
        assert_eq!(boot.revision, independent.revision);
        assert_eq!(boot.volume_flags, independent.volume_flags);
        assert_eq!(
            boot.bytes_per_sector_shift,
            independent.bytes_per_sector_shift
        );
        assert_eq!(
            boot.sectors_per_cluster_shift,
            independent.sectors_per_cluster_shift
        );
        assert_eq!(boot.number_of_fats, independent.number_of_fats);
        assert_eq!(boot.percent_in_use, independent.percent_in_use);
        assert!(report.differences.is_empty());
        for copy in [0, 12 * sector] {
            let region = &image[copy..copy + 12 * sector];
            let checksum = boot_checksum(region, sector);
            for offset in (11 * sector..12 * sector).step_by(4) {
                assert_eq!(u32_at(region, offset), checksum);
            }
        }
    }
}

#[test]
fn windows_layout_system_streams_match_independent_raw_metadata() {
    for sector in [512, 4096] {
        let (image, boot) = reference(sector);
        let root = chain(&image, &boot, boot.root_cluster, None);
        let volume = Volume::open(Bytes(image.clone())).unwrap();
        assert_eq!(&root[..96], &volume.root_bytes()[..96]);
        assert_eq!((root[0], root[32], root[64]), (0x83, 0x81, 0x82));
        let label: Vec<_> = (0..usize::from(root[1]))
            .map(|index| u16_at(&root, 2 + index * 2))
            .collect();
        assert_eq!(
            volume.metadata().label,
            Some(String::from_utf16(&label).unwrap())
        );
        let bitmap_entry = &root[32..64];
        let bitmap = volume.metadata().bitmaps[0].unwrap();
        assert_eq!(bitmap.first_cluster, 2);
        assert_eq!(bitmap.first_cluster, u32_at(bitmap_entry, 20));
        assert_eq!(bitmap.length, u64_at(bitmap_entry, 24));
        let bytes = chain(&image, &boot, bitmap.first_cluster, None);
        for cluster in 2..boot.cluster_count + 2 {
            let index = (cluster - 2) as usize;
            assert_eq!(
                volume.bitmap().allocated(cluster).unwrap(),
                bytes[index / 8] & (1 << (index % 8)) != 0
            );
        }
        let entry = &root[64..96];
        let upcase = volume.metadata().upcase;
        assert_eq!(upcase.first_cluster, 5);
        assert_eq!(boot.root_cluster, 9);
        assert_eq!(upcase.first_cluster, u32_at(entry, 20));
        assert_eq!(upcase.length, u64_at(entry, 24));
        let bytes = chain(&image, &boot, upcase.first_cluster, None);
        let bytes = &bytes[..upcase.length as usize];
        let checksum = bytes.iter().fold(0u32, |total, byte| {
            total.rotate_right(1).wrapping_add(u32::from(*byte))
        });
        assert_eq!(checksum, u32_at(entry, 4));
        assert_eq!(checksum, volume.metadata().upcase_checksum);
        let mut words = bytes.chunks_exact(2).map(|word| u16_at(word, 0));
        let mut unit = 0u32;
        while let Some(mapped) = words.next() {
            if mapped == 0xffff && unit < 65535 {
                let end = unit + u32::from(words.next().unwrap());
                while unit < end {
                    assert_eq!(volume.upcase().map(unit as u16), unit as u16);
                    unit += 1;
                }
            } else {
                assert_eq!(volume.upcase().map(unit as u16), mapped);
                unit += 1;
            }
        }
        assert_eq!(unit, 65536);
    }
}

#[test]
fn windows_layout_every_file_hash_matches_the_read_only_macos_driver() {
    for sector in [512, 4096] {
        let (image, _) = reference(sector);
        let volume = Volume::open(Bytes(image)).unwrap();
        let report = fs_exfat::check::check(&volume).unwrap();
        assert!(report.healthy());
        assert_eq!((report.files, report.directories), (4, 1));
        let mut observed = BTreeMap::new();
        for entry in volume.root_directory().unwrap() {
            assert!(!entry.is_directory());
            let file = volume.open_file(&entry).unwrap();
            let mut offset = 0;
            let mut hash = Sha256::new();
            let mut buffer = [0; 8191];
            loop {
                let count = file.read_at(offset, &mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
                offset += count as u64;
            }
            assert!(observed
                .insert(
                    entry.name().to_owned(),
                    format!("{offset}\t{:x}", hash.finalize())
                )
                .is_none());
        }
        let expected =
            std::fs::read_to_string(directory().join(format!("windows-files-{sector}.txt")))
                .unwrap();
        let expected: BTreeMap<_, _> = expected
            .lines()
            .map(|line| {
                let (name, record) = line.split_once('\t').unwrap();
                (name.to_owned(), record.to_owned())
            })
            .collect();
        assert_eq!(observed, expected);
        assert_eq!(observed.len(), 4);
    }
}
