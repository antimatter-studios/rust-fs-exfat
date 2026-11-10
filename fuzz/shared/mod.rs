//! What each fuzz target does with its bytes, shared verbatim by the cargo-fuzz
//! targets and tests/fuzz_decoders.rs, so the explorer and the gate cannot drift.
//!
//! Every function takes arbitrary bytes, holds the decoder's output to what the
//! decoder promises, and returns whether a decoder accepted the input. The gate
//! uses that answer to refuse a corpus that never gets past the first check.
//!
//! Checksums would stop a mutated input at the first comparison, so the directory
//! and image targets recompute them and try the input both ways. Nothing here
//! opens a device or a host path.
#![allow(dead_code)]

#[path = "../../tests/support/mod.rs"]
pub mod support;

use fs_core::BlockRead;
use fs_exfat::boot::{boot_checksum, BootSector};
use fs_exfat::check::{check, CheckReport};
use fs_exfat::directory::{self, DirectoryEntry};
use fs_exfat::file::File;
use fs_exfat::metadata::{
    checksum32, set_checksum, AllocationBitmap, FatEntry, RootMetadata, UpcaseTable,
};
use fs_exfat::primitives::{RawAllocationBitmap, RawRootMetadata, RawUpcaseTable};
use fs_exfat::timestamp::{Timestamp, Timestamps};
use fs_exfat::volume::{ReadLimits, Volume};
use std::collections::HashSet;
use std::sync::OnceLock;
use support::{file_set, metadata_volume, put16, put32, put64, seal_boot, seal_set, Bytes};

pub fn boot_sector(data: &[u8]) -> bool {
    let permissive = fs_exfat::primitives::BootSector::parse(data).ok();
    let header = fs_exfat::primitives::BootSectorHeader::parse(data).ok();
    assert_eq!(header.is_some(), data.len() >= 512);
    if let Some(header) = &header {
        assert_eq!(header.jump_valid, data[..3] == [0xeb, 0x76, 0x90]);
        assert_eq!(header.file_system_name_valid, &data[3..11] == b"EXFAT   ");
        assert_eq!(
            header.must_be_zero_valid,
            data[11..64].iter().all(|byte| *byte == 0)
        );
        assert_eq!(header.sector.signature_ok, data[510..512] == [0x55, 0xaa]);
        assert_eq!(header.file_system_name_valid, permissive.is_some());
        if let Some(permissive) = &permissive {
            assert_eq!(&header.sector, permissive);
        }
    }
    let mut padded = vec![0; 12 * 512];
    let length = data.len().min(padded.len());
    padded[..length].copy_from_slice(&data[..length]);
    let region = fs_exfat::primitives::BootRegion::parse(&padded, 512).unwrap();
    assert_eq!(
        region.computed_checksum,
        boot_checksum(&padded, 512).unwrap()
    );
    assert_eq!(
        region.header,
        fs_exfat::primitives::BootSectorHeader::parse(&padded).unwrap()
    );
    if let Some(boot) = &permissive {
        assert_eq!(boot.file_system_name, "EXFAT   ");
        assert_eq!(boot.signature_ok, data[510..512] == [0x55, 0xaa]);
    }
    for sector in [512, 1024, 2048, 4096] {
        let region = fs_exfat::primitives::BootRegion::parse(data, sector);
        assert_eq!(region.is_ok(), data.len() >= 12 * sector);
        if let Ok(region) = region {
            assert_eq!(
                region.computed_checksum,
                boot_checksum(data, sector).unwrap()
            );
            assert_eq!(&region.header, header.as_ref().unwrap());
        }
        let Ok(checksum) = boot_checksum(data, sector) else {
            continue;
        };
        let mut flags_changed = data.to_vec();
        for offset in [106, 107, 112] {
            flags_changed[offset] ^= 0xff;
        }
        assert_eq!(boot_checksum(&flags_changed, sector).ok(), Some(checksum));
    }
    let Ok(boot) = BootSector::parse(data) else {
        return permissive.is_some();
    };
    let permissive =
        permissive.expect("the strict decoder accepts only a subset of primitive boots");
    assert_eq!(boot.cluster_count, permissive.cluster_count);
    assert_eq!(boot.bytes_per_sector(), permissive.bytes_per_sector());
    assert_eq!(boot.cluster_size(), permissive.cluster_size());
    assert!(matches!(boot.bytes_per_sector(), 512 | 1024 | 2048 | 4096));
    assert!((boot.bytes_per_sector()..=32 * 1024 * 1024).contains(&boot.cluster_size()));
    let volume_bytes = boot.volume_length * boot.bytes_per_sector();
    let root = boot.cluster_offset(boot.root_cluster).unwrap();
    assert!(root + boot.cluster_size() <= volume_bytes);
    let last = boot.cluster_offset(boot.cluster_count + 1).unwrap();
    assert!(last + boot.cluster_size() <= volume_bytes);
    assert!(boot.cluster_offset(0).is_err());
    assert!(boot.cluster_offset(1).is_err());
    assert!(boot.cluster_offset(boot.cluster_count + 2).is_err());
    true
}

