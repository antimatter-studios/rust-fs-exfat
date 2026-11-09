use fs_exfat::primitives::{
    boot_checksum, decode_timestamp, decode_timestamp_with_offset, decode_utc_offset,
    find_bitmap_entry, find_sets, parse_set, set_checksum, stored_boot_checksum, u16_at, u32_at,
    u64_at, BootSector, EntrySet, Limits, BOOT_REGION_SECTORS, ENTRY_SIZE, FAT_BAD, FAT_END,
    FIRST_CLUSTER,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use unicode_normalization::UnicodeNormalization;

fn oracle_directory() -> PathBuf {
    std::env::var("EXFAT_ORACLE_DIR")
        .expect("run chore test:oracle on macOS to create independent reference images")
        .into()
}

fn formatter_value(sector: u64, label: &str) -> u64 {
    let text =
        std::fs::read_to_string(oracle_directory().join(format!("format-{sector}.txt"))).unwrap();
    text.lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == label).then(|| value.split_whitespace().next().unwrap().parse().unwrap())
        })
        .unwrap_or_else(|| panic!("newfs_exfat omitted {label}"))
}

fn reference(sector: u64) -> (Vec<u8>, BootSector) {
    let path = oracle_directory().join(format!("reference-{sector}.dmg"));
    assert!(std::fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_file());
    let image = std::fs::read(path).unwrap();
    let boot = BootSector::parse(&image[..512]).unwrap();
    (image, boot)
}

fn chain(image: &[u8], boot: &BootSector, first: u32, contiguous_length: Option<u64>) -> Vec<u8> {
    let mut current = first;
    let mut bytes = Vec::new();
    let mut seen = BTreeSet::new();
    loop {
        assert!((FIRST_CLUSTER..boot.last_cluster_exclusive()).contains(&current));
        assert!(seen.insert(current), "cycle in oracle directory chain");
        let start = boot.heap_offset() + u64::from(current - FIRST_CLUSTER) * boot.cluster_size();
        bytes.extend_from_slice(&image[start as usize..(start + boot.cluster_size()) as usize]);
        if let Some(length) = contiguous_length {
            if bytes.len() as u64 >= length {
                break;
            }
            current += 1;
        } else {
            let fat = u64::from(boot.fat_offset) * boot.bytes_per_sector();
            current = u32_at(image, (fat + u64::from(current) * 4) as usize);
            assert_ne!(current, FAT_BAD);
            if current == FAT_END {
                break;
            }
        }
    }
    bytes
}

fn walk(
    image: &[u8],
    boot: &BootSector,
    bytes: &[u8],
    prefix: &str,
    entries: &mut BTreeMap<String, EntrySet>,
) {
    assert!(prefix.split('/').count() < 64);
    let limits = Limits {
        cluster_count: boot.cluster_count,
        cluster_size: boot.cluster_size(),
    };
    for entry in find_sets(bytes, bytes.len(), limits) {
        assert!(entry.in_use && entry.checksum_ok);
        let raw = &bytes[entry.offset..entry.offset + entry.slots * ENTRY_SIZE];
        assert_eq!(set_checksum(raw), u16_at(raw, 2));
        assert_eq!(entry.attributes, u16_at(raw, 4));
        assert_eq!(entry.first_cluster, u32_at(raw, 52));
        assert_eq!(entry.valid_length, u64_at(raw, 40));
        assert_eq!(entry.length, u64_at(raw, 56));
        assert_eq!(entry.no_fat_chain, raw[33] & 2 != 0);
        assert_eq!(parse_set(bytes, entry.offset, limits).unwrap(), entry);
        let path: String = format!("{prefix}{}", entry.name).nfc().collect();
        if entry.is_dir {
            let child = chain(
                image,
                boot,
                entry.first_cluster,
                entry.no_fat_chain.then_some(entry.length),
            );
            walk(image, boot, &child, &format!("{path}/"), entries);
        }
        assert!(entries.insert(path, entry).is_none());
    }
}

