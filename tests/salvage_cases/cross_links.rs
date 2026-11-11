//! Clusters that two FAT links or two claims reach, over volumes built from
//! the specification: each FAT entry names the next cluster of one chain
//! (section 4.1.3), and a stream's FirstCluster starts its own (section
//! 6.4.3). The healthy forms, and a cross-link macOS `fsck_exfat -n`
//! refuses, are checked by the macOS oracle.

use super::support::Bytes;
use super::{fat_word, named, position, scan, Failing};
use fs_core::BlockRead;
use fs_exfat::fixtures::{
    bitmap_entry, entry_set, Builder, BITMAP_CLUSTER, CLUSTER, CLUSTER_COUNT, HEAP_OFFSET,
    ROOT_CLUSTER, SECTOR,
};
use fs_exfat::primitives::FAT_END;
use fs_exfat::salvage::{
    Candidate, CrossLink, CrossLinkReport, EntrySetDetail, ExfatSalvage, Extent, FatLink,
    SalvageError,
};
use std::ops::ControlFlow;

const BITMAP_BYTES: u64 = CLUSTER_COUNT.div_ceil(8) as u64;

fn clusters(count: u64) -> u64 {
    count * CLUSTER as u64
}

/// The bitmap at cluster 2 and the root at 3, each allocated with an end
/// marker, the root holding `sets` after the bitmap entry, and `links`
/// written to the FAT.
fn volume(sets: &[Vec<u8>], links: &[(u32, u32)]) -> Builder {
    let mut builder = Builder::new();
    for (cluster, value) in [(BITMAP_CLUSTER, FAT_END), (ROOT_CLUSTER, FAT_END)]
        .iter()
        .chain(links)
    {
        builder.allocate(*cluster);
        builder.fat(*cluster, *value);
    }
    let mut entries = vec![bitmap_entry(BITMAP_CLUSTER, BITMAP_BYTES)];
    entries.extend_from_slice(sets);
    builder.directory(ROOT_CLUSTER, &entries);
    builder
}

/// a.bin follows 10, 11, 12; b.bin starts at 20, whose FAT entry joins it
/// to 11, so both chains reach 11 and both claims reach 11 and 12.
fn joined() -> Builder {
    volume(
        &[
            entry_set("a.bin", false, 10, clusters(3), false, true),
            entry_set("b.bin", false, 20, clusters(3), false, true),
        ],
        &[(10, 11), (11, 12), (12, FAT_END), (20, 11)],
    )
}

fn listing(
    source: &dyn BlockRead,
    candidates: &[Candidate<EntrySetDetail>],
) -> (Vec<CrossLink>, CrossLinkReport) {
    let mut found = Vec::new();
    let report = ExfatSalvage::probe(source)
        .cross_links(source, candidates, &mut found)
        .unwrap();
    assert_eq!(report.cross_links, found.len() as u64);
    assert!(!report.stopped);
    for cross_link in &found {
        assert_eq!(
            cross_link.location,
            Extent::new(position(cross_link.cluster), CLUSTER as u64)
        );
        for link in &cross_link.links {
            assert_eq!(link.location, fat_word(link.from));
        }
    }
    (found, report)
}

fn link(from: u32, head: u32) -> FatLink {
    FatLink {
        from,
        location: fat_word(from),
        head,
    }
}

fn links(found: &[CrossLink]) -> Vec<(u32, Vec<(u32, u32)>)> {
    found
        .iter()
        .map(|cross_link| {
            let pairs = cross_link
                .links
                .iter()
                .map(|link| (link.from, link.head))
                .collect();
            (cross_link.cluster, pairs)
        })
        .collect()
}

#[test]
fn two_files_whose_chains_meet_are_listed_with_their_heads_and_records() {
    let source = Bytes(joined().image);
    let (_, candidates) = scan(&source);
    let a = named(&candidates, "a.bin").location.offset;
    let b = named(&candidates, "b.bin").location.offset;
    let (found, report) = listing(&source, &candidates);
    assert_eq!(
        found,
        [
            CrossLink {
                cluster: 11,
                location: Extent::new(position(11), CLUSTER as u64),
                links: vec![link(10, 10), link(20, 20)],
                candidates: vec![a, b],
            },
            // The chains share 12 through 11's single entry, so only the
            // claims cross there.
            CrossLink {
                cluster: 12,
                location: Extent::new(position(12), CLUSTER as u64),
                links: Vec::new(),
                candidates: vec![a, b],
            },
        ]
    );
    assert_eq!(
        report,
        CrossLinkReport {
            cross_links: 2,
            ..CrossLinkReport::default()
        }
    );

    // Without candidates, the FAT alone still names 11 and both heads.
    let (found, _) = listing(&source, &[]);
    assert_eq!(links(&found), [(11, vec![(10, 10), (20, 20)])]);
    assert!(found[0].candidates.is_empty());

    // The caller chooses the claimants: one claim crosses nothing, and a
    // record passed twice counts once.
    let only_a: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.location.offset == a)
        .cloned()
        .collect();
    let (found, _) = listing(&source, &only_a);
    assert_eq!(links(&found), [(11, vec![(10, 10), (20, 20)])]);
    assert_eq!(found[0].candidates, [a]);
    let twice = [only_a.clone(), only_a].concat();
    let (found, _) = listing(&source, &twice);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].candidates, [a]);

    let json = serde_json::to_value(&found[0]).unwrap();
    assert_eq!(json["links"][1]["from"], 20);
    assert_eq!(json["links"][1]["head"], 20);
    assert_eq!(json["candidates"][0], a);
}