pub fn fat_entry(data: &[u8]) -> bool {
    let (Some(value), Some(count)) = (u32_at(data, 0), u32_at(data, 4)) else {
        return false;
    };
    let Ok(entry) = FatEntry::parse(value, count) else {
        assert!(value != 0 && value != 0xffff_ffff && value != 0xffff_fff7);
        return false;
    };
    match entry {
        FatEntry::Free => assert_eq!(value, 0),
        FatEntry::Bad => assert_eq!(value, 0xffff_fff7),
        FatEntry::End => assert_eq!(value, 0xffff_ffff),
        FatEntry::Next(next) => {
            assert_eq!(next, value);
            assert!(next >= 2 && u64::from(next) < u64::from(count) + 2);
        }
    }
    true
}

pub fn allocation_bitmap(data: &[u8]) -> bool {
    let Some((head, bytes)) = data.split_first_chunk::<4>() else {
        return false;
    };
    let raw = u32::from_le_bytes(*head);
    let primitive = RawAllocationBitmap::parse(bytes, raw);
    assert_eq!(primitive.bytes, bytes);
    assert_eq!(
        primitive.length_ok,
        bytes.len() as u64 == u64::from(raw).div_ceil(8)
    );
    if let Ok(bitmap) = AllocationBitmap::parse(bytes.to_vec(), raw) {
        assert!(primitive.length_ok);
        check_bitmap(&bitmap, bytes, raw);
    }
    if bytes.is_empty() {
        return false;
    }
    // Any count that rounds up to this length must be accepted.
    let Ok(bits) = u32::try_from(bytes.len() * 8) else {
        return false;
    };
    let count = bits - (raw & 7);
    let bitmap = AllocationBitmap::parse(bytes.to_vec(), count).unwrap();
    check_bitmap(&bitmap, bytes, count);
    true
}

fn check_bitmap(bitmap: &AllocationBitmap, bytes: &[u8], count: u32) {
    for cluster in [0, 1, count.saturating_add(2), u32::MAX] {
        assert!(bitmap.allocated(cluster).is_err() || count >= u32::MAX - 2);
    }
    for index in 0..count.min(8192) {
        let expected = bytes[index as usize / 8] & (1 << (index % 8)) != 0;
        assert_eq!(bitmap.allocated(index + 2).unwrap(), expected);
    }
}

pub fn upcase_table(data: &[u8]) -> bool {
    let primitive = RawUpcaseTable::parse(data, checksum32(data));
    assert_eq!(primitive.data, data);
    assert!(primitive.checksum_ok);
    assert!(primitive.mappings.len() <= 65536);
    let _ = UpcaseTable::parse(data, checksum32(data).wrapping_add(1)).map(|_| {
        panic!("a table with the wrong checksum was accepted");
    });
    let Ok(table) = UpcaseTable::parse(data, checksum32(data)) else {
        return false;
    };
    assert!(primitive.length_ok && primitive.complete && primitive.ascii_ok);
    assert_eq!(primitive.issue, None);
    assert_eq!(table.map(u16::MAX), primitive.mappings[65535]);
    for unit in 0u16..128 {
        let expected = if (97..=122).contains(&unit) {
            unit - 32
        } else {
            unit
        };
        assert_eq!(table.map(unit), expected);
    }
    let units: Vec<u16> = data
        .chunks_exact(2)
        .take(255)
        .map(|word| u16::from_le_bytes([word[0], word[1]]))
        .collect();
    assert!(table.equal(&units, &units));
    let _ = table.name_hash(&units);
    assert_eq!(table.map(u16::MAX), table.map(u16::MAX));
    true
}

