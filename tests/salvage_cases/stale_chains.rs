//! Non-zero FAT entries on free clusters, over volumes built from the
//! specification: the bitmap marks allocation (section 7.1) and the FAT
//! links clusters (section 4.1.3). macOS clears freed entries, so these
//! states come from the specification rather than from macOS output.

use super::support::Bytes;
use super::{fat_word, position, Failing};
use fs_core::BlockRead;
use fs_exfat::fixtures::{
    bitmap_entry, entry_set, noise, Builder, BITMAP_CLUSTER, CLUSTER, CLUSTER_COUNT, ROOT_CLUSTER,
    SECTOR,
};
use fs_exfat::primitives::{FAT_BAD, FAT_END};
use fs_exfat::salvage::{
    ExfatSalvage, Extent, SalvageAdapter, SalvageError, StaleChain, StaleChainReport, StaleEnd,
};
use std::ops::ControlFlow;

const BITMAP_BYTES: u64 = CLUSTER_COUNT.div_ceil(8) as u64;

/// A bitmap at cluster 2, the root at 3 and a live FAT-chained file in
/// clusters 10 to 12, each allocated with an end marker. Every other cluster
/// is free with a zero FAT entry.
fn volume() -> Builder {
    let mut builder = Builder::new();
    for cluster in [BITMAP_CLUSTER, ROOT_CLUSTER, 10, 11, 12] {
        builder.allocate(cluster);
    }
    for (cluster, value) in [
        (2, FAT_END),
        (3, FAT_END),
        (10, 11),
        (11, 12),
        (12, FAT_END),
    ] {
        builder.fat(cluster, value);
    }
    builder.directory(
        ROOT_CLUSTER,
        &[
            bitmap_entry(BITMAP_CLUSTER, BITMAP_BYTES),
            entry_set("live.bin", false, 10, 3 * CLUSTER as u64, false, true),
        ],
    );
    builder
}

/// Writes FAT entries as a freeing that leaves the FAT alone would.
fn stale(builder: &mut Builder, links: &[(u32, u32)]) {
    for &(cluster, value) in links {
        builder.fat(cluster, value);
    }
}

fn listing(source: &dyn BlockRead) -> (Vec<StaleChain>, StaleChainReport) {
    let mut chains = Vec::new();
    let report = ExfatSalvage::probe(source)
        .stale_chains(source, &mut chains)
        .unwrap();
    assert_eq!(report.chains, chains.len() as u64);
    let entries: u64 = chains.iter().map(|chain| chain.entries.len() as u64).sum();
    assert_eq!(report.entries, entries);
    for chain in &chains {
        for entry in &chain.entries {
            assert_eq!(entry.location, fat_word(entry.cluster));
        }
    }
    (chains, report)
}

fn links(chain: &StaleChain) -> Vec<(u32, u32)> {
    chain
        .entries
        .iter()
        .map(|entry| (entry.cluster, entry.value))
        .collect()
}

fn clusters(first: u32, count: u64) -> Extent {
    Extent::new(position(first), count * CLUSTER as u64)
}

#[test]
fn free_clusters_with_zero_entries_are_not_listed() {
    let mut builder = volume();
    // A deleted contiguous file whose FAT entries were never written, and a
    // live contiguous file whose FAT entries the specification says not to
    // interpret (section 6.3.4.2): neither is a free cluster's link.
    builder.directory(
        ROOT_CLUSTER,
        &[
            bitmap_entry(BITMAP_CLUSTER, BITMAP_BYTES),
            entry_set("live.bin", false, 10, 3 * CLUSTER as u64, false, true),
            entry_set("gone.bin", false, 20, 2 * CLUSTER as u64, true, false),
            entry_set("whole.bin", false, 30, 2 * CLUSTER as u64, true, true),
        ],
    );
    builder.allocate(30);
    builder.allocate(31);
    stale(&mut builder, &[(30, 0x1234), (31, 7)]);
    let (chains, report) = listing(&Bytes(builder.image));
    assert!(chains.is_empty());
    assert_eq!(report, StaleChainReport::default());
}

