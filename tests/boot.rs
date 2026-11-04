mod support;

use fs_exfat::{
    boot::{boot_checksum, BootRegions, BootSector},
    device::Reader,
};
use support::{put32, put64, seal_boot, volume, Bytes};

fn inspect(data: Vec<u8>) -> BootRegions {
    BootRegions::read(&Reader::new(Bytes(data))).unwrap()
}

#[test]
fn all_sector_sizes_have_two_valid_boot_copies() {
    for shift in 9..=12 {
        let report = inspect(volume(shift));
        assert!(report.strict().is_ok(), "{report:?}");
        assert!(report.differences.is_empty());
        assert_eq!(report.strict().unwrap().bytes_per_sector(), 1 << shift);
    }
}

#[test]
fn mutable_fields_are_excluded_but_reported() {
    let mut data = volume(9);
    data[106] = 2;
    data[107] = 1;
    data[112] = 65;
    let report = inspect(data);
    assert!(report.strict().is_ok());
    assert_eq!(report.differences.len(), 3);
    assert!(report
        .differences
        .iter()
        .all(|difference| difference.is_mutable()));
}

#[test]
fn every_checksum_repetition_is_validated() {
    let mut data = volume(9);
    data[11 * 512 + 128] ^= 1;
    let report = inspect(data);
    assert!(!report.main.checksum_valid);
    assert!(report.backup.valid());
    assert!(report.strict().is_err());
}

#[test]
fn checksums_cover_extended_oem_and_reserved_sectors() {
    for position in [120, 512 + 15, 9 * 512 + 39, 10 * 512 + 13] {
        let mut data = volume(9);
        data[position] ^= 1;
        assert!(!inspect(data).main.checksum_valid);
    }
}

#[test]
fn extended_signatures_are_checked_even_with_a_valid_checksum() {
    let mut data = volume(9);
    data[1023] ^= 1;
    seal_boot(&mut data, 512);
    let report = inspect(data);
    assert!(report.main.checksum_valid);
    assert!(!report.main.extended_signatures_valid);
    assert!(report.strict().is_err());
}

#[test]
fn valid_but_different_geometry_is_reported_and_refused() {
    let mut data = volume(9);
    put32(&mut data[12 * 512..], 96, 3);
    seal_boot(&mut data[12 * 512..], 512);
    let report = inspect(data);
    assert!(report.main.valid() && report.backup.valid());
    assert!(report
        .differences
        .iter()
        .any(|difference| difference.offset == 96));
    assert!(report.strict().is_err());
}

#[test]
fn damaged_main_sector_size_still_reports_backup() {
    for shift in 9..=12 {
        let mut data = volume(shift);
        data[108] = 0xff;
        let report = inspect(data);
        assert!(!report.main.valid());
        assert!(report.backup.valid());
        assert!(report.strict().is_err());
    }
}

#[test]
fn plausible_but_wrong_main_sector_size_does_not_hide_the_backup() {
    let mut data = volume(9);
    data[108] = 10;
    let report = inspect(data);
    assert!(!report.main.valid());
    assert!(report.backup.valid(), "backup {:?}", report.backup.sector);
    assert!(report.strict().is_err());
}

#[test]
fn geometry_cannot_escape_device_or_overlap_structures() {
    for (offset, value) in [
        (80, 23),
        (84, 0),
        (88, 24),
        (92, 0),
        (92, u32::MAX),
        (96, 1),
        (96, 102),
    ] {
        let mut data = volume(9);
        put32(&mut data, offset, value);
        assert!(
            BootSector::parse(&data).is_err(),
            "offset {offset} value {value}"
        );
    }
    let mut data = volume(9);
    put64(&mut data, 72, u64::MAX);
    assert!(BootSector::parse(&data).is_err());
    let mut data = volume(9);
    put64(&mut data, 72, 8192);
    seal_boot(&mut data, 512);
    assert!(!inspect(data).main.fits_device);
}

#[test]
fn malformed_and_truncated_inputs_never_panic() {
    for length in 0..512 {
        assert!(BootSector::parse(&vec![0; length]).is_err());
    }
    for sector in [0, 1, 513, usize::MAX] {
        assert!(boot_checksum(&[], sector).is_err());
    }
    assert!(BootRegions::read(&Reader::new(Bytes(vec![0; 512]))).is_err());
    for (offset, value) in [
        (0, 0),
        (3, 0),
        (11, 1),
        (108, 8),
        (108, 13),
        (109, 17),
        (110, 0),
        (110, 3),
        (112, 101),
        (510, 0),
    ] {
        let mut data = volume(9);
        data[offset] = value;
        assert!(BootSector::parse(&data).is_err(), "offset {offset}");
    }
}

#[test]
fn cluster_addresses_are_bounded() {
    let boot = BootSector::parse(&volume(9)).unwrap();
    assert_eq!(boot.cluster_offset(2).unwrap(), 32 * 512);
    assert_eq!(boot.cluster_offset(101).unwrap(), 131 * 512);
    assert!(boot.cluster_offset(1).is_err());
    assert!(boot.cluster_offset(102).is_err());
}