pub fn root_metadata(data: &[u8]) -> bool {
    let Some((flags, entries)) = data.split_first() else {
        return false;
    };
    let fats = (flags & 3) + 1;
    let primitive = RawRootMetadata::parse(entries);
    assert_eq!(primitive.aligned, entries.len().is_multiple_of(32));
    let raw = RootMetadata::parse(entries, fats);
    let mut sealed = entries.to_vec();
    for entry in sealed.chunks_exact_mut(32) {
        if entry[0] == 0xa0 {
            let checksum = set_checksum(entry);
            put16(entry, 2, checksum);
        }
    }
    let resealed = RootMetadata::parse(&sealed, fats);
    let mut accepted = false;
    for metadata in [raw, resealed].into_iter().flatten() {
        assert!(primitive.aligned && primitive.bitmap_present[0] && primitive.upcase_present);
        accepted = true;
        assert!(metadata.bitmaps[0].is_some());
        assert_eq!(metadata.bitmaps[1].is_some(), fats == 2);
        assert!(metadata
            .label
            .iter()
            .all(|label| label.encode_utf16().count() <= 11));
        assert!(metadata.guid.iter().all(|guid| *guid != [0; 16]));
    }
    accepted
}

pub fn directory(data: &[u8]) -> bool {
    let Some((flags, entries)) = data.split_first() else {
        return false;
    };
    let (boot, upcase) = geometry();
    let root = flags & 1 != 0;
    check_raw_entry_set(entries);
    let raw = directory::parse(entries, boot, upcase, root);
    let mut sealed = entries.to_vec();
    reseal_sets(&mut sealed, upcase);
    check_raw_entry_set(&sealed);
    let resealed = directory::parse(&sealed, boot, upcase, root);
    let mut accepted = false;
    for listing in [raw, resealed].into_iter().flatten() {
        accepted = true;
        let mut names = HashSet::new();
        for entry in &listing {
            check_entry(entry, boot);
            let folded: Vec<u16> = entry
                .name_utf16()
                .iter()
                .map(|unit| upcase.map(*unit))
                .collect();
            assert!(names.insert(folded), "duplicate name accepted");
        }
    }
    accepted
}

fn check_raw_entry_set(data: &[u8]) {
    if let Some(entry_type) = fs_exfat::primitives::EntryType::parse(data) {
        assert_eq!(entry_type.value, data[0]);
        assert_eq!(entry_type.in_use, data[0] & 0x80 != 0);
        assert_eq!(entry_type.secondary, data[0] & 0x40 != 0);
        assert_eq!(entry_type.benign, data[0] & 0x20 != 0);
    }
    let Some(set) = fs_exfat::primitives::RawEntrySet::parse(data) else {
        return;
    };
    assert_eq!(set.bytes.len(), (usize::from(data[1]) + 1) * 32);
    assert_eq!(set.primary.raw, data[..32]);
    assert_eq!(set.secondaries.len(), usize::from(data[1]));
    assert_eq!(
        set.checksum_ok,
        set_checksum(set.bytes) == u16::from_le_bytes(data[2..4].try_into().unwrap())
    );
    let name = set.name();
    assert_eq!(name.utf16_ok, String::from_utf16(&name.utf16).is_ok());
    assert_eq!(name.text, String::from_utf16_lossy(&name.utf16));
    if let Some(stream) = &set.stream {
        assert_eq!(stream.first_cluster, u32_at(data, 52).unwrap());
        assert_eq!(
            stream.data_length,
            u64::from_le_bytes(data[56..64].try_into().unwrap())
        );
        assert!(name.utf16.len() <= usize::from(stream.name_length));
        assert_eq!(
            set.name_complete,
            name.utf16.len() == usize::from(stream.name_length)
        );
    } else {
        assert!(!set.name_complete);
    }
}

pub fn timestamp(data: &[u8]) -> bool {
    let (Some(raw), Some(increment), Some(offset)) = (u32_at(data, 0), data.get(4), data.get(5))
    else {
        return false;
    };
    let Ok(decoded) = Timestamp::decode(raw, *increment, *offset) else {
        return false;
    };
    check_timestamp(&decoded);
    assert_eq!(decoded.utc_offset_minutes.is_some(), offset & 0x80 != 0);
    true
}

pub fn timestamps(data: &[u8]) -> bool {
    let Ok(decoded) = Timestamps::parse(data) else {
        return false;
    };
    assert_eq!(data.len(), 32);
    for timestamp in [decoded.created, decoded.modified, decoded.accessed] {
        check_timestamp(&timestamp);
    }
    let raw = u32_at(data, 8).unwrap();
    assert_eq!(
        Timestamp::decode(raw, data[20], data[22]).unwrap(),
        decoded.created
    );
    true
}

