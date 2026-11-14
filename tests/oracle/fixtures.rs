use crate::image_support::{sparse_copy, Image};
use fs_exfat::fixtures::{
    created_on, entry_set, modified_on, seal_boot, seal_entry_set, special, upcase_entry,
    upcase_table, Builder, BITMAP_CLUSTER, CLUSTER, CLUSTER_COUNT, FAT_OFFSET, ROOT_CLUSTER,
    SECTOR,
};
use std::path::PathBuf;

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn length(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[test]
fn permissive_loading_and_checked_snapshots_agree_with_platform_structures() {
    use fs_exfat::primitives::{
        checked_bitmap_bit, checked_fat_entry, inspect_structures, read_structures,
    };

    for sector in [512, 4096] {
        let reference = Reference::open(sector);
        let loaded = read_structures(|offset, output| {
            let start = offset as usize;
            output.copy_from_slice(&reference.image.read(start..start + output.len()));
            Ok(())
        });
        if sector == 4096 {
            assert_eq!(
                loaded.unwrap_err().to_string(),
                "unsupported sector size 4096"
            );
            continue;
        }
        let loaded = loaded.unwrap();
        let geometry = inspect_structures(&loaded).unwrap();
        assert_eq!(geometry.boot.bytes_per_sector(), sector as u64);
        assert_eq!(geometry.boot.cluster_size(), reference.cluster as u64);
        assert_eq!(loaded.main, reference.image.read(0..sector * 12));
        assert_eq!(
            loaded.backup,
            reference.image.read(sector * 12..sector * 24)
        );
        assert_eq!(
            loaded.fat,
            reference
                .image
                .read(reference.fat..reference.fat + reference.image.word(84) as usize * sector)
        );
        assert_eq!(loaded.root, reference.root().0);
        let entry = reference.system_entry(0x81);
        let bitmap_start = reference.position(word(&entry, 20));
        assert_eq!(
            loaded.bitmap,
            reference
                .image
                .read(bitmap_start..bitmap_start + length(&entry, 24) as usize)
        );
        for cluster in 2..geometry.boot.cluster_count + 2 {
            assert_eq!(
                checked_fat_entry(&loaded.fat, cluster as usize),
                Some(reference.image.word(reference.fat + cluster as usize * 4))
            );
            let index = (cluster - 2) as usize;
            assert_eq!(
                checked_bitmap_bit(&loaded.bitmap, cluster),
                reference.image.byte(bitmap_start + index / 8) & (1 << (index % 8)) != 0
            );
        }
    }
}

#[test]
fn named_fixture_fields_match_formatter_entry_bytes_and_boot_offsets() {
    use fs_exfat::fixtures::{bitmap_entry, boot_field, BootField};

    for sector in [512, 4096] {
        let reference = Reference::open(sector);
        let entry = reference.system_entry(0x81);
        assert_eq!(bitmap_entry(word(&entry, 20), length(&entry, 24)), entry);
        for (field, start, length) in [
            (BootField::FileSystemName, 3, 8),
            (BootField::VolumeFlags, 106, 2),
            (BootField::BootSignature, 510, 2),
        ] {
            let original = reference.image.read(0..sector * 12).to_vec();
            let mut expected = original.clone();
            expected[start..start + length].fill(0xa5);
            assert_eq!(boot_field(original, field, &vec![0xa5; length]), expected);
        }
    }
}

fn directory() -> PathBuf {
    std::env::var("EXFAT_ORACLE_DIR")
        .expect("run chore test:oracle-macos on macOS to create independent reference images")
        .into()
}

struct Reference {
    image: Image,
    sector: usize,
    cluster: usize,
    heap: usize,
    fat: usize,
}

impl Reference {
    fn open(sector: usize) -> Self {
        let path = directory().join(format!("reference-{sector}.dmg"));
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_file());
        let image = Image::open(path);
        assert_eq!(1usize << image.byte(108), sector);
        Self {
            cluster: sector << image.byte(109),
            heap: image.word(88) as usize * sector,
            fat: image.word(80) as usize * sector,
            image,
            sector,
        }
    }

    fn position(&self, cluster: u32) -> usize {
        self.heap + (cluster as usize - 2) * self.cluster
    }

    fn chain(&self, first: u32) -> (Vec<u8>, Vec<usize>) {
        let mut bytes = Vec::new();
        let mut positions = Vec::new();
        let mut current = first;
        loop {
            let position = self.position(current);
            assert!(!positions.contains(&position), "cycle in platform image");
            positions.push(position);
            bytes.extend_from_slice(&self.image.read(position..position + self.cluster));
            current = self.image.word(self.fat + current as usize * 4);
            if current >= 0xffff_fff8 {
                return (bytes, positions);
            }
            assert!((2..self.image.word(92) + 2).contains(&current));
        }
    }

    fn root(&self) -> (Vec<u8>, Vec<usize>) {
        self.chain(self.image.word(96))
    }

    fn system_entry(&self, kind: u8) -> Vec<u8> {
        self.root()
            .0
            .chunks_exact(32)
            .take_while(|entry| entry[0] != 0)
            .find(|entry| entry[0] == kind)
            .expect("formatter omitted a required system entry")
            .to_vec()
    }
}

