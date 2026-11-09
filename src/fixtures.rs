//! In-memory exFAT image encoders, available only with `test-support`.
//!
//! These fixed-geometry fixtures preserve deliberately incomplete and damaged
//! images byte for byte. They are not healthy reference media or a formatter:
//! extended boot trailers, file name hashes and upcase payloads are absent in
//! the legacy volume builders. Nothing here opens or writes a device or file.
//!
//! The public image buffer and unchecked placement methods support malformed
//! test inputs. They may panic on invalid slices, offsets or cluster numbers.

use crate::primitives::{boot_checksum, FAT_END};

/// Bytes per sector in the fixed synthetic geometry.
pub const SECTOR: usize = 512;
/// Bytes per cluster in the fixed synthetic geometry.
pub const CLUSTER: usize = 4096;
/// Number of addressable data clusters, starting at cluster two.
pub const CLUSTER_COUNT: u32 = 200;
/// FAT offset in sectors.
pub const FAT_OFFSET: u32 = 24;
/// Reserved FAT length in sectors.
pub const FAT_LENGTH: u32 = 8;
/// Cluster heap offset in sectors.
pub const HEAP_OFFSET: u32 = 32;
/// First allocation-bitmap cluster.
pub const BITMAP_CLUSTER: u32 = 2;
/// First root-directory cluster.
pub const ROOT_CLUSTER: u32 = 3;

/// An in-memory image builder with fixed geometry and deliberately unchecked edits.
///
/// This is not a formatter: malformed structures are intentional test inputs.
/// Slice bounds and cluster arithmetic panic on out-of-image inputs.
pub struct Builder {
    /// Raw bytes, exposed for controlled corruption and construction from existing fixtures.
    pub image: Vec<u8>,
}

/// Produces repeatable xorshift bytes for synthetic file contents.
pub fn noise(seed: u32, length: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2654435761).wrapping_add(1);
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 8) as u8
        })
        .collect()
}

/// Encodes live or deleted file/stream/name entries, preserving permissive inputs.
///
/// Name hashes and UTC-offset flags remain zero. Checksums treat deleted entry
/// types as live, preserving the original pre-deletion checksum. Names, lengths
/// and cluster numbers are not validated.
pub fn entry_set(
    name: &str,
    is_dir: bool,
    first: u32,
    length: u64,
    no_fat_chain: bool,
    in_use: bool,
) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let name_entries = units.len().div_ceil(15);
    let secondary = 1 + name_entries;
    let mut set = vec![0u8; 32 * (secondary + 1)];
    set[0] = if in_use { 0x85 } else { 0x05 };
    set[1] = secondary as u8;
    set[4] = if is_dir { 0x10 } else { 0x20 };
    let timestamp: u32 = (42 << 25) | (9 << 21) | (5 << 16) | (17 << 11) | (53 << 5) | 27;
    set[8..12].copy_from_slice(&timestamp.to_le_bytes());
    set[12..16].copy_from_slice(&timestamp.to_le_bytes());
    set[32] = if in_use { 0xC0 } else { 0x40 };
    set[33] = 0x01 | if no_fat_chain { 0x02 } else { 0 };
    set[35] = units.len() as u8;
    set[40..48].copy_from_slice(&length.to_le_bytes());
    set[52..56].copy_from_slice(&first.to_le_bytes());
    set[56..64].copy_from_slice(&length.to_le_bytes());
    for index in 0..name_entries {
        let base = 32 * (2 + index);
        set[base] = if in_use { 0xC1 } else { 0x41 };
        for (position, unit) in units.iter().skip(index * 15).take(15).enumerate() {
            set[base + 2 + position * 2..base + 4 + position * 2]
                .copy_from_slice(&unit.to_le_bytes());
        }
    }
    seal_entry_set(&mut set);
    set
}

/// Changes when an entry says it was last modified.
pub fn modified_on(mut set: Vec<u8>, year: u32, month: u32, day: u32) -> Vec<u8> {
    let timestamp = ((year - 1980) << 25) | (month << 21) | (day << 16);
    set[12..16].copy_from_slice(&timestamp.to_le_bytes());
    seal_entry_set(&mut set);
    set
}

/// Changes when an entry says it was created.
pub fn created_on(mut set: Vec<u8>, year: u32, month: u32, day: u32) -> Vec<u8> {
    let timestamp = ((year - 1980) << 25) | (month << 21) | (day << 16);
    set[8..12].copy_from_slice(&timestamp.to_le_bytes());
    seal_entry_set(&mut set);
    set
}