fn check_timestamp(timestamp: &Timestamp) {
    assert!((1980..=2107).contains(&timestamp.year));
    assert!((1..=12).contains(&timestamp.month));
    assert!((1..=31).contains(&timestamp.day));
    assert!(timestamp.hour < 24 && timestamp.minute < 60 && timestamp.second < 60);
    assert!(timestamp.millisecond < 1000 && timestamp.millisecond.is_multiple_of(10));
    if let Some(offset) = timestamp.utc_offset_minutes {
        assert!((-960..=945).contains(&offset) && offset % 15 == 0);
    }
    let milliseconds = timestamp.unix_milliseconds();
    assert_eq!(
        milliseconds.is_some(),
        timestamp.utc_offset_minutes.is_some()
    );
    assert!(milliseconds.is_none_or(|value| value > 0));
}

/// Opens a synthetic volume patched as `data` asks, then uses it the way the CLI
/// and the C ABI do. The input is a list of patches rather than a whole image:
/// the volume has a 1 MiB minimum, and random changes across that much zeroed
/// space would rarely touch anything a decoder reads.
///
/// `flags region:u8 offset:u16 count:u8 bytes[count] ...`, where flag bit 0 leaves
/// the boot checksum stale, bit 1 the set checksums and bit 2 the upcase checksum.
/// A region of 0xff makes the rest of the input one path to look up.
pub fn image(data: &[u8]) -> bool {
    let (image, path) = patched_image(data);
    let Ok(volume) = Volume::open_with_limits(Bytes(image), IMAGE_LIMITS) else {
        return false;
    };
    let mut budget = 256;
    if let Ok(entries) = volume.root_directory() {
        walk(&volume, entries, 0, &mut budget);
    }
    if let Ok(report) = check(&volume) {
        assert_eq!(
            report.unreferenced_clusters + report.referenced_clusters,
            report.allocated_clusters
        );
    }
    for candidate in [path.as_str(), "SUB/deep/../DEEP/inner.txt", "chain.bin/x"] {
        if let Ok(node) = volume.lookup(candidate) {
            if let Some(entry) = node.entry() {
                check_entry(entry, volume.boot());
            }
        }
        let _ = volume.read_directory_path(candidate);
    }
    true
}

const IMAGE_LIMITS: ReadLimits = ReadLimits {
    max_metadata_bytes: 4 * 1024 * 1024,
    max_chain_clusters: 4096,
};

/// What `check` says of the unpatched template, for the gate to hold it to.
pub fn pristine_report() -> CheckReport {
    let volume = Volume::open_with_limits(Bytes(template().to_vec()), IMAGE_LIMITS).unwrap();
    check(&volume).unwrap()
}

fn walk<Device: BlockRead>(
    volume: &Volume<Device>,
    entries: Vec<DirectoryEntry>,
    depth: usize,
    budget: &mut usize,
) {
    for entry in entries {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        check_entry(&entry, volume.boot());
        if entry.is_directory() {
            if depth < 8 {
                if let Ok(children) = volume.read_directory(&entry) {
                    walk(volume, children, depth + 1, budget);
                }
            }
        } else if let Ok(file) = volume.open_file(&entry) {
            check_file(&file);
        }
    }
}

/// `DirectoryEntry` and `Stream` have no decoder of their own, so what they
/// promise is held here, on every entry the directory and image targets produce.
fn check_entry(entry: &DirectoryEntry, boot: &BootSector) {
    assert!(!entry.name().is_empty());
    assert!(entry.name_utf16().len() <= 255);
    assert_eq!(
        String::from_utf16(entry.name_utf16()).unwrap(),
        entry.name()
    );
    assert_eq!(entry.raw_primary()[0], 0x85);
    assert_eq!(entry.is_directory(), entry.attributes() & 0x10 != 0);
    let stream = entry.stream();
    assert!(stream.valid_data_length() <= stream.data_length());
    if stream.data_length() == 0 {
        assert_eq!(stream.first_cluster(), 0);
        assert!(!stream.no_fat_chain());
    } else {
        boot.cluster_offset(stream.first_cluster()).unwrap();
    }
    if entry.is_directory() {
        assert_eq!(stream.valid_data_length(), stream.data_length());
        assert_eq!(stream.data_length() % boot.cluster_size(), 0);
    }
    let times = entry.timestamps();
    for timestamp in [times.created, times.modified, times.accessed] {
        check_timestamp(&timestamp);
    }
}

