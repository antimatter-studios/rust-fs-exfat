//! Cuts the fuzz corpus from images an independent formatter wrote and the macOS
//! driver populated, and proves each seed reaches past its decoder's first check.
//! `EXFAT_FUZZ_SEEDS_OUT=$PWD/fuzz/corpus chore test:oracle-macos` writes the seeds there.

#[path = "../../fuzz/shared/mod.rs"]
mod shared;

use fs_core::BlockRead;
use fs_exfat::boot::BootSector;
use fs_exfat::metadata::SystemStream;
use fs_exfat::volume::Volume;
use shared::support::Bytes;
use std::path::{Path, PathBuf};

/// Keeps the seed small enough for the fuzzer to change usefully: the leading part of
/// a directory, ending on an entry set boundary.
fn whole_sets(directory: &[u8], limit: usize) -> &[u8] {
    let mut position = 0;
    while position + 32 <= limit.min(directory.len()) && directory[position] != 0 {
        let kind = directory[position];
        let length = if kind == 0x85 || kind & 0xe0 == 0xa0 {
            (usize::from(directory[position + 1]) + 1) * 32
        } else {
            32
        };
        if position + length > limit {
            break;
        }
        position += length;
    }
    &directory[..position]
}

fn stream<Device: BlockRead>(
    volume: &Volume<Device>,
    image: &[u8],
    system: SystemStream,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    for cluster in volume.cluster_chain(system.first_cluster).unwrap() {
        let start = volume.boot().cluster_offset(cluster).unwrap() as usize;
        bytes.extend_from_slice(&image[start..start + volume.boot().cluster_size() as usize]);
    }
    bytes.truncate(system.length as usize);
    bytes
}

fn prefixed(prefix: &[u8], body: &[u8]) -> Vec<u8> {
    [prefix, body].concat()
}

#[test]
fn seeds_cut_from_reference_images_reach_past_each_decoders_first_check() {
    let directory = PathBuf::from(
        std::env::var("EXFAT_ORACLE_DIR").expect("run chore test:oracle-macos on macOS"),
    );
    let out = std::env::var_os("EXFAT_FUZZ_SEEDS_OUT").map(PathBuf::from);
    let write = |target: &str, name: &str, bytes: &[u8]| {
        if let Some(out) = &out {
            let folder = out.join(target);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(Path::new(&folder).join(name), bytes).unwrap();
        }
    };
    let emit = |target: &str, name: &str, bytes: &[u8], run: fn(&[u8]) -> bool| {
        assert!(run(bytes), "{target}/{name} was rejected");
        write(target, name, bytes);
    };
    for sector in [512, 4096] {
        let image = std::fs::read(directory.join(format!("reference-{sector}.dmg"))).unwrap();
        let name = format!("newfs-{sector}.bin");
        let volume = Volume::open(Bytes(image.clone())).unwrap();
        let boot: &BootSector = volume.boot();
        let metadata = volume.metadata();

        emit("boot_sector", &name, &image[..512], shared::boot_sector);

        let fat = (u64::from(boot.fat_offset) * boot.bytes_per_sector()) as usize;
        for cluster in [boot.root_cluster, metadata.upcase.first_cluster] {
            let at = fat + cluster as usize * 4;
            let seed = prefixed(&image[at..at + 4], &boot.cluster_count.to_le_bytes());
            emit(
                "fat_entry",
                &format!("newfs-{sector}-{cluster}.bin"),
                &seed,
                shared::fat_entry,
            );
        }

        let bitmap = stream(&volume, &image, metadata.bitmaps[0].unwrap());
        let seed = prefixed(&boot.cluster_count.to_le_bytes(), &bitmap);
        emit("allocation_bitmap", &name, &seed, shared::allocation_bitmap);

        let upcase = stream(&volume, &image, metadata.upcase);
        emit("upcase_table", &name, &upcase, shared::upcase_table);

        let root = whole_sets(volume.root_bytes(), 4096);
        let seed = prefixed(&[boot.number_of_fats - 1], root);
        emit("root_metadata", &name, &seed, shared::root_metadata);
        let seed = prefixed(&[1], root);
        emit(
            "directory",
            &format!("root-{name}"),
            &seed,
            shared::directory,
        );

        let entries = volume.root_directory().unwrap();
        let nested = entries
            .iter()
            .find(|entry| entry.name() == "Nested")
            .unwrap();
        assert!(nested.stream().no_fat_chain());
        let start = boot
            .cluster_offset(nested.stream().first_cluster())
            .unwrap() as usize;
        let children = &image[start..start + nested.stream().data_length() as usize];
        let seed = prefixed(&[0], whole_sets(children, 4096));
        emit(
            "directory",
            &format!("nested-{name}"),
            &seed,
            shared::directory,
        );

        let file = entries
            .iter()
            .find(|entry| entry.name() == "timestamp.bin")
            .unwrap();
        let primary = file.raw_primary();
        emit("timestamps", &name, primary, shared::timestamps);
        let seed = [&primary[12..16], &primary[21..22], &primary[23..24]].concat();
        emit("timestamp", &name, &seed, shared::timestamp);
    }
    // Damage on purpose, so a seed here may rightly be refused.
    for (name, seed) in shared::image_seeds() {
        write("image", &format!("{name}.bin"), &seed);
    }
}
