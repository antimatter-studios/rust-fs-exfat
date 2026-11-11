//! The salvage targets: the entry-set carver, chain islands, every salvage
//! adapter verb and extension over a damaged synthetic volume, and arbitrary
//! materialization plans.
//!
//! Salvage promises evidence, not files, so these hold each result to what its
//! documentation says the evidence means, against the source bytes themselves.
//! A source here can refuse reads: [`Faulty`] fails the sectors an input names,
//! because unreadable bytes are what salvage exists for.

use super::support::{self, file_set, put16, put32, put64, seal_set};
use super::{
    apply_patches, geometry, reseal_sets, seal, template, u32_at, BITMAP, FAT, FLAT, REGIONS, ROOT,
    SECTOR,
};
use fs_core::BlockRead;
use fs_exfat::primitives::{
    self, chain_islands as islands_of, compute_stats, decode_timestamp_with_offset, find_sets,
    is_allocated, parse_set, set_checksum, EntrySet, FatCopy, FatLayout, Limits, RawEntrySet,
    RawUpcaseTable, UninterpretedKind, FAT_BAD, FAT_END, FIRST_CLUSTER, ISLAND_END_BROKEN,
    ISLAND_END_EOF, ISLAND_END_JOINS_ANOTHER,
};
use fs_exfat::salvage::{
    Candidate, CandidateKind, CandidateState, ClusterLayout, CopyDifference, CopyKind, Damage,
    DamageKind, EntrySetDetail, ExfatSalvage, Extent, Feature, Integrity, Parent, Plan, PlanStep,
    SalvageAdapter, SalvageError, StaleChain, StaleEnd, UninterpretedEntry, WalkEnd,
};
use std::collections::HashSet;
use std::io;
use std::ops::ControlFlow;

/// A source whose listed 512-byte sectors fail to read. It also holds the
/// crate to its promise that every read a device sees is 512-byte aligned.
pub struct Faulty {
    pub bytes: Vec<u8>,
    pub bad: Vec<u64>,
}

impl Faulty {
    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }

    /// Whether a sector reads: it lies wholly on the source and is not bad.
    fn sector_reads(&self, sector: u64) -> bool {
        (sector + 1) * 512 <= self.size() && !self.bad.contains(&sector)
    }

    /// Whether every byte of `start..end` reads through 512-byte reads.
    fn readable(&self, start: u64, end: u64) -> bool {
        start >= end || (start / 512..end.div_ceil(512)).all(|sector| self.sector_reads(sector))
    }

    fn at(&self, extent: Extent) -> &[u8] {
        &self.bytes[extent.offset as usize..extent.end().unwrap() as usize]
    }

    fn word(&self, offset: u64) -> Option<u32> {
        u32_at(&self.bytes, usize::try_from(offset).ok()?)
    }
}

impl BlockRead for Faulty {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        assert!(
            offset.is_multiple_of(512) && output.len().is_multiple_of(512),
            "a device read of {} bytes at {offset} is not 512-byte aligned",
            output.len()
        );
        let end = offset.saturating_add(output.len() as u64);
        if end > self.size() {
            return Err(fs_core::Error::ShortRead {
                offset,
                want: output.len(),
                got: 0,
            });
        }
        if !self.readable(offset, end) {
            return Err(fs_core::Error::Io(io::Error::other("bad sector")));
        }
        output.copy_from_slice(&self.bytes[offset as usize..end as usize]);
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.size()
    }
}

// ---------------------------------------------------------------------------
// The carver: parse_set and find_sets.

/// The specification's largest cluster count, and the plausibility limits the
/// salvage scan falls back to when no boot copy gives geometry.
const MAX_CLUSTER_COUNT: u32 = 0xFFFF_FFF5;
const UNKNOWN_LIMITS: Limits = Limits {
    cluster_count: MAX_CLUSTER_COUNT,
    cluster_size: 1 << 25,
};

/// Cluster counts a geometry byte chooses between, from the smallest heap to
/// the largest the specification allows.
const CLUSTER_COUNTS: [u32; 8] = [
    1,
    2,
    100,
    0x8000,
    0xFFFF,
    0x0100_0000,
    0xFFFF_FFF4,
    MAX_CLUSTER_COUNT,
];

/// File entry Reserved1 and Reserved2, then Stream Extension Reserved1 to 3, as
/// (offset, length) in a set: the bytes a set without a valid checksum must
/// leave zero to be taken for a real one.
const SET_RESERVED: [(usize, usize); 5] = [(6, 2), (25, 7), (34, 1), (38, 2), (48, 4)];

/// `geometry:u8 bytes...`. The geometry byte picks plausibility limits inside
/// what the specification allows: a cluster count and a cluster size of 512
/// bytes to 32 MiB. Wider values overflow the filter's arithmetic (#112). The bytes are carved as they stand and again with every
/// set's checksum recomputed, so both the checksum and the plausibility path
/// are reached.
pub fn entry_sets(data: &[u8]) -> bool {
    let Some((&shape, bytes)) = data.split_first() else {
        return false;
    };
    let limits = Limits {
        cluster_count: CLUSTER_COUNTS[usize::from(shape >> 4) % CLUSTER_COUNTS.len()],
        cluster_size: 512 << (shape & 15).min(16),
    };
    let mut sealed = bytes.to_vec();
    reseal_sets(&mut sealed, &geometry().1);
    let mut accepted = false;
    for variant in [bytes, &sealed[..]] {
        accepted |= carve(variant, limits);
    }
    accepted
}

fn carve(data: &[u8], limits: Limits) -> bool {
    // find_sets steps over a set its checksum vouches for, and one entry past
    // anything else.
    let mut expected = Vec::new();
    let mut position = 0;
    while position < data.len() {
        match parse_set(data, position, limits) {
            Some(set) => {
                check_set(data, &set, limits);
                position += if set.checksum_ok { set.slots * 32 } else { 32 };
                expected.push(set);
            }
            None => position += 32,
        }
    }
    assert_eq!(find_sets(data, data.len(), limits), expected);
    // Sets start before the search end, and may run past it.
    let half = data.len() / 2;
    let starting: Vec<EntrySet> = expected
        .iter()
        .filter(|set| set.offset < half)
        .cloned()
        .collect();
    assert_eq!(find_sets(data, half, limits), starting);
    // Any position may be asked for, aligned or not, inside the data or past it.
    for position in [1, 31, 33, data.len(), data.len() + 32, usize::MAX] {
        if let Some(set) = parse_set(data, position, limits) {
            check_set(data, &set, limits);
        }
    }
    !expected.is_empty()
}

/// What a carved set promises about the bytes it came from.
fn check_set(data: &[u8], set: &EntrySet, limits: Limits) {
    let at = set.offset;
    assert!((3..=256).contains(&set.slots), "{} slots", set.slots);
    let bytes = &data[at..at + set.slots * 32];
    assert_eq!(set.slots, usize::from(bytes[1]) + 1);
    assert!(matches!(bytes[0], 0x85 | 0x05));
    assert!(matches!(bytes[32], 0xc0 | 0x40));
    assert_eq!(set.in_use, bytes[0] & 0x80 != 0);
    let stored = u16::from_le_bytes([bytes[2], bytes[3]]);
    assert_eq!(set.checksum_ok, set_checksum(bytes) == stored);
    let attributes = u16::from_le_bytes([bytes[4], bytes[5]]);
    assert_eq!(set.attributes, attributes);
    assert_eq!(set.is_dir, attributes & 0x10 != 0);
    assert_eq!(set.no_fat_chain, bytes[33] & 2 != 0);
    assert_eq!(set.valid_length, u64_at(bytes, 40));
    assert_eq!(set.first_cluster, u32_at(bytes, 52).unwrap());
    assert_eq!(set.length, u64_at(bytes, 56));
    let name_length = usize::from(bytes[35]);
    assert!(name_length > 0 && !set.name.is_empty());
    let names = name_length.div_ceil(15);
    assert!(2 + names <= set.slots);
    for entry in 0..names {
        assert!(matches!(bytes[64 + entry * 32], 0xc1 | 0x41));
    }
    let units: Vec<u16> = (0..name_length)
        .map(|index| {
            let start = 64 + index / 15 * 32 + 2 + index % 15 * 2;
            u16::from_le_bytes([bytes[start], bytes[start + 1]])
        })
        .collect();
    assert_eq!(set.name, String::from_utf16_lossy(&units));
    for (field, offset, utc) in [
        (&set.created, 8, 22),
        (&set.modified, 12, 23),
        (&set.accessed, 16, 24),
    ] {
        let expected = decode_timestamp_with_offset(u32_at(bytes, offset).unwrap(), bytes[utc]);
        assert_eq!(field, &expected);
    }
    let raw = RawEntrySet::parse(&data[at..]).expect("a carved set is a raw set");
    assert_eq!(raw.name().text, set.name);
    if !set.checksum_ok {
        // Without a checksum only plausible values are taken.
        assert_eq!(attributes & !0x37, 0);
        assert!(set.length <= u64::from(limits.cluster_count) * limits.cluster_size);
        assert!(units.iter().all(|unit| *unit >= 0x20));
        let heap = u64::from(FIRST_CLUSTER)..u64::from(limits.cluster_count) + 2;
        assert!(set.first_cluster == 0 || heap.contains(&u64::from(set.first_cluster)));
        for (start, length) in SET_RESERVED {
            assert!(bytes[start..start + length].iter().all(|byte| *byte == 0));
        }
    }
}