fn reencoded_set(raw: &[u8], in_use: bool) -> Vec<u8> {
    let units: Vec<u16> = raw[64..]
        .chunks_exact(32)
        .flat_map(|slot| slot[2..].chunks_exact(2))
        .map(|unit| u16::from_le_bytes(unit.try_into().unwrap()))
        .take(usize::from(raw[35]))
        .collect();
    let name = String::from_utf16(&units).unwrap();
    let mut encoded = entry_set(
        &name,
        raw[4] & 0x10 != 0,
        word(raw, 52),
        length(raw, 56),
        raw[33] & 2 != 0,
        in_use,
    );
    assert_eq!(encoded.len(), raw.len());
    assert_eq!(encoded[0], if in_use { raw[0] } else { raw[0] & 0x7f });
    assert_eq!(encoded[1], raw[1]);
    assert_eq!(encoded[4] & 0x10, raw[4] & 0x10);
    assert_eq!(&encoded[33..36], &raw[33..36]);
    assert_eq!(&encoded[52..64], &raw[52..64]);
    for (ours, platform) in encoded[64..]
        .chunks_exact(32)
        .zip(raw[64..].chunks_exact(32))
    {
        assert_eq!(&ours[1..], &platform[1..]);
        assert_eq!(ours[0], platform[0] & if in_use { 0xff } else { 0x7f });
    }
    let created = word(raw, 8);
    let modified = word(raw, 12);
    encoded = created_on(
        encoded,
        1980 + (created >> 25),
        (created >> 21) & 15,
        (created >> 16) & 31,
    );
    encoded = modified_on(
        encoded,
        1980 + (modified >> 25),
        (modified >> 21) & 15,
        (modified >> 16) & 31,
    );
    assert_eq!(word(&encoded, 8), created & 0xffff_0000);
    assert_eq!(word(&encoded, 12), modified & 0xffff_0000);
    for (start, end) in [(4, 32), (36, 40), (40, 52)] {
        encoded[start..end].copy_from_slice(&raw[start..end]);
    }
    seal_entry_set(&mut encoded);
    assert_eq!(&encoded[2..4], &raw[2..4]);
    encoded
}

#[test]
fn fixture_boot_checksums_reproduce_both_platform_boot_copies() {
    let mut builder = Builder::new();
    let original = builder.image[..SECTOR * 24].to_vec();
    builder.image[..SECTOR * 24].fill(0);
    builder.write_boot();
    assert_eq!(builder.image[..SECTOR * 24], original);
    for sector in [512, 4096] {
        let reference = Reference::open(sector);
        assert_eq!(&builder.image[..64], &reference.image.read(0..64));
        assert_eq!(&builder.image[104..106], &reference.image.read(104..106));
        assert_eq!(builder.image[110], reference.image.byte(110));
        assert_eq!(&builder.image[510..512], &reference.image.read(510..512));
        assert_eq!(CLUSTER, reference.cluster);
        for copy in [0, sector * 12] {
            let expected = &reference.image.read(copy..copy + sector * 12);
            let mut encoded = expected.to_vec();
            encoded[sector * 11..].fill(0);
            seal_boot(&mut encoded, sector);
            assert_eq!(&encoded, expected);
            encoded[106] ^= 0x02;
            encoded[107] ^= 0x80;
            encoded[112] ^= 0xff;
            seal_boot(&mut encoded, sector);
            assert_eq!(&encoded[sector * 11..], &expected[sector * 11..]);
        }
    }
}

