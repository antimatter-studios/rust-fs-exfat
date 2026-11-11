//! Entry sets that cross from one directory cluster into the next cluster of
//! the directory's FAT chain when that cluster is not physically adjacent.
//! Section 6.3.2 of Microsoft's exFAT specification lets a set's entries lie
//! in consecutive positions of the directory, and section 6 lays a directory
//! out over its cluster chain, so a directory that grew after other clusters
//! were allocated splits such a set across distant clusters (#99).

use super::secondary_count::vendor_set;
use super::support::{file_set, metadata_volume, put32, Bytes};
use super::{named, scan, whole};
use fs_exfat::primitives::ENTRY_SIZE;
use fs_exfat::salvage::{
    CandidateKind, CandidateState, ExfatSalvage, Extent, Integrity, Parent, SalvageAdapter,
};
use fs_exfat::volume::Volume;
use serde_json::json;
use std::ops::ControlFlow;

/// One sector per cluster in the metadata volume.
const CLUSTER: usize = 512;
/// The metadata volume's cluster heap starts at sector 32.
const HEAP: usize = 32 * 512;
const FAT: usize = 24 * 512;
const ROOT: u32 = 2;
const BITMAP: u32 = 3;

fn position(cluster: u32) -> usize {
    HEAP + (cluster as usize - 2) * CLUSTER
}

/// The metadata volume with its root continued on `continuation`, none of
/// them next to the cluster before it, and `set` placed so that its first
/// `head` bytes end the root's first cluster and the rest follow in the
/// continuation clusters. The rest of the first cluster holds unused
/// entries, so the strict reader reads on to the set.
fn volume_with(set: &[u8], head: usize, continuation: &[u32]) -> Vec<u8> {
    let mut data = metadata_volume();
    let end = position(ROOT) + CLUSTER;
    data[position(ROOT) + 3 * ENTRY_SIZE..end - head].fill(1);
    let mut previous = ROOT;
    for &cluster in continuation {
        assert_ne!(cluster, previous + 1);
        put32(&mut data, FAT + previous as usize * 4, cluster);
        let index = (cluster - 2) as usize;
        data[position(BITMAP) + index / 8] |= 1 << (index % 8);
        previous = cluster;
    }
    put32(&mut data, FAT + previous as usize * 4, 0xffff_ffff);
    data[end - head..end].copy_from_slice(&set[..head]);
    for (chunk, &cluster) in set[head..].chunks(CLUSTER).zip(continuation) {
        let start = position(cluster);
        data[start..start + chunk.len()].copy_from_slice(chunk);
    }
    data
}

/// The strict reader lists the set as the root's only file, so the image is
/// a well-formed fragmented directory and not damage.
fn listed_by_the_strict_reader(image: &[u8], name: &str) {
    let volume = Volume::open(Bytes(image.to_vec())).unwrap();
    let root = volume.root_directory().unwrap();
    let names: Vec<_> = root.iter().map(|entry| entry.name()).collect();
    assert_eq!(names, [name]);
}

#[test]
fn a_set_split_across_distant_root_clusters_is_found_with_its_continuation() {
    let set = file_set("split.txt", 0x20, 7, 600, true);
    assert_eq!(set.len(), 3 * ENTRY_SIZE);
    let image = volume_with(&set, 2 * ENTRY_SIZE, &[6]);
    listed_by_the_strict_reader(&image, "split.txt");

    let source = Bytes(image);
    let (adapter, found) = scan(&source);
    let file = named(&found, "split.txt");
    assert_eq!(
        (file.kind, file.state, file.integrity),
        (
            CandidateKind::File,
            CandidateState::Live,
            Integrity::Verified
        )
    );
    assert_eq!(file.size, Some(600));
    assert_eq!(file.detail.first_cluster, 7);
    assert!(file.detail.no_fat_chain);
    let head = (position(ROOT) + CLUSTER - 2 * ENTRY_SIZE) as u64;
    assert_eq!(file.location, Extent::new(head, 2 * ENTRY_SIZE as u64));
    assert_eq!(
        serde_json::to_value(&file.detail).unwrap()["continuation"],
        json!([{ "offset": position(6), "length": ENTRY_SIZE }])
    );
    assert_eq!(
        found
            .iter()
            .filter(|candidate| candidate.location.offset == head)
            .count(),
        1
    );

    let tree = adapter.rebuild_tree(&source, &found).unwrap();
    let node = tree
        .nodes
        .iter()
        .find(|node| node.candidate == head)
        .unwrap();
    assert_eq!(node.parents, [Parent::Root]);

    // A scan of the record's own bytes finds it first, and a scan stopped
    // after it resumes where its record ends.
    let mut again = Vec::new();
    adapter
        .find_candidates(&source, file.location, &mut again)
        .unwrap();
    assert_eq!(again.first(), Some(file));
    let mut stop = |candidate: fs_exfat::salvage::Candidate<_>| {
        if candidate.location.offset == head {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let report = adapter
        .find_candidates(&source, whole(&source), &mut stop)
        .unwrap();
    assert_eq!(report.stopped_at, file.location.end());
}

#[test]
fn a_set_across_several_distant_clusters_is_found_whole() {
    let set = vendor_set(40);
    let length = set.len();
    assert_eq!(length, 41 * ENTRY_SIZE);
    let head = ENTRY_SIZE;
    let image = volume_with(&set, head, &[6, 8, 10]);
    listed_by_the_strict_reader(&image, "vendor.bin");

    let source = Bytes(image);
    let (_, found) = scan(&source);
    let file = named(&found, "vendor.bin");
    assert_eq!(file.integrity, Integrity::Verified);
    assert_eq!(
        file.location,
        Extent::new((position(ROOT) + CLUSTER - head) as u64, head as u64)
    );
    let last = length - head - 2 * CLUSTER;
    assert_eq!(
        serde_json::to_value(&file.detail).unwrap()["continuation"],
        json!([
            { "offset": position(6), "length": CLUSTER },
            { "offset": position(8), "length": CLUSTER },
            { "offset": position(10), "length": last },
        ])
    );
}

/// Without a FAT link to follow, nothing joins the two halves: a scan with
/// no geometry, or over a FAT that ends the root after its first cluster,
/// reports no set at the split.
#[test]
fn a_split_set_without_a_fat_link_is_not_invented() {
    let set = file_set("split.txt", 0x20, 7, 600, true);
    let mut image = volume_with(&set, 2 * ENTRY_SIZE, &[6]);
    put32(&mut image, FAT + ROOT as usize * 4, 0xffff_ffff);
    let source = Bytes(image);
    let (_, found) = scan(&source);
    assert!(found
        .iter()
        .all(|candidate| candidate.name.as_deref() != Some("split.txt")));

    let mut image = volume_with(&set, 2 * ENTRY_SIZE, &[6]);
    image[..512].fill(0);
    image[12 * 512..13 * 512].fill(0);
    let source = Bytes(image);
    let adapter = ExfatSalvage::probe(&source);
    assert!(adapter.boot().is_none());
    let mut found = Vec::new();
    adapter
        .find_candidates(&source, whole(&source), &mut found)
        .unwrap();
    assert!(found
        .iter()
        .all(|candidate| candidate.name.as_deref() != Some("split.txt")));
}