/// Starting points for `entry_sets` that a reference image cannot give: sets
/// that fail their checksum, deleted ones, and ones a reserved byte rules out.
pub fn entry_set_seeds() -> Vec<(&'static str, Vec<u8>)> {
    let with = |shape: u8, sets: &[Vec<u8>]| [vec![shape], sets.concat()].concat();
    let live = file_set("live.txt", 0x20, 9, 4096, false);
    let directory = file_set("Folder", 0x10, 12, 512, true);
    let mut deleted = file_set("deleted.bin", 0x20, 40, 70_000, false);
    for entry in deleted.chunks_exact_mut(32) {
        entry[0] &= 0x7f;
    }
    let mut stale = live.clone();
    put16(&mut stale, 2, 0x1234);
    let mut reserved = stale.clone();
    reserved[25] = 1;
    // Fourteen File Name entries and five Vendor Extensions: more than 18
    // secondaries, which a set may hold up to 255 of.
    let mut long = file_set(&"n".repeat(200), 0x20, 3, 1, false);
    for _ in 0..5 {
        long.extend([0xe0].into_iter().chain([0; 31]));
    }
    long[1] = 20;
    seal_set(&mut long);
    vec![
        ("sets", with(0x29, &[live.clone(), directory, deleted])),
        ("stale-checksum", with(0x29, &[stale.clone()])),
        ("stale-reserved", with(0x29, &[reserved, live])),
        ("long-name", with(0x0f, &[long])),
        ("smallest-heap", with(0x00, &[stale])),
    ]
}

// ---------------------------------------------------------------------------
// Chain islands.

/// `count:u16 fat:u32[count + 2] bitmap...`, each padded with zeros to the
/// length the cluster count needs. The cluster count is 1 to 1024.
pub fn chain_islands(data: &[u8]) -> bool {
    let count = u32::from(u16::from_le_bytes([
        data.first().copied().unwrap_or(0),
        data.get(1).copied().unwrap_or(0),
    ])) % 1024
        + 1;
    let last = count + FIRST_CLUSTER;
    let rest = data.get(2..).unwrap_or(&[]);
    let fat: Vec<u32> = (0..last as usize)
        .map(|index| {
            let mut word = [0; 4];
            for (byte, value) in word.iter_mut().zip(rest.iter().skip(index * 4)) {
                *byte = *value;
            }
            u32::from_le_bytes(word)
        })
        .collect();
    let mut bitmap = rest.get(last as usize * 4..).unwrap_or(&[]).to_vec();
    bitmap.resize(bitmap.len().max(count.div_ceil(8) as usize), 0);
    let mut boot = island_boot().clone();
    boot.cluster_count = count;

    let valid = |value: u32| (FIRST_CLUSTER..last).contains(&value);
    let mut targeted = vec![0u32; last as usize];
    for cluster in FIRST_CLUSTER..last {
        let value = fat[cluster as usize];
        if valid(value) {
            targeted[value as usize] += 1;
        }
    }
    let islands = islands_of(&boot, &fat, &bitmap);
    // An island starts at every in-use entry that no entry names, in order.
    let heads: Vec<u32> = (FIRST_CLUSTER..last)
        .filter(|&head| {
            let value = fat[head as usize];
            (value == FAT_END || valid(value)) && targeted[head as usize] == 0
        })
        .collect();
    assert_eq!(
        islands.iter().map(|island| island.head).collect::<Vec<_>>(),
        heads
    );
    let mut visited = vec![false; last as usize];
    for island in &islands {
        assert!(island.length >= 1 && island.allocated <= island.length);
        let mut current = island.head;
        let mut allocated = 0;
        for step in 0..island.length {
            assert!(valid(current) && !visited[current as usize], "{island:?}");
            visited[current as usize] = true;
            allocated += u64::from(is_allocated(&bitmap, current));
            if step + 1 < island.length {
                current = fat[current as usize];
            }
        }
        assert_eq!(current, island.tail, "{island:?}");
        assert_eq!(allocated, island.allocated, "{island:?}");
        let next = fat[island.tail as usize];
        match island.end {
            ISLAND_END_EOF => assert_eq!(next, FAT_END),
            ISLAND_END_BROKEN => assert!(next == 0 || next == FAT_BAD || !valid(next)),
            ISLAND_END_JOINS_ANOTHER => assert!(valid(next) && visited[next as usize]),
            other => panic!("unknown island end {other}"),
        }
    }

    // The statistics group the same islands.
    let stats = compute_stats(&boot, &fat, &bitmap);
    assert_eq!(
        stats.allocated + stats.free_with_fat + stats.free_zero_fat,
        u64::from(count)
    );
    let crossed = targeted.iter().filter(|count| **count > 1).count() as u64;
    assert_eq!(stats.cross_links, crossed);
    for (group, filter) in [
        (&stats.free_chains, 0u8),
        (&stats.allocated_chains, 1),
        (&stats.mixed_chains, 2),
    ] {
        let members: Vec<_> = islands
            .iter()
            .filter(|island| match filter {
                0 => island.allocated == 0,
                1 => island.allocated == island.length,
                _ => island.allocated != 0 && island.allocated != island.length,
            })
            .collect();
        assert_eq!(group.chains, members.len() as u64);
        assert_eq!(
            group.clusters,
            members.iter().map(|island| island.length).sum::<u64>()
        );
        assert_eq!(
            group.longest,
            members
                .iter()
                .map(|island| island.length)
                .max()
                .unwrap_or(0)
        );
    }
    !islands.is_empty()
}

fn island_boot() -> &'static primitives::BootSector {
    static BOOT: std::sync::OnceLock<primitives::BootSector> = std::sync::OnceLock::new();
    BOOT.get_or_init(|| {
        primitives::BootSectorHeader::parse(&support::volume(9))
            .unwrap()
            .sector
    })
}

/// Starting points for `chain_islands`: the template volume's FAT and bitmap,
/// and chains that end each way an island can.
pub fn chain_island_seeds() -> Vec<(&'static str, Vec<u8>)> {
    let seed = |count: u16, links: &[(u32, u32)], bitmap: &[u8]| {
        let mut fat = vec![0u32; usize::from(count) + 2];
        fat[0] = 0xffff_fff8;
        fat[1] = FAT_END;
        for &(cluster, value) in links {
            fat[cluster as usize] = value;
        }
        let mut input = (count - 1).to_le_bytes().to_vec();
        input.extend(fat.iter().flat_map(|word| word.to_le_bytes()));
        input.extend(bitmap);
        input
    };
    let image = template();
    let mut volume = 99u16.to_le_bytes().to_vec();
    volume.extend(&image[FAT..FAT + 102 * 4]);
    volume.extend(&image[BITMAP..BITMAP + 13]);
    vec![
        ("template-volume", volume),
        (
            "every-end",
            seed(
                24,
                &[
                    (2, 3),
                    (3, FAT_END),
                    (5, 6),
                    (6, 0),
                    (8, 9),
                    (9, FAT_BAD),
                    (11, 12),
                    (12, 3),
                    (14, 30),
                    (16, 17),
                    (17, 18),
                    (18, 17),
                ],
                &[0x0f, 0x30, 0x01],
            ),
        ),
        (
            "cross-linked",
            seed(8, &[(2, 4), (3, 4), (4, 5), (5, FAT_END)], &[0xff]),
        ),
    ]
}

// ---------------------------------------------------------------------------
// The salvage adapter over a damaged volume.

/// The template's layout when two FATs are asked for: four sectors each, the
/// Second FAT in what was the First's second half, and the Second Allocation
/// Bitmap in cluster 11, the first free one.
const FAT2: usize = FAT + 4 * SECTOR;
const BITMAP2: usize = FLAT + SECTOR;
const BITMAP2_CLUSTER: u32 = 11;
const FREE_HEAP: usize = BITMAP2 + SECTOR;

/// The regions a salvage patch may name: the image target's, then the Second
/// FAT and bitmap, the backup boot sector and two free heap clusters.
const SALVAGE_REGIONS: [(usize, usize); 10] = [
    REGIONS[0],
    REGIONS[1],
    REGIONS[2],
    REGIONS[3],
    REGIONS[4],
    REGIONS[5],
    (FAT2, 64),
    (BITMAP2, 16),
    (12 * SECTOR, SECTOR),
    (FREE_HEAP, 2 * SECTOR),
];

/// At most this many candidates are walked and passed to the tree, so one
/// input with thousands of carved sets stays quick.
const CANDIDATE_LIMIT: usize = 64;