/// Stores the entry-set checksum with the in-use bit restored for every slot.
///
/// Panics if fewer than four bytes are supplied.
pub fn seal_entry_set(set: &mut [u8]) {
    let mut checksum = 0u16;
    for (index, &byte) in set.iter().enumerate() {
        if index == 2 || index == 3 {
            continue;
        }
        let value = if index % 32 == 0 { byte | 0x80 } else { byte };
        checksum = checksum.rotate_right(1).wrapping_add(u16::from(value));
    }
    set[2..4].copy_from_slice(&checksum.to_le_bytes());
}

impl Builder {
    /// Allocates the fixed-size image, both boot copies and the two reserved FAT entries.
    pub fn new() -> Self {
        let sectors = HEAP_OFFSET as usize + CLUSTER_COUNT as usize * (CLUSTER / SECTOR);
        let mut builder = Self {
            image: vec![0u8; sectors * SECTOR],
        };
        builder.write_boot();
        builder.fat(0, 0xFFFF_FFF8);
        builder.fat(1, FAT_END);
        builder
    }

    /// Replaces both boot regions, retaining stale backup PercentInUse and zero extended trailers.
    pub fn write_boot(&mut self) {
        let mut sector = vec![0u8; SECTOR];
        sector[0..3].copy_from_slice(&[0xEB, 0x76, 0x90]);
        sector[3..11].copy_from_slice(b"EXFAT   ");
        sector[72..80].copy_from_slice(&((self.image.len() / SECTOR) as u64).to_le_bytes());
        sector[80..84].copy_from_slice(&FAT_OFFSET.to_le_bytes());
        sector[84..88].copy_from_slice(&FAT_LENGTH.to_le_bytes());
        sector[88..92].copy_from_slice(&HEAP_OFFSET.to_le_bytes());
        sector[92..96].copy_from_slice(&CLUSTER_COUNT.to_le_bytes());
        sector[96..100].copy_from_slice(&ROOT_CLUSTER.to_le_bytes());
        sector[100..104].copy_from_slice(&0xABCD_1234u32.to_le_bytes());
        sector[104..106].copy_from_slice(&0x0100u16.to_le_bytes());
        sector[108] = 9;
        sector[109] = 3;
        sector[110] = 1;
        sector[112] = 5;
        sector[510] = 0x55;
        sector[511] = 0xAA;
        self.image[..SECTOR].copy_from_slice(&sector);
        seal_boot(&mut self.image[..SECTOR * 12], SECTOR);
        let region = self.image[..SECTOR * 12].to_vec();
        self.image[SECTOR * 12..SECTOR * 24].copy_from_slice(&region);
        // The same difference as the real volume: backup PercentInUse is stale.
        self.image[SECTOR * 12 + 112] = 59;
    }

