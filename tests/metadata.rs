mod support;

use fs_exfat::metadata::{
    checksum32, set_checksum, AllocationBitmap, FatEntry, RootMetadata, UpcaseTable,
};
use fs_exfat::volume::{ReadLimits, Volume};
use support::{metadata_volume, put16, put32, put64, seal_boot, upcase_bytes, Bytes};

#[test]
fn reference_shaped_system_streams_load() {
    let volume = Volume::open(Bytes(metadata_volume())).unwrap();
    assert_eq!(volume.metadata().label.as_deref(), Some("M1TEST"));
    assert!(volume.metadata().guid.is_none());
    assert_eq!(volume.cluster_chain(2).unwrap(), [2]);
    assert!(volume.bitmap().allocated(2).unwrap());
    assert!(!volume.bitmap().allocated(5).unwrap());
    assert_eq!(volume.upcase().map(97), 65);
}

#[test]
fn bitmap_is_authoritative_even_when_free_clusters_have_stale_fat_entries() {
    let mut data = metadata_volume();
    put32(&mut data, 24 * 512 + 5 * 4, 0xffff_ffff);
    let volume = Volume::open(Bytes(data)).unwrap();
    assert_eq!(volume.fat_entry(5).unwrap(), FatEntry::End);
    assert!(!volume.bitmap().allocated(5).unwrap());
    assert!(volume.cluster_chain(5).is_err());
}

#[test]
fn cycles_bad_free_reserved_and_out_of_range_links_are_refused() {
    for value in [2, 0, 0xffff_fff7, 0xffff_fff8, 102] {
        let mut data = metadata_volume();
        put32(&mut data, 24 * 512 + 2 * 4, value);
        assert!(Volume::open(Bytes(data)).is_err(), "{value}");
    }
    assert!(FatEntry::parse(1, 100).is_err());
}

#[test]
fn system_stream_allocation_and_cross_links_are_checked() {
    let mut data = metadata_volume();
    data[33 * 512] &= !1;
    assert!(Volume::open(Bytes(data)).is_err());
    let mut data = metadata_volume();
    put32(&mut data, 32 * 512 + 52, 3);
    assert!(Volume::open(Bytes(data)).is_err());
}

#[test]
fn upcase_checksum_complete_range_and_ascii_mappings_are_mandatory() {
    let data = upcase_bytes();
    let table = UpcaseTable::parse(&data, checksum32(&data)).unwrap();
    assert_eq!(table.map(65535), 65535);
    assert!(table.equal(&[97, 98], &[65, 66]));
    assert!(!table.equal(&[97], &[65, 0]));
    assert!(UpcaseTable::parse(&data, 0).is_err());
    for data in [
        vec![],
        vec![0],
        vec![0xff, 0xff],
        vec![0xff, 0xff, 0, 0],
        vec![0; 256],
        vec![0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 2, 0],
    ] {
        assert!(UpcaseTable::parse(&data, checksum32(&data)).is_err());
    }
    let mut expanded = Vec::new();
    for unit in 0u32..65536 {
        let value = if (97..=122).contains(&unit) {
            unit - 32
        } else {
            unit
        };
        expanded.extend_from_slice(&(value as u16).to_le_bytes());
    }
    assert!(UpcaseTable::parse(&expanded, checksum32(&expanded)).is_ok());
    expanded[0] = 1;
    assert!(UpcaseTable::parse(&expanded, checksum32(&expanded)).is_err());
}

#[test]
fn root_metadata_requires_unique_mandatory_entries_and_valid_labels() {
    for (offset, value) in [(0, 0), (32, 0), (0, 0x84), (65, 12), (66, 0)] {
        let mut data = metadata_volume();
        data[32 * 512 + offset] = value;
        assert!(Volume::open(Bytes(data)).is_err(), "offset {offset}");
    }
    let data = metadata_volume();
    let mut root = data[32 * 512..33 * 512].to_vec();
    root.copy_within(..32, 96);
    assert!(RootMetadata::parse(&root, 1).is_err());
    assert!(RootMetadata::parse(&root[..31], 1).is_err());
}

#[test]
fn volume_guid_checksum_null_and_secondary_count_are_validated() {
    let mut data = metadata_volume();
    let start = 32 * 512 + 96;
    data[start] = 0xa0;
    data[start + 6..start + 22].copy_from_slice(&[1; 16]);
    let checksum = set_checksum(&data[start..start + 32]);
    put16(&mut data, start + 2, checksum);
    assert_eq!(
        Volume::open(Bytes(data.clone())).unwrap().metadata().guid,
        Some([1; 16])
    );
    for offset in [1, 2, 4, 6] {
        let mut damaged = data.clone();
        damaged[start + offset] ^= 1;
        assert!(Volume::open(Bytes(damaged)).is_err());
    }
}

#[test]
fn active_fat_and_matching_bitmap_are_selected_together() {
    let mut data = metadata_volume();
    data.copy_within(32 * 512..35 * 512, 40 * 512);
    data[32 * 512..35 * 512].fill(0);
    data.copy_within(24 * 512..32 * 512, 32 * 512);
    put32(&mut data, 24 * 512 + 2 * 4, 0);
    for start in [0, 12 * 512] {
        put32(&mut data, start + 88, 40);
        put16(&mut data, start + 106, 1);
        data[start + 110] = 2;
        seal_boot(&mut data[start..], 512);
    }
    let root = 40 * 512;
    data.copy_within(root..root + 32, root + 96);
    data[root + 97] = 1;
    put32(&mut data, root + 116, 5);
    put64(&mut data, root + 120, 13);
    put32(&mut data, 32 * 512 + 5 * 4, 0xffff_ffff);
    data[41 * 512] = 0;
    data[43 * 512] = 15;
    let volume = Volume::open(Bytes(data.clone())).unwrap();
    assert!(volume.bitmap().allocated(5).unwrap());
    assert_eq!(volume.fat_entry(2).unwrap(), FatEntry::End);
    data[root + 97] = 0;
    assert!(Volume::open(Bytes(data)).is_err());
}

#[test]
fn metadata_limits_and_bitmap_bounds_are_enforced() {
    assert!(Volume::open_with_limits(
        Bytes(metadata_volume()),
        ReadLimits {
            max_metadata_bytes: 511,
            max_chain_clusters: 1
        }
    )
    .is_err());
    assert!(Volume::open_with_limits(
        Bytes(metadata_volume()),
        ReadLimits {
            max_metadata_bytes: 4096,
            max_chain_clusters: 0
        }
    )
    .is_err());
    assert!(AllocationBitmap::parse(vec![], 100).is_err());
    let bitmap = AllocationBitmap::parse(vec![0xff; 13], 100).unwrap();
    assert!(bitmap.allocated(1).is_err());
    assert!(bitmap.allocated(102).is_err());
    assert!(bitmap.allocated(101).unwrap());
}