/// The source a salvage input reads unless it asks for the whole 2 MiB volume:
/// the template's heap ends at 66 KiB, and nothing in salvage reads
/// VolumeLength, so the zeros after it only slow the scan down.
const SALVAGE_BYTES: usize = 72 * 1024;

/// The most bytes a plan may write before its sink refuses more.
const SINK_LIMIT: usize = 1 << 20;

/// `flags:u8 cut:u8 bad:u8[4] patches...`, where the patches are the image
/// target's over [`SALVAGE_REGIONS`]. Flag bits 0 to 2 leave the boot, set and
/// upcase checksums stale as in `image`; bit 3 gives the volume two FATs and
/// bit 4 makes the Second active; bit 5 cuts 100 bytes more off the source;
/// bit 6 keeps the whole volume, so a scan crosses its 1 MiB window, where
/// otherwise the source is its first 72 KiB. A non-zero `cut` keeps at most the
/// first `cut` KiB, and each non-zero `bad` byte makes sector `bad - 1`
/// unreadable.
pub fn salvage(data: &[u8]) -> bool {
    let source = salvage_source(data);
    let adapter = ExfatSalvage::probe(&source);
    let Some(boot) = adapter.boot().cloned() else {
        no_geometry(&adapter, &source);
        scan(&adapter, &source, UNKNOWN_LIMITS);
        damage(&adapter, &source);
        reads(&adapter, &source, &[Extent::new(0, source.size())]);
        return false;
    };
    // Usable geometry is a boot copy whose arithmetic is safe.
    let shift = boot.bytes_per_sector_shift;
    assert!((9..=12).contains(&shift));
    assert!(boot.sectors_per_cluster_shift <= 25 - shift);
    assert!((1..=MAX_CLUSTER_COUNT).contains(&boot.cluster_count));
    assert_eq!(ExfatSalvage::from_boot(boot.clone()).boot(), Some(&boot));
    assert!(adapter.capabilities().missing.is_empty());
    let limits = Limits {
        cluster_count: boot.cluster_count,
        cluster_size: boot.cluster_size(),
    };
    let layout = FatLayout::from_boot(&boot);
    assert_eq!(adapter.fat(), Some(layout.active));

    let mut candidates = scan(&adapter, &source, limits);
    candidates.truncate(CANDIDATE_LIMIT);
    let fat = layout.offset(layout.active).unwrap();
    for candidate in &candidates {
        walk(&adapter, &source, candidate, fat);
    }
    tree(&adapter, &source, &candidates);
    damage(&adapter, &source);
    let extents: Vec<Extent> = candidates
        .iter()
        .take(4)
        .flat_map(|candidate| adapter.walk_extents(&source, candidate).unwrap().extents)
        .collect();
    reads(&adapter, &source, &extents);
    if let Some(candidate) = candidates.first() {
        materialize(&adapter, &source, &rebuild(&adapter, &source, candidate));
    }
    stale(&adapter, &source, fat, &boot);
    uninterpreted(&adapter, &source, &candidates);
    copies(&adapter, &source, &boot);
    upcase(&adapter, &source, &candidates);

    // The other pair, where the boot fields describe one, maps every claim
    // through its own FAT.
    assert_eq!(
        adapter
            .with_fat(FatCopy::First)
            .and_then(|other| other.fat()),
        Some(FatCopy::First)
    );
    let second = adapter.with_fat(FatCopy::Second);
    assert_eq!(second.is_some(), layout.second);
    if let Some(other) = adapter.with_fat(layout.active.other()) {
        let fat = layout.offset(layout.active.other()).unwrap();
        assert_eq!(other.fat(), Some(layout.active.other()));
        for candidate in candidates.iter().take(8) {
            walk(&other, &source, candidate, fat);
        }
        stale(&other, &source, fat, &boot);
    }
    true
}

/// The plan that rebuilds a candidate's file as its walk found it: its cluster
/// runs, zeros for what the walk could not reach, cut to the claimed length
/// and zeroed from the valid data length.
fn rebuild(adapter: &ExfatSalvage, source: &Faulty, candidate: &Candidate<EntrySetDetail>) -> Plan {
    let boot = adapter.boot().unwrap();
    let walk = adapter.walk_extents(source, candidate).unwrap();
    let size = boot.cluster_size();
    let mut steps: Vec<PlanStep> = walk
        .extents
        .iter()
        .map(|extent| PlanStep::Clusters {
            first: (extent.offset - boot.heap_offset()) / size + u64::from(FIRST_CLUSTER),
            count: extent.length.div_ceil(size),
        })
        .collect();
    steps.push(PlanStep::Zeros(walk.claimed - walk.mapped()));
    Plan {
        steps,
        length: Some(walk.claimed),
        valid_length: Some(candidate.detail.valid_data_length.min(walk.claimed)),
        metadata: fs_exfat::salvage::PlanMetadata {
            name: candidate.name.clone(),
            created: candidate.detail.created.clone(),
            modified: candidate.detail.modified.clone(),
            accessed: candidate.detail.accessed.clone(),
        },
    }
}

fn salvage_source(data: &[u8]) -> Faulty {
    let [flags, cut, bad @ ..] = data.get(..6).unwrap_or(&[0; 6]) else {
        unreachable!()
    };
    let whole = template();
    let mut image = if flags & 64 == 0 {
        whole[..SALVAGE_BYTES].to_vec()
    } else {
        whole.to_vec()
    };
    if flags & 8 != 0 {
        two_fats(&mut image, flags & 16 != 0);
    }
    apply_patches(&mut image, &SALVAGE_REGIONS, data.get(6..).unwrap_or(&[]));
    seal(&mut image, *flags);
    if *cut != 0 {
        image.truncate(usize::from(*cut) * 1024);
    }
    if flags & 32 != 0 {
        image.truncate(image.len() - 100);
    }
    let mut bad: Vec<u64> = bad
        .iter()
        .filter(|sector| **sector != 0)
        .map(|sector| u64::from(*sector) - 1)
        .collect();
    bad.sort_unstable();
    bad.dedup();
    Faulty { bytes: image, bad }
}

/// Gives the template a Second FAT and Second Allocation Bitmap, the TexFAT
/// layout of sections 3.1.16 and 7.1.2.1, in place of its Volume GUID entry.
fn two_fats(image: &mut [u8], second_active: bool) {
    put32(image, 84, 4);
    image[110] = 2;
    if second_active {
        image[106] |= 1;
    }
    put32(image, FAT + BITMAP2_CLUSTER as usize * 4, FAT_END);
    image.copy_within(FAT..FAT + 4 * SECTOR, FAT2);
    let entry = ROOT + 96;
    image[entry..entry + 32].fill(0);
    image[entry] = 0x81;
    image[entry + 1] = 1;
    put32(image, entry + 20, BITMAP2_CLUSTER);
    put64(image, entry + 24, 13);
    image[BITMAP + 1] |= 0x02;
    image.copy_within(BITMAP..BITMAP + 16, BITMAP2);
}

/// Without geometry the verbs that map claims refuse with `NoGeometry`; the
/// scan, the damage report and extent reads still run.
fn no_geometry(adapter: &ExfatSalvage, source: &Faulty) {
    let candidate = Candidate {
        location: Extent::new(0, 96),
        kind: CandidateKind::Directory,
        state: CandidateState::Live,
        integrity: Integrity::Verified,
        name: None,
        size: Some(512),
        detail: EntrySetDetail {
            attributes: 0x10,
            first_cluster: 2,
            valid_data_length: 512,
            no_fat_chain: false,
            created: None,
            modified: None,
            accessed: None,
            continuation: Vec::new(),
        },
    };
    let candidates = [candidate.clone()];
    let refused = |result: Result<(), SalvageError>| {
        assert!(
            matches!(result, Err(SalvageError::NoGeometry)),
            "{result:?}"
        )
    };
    refused(adapter.walk_extents(source, &candidate).map(drop));
    refused(adapter.rebuild_tree(source, &candidates).map(drop));
    refused(adapter.stale_chains(source, &mut Vec::new()).map(drop));
    refused(
        adapter
            .uninterpreted_entries(source, &candidates, &mut Vec::new())
            .map(drop),
    );
    refused(adapter.fat_copies(source, &mut Vec::new()).map(drop));
    refused(adapter.upcase_table(source).map(drop));
    refused(adapter.cluster_layout().map(drop));
    assert!(adapter.with_fat(FatCopy::First).is_none());
    assert!(adapter.fat().is_none());
}

