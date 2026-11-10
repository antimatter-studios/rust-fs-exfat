use crate::support::{put32, seal_boot, volume};
use fs_exfat::primitives::{BootRegion, BootSector, BootSectorHeader};

#[test]
fn damaged_boot_headers_return_fields_and_identity_flags() {
    for shift in 9..=12 {
        for offset in [0, 3, 11, 510] {
            let mut bytes = volume(shift);
            bytes[offset] ^= 1;
            let header = BootSectorHeader::parse(&bytes).unwrap();
            assert_eq!(header.sector.volume_length, bytes.len() as u64 >> shift);
            assert_eq!(header.sector.bytes_per_sector_shift, shift);
            assert_eq!(header.sector.cluster_count, 100);
            assert_eq!(header.jump_valid, offset != 0);
            assert_eq!(header.file_system_name_valid, offset != 3);
            assert_eq!(header.must_be_zero_valid, offset != 11);
            assert_eq!(header.sector.signature_ok, offset != 510);
            assert!(fs_exfat::boot::BootSector::parse(&bytes).is_err());
            assert_eq!(BootSector::parse(&bytes).is_err(), offset == 3);
        }
    }
}

#[test]
fn zero_boot_geometry_is_returned_without_validation() {
    let mut bytes = volume(9);
    bytes[64..113].fill(0);
    seal_boot(&mut bytes, 512);
    let region = BootRegion::parse(&bytes, 512).unwrap();
    let header = region.header;
    assert!(header.jump_valid);
    assert!(header.file_system_name_valid);
    assert!(header.must_be_zero_valid);
    assert!(header.sector.signature_ok);
    assert_eq!(header.sector.partition_offset, 0);
    assert_eq!(header.sector.volume_length, 0);
    assert_eq!(header.sector.fat_offset, 0);
    assert_eq!(header.sector.fat_length, 0);
    assert_eq!(header.sector.cluster_heap_offset, 0);
    assert_eq!(header.sector.cluster_count, 0);
    assert_eq!(header.sector.root_cluster, 0);
    assert_eq!(header.sector.revision, 0);
    assert_eq!(header.sector.bytes_per_sector_shift, 0);
    assert_eq!(header.sector.sectors_per_cluster_shift, 0);
    assert_eq!(header.sector.number_of_fats, 0);
    assert!(region.checksum_valid);
    assert!(region.extended_signatures_valid);
    assert!(fs_exfat::boot::BootSector::parse(&bytes).is_err());
}

#[test]
fn wrong_backup_checksums_return_raw_words_and_invalid_flags() {
    for shift in 9..=12 {
        let sector = 1usize << shift;
        for word_offset in [0, 4, sector - 4] {
            let mut bytes = volume(shift);
            let (main, backup) = bytes.split_at_mut(sector * 12);
            let main = BootRegion::parse(main, sector).unwrap();
            assert!(main.checksum_valid);
            assert!(main.extended_signatures_valid);
            let wrong = main.computed_checksum ^ 1;
            put32(backup, sector * 11 + word_offset, wrong);
            let backup = BootRegion::parse(backup, sector).unwrap();
            assert_eq!(backup.header.sector, main.header.sector);
            assert_eq!(backup.computed_checksum, main.computed_checksum);
            assert_eq!(
                backup.stored_checksum,
                if word_offset == 0 {
                    wrong
                } else {
                    main.stored_checksum
                }
            );
            assert!(!backup.checksum_valid);
            assert!(backup.extended_signatures_valid);
        }
    }
}

#[test]
fn damaged_extended_signatures_are_reported_without_rejection() {
    for shift in 9..=12 {
        let sector = 1usize << shift;
        for index in 1..=8 {
            let mut bytes = volume(shift);
            bytes[(index + 1) * sector - 1] ^= 1;
            seal_boot(&mut bytes, sector);
            let region = BootRegion::parse(&bytes, sector).unwrap();
            assert!(region.header.sector.signature_ok);
            assert!(region.checksum_valid);
            assert!(!region.extended_signatures_valid);
        }
    }
}

#[test]
fn boot_primitives_report_truncation_and_sector_size_errors() {
    let bytes = volume(9);
    for length in 0..512 {
        assert!(BootSectorHeader::parse(&bytes[..length]).is_err());
    }
    for sector in [0, 1, 513, usize::MAX] {
        assert_eq!(
            BootRegion::parse(&[], sector).unwrap_err().to_string(),
            "invalid exFAT: checksum sector size"
        );
    }
    for shift in 9..=12 {
        let sector = 1usize << shift;
        let bytes = volume(shift);
        for length in [0, 511, sector * 11 - 1, sector * 11, sector * 12 - 1] {
            assert_eq!(
                BootRegion::parse(&bytes[..length], sector)
                    .unwrap_err()
                    .to_string(),
                if length < sector * 11 {
                    "invalid exFAT: short boot region"
                } else {
                    "invalid exFAT: short checksum sector"
                }
            );
        }
    }
}
