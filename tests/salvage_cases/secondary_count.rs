//! File entry sets with more than eighteen secondary entries. Section 6.3.2
//! of Microsoft's exFAT specification lets SecondaryCount reach 255, section
//! 7.4.2 applies that range to File entries, and section 7.8 lets Vendor
//! Extension entries fill a set up to that limit. Salvage must find every
//! such set that the strict reader accepts.

use super::support::{file_set, metadata_volume, put32, seal_set, Bytes};
use super::{named, scan, whole};
use fs_exfat::primitives::ENTRY_SIZE;
use fs_exfat::salvage::{ExfatSalvage, Extent, Integrity, SalvageAdapter};
use fs_exfat::volume::Volume;

/// One sector per cluster in the metadata volume.
const CLUSTER: usize = 512;
/// The metadata volume's cluster heap starts at sector 32.
const HEAP: usize = 32 * 512;
const FAT: usize = 24 * 512;
const ROOT: u32 = 2;
const BITMAP: u32 = 3;
/// The first free cluster: the root continues here, so the set lies on
/// contiguous clusters.
const CONTINUATION: u32 = 5;

fn position(cluster: u32) -> usize {
    HEAP + (cluster as usize - 2) * CLUSTER
}

/// Table 36: EntryType E0h, AllocationPossible clear, a VendorGuid.
fn vendor_extension() -> [u8; ENTRY_SIZE] {
    let mut entry = [0u8; ENTRY_SIZE];
    entry[0] = 0xE0;
    entry[2..18]
        .copy_from_slice(b"\x6d\x2f\x4c\x3a\x91\x0e\x47\x52\xb3\x5c\x08\x1d\xe4\x27\x9a\x61");
    entry
}

/// A one-name-entry file set grown with Vendor Extension entries to
/// `secondaries` secondary entries and resealed.
fn vendor_set(secondaries: usize) -> Vec<u8> {
    let mut set = file_set("vendor.bin", 0x20, 0, 0, false);
    while set.len() < (secondaries + 1) * ENTRY_SIZE {
        set.extend(vendor_extension());
    }
    set[1] = secondaries as u8;
    seal_set(&mut set);
    set
}

/// The metadata volume with its root continued on contiguous clusters from
/// cluster 5 and `set` at the start of the continuation. The rest of the
/// root's first cluster holds unused entries, so the strict reader reads on.
fn volume_with(set: &[u8]) -> Vec<u8> {
    let mut data = metadata_volume();
    data[position(ROOT) + 3 * ENTRY_SIZE..position(ROOT) + CLUSTER].fill(1);
    let clusters = set.len().div_ceil(CLUSTER) as u32;
    let last = CONTINUATION + clusters - 1;
    put32(&mut data, FAT + ROOT as usize * 4, CONTINUATION);
    for cluster in CONTINUATION..last {
        put32(&mut data, FAT + cluster as usize * 4, cluster + 1);
    }
    put32(&mut data, FAT + last as usize * 4, 0xffff_ffff);
    for cluster in CONTINUATION..=last {
        let index = (cluster - 2) as usize;
        data[position(BITMAP) + index / 8] |= 1 << (index % 8);
    }
    let start = position(CONTINUATION);
    data[start..start + set.len()].copy_from_slice(set);
    data
}

fn found_by_both(secondaries: usize) {
    let set = vendor_set(secondaries);
    let image = volume_with(&set);

    let volume = Volume::open(Bytes(image.clone())).unwrap();
    let root = volume.root_directory().unwrap();
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].name(), "vendor.bin");

    let source = Bytes(image);
    let (_, found) = scan(&source);
    let file = named(&found, "vendor.bin");
    assert_eq!(file.integrity, Integrity::Verified);
    assert_eq!(
        file.location,
        Extent::new(
            position(CONTINUATION) as u64,
            ((secondaries + 1) * ENTRY_SIZE) as u64
        )
    );
}

#[test]
fn a_set_with_nineteen_secondaries_is_found_like_the_strict_reader_finds_it() {
    found_by_both(19);
}

#[test]
fn a_set_with_255_secondaries_is_found_like_the_strict_reader_finds_it() {
    found_by_both(255);
}

/// A SecondaryCount that runs past the bytes there is never allocated: the
/// strict reader reports the set as truncated, and salvage skips it without
/// reading past the source.
#[test]
fn a_count_past_the_directory_or_source_is_reported_or_skipped() {
    let mut set = file_set("cut.bin", 0x20, 0, 0, false);
    set[1] = 255;
    seal_set(&mut set);

    let mut data = metadata_volume();
    let end = position(ROOT) + CLUSTER;
    data[position(ROOT) + 3 * ENTRY_SIZE..end - set.len()].fill(1);
    data[end - set.len()..end].copy_from_slice(&set);
    assert!(matches!(
        Volume::open(Bytes(data)),
        Err(fs_exfat::Error::Invalid("truncated root entry set"))
    ));

    let mut data = metadata_volume();
    let size = data.len();
    data[size - set.len()..].copy_from_slice(&set);
    let source = Bytes(data);
    let adapter = ExfatSalvage::probe(&source);
    let mut found = Vec::new();
    adapter
        .find_candidates(&source, whole(&source), &mut found)
        .unwrap();
    assert!(found
        .iter()
        .all(|candidate| candidate.name.as_deref() != Some("cut.bin")));
}