/// Scans the whole source, checks every candidate against the carver over the
/// same bytes, and checks that a stopped scan resumes where its report says.
fn scan(adapter: &ExfatSalvage, source: &Faulty, limits: Limits) -> Vec<Candidate<EntrySetDetail>> {
    let size = source.size();
    for outside in [
        Extent::new(0, size + 1),
        Extent::new(size, 1),
        Extent::new(u64::MAX, 2),
    ] {
        let result = adapter.find_candidates(source, outside, &mut Vec::new());
        assert!(matches!(result, Err(SalvageError::Bounds)), "{result:?}");
    }
    let mut found = Vec::new();
    let report = adapter
        .find_candidates(source, Extent::new(0, size), &mut found)
        .unwrap();
    assert_eq!(report.candidates, found.len() as u64);
    assert_eq!(report.stopped_at, None);
    // Exactly the bytes that do not read are listed, merged.
    let mut expected: Vec<Extent> = Vec::new();
    for sector in 0..size.div_ceil(512) {
        if !source.sector_reads(sector) {
            let start = sector * 512;
            let piece = Extent::new(start, (start + 512).min(size) - start);
            match expected.last_mut() {
                Some(last) if last.end() == Some(start) => last.length += piece.length,
                _ => expected.push(piece),
            }
        }
    }
    assert_eq!(report.unreadable, expected);

    let mut previous: Option<&Candidate<EntrySetDetail>> = None;
    for candidate in &found {
        let at = candidate.location.offset;
        let end = candidate.location.end().unwrap();
        assert!(at.is_multiple_of(32) && end <= size, "{candidate:?}");
        assert!(
            source.readable(at, end),
            "a candidate overlaps unreadable bytes"
        );
        if let Some(previous) = previous {
            let step = if previous.integrity == Integrity::Verified {
                previous.location.length
            } else {
                32
            };
            assert!(at >= previous.location.offset + step, "{candidate:?}");
        }
        previous = Some(candidate);
        let continuation = &candidate.detail.continuation;
        if continuation.is_empty() {
            let set = parse_set(&source.bytes, at as usize, limits)
                .expect("a candidate is a set where it stands");
            assert_eq!(candidate, &as_candidate(set, at));
        } else {
            // A set that runs into a distant cluster (#99) is its entries in
            // the first cluster followed by each continued piece.
            let mut bytes = source.bytes[at as usize..end as usize].to_vec();
            for piece in continuation {
                let piece_end = piece.end().unwrap();
                assert!(piece_end <= size, "{candidate:?}");
                assert!(
                    source.readable(piece.offset, piece_end),
                    "a continuation overlaps unreadable bytes"
                );
                bytes.extend_from_slice(&source.bytes[piece.offset as usize..piece_end as usize]);
            }
            let set = parse_set(&bytes, 0, limits).expect("a split candidate is a set when joined");
            let mut whole = as_candidate(set, at);
            assert_eq!(whole.location.length, bytes.len() as u64, "{candidate:?}");
            whole.location = candidate.location;
            whole.detail.continuation = continuation.clone();
            assert_eq!(candidate, &whole);
        }
    }
    if expected.is_empty() {
        let split: Vec<u64> = found
            .iter()
            .filter(|candidate| !candidate.detail.continuation.is_empty())
            .map(|candidate| candidate.location.offset)
            .collect();
        let carved: Vec<_> = find_sets(&source.bytes, source.bytes.len(), limits)
            .into_iter()
            .filter(|set| !split.contains(&(set.offset as u64)))
            .map(|set| {
                let at = set.offset as u64;
                as_candidate(set, at)
            })
            .collect();
        // The carver reads linearly, so it finds the sets that lie in one
        // piece; the scan also follows the FAT for the ones that do not, in
        // place of whatever the linear read gave at that offset.
        let whole: Vec<_> = found
            .iter()
            .filter(|candidate| candidate.detail.continuation.is_empty())
            .cloned()
            .collect();
        assert_eq!(whole, carved, "the scan and the carver disagree");
    }
    round_trip(&found);

    // A scan stopped part way resumes from its report to the same candidates.
    if found.len() > 1 {
        let keep = found.len() / 2;
        let mut first = Vec::new();
        let mut sink = |candidate: Candidate<EntrySetDetail>| {
            first.push(candidate);
            if first.len() == keep {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let stopped = adapter
            .find_candidates(source, Extent::new(0, size), &mut sink)
            .unwrap();
        assert_eq!(stopped.candidates, keep as u64);
        let resume = stopped.stopped_at.expect("a stopped scan says where");
        let mut rest = Vec::new();
        adapter
            .find_candidates(source, Extent::new(resume, size - resume), &mut rest)
            .unwrap();
        first.extend(rest);
        assert_eq!(first, found, "a resumed scan differs");
    }
    found
}

/// The candidate the adapter makes of a carved set, from the field docs.
fn as_candidate(set: EntrySet, at: u64) -> Candidate<EntrySetDetail> {
    Candidate {
        location: Extent::new(at, (set.slots * 32) as u64),
        kind: if set.is_dir {
            CandidateKind::Directory
        } else {
            CandidateKind::File
        },
        state: if set.in_use {
            CandidateState::Live
        } else {
            CandidateState::Deleted
        },
        integrity: if set.checksum_ok {
            Integrity::Verified
        } else {
            Integrity::Failed
        },
        name: Some(set.name),
        size: Some(set.length),
        detail: EntrySetDetail {
            attributes: set.attributes,
            first_cluster: set.first_cluster,
            valid_data_length: set.valid_length,
            no_fat_chain: set.no_fat_chain,
            created: set.created,
            modified: set.modified,
            accessed: set.accessed,
            continuation: Vec::new(),
        },
    }
}

/// Clusters a walk's extents cover, in walk order.
fn clusters_of(boot: &primitives::BootSector, extents: &[Extent]) -> Vec<u32> {
    let size = boot.cluster_size();
    extents
        .iter()
        .flat_map(|extent| {
            let first = (extent.offset - boot.heap_offset()) / size;
            (first..first + extent.length.div_ceil(size)).map(|index| index as u32 + FIRST_CLUSTER)
        })
        .collect()
}

/// A walk keeps every extent it reached, inside the heap, linked as the FAT
/// links them, and ends for the reason it gives.
fn walk(adapter: &ExfatSalvage, source: &Faulty, candidate: &Candidate<EntrySetDetail>, fat: u64) {
    let boot = adapter.boot().unwrap();
    let walk = adapter.walk_extents(source, candidate).unwrap();
    assert_eq!(walk.claimed, candidate.size.unwrap_or(0));
    assert!(walk.mapped() <= walk.claimed, "{walk:?}");
    assert_eq!(
        walk.end == WalkEnd::Complete,
        walk.mapped() == walk.claimed,
        "{walk:?}"
    );
    let heap = boot.heap_offset();
    let heap_end = heap + u64::from(boot.cluster_count) * boot.cluster_size();
    for pair in walk.extents.windows(2) {
        assert_ne!(
            pair[0].end(),
            Some(pair[1].offset),
            "adjacent extents not merged"
        );
    }
    for extent in &walk.extents {
        assert!(extent.length > 0, "{walk:?}");
        assert!(
            extent.offset >= heap && extent.end().unwrap() <= heap_end,
            "{walk:?}"
        );
        assert!((extent.offset - heap).is_multiple_of(boot.cluster_size()));
    }
    let clusters = clusters_of(boot, &walk.extents);
    if let Some(first) = clusters.first() {
        assert_eq!(*first, candidate.detail.first_cluster);
    }
    let link = |cluster: u32| source.word(fat + u64::from(cluster) * 4);
    if candidate.detail.no_fat_chain {
        assert!(walk.extents.len() <= 1, "{walk:?}");
        assert!(!matches!(walk.end, WalkEnd::Cycle | WalkEnd::Unreadable(_)));
        return;
    }
    for pair in clusters.windows(2) {
        assert_eq!(link(pair[0]), Some(pair[1]), "a walk left its FAT chain");
    }
    let valid = |value: u32| (FIRST_CLUSTER..boot.last_cluster_exclusive()).contains(&value);
    match (walk.end, clusters.last()) {
        (WalkEnd::Cycle, Some(last)) => {
            let next = link(*last).unwrap();
            assert!(clusters.contains(&next), "{walk:?}");
        }
        (WalkEnd::Broken, Some(last)) => {
            let next = link(*last).unwrap();
            assert!(!valid(next), "a walk broke at a link to cluster {next}");
        }
        (WalkEnd::Unreadable(extent), last) => {
            let cluster = last.copied().unwrap_or(candidate.detail.first_cluster);
            assert_eq!(extent, Extent::new(fat + u64::from(cluster) * 4, 4));
            assert!(
                !source.readable(extent.offset, extent.end().unwrap()),
                "{walk:?}"
            );
        }
        _ => {}
    }
}

/// Every candidate's parents are exactly the directories whose walked clusters
/// hold its record: the root chain, and every directory candidate's claim up to
/// the 256 MiB directory limit.
fn tree(adapter: &ExfatSalvage, source: &Faulty, candidates: &[Candidate<EntrySetDetail>]) {
    const DIRECTORY_LIMIT: u64 = 256 << 20;
    let boot = adapter.boot().unwrap();
    let tree = adapter.rebuild_tree(source, candidates).unwrap();
    assert_eq!(tree.nodes.len(), candidates.len());
    let claim = |first: u32, size: u64, contiguous: bool| {
        let mut directory = as_candidate(placeholder_set(), 0);
        directory.size = Some(size);
        directory.detail.first_cluster = first;
        directory.detail.no_fat_chain = contiguous;
        adapter.walk_extents(source, &directory).unwrap().extents
    };
    // A cluster is in a claim when its first byte lies in one of the claim's
    // extents, which start on cluster boundaries.
    let holds = |extents: &[Extent], cluster: u32| {
        let offset = primitives::cluster_offset(boot, cluster);
        extents
            .iter()
            .any(|extent| extent.offset <= offset && extent.end().is_some_and(|end| offset < end))
    };
    let root = claim(boot.root_cluster, DIRECTORY_LIMIT, false);
    let directories: Vec<(u64, Vec<Extent>)> = candidates
        .iter()
        .filter(|candidate| candidate.kind == CandidateKind::Directory)
        .map(|directory| {
            let size = directory.size.unwrap_or(0).min(DIRECTORY_LIMIT);
            let extents = claim(
                directory.detail.first_cluster,
                size,
                directory.detail.no_fat_chain,
            );
            (directory.location.offset, extents)
        })
        .collect();
    for (node, candidate) in tree.nodes.iter().zip(candidates) {
        assert_eq!(node.candidate, candidate.location.offset);
        let unique: HashSet<_> = node.parents.iter().collect();
        assert_eq!(unique.len(), node.parents.len(), "a parent claim repeats");
        let heap = boot.heap_offset();
        let cluster = candidate
            .location
            .offset
            .checked_sub(heap)
            .map(|offset| offset / boot.cluster_size())
            .filter(|index| *index < u64::from(boot.cluster_count))
            .map(|index| index as u32 + FIRST_CLUSTER);
        let mut expected = HashSet::new();
        if let Some(cluster) = cluster {
            if holds(&root, cluster) {
                expected.insert(Parent::Root);
            }
            for (offset, extents) in &directories {
                if holds(extents, cluster) {
                    expected.insert(Parent::Candidate(*offset));
                }
            }
        }
        assert_eq!(unique, expected.iter().collect(), "{node:?}");
    }
    round_trip(&tree);
}

fn placeholder_set() -> EntrySet {
    EntrySet {
        offset: 0,
        name: String::new(),
        in_use: true,
        checksum_ok: true,
        attributes: 0x10,
        is_dir: true,
        first_cluster: 0,
        length: 0,
        valid_length: 0,
        no_fat_chain: false,
        created: None,
        modified: None,
        accessed: None,
        slots: 3,
    }
}

/// Damage is evidence: an unreadable finding names bytes that do not read, and
/// a stopped report is a prefix of the whole one.
fn damage(adapter: &ExfatSalvage, source: &Faulty) {
    let mut found: Vec<Damage> = Vec::new();
    let report = adapter.report_damage(source, &mut found).unwrap();
    assert_eq!(report.reported, found.len() as u64);
    assert!(!report.stopped);
    for finding in &found {
        assert!(!finding.structure.is_empty(), "{finding:?}");
        if finding.kind == DamageKind::Unreadable {
            let end = finding.location.end().unwrap_or(u64::MAX);
            assert!(
                !source.readable(finding.location.offset, end),
                "{finding:?}"
            );
        }
    }
    round_trip(&found);
    let prefix = prefix_of(&found, |sink| {
        let report = adapter.report_damage(source, sink).unwrap();
        (report.reported, report.stopped)
    });
    assert_eq!(prefix, found[..prefix.len()]);
}

/// Runs `listing` with a sink that stops after half of `whole`, and checks
/// that it says it stopped there.
fn prefix_of<T: Clone + std::fmt::Debug>(
    whole: &[T],
    listing: impl FnOnce(&mut dyn fs_exfat::salvage::Sink<T>) -> (u64, bool),
) -> Vec<T> {
    if whole.len() < 2 {
        return whole.to_vec();
    }
    let keep = whole.len() / 2;
    let mut taken = Vec::new();
    let mut sink = |item: T| {
        taken.push(item);
        if taken.len() == keep {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let (passed, stopped) = listing(&mut sink);
    assert_eq!(passed, keep as u64);
    assert!(stopped);
    taken
}

/// `read_extent` reads exactly the bytes asked for when they read, and refuses
/// what leaves the extent or the source.
fn reads(adapter: &ExfatSalvage, source: &Faulty, extents: &[Extent]) {
    let size = source.size();
    let outside = |result: Result<usize, SalvageError>| {
        assert!(matches!(result, Err(SalvageError::Bounds)), "{result:?}")
    };
    outside(adapter.read_extent(source, Extent::new(0, 4), 5, &mut [0; 8]));
    outside(adapter.read_extent(source, Extent::new(size, 1), 0, &mut [0; 8]));
    outside(adapter.read_extent(source, Extent::new(u64::MAX, 2), 0, &mut [0; 8]));
    let mut buffer = vec![0; 4096];
    for extent in extents.iter().take(8) {
        if extent.end().is_none_or(|end| end > size) {
            outside(adapter.read_extent(source, *extent, 0, &mut buffer));
            continue;
        }
        for offset in [0, 1, extent.length / 2, extent.length] {
            if offset > extent.length {
                continue;
            }
            let count = (extent.length - offset).min(buffer.len() as u64);
            let start = extent.offset + offset;
            match adapter.read_extent(source, *extent, offset, &mut buffer) {
                Ok(read) => {
                    assert_eq!(read as u64, count);
                    assert!(source.readable(start, start + count));
                    assert_eq!(&buffer[..read], source.at(Extent::new(start, count)));
                }
                Err(SalvageError::Device(_)) => {
                    assert!(!source.readable(start, start + count))
                }
                Err(error) => panic!("{extent:?} at {offset}: {error}"),
            }
        }
    }
}

/// Stale chains are free clusters with non-zero FAT entries, each listed once,
/// linked as their entries say, ending as their last entry says.
fn stale(adapter: &ExfatSalvage, source: &Faulty, fat: u64, boot: &primitives::BootSector) {
    let mut chains: Vec<StaleChain> = Vec::new();
    let report = adapter.stale_chains(source, &mut chains).unwrap();
    assert!(!report.stopped);
    assert_eq!(report.chains, chains.len() as u64);
    let entries: usize = chains.iter().map(|chain| chain.entries.len()).sum();
    assert_eq!(report.entries, entries as u64);
    let heap = u64::from(FIRST_CLUSTER)..u64::from(boot.cluster_count) + 2;
    let mut listed = HashSet::new();
    for chain in &chains {
        assert!(!chain.entries.is_empty());
        assert_eq!(chain.head(), Some(chain.entries[0].cluster));
        for entry in &chain.entries {
            assert!(heap.contains(&u64::from(entry.cluster)), "{chain:?}");
            assert_ne!(entry.value, 0, "{chain:?}");
            let location = Extent::new(fat + u64::from(entry.cluster) * 4, 4);
            assert_eq!(entry.location, location);
            assert_eq!(source.word(location.offset), Some(entry.value));
        }
        for pair in chain.entries.windows(2) {
            assert_eq!(pair[0].value, pair[1].cluster, "{chain:?}");
        }
        let mine: HashSet<u32> = chain.entries.iter().map(|entry| entry.cluster).collect();
        let last = chain.entries.last().unwrap().value;
        match chain.end {
            StaleEnd::End => assert_eq!(last, FAT_END),
            StaleEnd::BadMarker => assert_eq!(last, FAT_BAD),
            StaleEnd::OutOfHeap(target) => {
                assert_eq!(target, last);
                assert!(!heap.contains(&u64::from(target)));
            }
            StaleEnd::Cycle(target) => {
                assert_eq!(target, last);
                assert!(mine.contains(&target));
            }
            StaleEnd::JoinsChain(target) => {
                assert_eq!(target, last);
                assert!(listed.contains(&target), "{chain:?}");
            }
            StaleEnd::FreeTarget(target)
            | StaleEnd::JoinsAllocated(target)
            | StaleEnd::Unknown(target) => {
                assert_eq!(target, last);
                assert!(heap.contains(&u64::from(target)));
            }
        }
        for cluster in &mine {
            assert!(listed.insert(*cluster), "cluster {cluster} listed twice");
        }
        let bytes: u64 = chain.extents.iter().map(|extent| extent.length).sum();
        assert_eq!(bytes, chain.entries.len() as u64 * boot.cluster_size());
        for pair in chain.extents.windows(2) {
            assert_ne!(pair[0].end(), Some(pair[1].offset));
        }
    }
    let prefix = prefix_of(&chains, |sink| {
        let report = adapter.stale_chains(source, sink).unwrap();
        (report.chains, report.stopped)
    });
    assert_eq!(prefix, chains[..prefix.len()]);
}

/// Each uninterpreted entry is the entry the source holds where it says, of
/// the kind its type makes it, in a directory the tree would name.
fn uninterpreted(
    adapter: &ExfatSalvage,
    source: &Faulty,
    candidates: &[Candidate<EntrySetDetail>],
) {
    let mut entries: Vec<UninterpretedEntry> = Vec::new();
    let report = adapter
        .uninterpreted_entries(source, candidates, &mut entries)
        .unwrap();
    assert!(!report.stopped);
    assert_eq!(report.counts.total(), entries.len() as u64);
    let directories: HashSet<u64> = candidates
        .iter()
        .filter(|candidate| candidate.kind == CandidateKind::Directory)
        .map(|candidate| candidate.location.offset)
        .collect();
    for entry in &entries {
        assert_eq!(entry.location.length, 32);
        let at = entry.location.offset;
        assert!(source.readable(at, at + 32), "{entry:?}");
        assert_eq!(source.bytes[at as usize], entry.entry_type, "{entry:?}");
        assert_eq!(entry.in_use, entry.entry_type & 0x80 != 0);
        assert_eq!(UninterpretedKind::of(entry.entry_type), Some(entry.kind));
        match entry.directory {
            Parent::Root => {}
            Parent::Candidate(offset) => assert!(directories.contains(&offset), "{entry:?}"),
        }
        if let Some(set) = entry.set {
            assert_eq!(set.primary.length, 32);
            assert!(set.primary.offset <= at);
            if set.valid {
                assert!(set.checksum_ok);
            }
            if entry.kind.is_primary() {
                assert_eq!(set.primary.offset, at);
            }
        }
    }
    let mut counted = fs_exfat::salvage::UninterpretedCounts::default();
    for entry in &entries {
        counted.add(entry.kind);
    }
    assert_eq!(counted, report.counts);
    let prefix = prefix_of(&entries, |sink| {
        let report = adapter
            .uninterpreted_entries(source, candidates, sink)
            .unwrap();
        (report.counts.total(), report.stopped)
    });
    assert_eq!(prefix, entries[..prefix.len()]);
}

/// Every difference the copies report is one the bytes hold, and with every
/// cluster compared the count is every difference there is.
fn copies(adapter: &ExfatSalvage, source: &Faulty, boot: &primitives::BootSector) {
    let layout = FatLayout::from_boot(boot);
    let mut differences: Vec<CopyDifference> = Vec::new();
    let report = adapter.fat_copies(source, &mut differences).unwrap();
    assert!(!report.stopped);
    assert_eq!(report.active, layout.active);
    let pairs: Vec<FatCopy> = layout.copies().collect();
    assert_eq!(
        report
            .pairs
            .iter()
            .map(|pair| pair.copy)
            .collect::<Vec<_>>(),
        pairs
    );
    for pair in &report.pairs {
        assert_eq!(
            pair.fat,
            Extent::new(layout.offset(pair.copy).unwrap(), layout.length)
        );
    }
    let count = |kind| {
        differences
            .iter()
            .filter(|found| found.kind == kind)
            .count() as u64
    };
    assert_eq!(report.fat_differences, count(CopyKind::Fat));
    assert_eq!(report.bitmap_differences, count(CopyKind::Bitmap));
    assert!(report.fat_differences <= report.fat_compared);
    assert!(report.bitmap_differences <= report.bitmap_compared);
    assert!(report.fat_compared <= u64::from(boot.cluster_count));
    assert!(report.bitmap_compared <= u64::from(boot.cluster_count));
    if !layout.second {
        assert!(differences.is_empty());
        assert_eq!((report.fat_compared, report.bitmap_compared), (0, 0));
        return;
    }
    let heap = u64::from(FIRST_CLUSTER)..u64::from(boot.cluster_count) + 2;
    let fats = [
        layout.offset(FatCopy::First).unwrap(),
        layout.offset(FatCopy::Second).unwrap(),
    ];
    let mut last: Option<(CopyKind, u32)> = None;
    for difference in &differences {
        assert!(
            heap.contains(&u64::from(difference.cluster)),
            "{difference:?}"
        );
        assert_ne!(difference.first, difference.second, "{difference:?}");
        if let Some((kind, cluster)) = last {
            let order = |kind| u8::from(kind == CopyKind::Bitmap);
            assert!(
                (order(kind), cluster) < (order(difference.kind), difference.cluster),
                "differences out of order"
            );
        }
        last = Some((difference.kind, difference.cluster));
        match difference.kind {
            CopyKind::Fat => {
                for (fat, location, value) in [
                    (fats[0], difference.first_location, difference.first),
                    (fats[1], difference.second_location, difference.second),
                ] {
                    let expected = Extent::new(fat + u64::from(difference.cluster) * 4, 4);
                    assert_eq!(location, expected);
                    assert_eq!(source.word(location.offset), Some(value));
                }
            }
            CopyKind::Bitmap => {
                let index = difference.cluster - FIRST_CLUSTER;
                for (location, value) in [
                    (difference.first_location, difference.first),
                    (difference.second_location, difference.second),
                ] {
                    assert_eq!(location.length, 1);
                    assert!(value <= 1);
                    let byte = source.at(location)[0];
                    assert_eq!(u32::from(byte >> (index % 8) & 1), value, "{difference:?}");
                }
            }
        }
    }
    if report.fat_compared == u64::from(boot.cluster_count) {
        let differing = heap
            .clone()
            .filter(|cluster| {
                source.word(fats[0] + cluster * 4) != source.word(fats[1] + cluster * 4)
            })
            .count() as u64;
        assert_eq!(report.fat_differences, differing);
    }
    let prefix = prefix_of(&differences, |sink| {
        let report = adapter.fat_copies(source, sink).unwrap();
        (
            report.fat_differences + report.bitmap_differences,
            report.stopped,
        )
    });
    assert_eq!(prefix, differences[..prefix.len()]);
}

/// The volume's Up-case Table is used only whole and valid, and names compare
/// by Table 24 at least.
fn upcase(adapter: &ExfatSalvage, source: &Faulty, candidates: &[Candidate<EntrySetDetail>]) {
    let report = adapter.upcase_table(source).unwrap();
    if report.usable {
        assert_eq!(report.mappings.len(), 65536);
        assert!(report.unreadable.is_empty());
        let entry = report.entry.expect("a usable table has an entry");
        let bytes: Vec<u8> = report
            .extents
            .iter()
            .flat_map(|extent| source.at(*extent).to_vec())
            .collect();
        let checksum = source.word(entry.offset + 4).unwrap();
        let table = RawUpcaseTable::parse(&bytes, checksum);
        assert!(table.usable());
        assert_eq!(table.mappings, report.mappings);
        assert_eq!(table.compressed, report.compressed);
    } else {
        assert!(report.mappings.is_empty());
        for unit in [0, 0x41, 0x61, 0x7a, 0x7b, 0xe9, 0xffff] {
            assert_eq!(report.upcase(unit), primitives::mandatory_upcase(unit));
        }
    }
    if let Some(entry) = report.entry {
        assert_eq!(entry.length, 32);
        if source.readable(entry.offset, entry.offset + 32) {
            assert_eq!(source.bytes[entry.offset as usize], 0x82);
        }
    }
    for name in candidates
        .iter()
        .filter_map(|candidate| candidate.name.as_ref())
    {
        assert!(report.same_name(name, name));
        assert!(report.same_name(name, &name.to_ascii_uppercase()));
        assert!(!report.same_name(name, &format!("{name}x")));
    }
    // Without its 65536 mappings, which cost more to round-trip than the rest of
    // the target does to run.
    round_trip(&fs_exfat::salvage::UpcaseReport {
        mappings: Vec::new(),
        ..report
    });
}

/// What salvage finds on a `salvage` input, for the gate to hold the seeds to.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SalvageFindings {
    pub candidates: usize,
    pub orphans: usize,
    pub damage: usize,
    pub stale_entries: u64,
    pub uninterpreted: u64,
    pub upcase_usable: bool,
    pub fat_differences: u64,
    pub bitmap_differences: u64,
    pub fats_compared: bool,
}

pub fn salvage_findings(data: &[u8]) -> SalvageFindings {
    let source = salvage_source(data);
    let adapter = ExfatSalvage::probe(&source);
    let mut candidates = Vec::new();
    adapter
        .find_candidates(&source, Extent::new(0, source.size()), &mut candidates)
        .unwrap();
    let mut damage = Vec::new();
    adapter.report_damage(&source, &mut damage).unwrap();
    let tree = adapter.rebuild_tree(&source, &candidates).unwrap();
    let stale = adapter.stale_chains(&source, &mut Vec::new()).unwrap();
    let uninterpreted = adapter
        .uninterpreted_entries(&source, &candidates, &mut Vec::new())
        .unwrap();
    let copies = adapter.fat_copies(&source, &mut Vec::new()).unwrap();
    let count = adapter.boot().unwrap().cluster_count;
    SalvageFindings {
        candidates: candidates.len(),
        orphans: tree.orphans().count(),
        damage: damage.len(),
        stale_entries: stale.entries,
        uninterpreted: uninterpreted.counts.total(),
        upcase_usable: adapter.upcase_table(&source).unwrap().usable,
        fat_differences: copies.fat_differences,
        bitmap_differences: copies.bitmap_differences,
        fats_compared: copies.fat_compared == u64::from(count)
            && copies.bitmap_compared == u64::from(count),
    }
}

/// Starting points for `salvage`: the volume whole, and each kind of damage
/// salvage reports, on one FAT and on two.
pub fn salvage_seeds() -> Vec<(&'static str, Vec<u8>)> {
    let input = |flags: u8, cut: u8, bad: [u8; 4], patches: &[(u8, u16, &[u8])]| {
        let mut input = vec![flags, cut];
        input.extend(bad);
        for (region, offset, bytes) in patches {
            input.push(*region);
            input.extend(offset.to_le_bytes());
            input.push(bytes.len() as u8);
            input.extend(*bytes);
        }
        input
    };
    let none = [0; 4];
    let mut deleted = file_set("gone.txt", 0x20, 12, 900, false);
    for entry in deleted.chunks_exact_mut(32) {
        entry[0] &= 0x7f;
    }
    let mut vendor = vec![0u8; 32];
    vendor[0] = 0xe0;
    vec![
        ("pristine", input(0, 0, none, &[])),
        ("whole-volume", input(64, 0, none, &[])),
        ("two-fats", input(8, 0, none, &[])),
        ("two-fats-second-active", input(24, 0, none, &[])),
        (
            "two-fats-differ",
            input(
                8,
                0,
                none,
                &[(6, 7 * 4, &0u32.to_le_bytes()), (7, 0, &[0x7f])],
            ),
        ),
        ("bad-root-sector", input(0, 0, [33, 0, 0, 0], &[])),
        ("bad-fat-sector", input(0, 0, [25, 0, 0, 0], &[])),
        ("bad-fat-neighbour-sector", input(0, 0, [32, 0, 0, 0], &[])),
        ("bad-boot-sectors", input(0, 0, [1, 13, 0, 0], &[])),
        ("main-boot-stale", input(1, 0, none, &[(0, 3, b"NOTEXFAT")])),
        ("truncated", input(32, 21, none, &[])),
        (
            "stale-chain",
            input(
                0,
                0,
                none,
                &[(1, 12 * 4, &[13, 0, 0, 0, 0xff, 0xff, 0xff, 0xff])],
            ),
        ),
        ("deleted-set", input(0, 0, none, &[(9, 0, &deleted)])),
        ("vendor-entry", input(0, 0, none, &[(5, 0, &vendor)])),
        (
            "fat-cycle",
            input(0, 0, none, &[(1, 7 * 4, &7u32.to_le_bytes())]),
        ),
        ("upcase-damaged", input(4, 0, none, &[(4, 10, &[0x41, 0])])),
    ]
}

// ---------------------------------------------------------------------------
// Materialization plans.

/// A sink that refuses to grow past [`SINK_LIMIT`], so a plan of endless zeros
/// stops at its first refused write.
struct Capped(Vec<u8>);

impl io::Write for Capped {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len() + bytes.len() > SINK_LIMIT {
            return Err(io::Error::other("sink full"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// `flags:u8 size:u8 bad:u8[2] plan...`. The source holds `size` KiB of a
/// counting pattern, 300 bytes more when flag bit 0 is set, and each non-zero
/// `bad` byte makes sector `bad - 1` unreadable. Flag bit 2 gives the adapter
/// geometry, a heap of 64 clusters from byte 2048 of 512 bytes shifted left by
/// flag bits 3 and 4, so cluster steps map; without it they are refused. Flag
/// bit 1 reads the plan as JSON, the form the plan types serialize to.
/// Otherwise each tag byte, modulo 11, adds a step or sets a field from the
/// little-endian fields after it: 0 an extent of a 24-bit offset and a 16-bit
/// length, 1 one of two 24-bit fields, 2 one of two 64-bit fields, 3 zeros of a
/// 16-bit length, 4 zeros of a 64-bit length, 5 a count byte and that many
/// literal bytes, 6 a cluster run of two 8-bit fields, 7 one of two 64-bit
/// fields, 8 a 24-bit final length, 9 a 24-bit valid length, and 10 a count
/// byte and that many bytes of name.
pub fn plan(data: &[u8]) -> bool {
    let [flags, size, bad @ ..] = data.get(..4).unwrap_or(&[0; 4]) else {
        unreachable!()
    };
    let body = data.get(4..).unwrap_or(&[]);
    let length = usize::from(*size) * 1024 + if flags & 1 != 0 { 300 } else { 0 };
    let source = Faulty {
        bytes: (0..length)
            .map(|index| (index * 7 + index / 251) as u8)
            .collect(),
        bad: bad
            .iter()
            .filter(|sector| **sector != 0)
            .map(|sector| u64::from(*sector) - 1)
            .collect(),
    };
    let plan = if flags & 2 != 0 {
        match serde_json::from_slice::<Plan>(body) {
            Ok(plan) => plan,
            Err(_) => return false,
        }
    } else {
        decode_plan(body)
    };
    round_trip(&plan);
    let adapter = if flags & 4 != 0 {
        let mut boot = island_boot().clone();
        boot.cluster_heap_offset = 4;
        boot.cluster_count = 64;
        boot.sectors_per_cluster_shift = (flags >> 3) & 3;
        ExfatSalvage::from_boot(boot)
    } else {
        ExfatSalvage::probe(&source)
    };
    materialize(&adapter, &source, &plan)
}

fn decode_plan(mut body: &[u8]) -> Plan {
    let mut plan = Plan::default();
    while let Some((&tag, rest)) = body.split_first() {
        body = rest;
        if decode_step(tag, &mut body, &mut plan).is_none() {
            break;
        }
    }
    plan
}

fn decode_step(tag: u8, body: &mut &[u8], plan: &mut Plan) -> Option<()> {
    let step = match tag % 11 {
        0 => PlanStep::Extent(Extent::new(field(3, body)?, field(2, body)?)),
        1 => PlanStep::Extent(Extent::new(field(3, body)?, field(3, body)?)),
        2 => PlanStep::Extent(Extent::new(field(8, body)?, field(8, body)?)),
        3 => PlanStep::Zeros(field(2, body)?),
        4 => PlanStep::Zeros(field(8, body)?),
        5 => PlanStep::Bytes(literal(body)?),
        6 => PlanStep::Clusters {
            first: field(1, body)?,
            count: field(1, body)?,
        },
        7 => PlanStep::Clusters {
            first: field(8, body)?,
            count: field(8, body)?,
        },
        8 => {
            plan.length = Some(field(3, body)?);
            return Some(());
        }
        9 => {
            plan.valid_length = Some(field(3, body)?);
            return Some(());
        }
        _ => {
            plan.metadata.name = Some(String::from_utf8_lossy(&literal(body)?).into_owned());
            return Some(());
        }
    };
    plan.steps.push(step);
    Some(())
}

/// A little-endian field of `width` bytes taken off the front of `body`.
fn field(width: usize, body: &mut &[u8]) -> Option<u64> {
    let (bytes, rest) = body.split_at_checked(width)?;
    *body = rest;
    let mut word = [0; 8];
    word[..width].copy_from_slice(bytes);
    Some(u64::from_le_bytes(word))
}

/// A count byte and that many bytes, taken off the front of `body`.
fn literal(body: &mut &[u8]) -> Option<Vec<u8>> {
    let count = field(1, body)? as usize;
    let (bytes, rest) = body.split_at_checked(count)?;
    *body = rest;
    Some(bytes.to_vec())
}

/// What one output byte of a plan is, from the plan's own words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Want {
    Source(u64),
    Zero,
    Byte(u8),
}

/// What the plan type documents, worked out here without its code: whether a
/// plan can run over a source of `size` bytes with this cluster layout, and if
/// so its final and valid lengths. Every extent and cluster run must lie on the
/// source and in the layout; a final length may cut part of the last step that
/// produces bytes but no more and may not run past the steps; a valid length
/// may not pass the final length.
fn plan_lengths(plan: &Plan, size: u64, layout: Option<&ClusterLayout>) -> Option<(u64, u64)> {
    let mut total = 0u64;
    let mut last_start = 0;
    for step in &plan.steps {
        let length = match step {
            PlanStep::Bytes(bytes) => bytes.len() as u64,
            PlanStep::Zeros(length) => *length,
            PlanStep::Extent(extent) => {
                extent.end().filter(|end| *end <= size)?;
                extent.length
            }
            PlanStep::Clusters { first, count } => {
                let extent = layout?.extent(*first, *count)?;
                extent.end().filter(|end| *end <= size)?;
                extent.length
            }
        };
        if length > 0 {
            last_start = total;
        }
        total = total.checked_add(length)?;
    }
    let length = match plan.length {
        None => total,
        Some(length) if length == total || (length < total && length > last_start) => length,
        Some(_) => return None,
    };
    let valid = plan.valid_length.unwrap_or(length);
    (valid <= length).then_some((length, valid))
}

/// Each output byte of a plan whose lengths [`plan_lengths`] accepted.
fn plan_output(plan: &Plan, layout: Option<&ClusterLayout>, length: u64, valid: u64) -> Vec<Want> {
    let mut output = Vec::with_capacity(length as usize);
    for step in &plan.steps {
        let remaining = length - output.len() as u64;
        match step {
            PlanStep::Bytes(bytes) => output.extend(
                bytes
                    .iter()
                    .take(remaining as usize)
                    .map(|byte| Want::Byte(*byte)),
            ),
            PlanStep::Zeros(count) => output.extend(std::iter::repeat_n(
                Want::Zero,
                (*count).min(remaining) as usize,
            )),
            PlanStep::Extent(_) | PlanStep::Clusters { .. } => {
                let extent = match step {
                    PlanStep::Extent(extent) => *extent,
                    PlanStep::Clusters { first, count } => {
                        layout.unwrap().extent(*first, *count).unwrap()
                    }
                    _ => unreachable!(),
                };
                let count = extent.length.min(remaining);
                output.extend((extent.offset..extent.offset + count).map(Want::Source));
            }
        }
    }
    assert_eq!(output.len() as u64, length);
    for want in &mut output[valid as usize..] {
        *want = Want::Zero;
    }
    output
}

/// A plan runs exactly when its words allow it; it then materializes to its
/// final length, each byte the one the plan names, or zeros flagged
/// unreadable near a sector that does not read, with every byte at and past
/// the valid length zero and never read.
fn materialize(adapter: &ExfatSalvage, source: &Faulty, plan: &Plan) -> bool {
    let size = source.size();
    let has_clusters = plan
        .steps
        .iter()
        .any(|step| matches!(step, PlanStep::Clusters { .. }));
    let layout = adapter.cluster_layout().ok();
    match (adapter.boot(), &layout) {
        (Some(boot), Some(layout)) => assert_eq!(
            *layout,
            ClusterLayout {
                first: u64::from(FIRST_CLUSTER),
                count: u64::from(boot.cluster_count),
                size: boot.cluster_size(),
                offset: boot.heap_offset(),
            }
        ),
        (None, None) => {}
        (boot, layout) => panic!("geometry {boot:?} but cluster layout {layout:?}"),
    }
    let steps_total = plan.steps.iter().try_fold(0u64, |total, step| match step {
        PlanStep::Extent(extent) => total.checked_add(extent.length),
        PlanStep::Zeros(length) => total.checked_add(*length),
        PlanStep::Bytes(bytes) => total.checked_add(bytes.len() as u64),
        PlanStep::Clusters { .. } => None,
    });
    assert_eq!(plan.length(), plan.length.or(steps_total));

    // Validation knows no layout, so it refuses cluster steps.
    let validated = plan.validate(size);
    let alone = plan_lengths(plan, size, None);
    assert_eq!(
        validated.as_ref().ok(),
        alone.map(|(length, _)| length).as_ref()
    );
    if has_clusters {
        assert!(
            matches!(
                validated,
                Err(SalvageError::Unsupported(Feature::ClusterSteps))
                    | Err(SalvageError::Bounds)
                    | Err(SalvageError::Plan(_))
            ),
            "{validated:?}"
        );
    }

    let lengths = plan_lengths(plan, size, layout.as_ref());
    let mut sink = Capped(Vec::new());
    let result = adapter.materialize(source, plan, &mut sink);
    let Some((length, valid)) = lengths else {
        assert!(result.is_err(), "a plan its words refuse ran: {result:?}");
        assert!(sink.0.is_empty(), "a refused plan wrote output");
        return false;
    };
    if length > SINK_LIMIT as u64 {
        assert!(matches!(result, Err(SalvageError::Sink(_))), "{result:?}");
        return true;
    }
    let report = result.unwrap();
    let output = sink.0;
    assert_eq!(report.length, length);
    assert_eq!(output.len() as u64, length);
    let wanted = plan_output(plan, layout.as_ref(), length, valid);
    let last = size.div_ceil(512).saturating_sub(1);
    let mut at = 0;
    for range in &report.ranges {
        assert_eq!(range.output.offset, at, "ranges are not contiguous");
        assert!(range.output.length > 0);
        at += range.output.length;
        let span = range.output.offset as usize..range.output.end().unwrap() as usize;
        let written = &output[span.clone()];
        let wants = &wanted[span];
        let Some(from) = range.source else {
            assert!(range.readable);
            for (byte, want) in written.iter().zip(wants) {
                match want {
                    Want::Zero => assert_eq!(*byte, 0),
                    Want::Byte(value) => assert_eq!(byte, value),
                    Want::Source(_) => panic!("{range:?} names no source for source bytes"),
                }
            }
            continue;
        };
        assert_eq!(from.length, range.output.length);
        for (index, want) in wants.iter().enumerate() {
            assert_eq!(*want, Want::Source(from.offset + index as u64), "{range:?}");
        }
        assert_eq!(report.source_of(range.output.offset), Some(from.offset));
        let end = from.end().unwrap();
        if range.readable {
            assert!(source.readable(from.offset, end), "{range:?}");
            assert_eq!(written, source.at(from));
        } else {
            assert!(written.iter().all(|byte| *byte == 0), "{range:?}");
            // Lost bytes are narrowed to within a sector of one that fails.
            for sector in from.offset / 512..end.div_ceil(512) {
                let near = sector.saturating_sub(1)..=(sector + 1).min(last);
                assert!(
                    near.clone().any(|other| !source.sector_reads(other)),
                    "{range:?} lost sector {sector}, which reads"
                );
            }
        }
    }
    assert_eq!(at, length);
    assert_eq!(report.source_of(length), None);
    let unreadable: Vec<Extent> = report.unreadable().collect();
    assert_eq!(
        unreadable,
        report
            .ranges
            .iter()
            .filter(|range| !range.readable)
            .map(|range| range.output)
            .collect::<Vec<_>>()
    );
    round_trip(&report);
    true
}

/// Starting points for `plan`: each step kind and length field, in both
/// encodings, over a source with an unreadable sector and one without.
pub fn plan_seeds() -> Vec<(&'static str, Vec<u8>)> {
    let binary =
        |flags: u8, size: u8, bad: [u8; 2], body: &[u8]| [&[flags, size][..], &bad, body].concat();
    let json = |flags: u8, size: u8, bad: [u8; 2], plan: &Plan| {
        binary(2 | flags, size, bad, &serde_json::to_vec(plan).unwrap())
    };
    let every = Plan::new(vec![
        PlanStep::Bytes(b"head".to_vec()),
        PlanStep::Extent(Extent::new(100, 2000)),
        PlanStep::Zeros(17),
        PlanStep::Extent(Extent::new(4096, 512)),
    ]);
    let file = Plan {
        steps: vec![
            PlanStep::Clusters { first: 2, count: 2 },
            PlanStep::Clusters { first: 6, count: 1 },
        ],
        length: Some(1400),
        valid_length: Some(1100),
        metadata: fs_exfat::salvage::PlanMetadata {
            name: Some("rebuilt.bin".into()),
            ..Default::default()
        },
    };
    let mut steps = vec![0u8];
    steps.extend(&100u32.to_le_bytes()[..3]);
    steps.extend(2000u16.to_le_bytes());
    steps.push(3);
    steps.extend(17u16.to_le_bytes());
    steps.extend([5, 4]);
    steps.extend(b"tail");
    steps.push(1);
    steps.extend(&0u32.to_le_bytes()[..3]);
    steps.extend(&70_000u32.to_le_bytes()[..3]);
    let mut clusters = vec![6, 2, 3, 6, 9, 1, 8];
    clusters.extend(&3500u32.to_le_bytes()[..3]);
    clusters.push(9);
    clusters.extend(&3300u32.to_le_bytes()[..3]);
    clusters.extend([10, 3]);
    clusters.extend(b"run");
    vec![
        ("json-every-step", json(0, 8, [0, 0], &every)),
        ("json-unreadable", json(0, 8, [3, 0], &every)),
        ("json-past-source", json(0, 2, [0, 0], &every)),
        ("json-clusters-lengths", json(4, 8, [6, 0], &file)),
        ("json-clusters-no-geometry", json(0, 8, [0, 0], &file)),
        ("binary-steps", binary(0, 80, [0, 0], &steps)),
        (
            "binary-unaligned-unreadable",
            binary(1, 80, [2, 130], &steps),
        ),
        (
            "binary-endless-zeros",
            binary(
                0,
                1,
                [0, 0],
                &[4, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
            ),
        ),
        ("binary-clusters", binary(4 | 8, 16, [9, 0], &clusters)),
    ]
}

// ---------------------------------------------------------------------------

/// Salvage results are plain data: each survives a JSON round trip unchanged.
fn round_trip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_vec(value).unwrap();
    assert_eq!(&serde_json::from_slice::<T>(&json).unwrap(), value);
}

fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}
