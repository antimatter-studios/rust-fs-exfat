#[path = "../support/mod.rs"]
mod support;

#[path = "../support/image.rs"]
mod image_support;

use fs_exfat::primitives::RawUpcaseTable;
use fs_exfat::salvage::ExfatSalvage;
use fs_exfat::volume::Volume;
use image_support::Image;
use std::path::PathBuf;

#[test]
fn macos_formatter_system_metadata_is_readable() {
    let directory = PathBuf::from(
        std::env::var("EXFAT_ORACLE_DIR").expect("run chore test:oracle-macos on macOS"),
    );
    for sector in [512, 4096] {
        let image = directory.join(format!("reference-{sector}.dmg"));
        assert!(std::fs::symlink_metadata(&image)
            .unwrap()
            .file_type()
            .is_file());
        let volume = Volume::open(Image::open(image)).unwrap();
        assert_eq!(volume.metadata().label.as_deref(), Some("M1ORACLE"));
        assert_eq!(volume.metadata().upcase_checksum, 0xe619_d30d);
        assert_eq!(volume.upcase().map(u16::from(b'z')), u16::from(b'Z'));
        let guid = volume.metadata().guid.unwrap();
        let guid_text = format!(
            "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            u32::from_le_bytes(guid[..4].try_into().unwrap()),
            u16::from_le_bytes(guid[4..6].try_into().unwrap()),
            u16::from_le_bytes(guid[6..8].try_into().unwrap()),
            guid[8],
            guid[9],
            guid[10],
            guid[11],
            guid[12],
            guid[13],
            guid[14],
            guid[15]
        );
        let driver =
            std::fs::read_to_string(directory.join(format!("driver-{sector}.txt"))).unwrap();
        assert_eq!(driver, format!("M1ORACLE\n{guid_text}\nread-only\n"));
        let formatter =
            std::fs::read_to_string(directory.join(format!("format-{sector}.txt"))).unwrap();
        for (label, expected) in [
            (
                "Bitmap start",
                u64::from(volume.metadata().bitmaps[0].unwrap().first_cluster),
            ),
            (
                "Bitmap file size",
                volume.metadata().bitmaps[0].unwrap().length,
            ),
            (
                "Upcase start",
                u64::from(volume.metadata().upcase.first_cluster),
            ),
            ("Upcase file size", volume.metadata().upcase.length),
        ] {
            let observed: u64 = formatter
                .lines()
                .find(|line| line.split(':').next().unwrap().trim() == label)
                .unwrap()
                .split(':')
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(expected, observed, "{label}");
        }
    }
}

#[test]
fn macos_formatter_writes_the_compressed_recommended_upcase_table() {
    // Section 7.2.5.1: the recommended table, in compressed form, has
    // TableChecksum E619D30Dh. newfs_exfat writes it, and the strict reader
    // and salvage expand it to the same 65536 mappings (#42).
    let directory = PathBuf::from(
        std::env::var("EXFAT_ORACLE_DIR").expect("run chore test:oracle-macos on macOS"),
    );
    for sector in [512, 4096] {
        let image = Image::open(directory.join(format!("reference-{sector}.dmg")));
        let source = image.clone();
        let report = ExfatSalvage::probe(&source).upcase_table(&source).unwrap();
        let mut table = Vec::new();
        for extent in &report.extents {
            table.extend_from_slice(
                &image.read(extent.offset as usize..extent.end().unwrap() as usize),
            );
        }
        assert_eq!(table.len(), 5836, "{sector}");
        let raw = RawUpcaseTable::parse(&table, 0xe619_d30d);
        assert!(
            raw.checksum_ok && raw.compressed && raw.usable(),
            "{sector}"
        );
        assert!(report.compressed && report.usable, "{sector}");
        assert_eq!(report.mappings, raw.mappings, "{sector}");
        let volume = Volume::open(source).unwrap();
        for unit in 0..=u16::MAX {
            assert_eq!(volume.upcase().map(unit), raw.mappings[usize::from(unit)]);
        }
    }
}