#[test]
fn fixture_fat_bitmap_and_data_placement_match_platform_bytes() {
    for sector in [512, 4096] {
        let reference = Reference::open(sector);
        let bitmap = reference.system_entry(0x81);
        let (bitmap_bytes, _) = reference.chain(word(&bitmap, 20));
        let mut builder = Builder::new();
        for cluster in 0..CLUSTER_COUNT + 2 {
            builder.fat(
                cluster,
                reference.image.word(reference.fat + cluster as usize * 4),
            );
        }
        let fat_start = FAT_OFFSET as usize * SECTOR;
        let fat_bytes = (CLUSTER_COUNT as usize + 2) * 4;
        assert_eq!(
            &builder.image[fat_start..fat_start + fat_bytes],
            &reference
                .image
                .read(reference.fat..reference.fat + fat_bytes)
        );
        for cluster in 2..CLUSTER_COUNT + 2 {
            let index = (cluster - 2) as usize;
            if bitmap_bytes[index / 8] & (1 << (index % 8)) != 0 {
                builder.allocate(cluster);
            }
        }
        let bitmap_start = Builder::cluster_position(BITMAP_CLUSTER);
        assert_eq!(
            &builder.image[bitmap_start..bitmap_start + CLUSTER_COUNT as usize / 8],
            &bitmap_bytes[..CLUSTER_COUNT as usize / 8]
        );
        let data = &reference
            .image
            .read(reference.heap..reference.heap + CLUSTER + 37);
        builder.put(10, 17, data);
        let start = Builder::cluster_position(10) + 17;
        assert_eq!(&builder.image[start..start + data.len()], data);
    }
}

#[test]
fn fixture_system_entries_and_upcase_words_reproduce_formatter_output() {
    for sector in [512, 4096] {
        let reference = Reference::open(sector);
        let bitmap = reference.system_entry(0x81);
        assert_eq!(
            special(0x81, word(&bitmap, 20), length(&bitmap, 24)),
            bitmap
        );
        let upcase = reference.system_entry(0x82);
        let (mut table, _) = reference.chain(word(&upcase, 20));
        table.truncate(length(&upcase, 24) as usize);
        let words: Vec<_> = table
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes(bytes.try_into().unwrap()))
            .collect();
        assert_eq!(upcase_table(&words), table);
        assert_eq!(upcase_entry(word(&upcase, 20), &table), upcase);
    }
}

#[test]
fn fixture_live_deleted_sets_and_dates_match_platform_entries() {
    for sector in [512, 4096] {
        let reference = Reference::open(sector);
        let (root, _) = reference.root();
        let mut offset = 0;
        let mut count = 0;
        while root[offset] != 0 {
            let slots = if root[offset] == 0x85 {
                usize::from(root[offset + 1]) + 1
            } else {
                1
            };
            if slots > 1 {
                let raw = &root[offset..offset + slots * 32];
                assert_eq!(reencoded_set(raw, true), raw);
                let mut deleted = reencoded_set(raw, false);
                for entry in deleted.chunks_exact_mut(32) {
                    entry[0] |= 0x80;
                }
                assert_eq!(deleted, raw);
                count += 1;
            }
            offset += slots * 32;
        }
        assert!(count >= 6, "platform root lacks expected live entries");
    }
}

#[test]
fn platform_checker_accepts_reencoded_healthy_directory_forms() {
    for sector in [512, 4096] {
        let reference = Reference::open(sector);
        let (root, positions) = reference.root();
        let mut builder = Builder::new();
        let mut sets = Vec::new();
        let mut offset = 0;
        while root[offset] != 0 {
            let slots = if root[offset] == 0x85 {
                usize::from(root[offset + 1]) + 1
            } else {
                1
            };
            let raw = &root[offset..offset + slots * 32];
            sets.push(if slots > 1 {
                reencoded_set(raw, true)
            } else {
                raw.to_vec()
            });
            offset += slots * 32;
        }
        builder.directory(ROOT_CLUSTER, &sets);
        let start = Builder::cluster_position(ROOT_CLUSTER);
        assert_eq!(&builder.image[start..start + offset], &root[..offset]);
        let path = directory().join(format!("reencoded-fixtures-{}.dmg", reference.sector));
        sparse_copy(&reference.image.path, &path);
        let image = Image::open(&path);
        for (index, bytes) in builder.image[start..start + offset]
            .chunks(reference.cluster)
            .enumerate()
        {
            image.patch(positions[index], bytes);
        }
    }
}
