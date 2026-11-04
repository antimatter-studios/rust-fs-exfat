mod support;

use fs_exfat::directory::parse;
use fs_exfat::volume::Volume;
use support::{file_set, metadata_volume, put16, put32, put64, seal_set, Bytes};

#[test]
fn valid_names_metadata_and_multiple_name_entries_decode() {
    let volume = Volume::open(Bytes(metadata_volume())).unwrap();
    for name in ["hello.txt", "a long Unicode name 😀.txt", &"n".repeat(255)] {
        let set = file_set(name, 0x20, 0, 0, false);
        let entries = parse(&set, volume.boot(), volume.upcase(), false).unwrap();
        assert_eq!(entries[0].name(), name);
        assert_eq!(entries[0].attributes(), 0x20);
        assert_eq!(entries[0].stream().data_length(), 0);
    }
}

#[test]
fn entry_sets_cross_a_fat_linked_root_cluster_boundary() {
    let mut data = metadata_volume();
    data[32 * 512 + 96..33 * 512].fill(1);
    put32(&mut data, 24 * 512 + 2 * 4, 5);
    put32(&mut data, 24 * 512 + 5 * 4, 0xffff_ffff);
    data[33 * 512] |= 8;
    let set = file_set("boundary.txt", 0x20, 0, 0, false);
    data[33 * 512 - 32..33 * 512].copy_from_slice(&set[..32]);
    data[35 * 512..35 * 512 + 64].copy_from_slice(&set[32..]);
    let volume = Volume::open(Bytes(data)).unwrap();
    assert_eq!(volume.root_directory().unwrap()[0].name(), "boundary.txt");
}

#[test]
fn checksum_count_order_and_name_hash_damage_are_refused() {
    let volume = Volume::open(Bytes(metadata_volume())).unwrap();
    let valid = file_set("hello.txt", 0x20, 0, 0, false);
    for offset in [2, 20] {
        let mut damaged = valid.clone();
        damaged[offset] ^= 1;
        assert!(parse(&damaged, volume.boot(), volume.upcase(), false).is_err());
    }
    for (offset, value) in [
        (1, 1),
        (1, 255),
        (32, 0xc1),
        (33, 0),
        (35, 0),
        (36, 0),
        (64, 0xc0),
        (65, 1),
    ] {
        let mut damaged = valid.clone();
        damaged[offset] = value;
        seal_set(&mut damaged);
        assert!(
            parse(&damaged, volume.boot(), volume.upcase(), false).is_err(),
            "{offset}"
        );
    }
    assert!(parse(&valid[..95], volume.boot(), volume.upcase(), false).is_err());
}

#[test]
fn invalid_names_and_case_folded_duplicates_are_refused() {
    let volume = Volume::open(Bytes(metadata_volume())).unwrap();
    for name in [".", "..", "bad/name", "bad\\name", "bad\0name", "bad:name"] {
        assert!(parse(
            &file_set(name, 0, 0, 0, false),
            volume.boot(),
            volume.upcase(),
            false
        )
        .is_err());
    }
    let mut set = file_set("invalid", 0, 0, 0, false);
    put16(&mut set, 66, 0xd800);
    let units: Vec<_> = "invalid"
        .encode_utf16()
        .enumerate()
        .map(|(index, unit)| if index == 0 { 0xd800 } else { unit })
        .collect();
    put16(&mut set, 36, volume.upcase().name_hash(&units));
    seal_set(&mut set);
    assert!(parse(&set, volume.boot(), volume.upcase(), false).is_err());
    let mut data = file_set("hello", 0, 0, 0, false);
    data.extend(file_set("HELLO", 0, 0, 0, false));
    assert!(parse(&data, volume.boot(), volume.upcase(), false).is_err());
}

#[test]
fn inactive_entries_end_marker_and_benign_extensions_are_handled() {
    let volume = Volume::open(Bytes(metadata_volume())).unwrap();
    let mut set = file_set("hello", 0, 0, 0, false);
    set.extend([0; 32]);
    set[96] = 0xe0;
    set[1] += 1;
    seal_set(&mut set);
    let mut data = vec![1; 32];
    data.extend(set);
    data.extend([0; 32]);
    data.extend([0x84; 32]);
    assert_eq!(
        parse(&data, volume.boot(), volume.upcase(), false)
            .unwrap()
            .len(),
        1
    );
    data[0] = 0xc1;
    assert!(parse(&data, volume.boot(), volume.upcase(), false).is_err());
}

#[test]
fn stream_geometry_and_directory_lengths_are_validated() {
    let volume = Volume::open(Bytes(metadata_volume())).unwrap();
    for (first, length, contiguous) in [
        (0, 1, false),
        (1, 512, false),
        (101, 1024, true),
        (0, 0, true),
        (5, 0, false),
    ] {
        let set = file_set("bad", 0x20, first, length, contiguous);
        assert!(parse(&set, volume.boot(), volume.upcase(), false).is_err());
    }
    let mut set = file_set("bad", 0x20, 5, 512, true);
    put64(&mut set, 40, 513);
    seal_set(&mut set);
    assert!(parse(&set, volume.boot(), volume.upcase(), false).is_err());
    assert!(parse(
        &file_set("dir", 0x10, 5, 1, true),
        volume.boot(),
        volume.upcase(),
        false
    )
    .is_err());
    let empty = parse(
        &file_set("empty", 0x10, 0, 0, false),
        volume.boot(),
        volume.upcase(),
        false,
    )
    .unwrap();
    assert!(volume.read_directory(&empty[0]).unwrap().is_empty());
}

#[test]
fn contiguous_subdirectories_ignore_stale_fat_and_require_allocation() {
    let mut data = metadata_volume();
    data[33 * 512] |= 8;
    let set = file_set("sub", 0x10, 5, 512, true);
    data[32 * 512 + 96..32 * 512 + 96 + set.len()].copy_from_slice(&set);
    let child = file_set("child", 0x20, 0, 0, false);
    data[35 * 512..35 * 512 + child.len()].copy_from_slice(&child);
    let volume = Volume::open(Bytes(data.clone())).unwrap();
    let root = volume.root_directory().unwrap();
    assert_eq!(volume.read_directory(&root[0]).unwrap()[0].name(), "child");
    data[33 * 512] &= !8;
    let volume = Volume::open(Bytes(data)).unwrap();
    assert!(volume.read_directory(&root[0]).is_err());
}

#[test]
fn benign_primary_sets_require_a_checksum_and_critical_entries_fail() {
    let volume = Volume::open(Bytes(metadata_volume())).unwrap();
    let mut data = vec![0; 64];
    data[0] = 0xa1;
    data[1] = 1;
    data[32] = 0xe0;
    seal_set(&mut data);
    assert!(parse(&data, volume.boot(), volume.upcase(), false)
        .unwrap()
        .is_empty());
    data[2] ^= 1;
    assert!(parse(&data, volume.boot(), volume.upcase(), false).is_err());
    data[0] = 0x84;
    assert!(parse(&data, volume.boot(), volume.upcase(), false).is_err());
    data[0] = 0xa0;
    data[1] = 0;
    seal_set(&mut data[..32]);
    assert!(parse(&data, volume.boot(), volume.upcase(), false).is_err());
}