fn check_file<Device: BlockRead>(file: &File<'_, Device>) {
    let (length, valid) = (file.len(), file.valid_data_length());
    assert!(valid <= length);
    assert_eq!(file.is_empty(), length == 0);
    let offsets = [
        0,
        valid.saturating_sub(1),
        valid,
        length.saturating_sub(1),
        length,
        length.saturating_add(1),
        u64::MAX,
    ];
    for offset in offsets {
        let mut output = [0xff; 1100];
        let Ok(count) = file.read_at(offset, &mut output) else {
            continue;
        };
        let expected = length.saturating_sub(offset).min(output.len() as u64) as usize;
        assert_eq!(count, expected);
        for (index, byte) in output[..count].iter().enumerate() {
            assert!(
                offset + (index as u64) < valid || *byte == 0,
                "byte past the valid length was not zero"
            );
        }
        assert!(output[count..].iter().all(|byte| *byte == 0xff));
    }
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

/// Recomputes each set's checksum, and a file set's name hash, so that a mutated
/// input reaches the checks behind them.
fn reseal_sets(data: &mut [u8], upcase: &UpcaseTable) {
    let mut position = 0;
    while position + 32 <= data.len() {
        let kind = data[position];
        if kind == 0 {
            break;
        }
        if kind & 0x80 == 0 || matches!(kind, 0x81..=0x83) {
            position += 32;
            continue;
        }
        let length = (usize::from(data[position + 1]) + 1) * 32;
        let Some(set) = data.get_mut(position..position + length) else {
            break;
        };
        if kind == 0x85 && set.len() >= 64 {
            let units: Vec<u16> = (0..usize::from(set[35]))
                .map_while(|index| {
                    let start = 64 + index / 15 * 32 + 2 + index % 15 * 2;
                    let bytes = set.get(start..start + 2)?;
                    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
                })
                .collect();
            put16(set, 36, upcase.name_hash(&units));
        }
        seal_set(set);
        position += length;
    }
}

/// The geometry the directory target validates streams against: 32768 clusters of
/// 4 KiB. No device stands behind it; `directory::parse` reads only its fields.
fn geometry() -> &'static (BootSector, UpcaseTable) {
    static GEOMETRY: OnceLock<(BootSector, UpcaseTable)> = OnceLock::new();
    GEOMETRY.get_or_init(|| {
        let mut sector = support::volume(9);
        put64(&mut sector, 72, 0x80000);
        put32(&mut sector, 84, 300);
        put32(&mut sector, 88, 512);
        put32(&mut sector, 92, 0x8000);
        sector[109] = 3;
        let boot = BootSector::parse(&sector).unwrap();
        let upcase = support::upcase_bytes();
        let table = UpcaseTable::parse(&upcase, checksum32(&upcase)).unwrap();
        (boot, table)
    })
}

const SECTOR: usize = 512;
const FAT: usize = 24 * SECTOR;
const ROOT: usize = 32 * SECTOR;
const BITMAP: usize = 33 * SECTOR;
const UPCASE: usize = 34 * SECTOR;
const SUB: usize = 35 * SECTOR;
const CHAIN_FIRST: usize = 37 * SECTOR;
const DEEP: usize = 38 * SECTOR;
const CHAIN_SECOND: usize = 39 * SECTOR;
const FLAT: usize = 40 * SECTOR;

/// Everything on the template a decoder reads, in the order a patch names it.
const REGIONS: [(usize, usize); 6] = [
    (0, SECTOR),
    (FAT, 64),
    (ROOT, SECTOR),
    (BITMAP, 16),
    (UPCASE, 124),
    (SUB, 3 * SECTOR + SECTOR),
];