#[test]
fn a_freed_chain_is_listed_to_its_end_marker() {
    let mut builder = volume();
    stale(
        &mut builder,
        &[(20, 21), (21, 22), (22, FAT_END), (30, 35), (35, FAT_END)],
    );
    for cluster in [20, 21, 22, 30, 35] {
        builder.put(cluster, 0, &noise(cluster, CLUSTER));
    }
    let image = builder.image.clone();
    let source = Bytes(builder.image);
    let (chains, _) = listing(&source);
    assert_eq!(chains.len(), 2);
    assert_eq!(links(&chains[0]), [(20, 21), (21, 22), (22, FAT_END)]);
    assert_eq!(chains[0].extents, [clusters(20, 3)]);
    assert_eq!(links(&chains[1]), [(30, 35), (35, FAT_END)]);
    assert_eq!(chains[1].extents, [clusters(30, 1), clusters(35, 1)]);
    for chain in &chains {
        assert_eq!(chain.end, StaleEnd::End);
        assert!(!chain.end.is_break() && !chain.end.is_join());
    }
    assert_eq!(chains[1].head(), Some(30));

    // The extents read back the freed clusters' bytes as they stand.
    let adapter = ExfatSalvage::probe(&source);
    let mut bytes = vec![0; CLUSTER];
    adapter
        .read_extent(&source, chains[1].extents[1], 0, &mut bytes)
        .unwrap();
    let start = position(35) as usize;
    assert_eq!(bytes, image[start..start + CLUSTER]);
}

#[test]
fn breaks_end_at_free_bad_and_out_of_heap_targets() {
    let past_heap = CLUSTER_COUNT + 2;
    let mut builder = volume();
    stale(
        &mut builder,
        &[
            (40, 41),
            (50, FAT_BAD),
            (60, 61),
            (61, past_heap),
            (70, 1),
            (80, 0xFFFF_FFF8),
        ],
    );
    let (chains, _) = listing(&Bytes(builder.image));
    let found: Vec<_> = chains
        .iter()
        .map(|chain| (links(chain), chain.end))
        .collect();
    assert_eq!(
        found,
        [
            (vec![(40, 41)], StaleEnd::FreeTarget(41)),
            (vec![(50, FAT_BAD)], StaleEnd::BadMarker),
            (
                vec![(60, 61), (61, past_heap)],
                StaleEnd::OutOfHeap(past_heap)
            ),
            (vec![(70, 1)], StaleEnd::OutOfHeap(1)),
            (vec![(80, 0xFFFF_FFF8)], StaleEnd::OutOfHeap(0xFFFF_FFF8)),
        ]
    );
    assert!(chains.iter().all(|chain| chain.end.is_break()));
}

#[test]
fn joins_end_at_live_clusters_and_earlier_chains_and_loops_are_listed_once() {
    let mut builder = volume();
    stale(
        &mut builder,
        &[
            // Into the live file.
            (90, 91),
            (91, 11),
            // An end, then a shorter chain joining its middle.
            (20, 21),
            (21, 22),
            (22, FAT_END),
            (100, 21),
            // Two heads converging: the lower head's walk holds the join.
            (105, 112),
            (110, 111),
            (111, 112),
            (112, FAT_END),
            // A loop behind a head, and a loop that nothing heads.
            (130, 131),
            (131, 132),
            (132, 131),
            (120, 121),
            (121, 122),
            (122, 120),
        ],
    );
    let (chains, _) = listing(&Bytes(builder.image));
    let found: Vec<_> = chains
        .iter()
        .map(|chain| (links(chain), chain.end))
        .collect();
    assert_eq!(
        found,
        [
            (vec![(20, 21), (21, 22), (22, FAT_END)], StaleEnd::End),
            (vec![(90, 91), (91, 11)], StaleEnd::JoinsAllocated(11)),
            (vec![(100, 21)], StaleEnd::JoinsChain(21)),
            (vec![(105, 112), (112, FAT_END)], StaleEnd::End),
            (vec![(110, 111), (111, 112)], StaleEnd::JoinsChain(112)),
            (
                vec![(130, 131), (131, 132), (132, 131)],
                StaleEnd::Cycle(131)
            ),
            (
                vec![(120, 121), (121, 122), (122, 120)],
                StaleEnd::Cycle(120)
            ),
        ]
    );
    let mut listed: Vec<_> = chains
        .iter()
        .flat_map(|chain| chain.entries.iter().map(|entry| entry.cluster))
        .collect();
    let total = listed.len();
    listed.sort();
    listed.dedup();
    assert_eq!(listed.len(), total, "a cluster was listed twice");
    assert!(chains[1].end.is_join() && chains[2].end.is_join());
}