#[test]
fn a_healthy_volume_lists_nothing() {
    let builder = volume(
        &[
            entry_set("chained.bin", false, 10, clusters(3), false, true),
            entry_set("whole.bin", false, 30, clusters(4), true, true),
            entry_set("empty.bin", false, 0, 0, false, true),
        ],
        &[(10, 11), (11, 12), (12, FAT_END)],
    );
    let source = Bytes(builder.image);
    let (_, candidates) = scan(&source);
    assert_eq!(candidates.len(), 3);
    let (found, report) = listing(&source, &candidates);
    assert!(found.is_empty());
    assert_eq!(report, CrossLinkReport::default());
}

#[test]
fn contiguous_claims_that_overlap_are_listed_without_fat_links() {
    let builder = volume(
        &[
            entry_set("c.bin", false, 30, clusters(4), true, true),
            entry_set("d.bin", false, 32, clusters(4), true, true),
            entry_set("e.bin", false, 33, clusters(1), true, false),
        ],
        &[],
    );
    let source = Bytes(builder.image);
    let (_, candidates) = scan(&source);
    let [c, d, e] =
        ["c.bin", "d.bin", "e.bin"].map(|name| named(&candidates, name).location.offset);
    let (found, _) = listing(&source, &candidates);
    let listed: Vec<_> = found
        .iter()
        .map(|cross_link| (cross_link.cluster, cross_link.candidates.clone()))
        .collect();
    // c.bin holds 30 to 33, d.bin 32 to 35 and the deleted e.bin 33.
    assert_eq!(listed, [(32, vec![c, d]), (33, vec![c, d, e])]);
    assert!(found.iter().all(|cross_link| cross_link.links.is_empty()));

    // A deleted record counts only when the caller passes it.
    let live: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.location.offset != e)
        .cloned()
        .collect();
    let (found, _) = listing(&source, &live);
    let clusters: Vec<_> = found.iter().map(|cross_link| cross_link.cluster).collect();
    assert_eq!(clusters, [32, 33]);
    assert_eq!(found[1].candidates, [c, d]);
}

#[test]
fn each_link_is_traced_back_to_a_head_or_to_where_chains_met_first() {
    // 41 and 50 both lead to 42; 42 and 60 both lead to 43; 70 and 72 both
    // lead to 71, and 72 is reached from 71 itself, a loop.
    let builder = volume(
        &[],
        &[
            (40, 41),
            (41, 42),
            (42, 43),
            (43, FAT_END),
            (50, 42),
            (60, 43),
            (70, 71),
            (71, 72),
            (72, 71),
        ],
    );
    let source = Bytes(builder.image);
    let (found, report) = listing(&source, &[]);
    assert_eq!(
        links(&found),
        [
            (42, vec![(41, 40), (50, 50)]),
            (43, vec![(42, 42), (60, 60)]),
            (71, vec![(70, 70), (72, 71)]),
        ]
    );
    assert_eq!(report.cross_links, 3);
    assert_eq!(report.unclassified, 0);
}

#[test]
fn unreadable_fat_bytes_and_clusters_past_the_source_are_counted_not_guessed() {
    // Sector 1 of the FAT holds the entries of clusters 128 to 255.
    let mut builder = joined();
    for (cluster, value) in [(130, 131), (131, FAT_END), (140, 131)] {
        builder.fat(cluster, value);
    }
    let fat_sector = Extent::new(fat_word(128).offset, SECTOR as u64);
    let source = Failing {
        inner: Bytes(builder.image.clone()),
        bad: vec![fat_sector],
    };
    let (found, report) = listing(&source, &[]);
    assert_eq!(links(&found), [(11, vec![(10, 10), (20, 20)])]);
    // Clusters 128 to 201 have unreadable FAT entries.
    assert_eq!(report.unclassified, 74);
    assert_eq!(report.unreadable, [fat_sector]);

    // A source that ends before cluster 100: the links to 150 and the claim
    // of f.bin's second cluster reach past it, and nothing there is listed.
    let mut builder = volume(
        &[entry_set("f.bin", false, 80, clusters(2), false, true)],
        &[(80, 150), (90, 150), (150, FAT_END)],
    );
    builder
        .image
        .truncate(HEAP_OFFSET as usize * SECTOR + 98 * CLUSTER);
    let source = Bytes(builder.image);
    let (_, candidates) = scan(&source);
    let (found, report) = listing(&source, &candidates);
    assert!(found.is_empty());
    assert_eq!(report.past_source, 3);
    assert_eq!(report.unclassified, 0);

    let blank = Bytes(vec![0; 64 * SECTOR]);
    let error = ExfatSalvage::probe(&blank)
        .cross_links(&blank, &[], &mut Vec::new())
        .unwrap_err();
    assert!(matches!(error, SalvageError::NoGeometry));
}

#[test]
fn a_stopping_sink_ends_the_listing() {
    let source = Bytes(joined().image);
    let (_, candidates) = scan(&source);
    let mut first = Vec::new();
    let report = ExfatSalvage::probe(&source)
        .cross_links(&source, &candidates, &mut |cross_link: CrossLink| {
            first.push(cross_link);
            ControlFlow::Break(())
        })
        .unwrap();
    assert!(report.stopped);
    assert_eq!((report.cross_links, first.len()), (1, 1));
    assert_eq!(first[0].cluster, 11);
}