/// A volume holding one of each thing the reader tells apart: a label and GUID, a
/// contiguous directory whose child's set straddles a cluster boundary, a FAT-linked
/// directory, a fragmented file with a reduced valid length, a contiguous file and
/// an empty one. Each cluster is 512 bytes, cluster N at sector 30 + N.
fn template() -> &'static [u8] {
    static TEMPLATE: OnceLock<Vec<u8>> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        let mut data = metadata_volume();
        for (cluster, value) in [
            (5, 6),
            (6, 0xffff_ffff),
            (7, 9),
            (8, 0xffff_ffff),
            (9, 0xffff_ffff),
            (10, 0xffff_ffff),
        ] {
            put32(&mut data, FAT + cluster * 4, value);
        }
        data[BITMAP] = 0xff;
        data[BITMAP + 1] = 0x01;

        let mut guid = vec![0; 32];
        guid[0] = 0xa0;
        guid[6..22].copy_from_slice(&[7; 16]);
        seal_set(&mut guid);
        let mut chain = file_set("chain.bin", 0x20, 7, 1024, false);
        put64(&mut chain, 40, 700);
        seal_set(&mut chain);
        let mut root = guid;
        root.extend(file_set("Sub", 0x10, 5, 1024, true));
        root.extend(chain);
        root.extend(file_set("flat.bin", 0x20, 10, 512, true));
        root.extend(file_set("empty.txt", 0x20, 0, 0, false));
        data[ROOT + 96..ROOT + 96 + root.len()].copy_from_slice(&root);

        data[SUB..SUB + 448].chunks_exact_mut(32).for_each(|entry| {
            entry[0] = 0x05;
        });
        let mut sub = file_set("a long file name spanning entries.txt", 0x20, 0, 0, false);
        sub.extend(file_set("Deep", 0x10, 8, 512, false));
        data[SUB + 448..SUB + 448 + sub.len()].copy_from_slice(&sub);
        let inner = file_set("inner.txt", 0x20, 0, 0, false);
        data[DEEP..DEEP + inner.len()].copy_from_slice(&inner);

        data[CHAIN_FIRST..CHAIN_FIRST + SECTOR].fill(0xa5);
        data[CHAIN_SECOND..CHAIN_SECOND + SECTOR].fill(0x5a);
        data[FLAT..FLAT + SECTOR].fill(0x3c);
        data
    })
}

fn patched_image(input: &[u8]) -> (Vec<u8>, String) {
    let mut image = template().to_vec();
    let mut path = String::new();
    let (flags, mut rest) = input.split_first().map_or((0, input), |(f, r)| (*f, r));
    while let [region, low, high, count, tail @ ..] = rest {
        if *region == 0xff {
            path = String::from_utf8_lossy(&rest[1..]).into_owned();
            break;
        }
        let (start, size) = REGIONS[usize::from(*region) % REGIONS.len()];
        let offset = usize::from(u16::from_le_bytes([*low, *high])) % size;
        let count = usize::from(*count).min(tail.len());
        let (bytes, remainder) = tail.split_at(count);
        let length = count.min(size - offset);
        image[start + offset..start + offset + length].copy_from_slice(&bytes[..length]);
        rest = remainder;
    }
    if flags & 2 == 0 {
        let upcase_table = &geometry().1;
        reseal_sets(&mut image[ROOT..ROOT + SECTOR], upcase_table);
        reseal_sets(&mut image[SUB..SUB + 2 * SECTOR], upcase_table);
        reseal_sets(&mut image[DEEP..DEEP + SECTOR], upcase_table);
    }
    if flags & 4 == 0 && image[ROOT + 32] == 0x82 {
        let length = support::upcase_bytes().len();
        let checksum = checksum32(&image[UPCASE..UPCASE + length]);
        put32(&mut image, ROOT + 36, checksum);
    }
    if flags & 1 == 0 {
        seal_boot(&mut image, SECTOR);
        image.copy_within(..SECTOR * 12, SECTOR * 12);
    }
    (image, path)
}

/// One patch input for each way of breaking the template that a person would
/// think of first, as starting points for the `image` target.
pub fn image_seeds() -> Vec<(&'static str, Vec<u8>)> {
    let patch = |region: u8, offset: u16, bytes: &[u8]| {
        let mut input = vec![0, region];
        input.extend(offset.to_le_bytes());
        input.push(bytes.len() as u8);
        input.extend(bytes);
        input
    };
    vec![
        ("pristine", vec![0]),
        ("fat-cycle", patch(1, 7 * 4, &7u32.to_le_bytes())),
        ("fat-free-in-chain", patch(1, 7 * 4, &0u32.to_le_bytes())),
        (
            "fat-bad-in-chain",
            patch(1, 7 * 4, &0xffff_fff7u32.to_le_bytes()),
        ),
        ("bitmap-all-free", patch(3, 0, &[0, 0])),
        ("bitmap-all-used", patch(3, 0, &[0xff; 13])),
        ("sectors-per-cluster", patch(0, 109, &[1])),
        ("path-through-sub", {
            let mut input = vec![0, 0xff];
            input.extend(b"sub/DEEP/../deep/INNER.TXT");
            input
        }),
    ]
}
