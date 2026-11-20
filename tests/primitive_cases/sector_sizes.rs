//! The primitive structure reader and the salvage adapter open volumes of
//! every sector size the boot sector allows, and place the FAT, the bitmap,
//! the root directory and the boot checksum regions from the volume's own
//! sector size. The volumes are formatted by this crate's formatter and filled
//! by its writer, so these prove the reader agrees with the layout the writer
//! and formatter produce at each sector size. `tests/kernel.rs` and
//! `tests/oracle` compare the same listings against the Linux kernel and macOS.

use crate::write_support::Memory;
use fs_core::BlockRead;
use fs_exfat::mkfs::{self, FormatOptions};
use fs_exfat::primitives::{find_sets, inspect_structures, read_structures, Limits, StructureCopy};
use fs_exfat::salvage::{CandidateKind, CandidateState, ExfatSalvage, Extent, SalvageAdapter};
use fs_exfat::write::WriteVolume;
use sha2::{Digest, Sha256};

const VOLUME_BYTES: u64 = 8 << 20;
const CLUSTER_BYTES: u32 = 4096;

fn content(seed: u32, length: usize) -> Vec<u8> {
    (0..length as u32)
        .map(|index| (index.wrapping_mul(2_654_435_761).wrapping_add(seed) >> 13) as u8)
        .collect()
}

/// The same files, formatted and written at the given sector size.
fn volume(sector: u32) -> Vec<u8> {
    let device = Memory::new(vec![0; VOLUME_BYTES as usize]);
    mkfs::format(
        &device,
        &FormatOptions {
            bytes_per_sector: Some(sector),
            cluster_size: Some(CLUSTER_BYTES),
            volume_serial: Some(0x1234_5678),
            label: Some("SECTORS".into()),
            ..FormatOptions::default()
        },
    )
    .unwrap();
    let mut mounted = WriteVolume::mount(device.clone()).unwrap();
    mounted.write_file("/hello.txt", &content(1, 100)).unwrap();
    mounted.write_file("/big.bin", &content(2, 30_000)).unwrap();
    mounted.mkdir("/dir").unwrap();
    mounted
        .write_file("/dir/inner.bin", &content(3, 9_000))
        .unwrap();
    mounted.unmount().unwrap();
    device.snapshot()
}

struct Source(Vec<u8>);

impl BlockRead for Source {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        let start = offset as usize;
        output.copy_from_slice(self.0.get(start..start + output.len()).ok_or(
            fs_core::Error::OutOfBounds {
                offset,
                len: output.len() as u64,
                size: self.0.len() as u64,
            },
        )?);
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.0.len() as u64
    }
}

fn structures(image: &[u8]) -> Result<StructureCopy, fs_exfat::primitives::StructureError> {
    read_structures(|offset, output| {
        let start = offset as usize;
        output.copy_from_slice(&image[start..start + output.len()]);
        Ok(())
    })
}

/// Every live file and directory the salvage adapter finds: name, size and the
/// SHA-256 of the bytes its extents hold, in name order.
fn listing(image: &[u8]) -> Vec<(String, bool, u64, String)> {
    let source = Source(image.to_vec());
    let adapter = ExfatSalvage::probe(&source);
    let mut found = Vec::new();
    adapter
        .find_candidates(&source, Extent::new(0, source.size_bytes()), &mut found)
        .unwrap();
    let mut listing = Vec::new();
    for candidate in found
        .iter()
        .filter(|candidate| candidate.state == CandidateState::Live)
        .filter(|candidate| matches!(candidate.kind, CandidateKind::File))
    {
        let walk = adapter.walk_extents(&source, candidate).unwrap();
        let mut hash = Sha256::new();
        for extent in walk.sources() {
            let mut bytes = vec![0; extent.length as usize];
            adapter.read_extent(&source, extent, 0, &mut bytes).unwrap();
            hash.update(&bytes);
        }
        listing.push((
            candidate.name.clone().unwrap(),
            false,
            candidate.size.unwrap(),
            format!("{:x}", hash.finalize()),
        ));
    }
    listing.sort();
    listing
}