#[test]
fn unreadable_or_missing_records_are_counted_not_guessed() {
    let mut builder = volume();
    // Sector 1 of the FAT holds the entries of clusters 128 to 255.
    stale(
        &mut builder,
        &[(20, 21), (21, FAT_END), (30, 150), (150, FAT_END)],
    );
    let fat_sector = Extent::new(fat_word(128).offset, SECTOR as u64);
    let source = Failing {
        inner: Bytes(builder.image.clone()),
        bad: vec![fat_sector],
    };
    let (chains, report) = listing(&source);
    assert_eq!(links(&chains[0]), [(20, 21), (21, FAT_END)]);
    assert_eq!(chains[1].end, StaleEnd::Unknown(150));
    assert_eq!(chains.len(), 2);
    // Clusters 128 to 201 are free with unreadable FAT entries.
    assert_eq!(report.unclassified, 74);
    assert_eq!(report.unreadable, [fat_sector]);

    let bitmap = Extent::new(position(BITMAP_CLUSTER), BITMAP_BYTES);
    let source = Failing {
        inner: Bytes(builder.image.clone()),
        bad: vec![bitmap],
    };
    let (chains, report) = listing(&source);
    assert!(chains.is_empty());
    assert_eq!(report.unclassified, u64::from(CLUSTER_COUNT));
    assert_eq!(report.unreadable, [bitmap]);

    // No in-use bitmap entry: nothing can be classified, and the report says so.
    let mut unlisted = builder.image.clone();
    unlisted[position(ROOT_CLUSTER) as usize] &= 0x7f;
    let (chains, report) = listing(&Bytes(unlisted));
    assert!(chains.is_empty());
    assert_eq!(report.unclassified, u64::from(CLUSTER_COUNT));

    let blank = Bytes(vec![0; builder.image.len()]);
    let error = ExfatSalvage::probe(&blank)
        .stale_chains(&blank, &mut Vec::new())
        .unwrap_err();
    assert!(matches!(error, SalvageError::NoGeometry));

    let source = Bytes(builder.image);
    let mut first = Vec::new();
    let report = ExfatSalvage::probe(&source)
        .stale_chains(&source, &mut |chain: StaleChain| {
            first.push(chain);
            ControlFlow::Break(())
        })
        .unwrap();
    assert!(report.stopped);
    assert_eq!((report.chains, first.len()), (1, 1));
    assert_eq!(first[0].head(), Some(20));
}

#[test]
fn the_active_fat_and_its_bitmap_are_used_and_listings_serialize() {
    let mut builder = volume();
    let mut second_bitmap = bitmap_entry(5, BITMAP_BYTES);
    second_bitmap[1] = 1;
    builder.put(ROOT_CLUSTER, 0, &[0; CLUSTER]);
    builder.directory(
        ROOT_CLUSTER,
        &[bitmap_entry(BITMAP_CLUSTER, BITMAP_BYTES), second_bitmap],
    );
    // The first FAT and bitmap hold one stale chain, the second another. Each
    // FAT is four sectors, so the second ends where the heap begins.
    stale(&mut builder, &[(5, FAT_END), (20, FAT_END)]);
    let second_fat = fat_word(0).offset as usize + 4 * SECTOR;
    let mut second = vec![(0, 0xFFFF_FFF8), (1, FAT_END)];
    second.extend([(2, FAT_END), (3, FAT_END), (5, FAT_END), (40, 41), (41, 6)]);
    for (cluster, value) in second {
        let at = second_fat + cluster * 4;
        builder.image[at..at + 4].copy_from_slice(&u32::to_le_bytes(value));
    }
    let bits = position(5) as usize;
    builder.image[bits] = 0b0000_1011; // Clusters 2, 3 and 5.
    let source = Bytes(builder.image);
    let mut boot = ExfatSalvage::probe(&source).boot().unwrap().clone();
    boot.number_of_fats = 2;
    boot.fat_length = 4;
    boot.volume_flags = 1;
    let mut chains = Vec::new();
    let report = ExfatSalvage::from_boot(boot)
        .stale_chains(&source, &mut chains)
        .unwrap();
    assert_eq!(chains.len(), 1);
    assert_eq!(links(&chains[0]), [(40, 41), (41, 6)]);
    assert_eq!(
        chains[0].entries[0].location.offset,
        second_fat as u64 + 160
    );
    // Cluster 6 is free in the second bitmap with a zero entry in the second FAT.
    assert_eq!(chains[0].end, StaleEnd::FreeTarget(6));

    let json = serde_json::to_string(&(&chains, &report)).unwrap();
    assert!(json.contains(r#""end":{"free-target":6}"#), "{json}");
    let back: (Vec<StaleChain>, StaleChainReport) = serde_json::from_str(&json).unwrap();
    assert_eq!(back, (chains, report));
    let ends: Vec<StaleEnd> = serde_json::from_str(r#"["end",{"joins-chain":21}]"#).unwrap();
    assert_eq!(ends, [StaleEnd::End, StaleEnd::JoinsChain(21)]);
}