#[test]
fn primitive_boot_fields_and_checksums_match_macos_formatter_and_checker() {
    for sector in [512, 4096] {
        let (image, boot) = reference(sector);
        assert_eq!(boot.file_system_name, "EXFAT   ");
        assert!(boot.signature_ok);
        assert_eq!(boot.bytes_per_sector(), sector);
        assert_eq!(boot.cluster_size(), 4096);
        assert_eq!(boot.partition_offset, 0);
        assert_eq!(boot.volume_length * sector, 64 * 1024 * 1024);
        assert_eq!(boot.revision, 0x100);
        assert_eq!(boot.number_of_fats, 1);
        assert!(!boot.volume_dirty());
        assert!(boot.percent_in_use <= 100);
        assert_eq!(boot.volume_serial, u32_at(&image, 100));
        assert_eq!(boot.volume_flags, u16_at(&image, 106));
        assert_eq!(boot.volume_length, u64_at(&image, 72));
        for (label, value) in [
            ("FAT offset", u64::from(boot.fat_offset)),
            ("# FAT sectors", u64::from(boot.fat_length)),
            ("Cluster offset", boot.heap_offset() / sector),
            (
                "# Clusters",
                u64::from(boot.last_cluster_exclusive() - FIRST_CLUSTER),
            ),
            ("Root start", u64::from(boot.root_cluster)),
        ] {
            assert_eq!(value, formatter_value(sector, label), "{label}");
        }
        for copy in [0, BOOT_REGION_SECTORS * sector] {
            let region = &image[copy as usize..(copy + BOOT_REGION_SECTORS * sector) as usize];
            let stored = stored_boot_checksum(region, sector as usize);
            assert_eq!(boot_checksum(region, sector as usize), stored);
            for word in region[11 * sector as usize..].chunks_exact(4) {
                assert_eq!(u32_at(word, 0), stored);
            }
            let mut decoded = BootSector::parse(region).unwrap();
            decoded.volume_flags = boot.volume_flags;
            decoded.percent_in_use = boot.percent_in_use;
            assert_eq!(decoded, boot);
        }
    }
}

#[test]
fn primitive_entry_sets_and_checksums_match_every_macos_driver_directory_record() {
    for sector in [512, 4096] {
        let (image, boot) = reference(sector);
        let root = chain(&image, &boot, boot.root_cluster, None);
        let mut entries = BTreeMap::new();
        walk(&image, &boot, &root, "", &mut entries);
        let observed: BTreeSet<_> = entries
            .iter()
            .map(|(path, entry)| {
                format!(
                    "{}\t{}\t{path}",
                    if entry.is_dir { 'd' } else { 'f' },
                    if entry.is_dir { 0 } else { entry.length }
                )
            })
            .collect();
        let listing =
            std::fs::read_to_string(oracle_directory().join(format!("listing-{sector}.txt")))
                .unwrap();
        let expected: BTreeSet<String> = listing.lines().map(|line| line.nfc().collect()).collect();
        assert_eq!(observed, expected);
        assert!(entries.len() > 185);
        let tail = &entries["valid-tail.bin"];
        assert_eq!((tail.length, tail.valid_length), (12288, 4096));
        assert!(entries["Empty"].is_dir);
        assert!(entries["Nested/a long file name spanning entries.txt"].slots > 3);
    }
}

#[test]
fn primitive_bitmap_location_matches_newfs_exfat_without_using_the_strict_reader() {
    for sector in [512, 4096] {
        let (image, boot) = reference(sector);
        let root = chain(&image, &boot, boot.root_cluster, None);
        let (cluster, length) = find_bitmap_entry(&root).expect("newfs_exfat must create a bitmap");
        assert_eq!(u64::from(cluster), formatter_value(sector, "Bitmap start"));
        assert_eq!(length, formatter_value(sector, "Bitmap file size"));
    }
}

#[test]
fn primitive_timestamp_strings_match_macos_stat_at_the_preserved_two_second_precision() {
    for sector in [512, 4096] {
        let (image, boot) = reference(sector);
        let root = chain(&image, &boot, boot.root_cluster, None);
        let limits = Limits {
            cluster_count: boot.cluster_count,
            cluster_size: boot.cluster_size(),
        };
        let entry = find_sets(&root, root.len(), limits)
            .into_iter()
            .find(|entry| entry.name == "timestamp.bin")
            .unwrap();
        let raw = &root[entry.offset..entry.offset + ENTRY_SIZE];
        let expected =
            std::fs::read_to_string(oracle_directory().join(format!("files-{sector}.txt")))
                .unwrap();
        let fields: Vec<_> = expected
            .lines()
            .find(|line| line.ends_with("\ttimestamp.bin"))
            .unwrap()
            .splitn(6, '\t')
            .collect();
        let mut comparisons = String::new();
        for (field, offset, text) in [
            (4, 8, entry.created),
            (2, 12, entry.modified),
            (3, 16, entry.accessed),
        ] {
            let packed = u32_at(raw, offset);
            let zone = raw[22 + (offset - 8) / 4];
            let local = decode_timestamp(packed).unwrap();
            let suffix = decode_utc_offset(zone).expect("macOS must record a valid UTC offset");
            let decoded = decode_timestamp_with_offset(packed, zone).unwrap();
            assert_eq!(decoded, local + &suffix);
            assert_eq!(text.as_deref(), Some(decoded.as_str()));
            comparisons.push_str(&format!("{decoded}\t{}\n", fields[field]));
        }
        let mut python = Command::new("/usr/bin/python3").args(["-c", "import datetime, sys\nfor line in sys.stdin:\n    text, millis = line.strip().split('\\t')\n    observed = int(datetime.datetime.fromisoformat(text).timestamp())\n    expected = int(millis) // 2000 * 2\n    assert observed == expected, (text, observed, expected)\n"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("chore test:oracle requires /usr/bin/python3");
        python
            .stdin
            .take()
            .unwrap()
            .write_all(comparisons.as_bytes())
            .unwrap();
        let output = python.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