#[test]
fn the_structure_reader_opens_a_4096_byte_sector_volume() {
    let image = volume(4096);
    let copy = structures(&image).expect("a 4096-byte-sector volume opens");
    let geometry = inspect_structures(&copy).expect("and its structures decode");
    assert_eq!(geometry.boot.bytes_per_sector(), 4096);
    assert_eq!(copy.main.len(), 12 * 4096, "the boot region is 12 sectors");
    assert_eq!(copy.backup, image[12 * 4096..24 * 4096]);
    assert_eq!(copy.main, image[..12 * 4096]);
    assert_eq!(
        copy.fat.len() as u64,
        u64::from(geometry.boot.fat_length) * 4096,
        "the FAT is read from the volume's own sector size"
    );
    assert_eq!(
        copy.fat,
        image[geometry.boot.fat_offset as usize * 4096..][..copy.fat.len()],
    );
    assert_eq!(
        geometry.boot_check.main_stored, geometry.boot_check.main_computed,
        "the main boot checksum covers eleven 4096-byte sectors"
    );
    assert_eq!(
        geometry.boot_check.backup_stored,
        geometry.boot_check.backup_computed
    );
    assert!(geometry.boot_check.regions_identical);
    let root = geometry.root_clusters[0];
    let root_position = (u64::from(geometry.boot.cluster_heap_offset) * 4096
        + u64::from(root - 2) * geometry.boot.cluster_size()) as usize;
    assert_eq!(copy.root[..64], image[root_position..root_position + 64]);
}

#[test]
fn every_supported_sector_size_opens_and_decodes() {
    for sector in [512u32, 1024, 2048, 4096] {
        let image = volume(sector);
        let copy = structures(&image).unwrap_or_else(|error| panic!("{sector}: {error}"));
        let geometry =
            inspect_structures(&copy).unwrap_or_else(|error| panic!("{sector}: {error}"));
        assert_eq!(geometry.boot.bytes_per_sector(), u64::from(sector));
        assert_eq!(
            geometry.boot_check.main_stored,
            geometry.boot_check.main_computed
        );
        assert_eq!(
            geometry.boot_check.backup_stored,
            geometry.boot_check.backup_computed
        );
    }
}

#[test]
fn salvage_lists_the_same_files_and_bytes_at_512_and_4096_bytes_per_sector() {
    let small = listing(&volume(512));
    assert_eq!(
        small
            .iter()
            .map(|entry| entry.0.as_str())
            .collect::<Vec<_>>(),
        ["big.bin", "hello.txt", "inner.bin"],
    );
    assert_eq!(
        small.iter().map(|entry| entry.2).collect::<Vec<_>>(),
        [30_000, 100, 9_000]
    );
    for sector in [1024u32, 2048, 4096] {
        assert_eq!(listing(&volume(sector)), small, "{sector} bytes per sector");
    }
}

#[test]
fn the_structure_reader_finds_the_same_root_entries_at_each_sector_size() {
    let root = |sector: u32| {
        let copy = structures(&volume(sector)).unwrap();
        let boot = inspect_structures(&copy).unwrap().boot;
        let limits = Limits {
            cluster_count: boot.cluster_count,
            cluster_size: boot.cluster_size(),
        };
        find_sets(&copy.root, copy.root.len(), limits)
            .into_iter()
            .filter(|set| set.in_use && set.checksum_ok)
            .map(|set| (set.name, set.is_dir, set.length))
            .collect::<Vec<_>>()
    };
    let small = root(512);
    assert_eq!(small.len(), 3, "{small:?}");
    for sector in [1024u32, 2048, 4096] {
        assert_eq!(root(sector), small, "{sector} bytes per sector");
    }
}
