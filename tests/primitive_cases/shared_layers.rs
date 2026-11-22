//! rust-fs-core's three shared layers over `fs_exfat::layers::ExfatVolume`:
//! the core conformance helpers on volumes this crate formats and fills, at
//! 512 and 4096 bytes per sector, and the volume layer's verdicts on copies
//! with a damaged main boot region and with VolumeDirty set. These prove the
//! layers keep the shared contracts over this crate's own layout;
//! `tests/images.rs` compares the same layers with the Linux kernel's
//! listing of kernel-made volumes.

use crate::write_support::Memory;
use fs_core::layers::conformance::{self, Expected, Limits};
use fs_core::layers::salvage::{DamageKind, Integrity};
use fs_core::layers::{FileAPI, HealthState, OpenVolume, PrimitiveAPI, Recognition, VolumeAPI};
use fs_core::BlockRead;
use fs_exfat::layers::ExfatVolume;
use fs_exfat::mkfs::{self, FormatOptions};
use fs_exfat::write::WriteVolume;
use std::sync::Arc;

const VOLUME_BYTES: u64 = 8 << 20;

fn content(seed: u32, length: usize) -> Vec<u8> {
    (0..length as u32)
        .map(|index| (index.wrapping_mul(2_654_435_761).wrapping_add(seed) >> 13) as u8)
        .collect()
}

/// Files in the root and two levels down, one of them empty, at `sector`
/// bytes per sector and 4 KiB clusters.
fn volume(sector: u32) -> Vec<u8> {
    let device = Memory::new(vec![0; VOLUME_BYTES as usize]);
    mkfs::format(
        &device,
        &FormatOptions {
            bytes_per_sector: Some(sector),
            cluster_size: Some(4096),
            volume_serial: Some(0x1234_5678),
            label: Some("LAYERS".into()),
            ..FormatOptions::default()
        },
    )
    .unwrap();
    let mut mounted = WriteVolume::mount(device.clone()).unwrap();
    mounted.write_file("/Hello.txt", &content(1, 100)).unwrap();
    mounted.write_file("/big.bin", &content(2, 70_000)).unwrap();
    mounted.write_file("/empty", b"").unwrap();
    mounted.mkdir("/dir").unwrap();
    mounted.mkdir("/dir/deeper").unwrap();
    mounted
        .write_file("/dir/deeper/inner.bin", &content(3, 9_000))
        .unwrap();
    mounted.unmount().unwrap();
    device.snapshot()
}

struct Bytes(Vec<u8>);

impl BlockRead for Bytes {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        let range = usize::try_from(offset)
            .ok()
            .and_then(|start| Some(start..start.checked_add(output.len())?))
            .filter(|range| range.end <= self.0.len())
            .ok_or(fs_core::Error::OutOfBounds {
                offset,
                len: output.len() as u64,
                size: self.0.len() as u64,
            })?;
        output.copy_from_slice(&self.0[range]);
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.0.len() as u64
    }
}

fn source(image: Vec<u8>) -> Arc<dyn BlockRead> {
    Arc::new(Bytes(image))
}

fn expected() -> Vec<Expected> {
    vec![
        Expected::file("/Hello.txt", &content(1, 100)),
        // exFAT names compare through the up-case table.
        Expected::file("/HELLO.TXT", &content(1, 100)),
        Expected::file("/big.bin", &content(2, 70_000)),
        Expected::file("/empty", b""),
        Expected::file("/dir/deeper/inner.bin", &content(3, 9_000)),
        Expected::directory("/dir"),
        Expected::directory("/DIR/Deeper"),
        Expected::absent("/dir/missing"),
    ]
}

#[test]
fn every_layer_conforms_at_512_and_4096_bytes_per_sector() {
    for sector in [512, 4096] {
        let volume = ExfatVolume::open(source(volume(sector)))
            .unwrap_or_else(|error| panic!("{sector}: {error}"));
        assert_eq!(volume.format(), "exfat");
        let geometry = volume.geometry().unwrap();
        assert_eq!(geometry.sector_size, sector);
        assert_eq!(geometry.allocation_unit, 4096);
        assert_eq!(geometry.declared_bytes, VOLUME_BYTES);
        assert_eq!(geometry.label.as_deref(), Some("LAYERS"));
        assert_eq!(geometry.serial.as_deref(), Some("1234-5678"));
        conformance::check_healthy(&volume, &expected(), &Limits::default()).assert_passed();
        conformance::check_files_against_candidates(&volume, &Limits::default()).assert_passed();
    }
}

#[test]
fn probe_tells_a_healthy_volume_from_a_damaged_one_and_from_none() {
    let healthy = volume(4096);
    let probe = ExfatVolume::probe(&Bytes(healthy.clone())).unwrap();
    assert_eq!(probe.recognition, Recognition::Yes, "{probe:?}");
    assert_eq!(probe.declared_bytes, Some(VOLUME_BYTES));
    let probe = ExfatVolume::probe(&Bytes(damaged_main(healthy))).unwrap();
    assert_eq!(probe.recognition, Recognition::Damaged, "{probe:?}");
    let probe = ExfatVolume::probe(&Bytes(vec![0; 1 << 20])).unwrap();
    assert_eq!(probe.recognition, Recognition::No, "{probe:?}");
}

/// One byte of the main boot region's first extended boot sector changed,
/// so its checksum fails and its fields still decode.
fn damaged_main(mut image: Vec<u8>) -> Vec<u8> {
    let sector = 1usize << image[108];
    image[sector + 7] ^= 0x5a;
    image
}

#[test]
fn a_damaged_main_boot_region_opens_for_salvage_only_and_conforms() {
    for sector in [512, 4096] {
        let image = damaged_main(volume(sector));
        assert!(ExfatVolume::open(source(image.clone())).is_err());
        let volume = ExfatVolume::open_for_salvage(source(image)).unwrap();
        let copies = volume.superblocks().unwrap();
        assert_eq!(copies.len(), 2);
        assert_eq!(copies[0].integrity, Integrity::Failed);
        assert_eq!(copies[1].integrity, Integrity::Verified);
        assert!(copies.iter().all(|copy| copy.superblock.is_some()));
        assert_eq!(copies[1].location.offset, 12 * u64::from(sector));
        let health = volume.health().unwrap();
        assert_eq!(health.state, HealthState::Damaged, "{health:?}");
        assert!(
            health
                .findings
                .iter()
                .any(|f| f.kind == DamageKind::Checksum && f.structure == "main boot region"),
            "{health:?}"
        );
        assert!(!volume.support().unwrap().files);
        assert!(volume.root().is_err());
        let structures = volume.structures().unwrap();
        assert_eq!(structures[0].integrity, Integrity::Failed);
        assert_eq!(structures[1].integrity, Integrity::Verified);
        conformance::check_damaged(&volume, &Limits::default()).assert_passed();
    }
}

#[test]
fn volume_dirty_reads_as_needing_recovery() {
    let mut image = volume(512);
    image[106] |= 0b10;
    let volume = ExfatVolume::open(source(image)).unwrap();
    let health = volume.health().unwrap();
    assert_eq!(health.state, HealthState::NeedsRecovery, "{health:?}");
    assert_eq!(health.findings.len(), 1, "{health:?}");
}
