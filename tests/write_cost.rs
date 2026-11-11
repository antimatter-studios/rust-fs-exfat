//! What a write costs as a volume grows (#153). A mount caches the
//! directories and cluster chains it has validated, so the device reads one
//! call needs do not grow with the depth of its path, the length of the
//! file's chain or the size of the directory it changes. Device reads are
//! counted rather than timed, so the measure is exact on any host.

mod write_support;

use fs_exfat::volume::Volume;
use fs_exfat::write::WriteVolume;
use write_support::{format, Layout, Memory};

const MIB: usize = 1024 * 1024;

fn mounted(layout: &Layout) -> (WriteVolume<Memory>, Memory) {
    let (image, _) = format(layout);
    let device = Memory::new(image);
    (WriteVolume::mount(device.clone()).unwrap(), device)
}

/// The device reads `step` makes: calls and bytes.
fn cost(
    volume: &mut WriteVolume<Memory>,
    device: &Memory,
    step: impl FnOnce(&mut WriteVolume<Memory>),
) -> (u64, u64) {
    let before = device.read_count();
    step(volume);
    let after = device.read_count();
    (after.0 - before.0, after.1 - before.1)
}

#[test]
fn a_write_reads_no_more_at_the_end_of_a_deep_path() {
    let (mut volume, device) = mounted(&Layout::default());
    volume.mkdir("/shallow").unwrap();
    volume.write_file("/shallow/file", &[1; 4096]).unwrap();
    let mut deep = String::new();
    for level in 0..24 {
        deep.push_str(&format!("/level {level}"));
        volume.mkdir(&deep).unwrap();
    }
    let deep = format!("{deep}/file");
    volume.write_file(&deep, &[1; 4096]).unwrap();
    for path in ["/shallow/file", deep.as_str()] {
        volume.pwrite(path, 0, &[2; 4096]).unwrap();
    }
    let shallow = cost(&mut volume, &device, |volume| {
        volume.pwrite("/shallow/file", 0, &[3; 4096]).unwrap();
    });
    let deeper = cost(&mut volume, &device, |volume| {
        volume.pwrite(&deep, 0, &[3; 4096]).unwrap();
    });
    assert_eq!(deeper, shallow, "24 directories deeper reads the same");
    volume.unmount().unwrap();
}

/// A 256 MiB file written in 1 MiB calls, with a one-cluster file grown
/// between them so that the large one is a FAT chain of 256 runs.
#[test]
fn a_256_mib_fat_chained_file_written_in_1_mib_calls_reads_the_same_per_call() {
    let layout = Layout {
        bytes: 272 * MIB as u64,
        ..Layout::default()
    };
    let (mut volume, device) = mounted(&layout);
    volume.create("/large.bin").unwrap();
    volume.create("/between.bin").unwrap();
    let chunk = vec![0x5a; MIB];
    let mut calls = Vec::new();
    for index in 0..256 {
        calls.push(cost(&mut volume, &device, |volume| {
            volume
                .pwrite("/large.bin", (index * MIB) as u64, &chunk)
                .unwrap();
        }));
        volume
            .pwrite("/between.bin", index as u64 * 4096, &[index as u8; 4096])
            .unwrap();
    }
    let entry = volume
        .reader()
        .lookup("/large.bin")
        .unwrap()
        .entry()
        .unwrap()
        .clone();
    assert_eq!(entry.stream().data_length(), 256 * MIB as u64);
    assert!(!entry.stream().no_fat_chain(), "the file is a FAT chain");
    // Each call reads the few FAT, bitmap and directory sectors it changes,
    // so one that crosses into a new sector may read one or two more.
    let most = |calls: &[(u64, u64)]| calls.iter().map(|cost| cost.0).max().unwrap();
    assert!(
        most(&calls[192..]) <= most(&calls[8..64]) + 2,
        "a call at the end of the chain reads no more than one near its start: {:?} against {:?}",
        &calls[192..],
        &calls[8..64]
    );
    let start = cost(&mut volume, &device, |volume| {
        volume.pwrite("/large.bin", 0, &chunk[..4096]).unwrap();
    });
    let end = cost(&mut volume, &device, |volume| {
        volume
            .pwrite("/large.bin", (256 * MIB - 4096) as u64, &chunk[..4096])
            .unwrap();
    });
    assert_eq!(
        end, start,
        "an overwrite at the end reads what one at the start does"
    );
    volume.unmount().unwrap();
}

/// 10,000 files created in the root directory, which grows past 900 KiB.
#[test]
fn ten_thousand_files_in_one_directory_read_the_same_per_file() {
    let (mut volume, device) = mounted(&Layout::default());
    let mut batches = Vec::new();
    let mut batch = (0, 0);
    for index in 0..10_000 {
        let step = cost(&mut volume, &device, |volume| {
            volume.create(&format!("/file {index:05}")).unwrap();
        });
        batch.0 += step.0;
        batch.1 += step.1;
        if index % 100 == 99 {
            batches.push(batch);
            batch = (0, 0);
        }
        // A quadratic cost shows long before the last file.
        if index == 1999 {
            assert!(
                batches[19].0 <= batches[1].0 + 10,
                "files 1,900 to 2,000 read no more than files 100 to 200: {:?} against {:?}",
                batches[19],
                batches[1]
            );
        }
    }
    assert!(
        batches[99].0 <= batches[1].0 + 10,
        "files 9,900 to 10,000 read no more than files 100 to 200: {:?} against {:?}",
        batches[99],
        batches[1]
    );
    volume.unmount().unwrap();
    let image = Volume::open(Memory::new(device.snapshot())).unwrap();
    assert_eq!(image.root_directory().unwrap().len(), 10_000);
}
