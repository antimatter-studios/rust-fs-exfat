//! The driver conformance set, run against the exFAT salvage adapter and the
//! strict volume API. Images are synthetic; healthy forms of their structures
//! are checked against macOS by the oracle tests.

use fs_core::BlockRead;
use fs_exfat::conformance::{self, Damaged, Driver, Evidence};
use fs_exfat::salvage::{DamageKind, ExfatSalvage, Extent, StaleEnd};
use fs_exfat::volume::Volume;

mod support;

use support::{file_set, metadata_volume, put32, put64, seal_set};

const SECTOR: usize = 512;
/// Clusters are one sector in the support volume.
const CLUSTER: usize = SECTOR;
const FAT: usize = 24 * SECTOR;
const ROOT: usize = 32 * SECTOR;
const BITMAP: usize = 33 * SECTOR;

fn cluster(index: u32) -> usize {
    ROOT + (index as usize - 2) * CLUSTER
}

/// A strict-valid volume: a contiguous file with a valid-data tail, an empty
/// file and a directory in the root; in that directory a small file, a
/// FAT-chained file and a deleted FAT-chained file whose entry stayed.
fn healthy() -> Vec<u8> {
    let mut bytes = metadata_volume();
    let mut example = file_set("Example.txt", 0x20, 5, 1030, true);
    put64(&mut example, 40, 513);
    seal_set(&mut example);
    let root = [
        example,
        file_set("Empty.bin", 0x20, 0, 0, false),
        file_set("Sub", 0x10, 8, CLUSTER as u64, true),
    ]
    .concat();
    bytes[ROOT + 96..ROOT + 96 + root.len()].copy_from_slice(&root);

    // Deletion clears the in-use bits and keeps the live checksum.
    let mut old = file_set("Old.txt", 0x20, 12, 100, false);
    for slot in (0..old.len()).step_by(32) {
        old[slot] &= 0x7f;
    }
    let sub = [
        file_set("Child.bin", 0x20, 9, 7, true),
        file_set("Chained.bin", 0x20, 10, 2 * CLUSTER as u64, false),
        old,
    ]
    .concat();
    bytes[cluster(8)..cluster(8) + sub.len()].copy_from_slice(&sub);

    // Clusters 2 to 11 are in use; 12 held the deleted file. Its FAT entry
    // stays: only the bitmap records allocation (specification section 7.1),
    // so a free cluster's non-zero entry is legal, not damage.
    bytes[BITMAP] = 0xff;
    bytes[BITMAP + 1] = 0x03;
    put32(&mut bytes, FAT + 10 * 4, 11);
    put32(&mut bytes, FAT + 11 * 4, 0xffff_ffff);
    put32(&mut bytes, FAT + 12 * 4, 0xffff_ffff);
    for (index, value) in bytes[cluster(5)..cluster(8)].iter_mut().enumerate() {
        *value = b'a' + (index % 26) as u8;
    }
    bytes[cluster(9)..cluster(9) + 7].copy_from_slice(b"nested\n");
    for (index, value) in bytes[cluster(10)..cluster(13)].iter_mut().enumerate() {
        *value = b'A' + (index % 26) as u8;
    }
    bytes
}

fn damaged(name: &str, evidence: Evidence, damage: impl FnOnce(&mut Vec<u8>)) -> Damaged {
    let mut image = healthy();
    damage(&mut image);
    Damaged::new(name, image, evidence)
}

struct Exfat;

impl Driver for Exfat {
    type Adapter = ExfatSalvage;
    type StrictError = fs_exfat::Error;

    fn salvage(&self, source: &dyn BlockRead) -> ExfatSalvage {
        ExfatSalvage::probe(source)
    }

    /// Opens the volume, then lists every directory and reads every file.
    fn strict(&self, source: &dyn BlockRead) -> Result<(), fs_exfat::Error> {
        let volume = Volume::open(source)?;
        let mut pending = volume.root_directory()?;
        let mut buffer = vec![0; 4096];
        while let Some(entry) = pending.pop() {
            if entry.is_directory() {
                pending.extend(volume.read_directory(&entry)?);
                continue;
            }
            let file = volume.open_file(&entry)?;
            let mut offset = 0;
            while offset < file.len() {
                offset += file.read_at(offset, &mut buffer)? as u64;
            }
        }
        Ok(())
    }

    fn healthy(&self) -> Vec<u8> {
        healthy()
    }

    fn damaged(&self) -> Vec<Damaged> {
        vec![
            damaged(
                "main boot checksum",
                Evidence::Damage(DamageKind::Checksum),
                |image| image[SECTOR + 16] ^= 0xff,
            ),
            damaged(
                "root directory chain",
                Evidence::Damage(DamageKind::Broken),
                |image| put32(image, FAT + 2 * 4, 0),
            ),
            damaged(
                "allocation bitmap length",
                Evidence::Damage(DamageKind::Mismatch),
                |image| put64(image, ROOT + 24, 14),
            ),
            damaged(
                "upcase table checksum",
                Evidence::Damage(DamageKind::Checksum),
                |image| image[ROOT + 36] ^= 0x01,
            ),
            damaged("entry set checksum", Evidence::FailedCandidate, |image| {
                image[ROOT + 96 + 64 + 2] ^= 0x01
            }),
            damaged("broken FAT chain", Evidence::BrokenWalk, |image| {
                put32(image, FAT + 10 * 4, 0)
            }),
            Damaged::new(
                "unreadable root directory",
                healthy(),
                Evidence::Damage(DamageKind::Unreadable),
            )
            .unreadable(Extent::new(ROOT as u64, CLUSTER as u64)),
        ]
    }
}

#[test]
fn the_exfat_driver_conforms() {
    conformance::run(&Exfat).assert_conforms();
}

#[test]
fn the_healthy_image_keeps_a_stale_fat_entry() {
    let source = support::Bytes(healthy());
    let mut chains = Vec::new();
    let report = ExfatSalvage::probe(&source)
        .stale_chains(&source, &mut chains)
        .unwrap();
    assert_eq!(report.unclassified, 0);
    assert_eq!(chains.len(), 1);
    assert_eq!(chains[0].head(), Some(12));
    assert_eq!(chains[0].end, StaleEnd::End);
}