    /// Stores one little-endian FAT word without changing allocation bits.
    pub fn fat(&mut self, cluster: u32, value: u32) {
        let position = FAT_OFFSET as usize * SECTOR + cluster as usize * 4;
        self.image[position..position + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// Returns the byte offset of a cluster in the fixed geometry.
    ///
    /// Panics on arithmetic overflow or a cluster number below two.
    pub fn cluster_position(cluster: u32) -> usize {
        HEAP_OFFSET as usize * SECTOR + (cluster as usize - 2) * CLUSTER
    }

    /// Places bytes at a cluster-relative offset; writes may span cluster boundaries.
    pub fn put(&mut self, cluster: u32, offset: usize, bytes: &[u8]) {
        let position = Self::cluster_position(cluster) + offset;
        self.image[position..position + bytes.len()].copy_from_slice(bytes);
    }

    /// Sets a cluster allocation bit without changing its FAT word.
    pub fn allocate(&mut self, cluster: u32) {
        let index = (cluster - 2) as usize;
        self.image[Self::cluster_position(BITMAP_CLUSTER) + index / 8] |= 1 << (index % 8);
    }

    /// Places entry sets consecutively without clearing the remaining directory bytes.
    pub fn directory(&mut self, cluster: u32, sets: &[Vec<u8>]) {
        let mut offset = 0;
        for set in sets {
            self.put(cluster, offset, set);
            offset += set.len();
        }
    }
}

/// Encodes a raw system entry with its type, first cluster and byte length.
///
/// Other fields stay zero, including an upcase-entry checksum. Use
/// [`upcase_entry`] when a real table checksum is required.
pub fn special(kind: u8, first: u32, length: u64) -> Vec<u8> {
    let mut entry = vec![0u8; 32];
    entry[0] = kind;
    entry[20..24].copy_from_slice(&first.to_le_bytes());
    entry[24..32].copy_from_slice(&length.to_le_bytes());
    entry
}

/// Encodes the first allocation-bitmap entry, leaving flags and reserved bytes zero.
pub fn bitmap_entry(first: u32, length: u64) -> Vec<u8> {
    special(0x81, first, length)
}

/// Named boot-sector fields, distinct from the extended-sector signatures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootField {
    /// Eight-byte filesystem name at offsets 3 through 10.
    FileSystemName,
    /// Two-byte little-endian VolumeFlags at offsets 106 and 107.
    VolumeFlags,
    /// Two-byte boot signature at offsets 510 and 511, even on larger sectors.
    BootSignature,
}

/// Replaces a named field in a sector or boot region, retaining all other bytes.
///
/// Checksums are not resealed and other boot copies are not updated. Panics when
/// the supplied value has the wrong field width or the buffer is too short.
pub fn boot_field(mut sector: Vec<u8>, field: BootField, value: &[u8]) -> Vec<u8> {
    let range = match field {
        BootField::FileSystemName => 3..11,
        BootField::VolumeFlags => 106..108,
        BootField::BootSignature => 510..512,
    };
    sector[range].copy_from_slice(value);
    sector
}

/// Builds a volume with a live tree, an orphan tree and every damage class.
pub fn build_volume() -> Vec<u8> {
    let mut builder = Builder::new();
    for cluster in [BITMAP_CLUSTER, ROOT_CLUSTER, 4, 5, 10, 11, 61] {
        builder.allocate(cluster);
    }
    builder.fat(ROOT_CLUSTER, FAT_END);
    builder.directory(
        ROOT_CLUSTER,
        &[
            special(0x81, BITMAP_CLUSTER, u64::from(CLUSTER_COUNT).div_ceil(8)),
            special(0x82, 4, 100),
            entry_set("visible", true, 5, CLUSTER as u64, true, true),
            entry_set("gone", true, 6, CLUSTER as u64, true, false),
        ],
    );
    builder.directory(
        5,
        &[entry_set(
            "keep.mp3",
            false,
            10,
            2 * CLUSTER as u64,
            true,
            true,
        )],
    );
    builder.put(10, 0, b"ID3\x04");
    builder.put(10, 16, &noise(10, 2000));
    builder.put(11, 0, &noise(11, CLUSTER));

    // Cluster 6 was a folder, now holding other data.
    builder.put(6, 0, &noise(6, CLUSTER));

    // Orphan folder: nothing points at cluster 20.
    builder.directory(
        20,
        &[
            entry_set(
                "track.mp3",
                false,
                30,
                3 * CLUSTER as u64 - 100,
                false,
                true,
            ),
            entry_set(
                ".track.mp3.abc123",
                false,
                30,
                3 * CLUSTER as u64 - 100,
                false,
                false,
            ),
            entry_set("photo.jpg", false, 40, 2 * CLUSTER as u64, true, true),
            entry_set("album", true, 21, CLUSTER as u64, true, true),
            entry_set("wiped.mp3", false, 50, CLUSTER as u64, true, true),
            entry_set("reused.jpg", false, 60, 2 * CLUSTER as u64, true, true),
            entry_set("short.bin", false, 70, 2 * CLUSTER as u64, false, true),
            entry_set("notes.xyz", false, 80, 100, true, true),
        ],
    );
    builder.directory(
        21,
        &[entry_set("song.mp3", false, 90, CLUSTER as u64, true, true)],
    );

    builder.fat(30, 31);
    builder.fat(31, 33);
    builder.fat(33, FAT_END);
    builder.put(30, 0, b"ID3\x03");
    builder.put(30, 16, &noise(30, CLUSTER - 16));
    builder.put(31, 0, &noise(31, CLUSTER));
    builder.put(33, 0, &noise(33, CLUSTER));
    builder.put(40, 0, &[0xFF, 0xD8, 0xFF, 0xE0]);
    builder.put(40, 16, &noise(40, CLUSTER - 16));
    builder.put(60, 0, &[0xFF, 0xD8, 0xFF, 0xE0]);
    builder.put(61, 0, &noise(61, CLUSTER));
    builder.fat(70, FAT_END);
    builder.put(70, 0, &noise(70, CLUSTER));
    builder.put(80, 0, &noise(80, CLUSTER));
    builder.put(90, 0, b"ID3\x04");
    builder.put(90, 16, &noise(90, CLUSTER - 16));

    // A damaged entry: one name character altered after the checksum was set.
    let mut damaged = entry_set("broken.jpg", false, 95, CLUSTER as u64, true, false);
    damaged[64 + 4] = b'X';
    builder.directory(22, &[damaged]);

    // A folder reachable only through a damaged entry: cluster 23 holds nothing
    // but a broken entry, and that entry points at a real directory at cluster 24.
    let mut lost_link = entry_set("lost-album", true, 24, CLUSTER as u64, true, true);
    lost_link[64 + 2] = b'X';
    builder.directory(23, &[lost_link]);
    builder.directory(
        24,
        &[entry_set("deep.mp3", false, 96, CLUSTER as u64, true, true)],
    );
    builder.put(96, 0, b"ID3\x04");
    builder.put(96, 16, &noise(96, CLUSTER - 16));

    // Two small text files sharing clusters with older, random data. The first
    // is intact past its own bytes, and the second was overwritten at the start.
    builder.directory(
        25,
        &[
            entry_set("small.php", false, 110, 100, true, true),
            entry_set("clobbered.php", false, 112, 1000, true, true),
        ],
    );
    builder.put(110, 200, &noise(110, CLUSTER - 200));
    builder.put(110, 0, &[b'<'; 100]);
    builder.put(112, 0, &noise(112, CLUSTER));

    // A chain no entry claims, starting with a JPEG header.
    builder.fat(100, 101);
    builder.fat(101, FAT_END);
    builder.put(100, 0, &[0xFF, 0xD8, 0xFF, 0xE0]);
    builder.put(100, 16, &noise(100, CLUSTER - 16));
    builder.put(101, 0, &noise(101, CLUSTER));

    // short.bin stops after one cluster. This nameless chain is its missing half.
    builder.fat(75, FAT_END);
    builder.put(75, 0, &noise(75, CLUSTER));
    builder.image
}

/// Builds overlapping live/deleted streams with creation dates and reused data.
pub fn reuse_image() -> Vec<u8> {
    let mut builder = Builder {
        image: build_volume(),
    };
    builder.directory(
        26,
        &[
            created_on(
                entry_set("film.mp3", false, 130, 4 * CLUSTER as u64, true, true),
                2023,
                5,
                1,
            ),
            created_on(
                entry_set("old.txt", false, 131, 100, true, false),
                2020,
                1,
                1,
            ),
            created_on(
                entry_set("mixed.mp3", false, 140, 3 * CLUSTER as u64, true, true),
                2021,
                1,
                1,
            ),
            created_on(
                entry_set("stale.txt", false, 141, 100, true, false),
                2024,
                1,
                1,
            ),
        ],
    );
    for first in [130, 140] {
        builder.put(first, 0, b"ID3\x04");
        builder.put(first, 16, &noise(first, CLUSTER - 16));
    }
    for cluster in (131..134).chain(141..143) {
        builder.put(cluster, 0, &noise(cluster, CLUSTER));
    }
    builder.image
}

pub fn volume_macos_has_visited() -> Vec<u8> {
    let mut builder = Builder::new();
    for cluster in [BITMAP_CLUSTER, ROOT_CLUSTER, 4, 5, 6, 7, 8, 9] {
        builder.allocate(cluster);
    }
    builder.fat(ROOT_CLUSTER, FAT_END);
    let cluster = CLUSTER as u64;
    builder.directory(
        ROOT_CLUSTER,
        &[
            special(0x81, BITMAP_CLUSTER, u64::from(CLUSTER_COUNT).div_ceil(8)),
            special(0x82, 4, 100),
            modified_on(
                entry_set(".fseventsd", true, 5, cluster, true, true),
                2026,
                11,
                4,
            ),
            modified_on(
                entry_set(".Spotlight-V100", true, 6, cluster, true, true),
                2022,
                9,
                8,
            ),
            entry_set("holiday", true, 7, cluster, true, true),
        ],
    );
    builder.directory(
        5,
        &[
            modified_on(entry_set("0001", false, 20, 100, true, true), 2026, 11, 4),
            modified_on(
                entry_set("0002", false, 21, cluster + 1, true, true),
                2025,
                5,
                2,
            ),
        ],
    );
    builder.directory(
        6,
        &[modified_on(
            entry_set("Store", true, 8, cluster, true, true),
            2022,
            9,
            8,
        )],
    );
    builder.directory(
        8,
        &[modified_on(
            entry_set("live.0", false, 23, 200, true, true),
            2025,
            5,
            2,
        )],
    );
    // A person's own file, changed on a mount date, was not written by the mount.
    builder.directory(
        7,
        &[
            modified_on(
                entry_set("photo.jpg", false, 24, 300, true, true),
                2026,
                11,
                4,
            ),
            modified_on(
                entry_set(".fseventsd", true, 9, cluster, true, true),
                2019,
                1,
                1,
            ),
        ],
    );
    // A copy of that folder from another disk says nothing about this one.
    builder.directory(
        9,
        &[modified_on(
            entry_set("0003", false, 25, 400, true, true),
            2019,
            1,
            1,
        )],
    );
    builder.image
}

pub fn build_jigsaw_volume() -> Vec<u8> {
    let mut builder = Builder::new();
    for cluster in [BITMAP_CLUSTER, ROOT_CLUSTER, 4] {
        builder.allocate(cluster);
    }
    builder.fat(ROOT_CLUSTER, FAT_END);
    builder.directory(
        ROOT_CLUSTER,
        &[
            special(0x81, BITMAP_CLUSTER, u64::from(CLUSTER_COUNT).div_ceil(8)),
            special(0x82, 4, 100),
        ],
    );
    let jpeg = |builder: &mut Builder, cluster: u32| {
        builder.put(cluster, 0, &[0xFF, 0xD8, 0xFF, 0xE0]);
        builder.put(cluster, 16, &noise(cluster, CLUSTER - 16));
    };
    let mp3 = |builder: &mut Builder, cluster: u32| {
        builder.put(cluster, 0, b"ID3\x04");
        builder.put(cluster, 16, &noise(cluster, CLUSTER - 16));
    };
    // Piece A at cluster 20, with a subfolder of its own at 21.
    builder.directory(
        20,
        &[
            entry_set("a1.jpg", false, 22, CLUSTER as u64, true, true),
            entry_set("shared.jpg", false, 23, CLUSTER as u64, true, true),
            entry_set("sub", true, 21, CLUSTER as u64, true, true),
        ],
    );
    jpeg(&mut builder, 22);
    builder.directory(
        21,
        &[entry_set("deep.mp3", false, 24, CLUSTER as u64, true, true)],
    );
    mp3(&mut builder, 24);
    // Piece B at cluster 30, holding the good copy of shared.jpg.
    builder.directory(
        30,
        &[
            entry_set("b1.mp3", false, 31, CLUSTER as u64, true, true),
            entry_set("SHARED.jpg", false, 32, CLUSTER as u64, true, true),
        ],
    );
    mp3(&mut builder, 31);
    jpeg(&mut builder, 32);
    // Pieces C and D.
    builder.directory(
        40,
        &[entry_set("c.mp3", false, 41, CLUSTER as u64, true, true)],
    );
    mp3(&mut builder, 41);
    builder.directory(
        50,
        &[entry_set("d.mp3", false, 51, CLUSTER as u64, true, true)],
    );
    mp3(&mut builder, 51);
    builder.image
}

/// Fills the checksum sector of a twelve-sector boot region.
///
/// VolumeFlags and PercentInUse are excluded, as in the captured fixed-geometry
/// builder. Panics if the region is shorter than twelve sectors.
pub fn seal_boot(region: &mut [u8], bytes_per_sector: usize) {
    let checksum = boot_checksum(region, bytes_per_sector);
    for position in (bytes_per_sector * 11..bytes_per_sector * 12).step_by(4) {
        region[position..position + 4].copy_from_slice(&checksum.to_le_bytes());
    }
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

/// Encodes upcase-table words, including caller-supplied identity-run markers.
///
/// This adds no compression or Unicode mapping policy; words are stored verbatim.
pub fn upcase_table(words: &[u16]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

/// Encodes an upcase entry with the checksum and byte length of its actual table.
///
/// The legacy volume builders keep using [`special`] with a placeholder length
/// and no payload; this helper does not change those captured fixture bytes.
pub fn upcase_entry(first: u32, table: &[u8]) -> Vec<u8> {
    let mut entry = special(0x82, first, table.len() as u64);
    let checksum = table.iter().fold(0u32, |sum, byte| {
        sum.rotate_right(1).wrapping_add(u32::from(*byte))
    });
    entry[4..8].copy_from_slice(&checksum.to_le_bytes());
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_builder_has_the_same_bytes_as_new() {
        assert_eq!(Builder::default().image, Builder::new().image);
    }

    #[test]
    fn upcase_words_and_identity_runs_are_stored_verbatim() {
        assert_eq!(
            upcase_table(&[0xffff, 97, 65, 0x1234]),
            [0xff, 0xff, 0x61, 0, 0x41, 0, 0x34, 0x12]
        );
    }

    #[test]
    fn empty_upcase_metadata_has_no_checksum_or_length() {
        let entry = upcase_entry(4, &[]);
        assert_eq!(entry, special(0x82, 4, 0));
    }
}
