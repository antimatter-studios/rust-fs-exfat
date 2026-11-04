#[path = "../support/mod.rs"]
mod support;

use fs_exfat::volume::Volume;
use std::path::PathBuf;
use support::Bytes;

#[test]
fn macos_formatter_system_metadata_is_readable() {
    let directory =
        PathBuf::from(std::env::var("EXFAT_ORACLE_DIR").expect("run chore test:oracle on macOS"));
    for sector in [512, 4096] {
        let image = directory.join(format!("reference-{sector}.dmg"));
        assert!(std::fs::symlink_metadata(&image)
            .unwrap()
            .file_type()
            .is_file());
        let volume = Volume::open(Bytes(std::fs::read(image).unwrap())).unwrap();
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
