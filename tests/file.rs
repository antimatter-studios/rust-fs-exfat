mod support;

use fs_core::BlockRead;
use fs_exfat::volume::Volume;
use std::sync::{Arc, Mutex};
use support::{file_set, metadata_volume, put32, put64, seal_set, Bytes};

fn image(contiguous: bool, valid: u64) -> Vec<u8> {
    let mut data = metadata_volume();
    data[33 * 512] |= if contiguous { 0x18 } else { 0x28 };
    let mut set = file_set("file.bin", 0x20, 5, 1024, contiguous);
    put64(&mut set, 40, valid);
    seal_set(&mut set);
    data[32 * 512 + 96..32 * 512 + 96 + set.len()].copy_from_slice(&set);
    if !contiguous {
        put32(&mut data, 24 * 512 + 5 * 4, 7);
        put32(&mut data, 24 * 512 + 7 * 4, 0xffff_ffff);
    }
    data[35 * 512..36 * 512].fill(0xa5);
    let second = if contiguous { 36 } else { 37 };
    data[second * 512..(second + 1) * 512].fill(0x5a);
    data
}

#[test]
fn contiguous_files_ignore_free_and_bad_fat_entries() {
    let mut data = image(true, 1024);
    put32(&mut data, 24 * 512 + 5 * 4, 0xffff_fff7);
    let volume = Volume::open(Bytes(data)).unwrap();
    let entry = volume.root_directory().unwrap().remove(0);
    let file = volume.open_file(&entry).unwrap();
    assert_eq!(file.len(), 1024);
    assert!(!file.is_empty());
    let mut output = [0; 700];
    assert_eq!(file.read_at(400, &mut output).unwrap(), 624);
    assert!(output[..112].iter().all(|byte| *byte == 0xa5));
    assert!(output[112..624].iter().all(|byte| *byte == 0x5a));
    assert!(output[624..].iter().all(|byte| *byte == 0));
}

#[test]
fn fragmented_files_follow_the_fat_instead_of_physical_neighbors() {
    let volume = Volume::open(Bytes(image(false, 1024))).unwrap();
    let entry = volume.root_directory().unwrap().remove(0);
    let file = volume.open_file(&entry).unwrap();
    let mut output = [0; 1024];
    assert_eq!(file.read_at(0, &mut output).unwrap(), 1024);
    assert!(output[..512].iter().all(|byte| *byte == 0xa5));
    assert!(output[512..].iter().all(|byte| *byte == 0x5a));
}

#[test]
fn valid_data_length_never_exposes_uninitialized_bytes() {
    for contiguous in [false, true] {
        for valid in [0, 1, 511, 512, 513, 1024] {
            let volume = Volume::open(Bytes(image(contiguous, valid))).unwrap();
            let entry = volume.root_directory().unwrap().remove(0);
            let file = volume.open_file(&entry).unwrap();
            let mut output = [0xff; 1024];
            assert_eq!(file.valid_data_length(), valid);
            assert_eq!(file.read_at(0, &mut output).unwrap(), 1024);
            assert!(output[valid as usize..].iter().all(|byte| *byte == 0));
            if valid > 0 {
                assert_eq!(output[0], 0xa5);
            }
        }
    }
}

struct Observed {
    data: Bytes,
    reads: Arc<Mutex<Vec<u64>>>,
}

impl BlockRead for Observed {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        self.reads.lock().unwrap().push(offset);
        self.data.read_at(offset, output)
    }

    fn size_bytes(&self) -> u64 {
        self.data.size_bytes()
    }
}

#[test]
fn invalid_tail_and_eof_reads_do_not_touch_the_device() {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let volume = Volume::open(Observed {
        data: Bytes(image(true, 513)),
        reads: reads.clone(),
    })
    .unwrap();
    let entry = volume.root_directory().unwrap().remove(0);
    let file = volume.open_file(&entry).unwrap();
    reads.lock().unwrap().clear();
    let mut output = [0xff; 511];
    assert_eq!(file.read_at(513, &mut output).unwrap(), 511);
    assert_eq!(output, [0; 511]);
    assert_eq!(file.read_at(1024, &mut output).unwrap(), 0);
    assert_eq!(file.read_at(u64::MAX, &mut output).unwrap(), 0);
    assert_eq!(file.read_at(0, &mut []).unwrap(), 0);
    assert!(reads.lock().unwrap().is_empty());
    assert_eq!(file.read_at(512, &mut output).unwrap(), 511);
    assert_eq!(output[0], 0x5a);
    assert!(output[1..].iter().all(|byte| *byte == 0));
    assert_eq!(*reads.lock().unwrap(), [36 * 512]);
}

#[test]
fn allocation_short_long_cyclic_and_free_chains_are_refused() {
    for value in [0, 0xffff_ffff, 5, 4] {
        let mut data = image(false, 1024);
        put32(&mut data, 24 * 512 + 5 * 4, value);
        let volume = Volume::open(Bytes(data)).unwrap();
        let entry = volume.root_directory().unwrap().remove(0);
        assert!(volume.open_file(&entry).is_err());
    }
    let mut data = image(true, 1024);
    data[33 * 512] &= !0x10;
    let volume = Volume::open(Bytes(data)).unwrap();
    assert!(volume
        .open_file(&volume.root_directory().unwrap()[0])
        .is_err());
    let mut data = image(false, 1024);
    data[33 * 512] |= 0x10;
    put32(&mut data, 24 * 512 + 7 * 4, 6);
    put32(&mut data, 24 * 512 + 6 * 4, 0xffff_ffff);
    let volume = Volume::open(Bytes(data)).unwrap();
    assert!(volume
        .open_file(&volume.root_directory().unwrap()[0])
        .is_err());
}

#[test]
fn empty_files_and_directories_have_distinct_open_semantics() {
    let volume = Volume::open(Bytes(metadata_volume())).unwrap();
    let empty = fs_exfat::directory::parse(
        &file_set("empty", 0x20, 0, 0, false),
        volume.boot(),
        volume.upcase(),
        false,
    )
    .unwrap();
    let file = volume.open_file(&empty[0]).unwrap();
    assert!(file.is_empty());
    assert_eq!(file.read_at(0, &mut [1]).unwrap(), 0);
    let directory = fs_exfat::directory::parse(
        &file_set("dir", 0x10, 0, 0, false),
        volume.boot(),
        volume.upcase(),
        false,
    )
    .unwrap();
    assert!(volume.open_file(&directory[0]).is_err());
}
