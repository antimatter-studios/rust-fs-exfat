//! Both FAT and Allocation Bitmap pairs of a TexFAT volume (#40), over images
//! built from the specification's encoding. macOS does not implement TexFAT,
//! so no platform oracle can check these; the expected values come from
//! `Spec`, a reader written here from the specification alone, which shares no
//! code with the crate's decoders.

use super::support::{put16, put32, seal_boot, Bytes};
use super::{position, Failing};
use fs_core::BlockRead;
use fs_exfat::fixtures::{bitmap_entry, entry_set, Builder, CLUSTER, CLUSTER_COUNT, SECTOR};
use fs_exfat::primitives::{FAT_BAD, FAT_END};
use fs_exfat::salvage::{
    Candidate, CopyDifference, CopyKind, CopyPair, CopyReport, EntrySetDetail, ExfatSalvage,
    Extent, FatCopy, SalvageAdapter, StaleChain, WalkEnd,
};
use std::ops::ControlFlow;

/// Each FAT is four sectors: the first at sector 24, the second at 28.
const FAT_SECTORS: u32 = 4;
const BITMAP_BYTES: u64 = CLUSTER_COUNT.div_ceil(8) as u64;
const SECOND_BITMAP: u32 = 4;

/// A reader of the specification's layout, independent of the crate.
struct Spec<'a>(&'a [u8]);

impl Spec<'_> {
    fn u32_at(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.0[at..at + 4].try_into().unwrap())
    }

    /// BytesPerSectorShift, boot byte 108 (section 3.1.14).
    fn sector(&self) -> usize {
        1 << self.0[108]
    }

    /// ClusterCount, boot bytes 92-95 (section 3.1.9).
    fn cluster_count(&self) -> u32 {
        self.u32_at(92)
    }

    /// FAT `n` lies FatLength sectors after FAT `n - 1`, the first at
    /// FatOffset (sections 3.1.6 and 3.1.7, Table 3).
    fn fat(&self, n: usize) -> usize {
        (self.u32_at(80) as usize + n * self.u32_at(84) as usize) * self.sector()
    }

    /// FatEntry[cluster] of FAT `n` (section 4.1).
    fn entry(&self, n: usize, cluster: u32) -> u32 {
        self.u32_at(self.fat(n) + cluster as usize * 4)
    }

    /// Cluster 2 starts at ClusterHeapOffset, bytes 88-91 (sections 3.1.8,
    /// 3.1.15 and 5.1).
    fn cluster(&self, cluster: u32) -> usize {
        let size = self.sector() << self.0[109];
        self.u32_at(88) as usize * self.sector() + (cluster as usize - 2) * size
    }

    /// The clusters FAT `n` links from `first` to FFFFFFFFh (section 4.1.3).
    fn chain(&self, n: usize, first: u32) -> Vec<u32> {
        let mut chain = vec![first];
        while chain.len() <= self.cluster_count() as usize {
            match self.entry(n, *chain.last().unwrap()) {
                next @ 2..=0xFFFF_FFF6 => chain.push(next),
                _ => break,
            }
        }
        chain
    }

    /// The source offset of each byte of Allocation Bitmap `n`: the root's
    /// entry of type 81h whose BitmapIdentifier, bit 0 of byte 1, is `n`
    /// (sections 7.1 and 7.1.2.1), followed through FAT `n`.
    fn bitmap(&self, n: usize) -> Vec<usize> {
        let size = self.sector() << self.0[109];
        let root = self.chain(n, self.u32_at(96));
        let entry = root
            .iter()
            .flat_map(|cluster| (0..size).step_by(32).map(|at| self.cluster(*cluster) + at))
            .find(|at| self.0[*at] == 0x81 && usize::from(self.0[at + 1] & 1) == n)
            .unwrap();
        let length = u64::from_le_bytes(self.0[entry + 24..entry + 32].try_into().unwrap());
        self.chain(n, self.u32_at(entry + 20))
            .iter()
            .flat_map(|cluster| (0..size).map(|at| self.cluster(*cluster) + at))
            .take(length as usize)
            .collect()
    }

    /// Bit `cluster - 2` of a bitmap, lowest-order bit first (section 7.1.5).
    fn bit(&self, bitmap: &[usize], cluster: u32) -> u32 {
        let index = cluster as usize - 2;
        u32::from(self.0[bitmap[index / 8]] >> (index % 8) & 1)
    }

    /// Every heap cluster the two FATs, then the two bitmaps, record
    /// differently.
    fn differences(&self) -> Vec<CopyDifference> {
        let heap = 2..self.cluster_count() + 2;
        let at = |offset: usize, length| Extent::new(offset as u64, length);
        let mut found: Vec<_> = heap
            .clone()
            .filter(|cluster| self.entry(0, *cluster) != self.entry(1, *cluster))
            .map(|cluster| CopyDifference {
                kind: CopyKind::Fat,
                cluster,
                first: self.entry(0, cluster),
                second: self.entry(1, cluster),
                first_location: at(self.fat(0) + cluster as usize * 4, 4),
                second_location: at(self.fat(1) + cluster as usize * 4, 4),
            })
            .collect();
        let (first, second) = (self.bitmap(0), self.bitmap(1));
        found.extend(
            heap.filter(|cluster| self.bit(&first, *cluster) != self.bit(&second, *cluster))
                .map(|cluster| CopyDifference {
                    kind: CopyKind::Bitmap,
                    cluster,
                    first: self.bit(&first, cluster),
                    second: self.bit(&second, cluster),
                    first_location: at(first[(cluster as usize - 2) / 8], 1),
                    second_location: at(second[(cluster as usize - 2) / 8], 1),
                }),
        );
        found
    }
}

/// Writes FatEntry[cluster] of FAT `n`.
fn fat(image: &mut [u8], n: usize, cluster: u32, value: u32) {
    let start = (24 + n * FAT_SECTORS as usize) * SECTOR;
    put32(image, start + cluster as usize * 4, value);
}

/// Sets clusters' bits in the bitmap stored at `bitmap`.
fn allocate(image: &mut [u8], bitmap: u32, clusters: &[u32]) {
    for cluster in clusters {
        let index = (*cluster - 2) as usize;
        image[position(bitmap) as usize + index / 8] |= 1 << (index % 8);
    }
}

/// A two-FAT volume with ActiveFat 1. The Second FAT and bitmap give
/// `data.bin` clusters 10 to 12; the stale First pair still holds it as two
/// clusters, cluster 20 as bad and a freed chain 30, 31.
fn texfat() -> Vec<u8> {
    let mut image = Builder::new().image;
    for start in [0, 12 * SECTOR] {
        put32(&mut image, start + 84, FAT_SECTORS);
        put16(&mut image, start + 106, 1);
        image[start + 110] = 2;
        seal_boot(&mut image[start..], SECTOR);
    }
    let mut second = bitmap_entry(SECOND_BITMAP, BITMAP_BYTES);
    second[1] = 1;
    let file = entry_set("data.bin", false, 10, 3 * CLUSTER as u64, false, true);
    let root = position(3) as usize;
    for (index, entry) in [bitmap_entry(2, BITMAP_BYTES), second, file]
        .iter()
        .enumerate()
    {
        image[root + index * 32..root + index * 32 + entry.len()].copy_from_slice(entry);
    }
    let links = [(0, 0xFFFF_FFF8), (1, FAT_END), (2, FAT_END), (3, FAT_END)];
    for (cluster, value) in links.into_iter().chain([(4, FAT_END), (10, 11)]) {
        fat(&mut image, 0, cluster, value);
        fat(&mut image, 1, cluster, value);
    }
    for (cluster, value) in [(11, FAT_END), (20, FAT_BAD), (30, 31), (31, FAT_END)] {
        fat(&mut image, 0, cluster, value);
    }
    fat(&mut image, 1, 11, 12);
    fat(&mut image, 1, 12, FAT_END);
    allocate(&mut image, 2, &[2, 3, 4, 10, 11, 20]);
    allocate(&mut image, SECOND_BITMAP, &[2, 3, 4, 10, 11, 12]);
    image
}

fn compare(source: &dyn BlockRead) -> (Vec<CopyDifference>, CopyReport) {
    let mut found = Vec::new();
    let report = ExfatSalvage::probe(source)
        .fat_copies(source, &mut found)
        .unwrap();
    (found, report)
}

#[test]
fn both_fats_and_bitmaps_are_compared_cluster_by_cluster_at_their_offsets() {
    let image = texfat();
    let expected = Spec(&image).differences();
    let clusters: Vec<_> = expected.iter().map(|d| (d.kind, d.cluster)).collect();
    let fats = [11, 12, 20, 30, 31].map(|cluster| (CopyKind::Fat, cluster));
    assert_eq!(clusters[..5], fats);
    assert_eq!(
        clusters[5..],
        [(CopyKind::Bitmap, 12), (CopyKind::Bitmap, 20)]
    );

    let (found, report) = compare(&Bytes(image.clone()));
    assert_eq!(found, expected);
    let pair = |copy, fat_sector: u64, bitmap| CopyPair {
        copy,
        fat: Extent::new(fat_sector * SECTOR as u64, 4 * SECTOR as u64),
        bitmap_entry: true,
        bitmap: vec![Extent::new(position(bitmap), BITMAP_BYTES)],
    };
    let heap = u64::from(CLUSTER_COUNT);
    assert_eq!(
        report,
        CopyReport {
            active: FatCopy::Second,
            pairs: vec![
                pair(FatCopy::First, 24, 2),
                pair(FatCopy::Second, 28, SECOND_BITMAP)
            ],
            fat_compared: heap,
            fat_differences: 5,
            bitmap_compared: heap,
            bitmap_differences: 2,
            unreadable: vec![],
            stopped: false,
        }
    );
    let json = serde_json::to_string(&(&found, &report)).unwrap();
    assert!(json.contains(r#""kind":"bitmap""#) && json.contains(r#""active":"second""#));
    let back: (Vec<CopyDifference>, CopyReport) = serde_json::from_str(&json).unwrap();
    assert_eq!(back, (found, report));

    // The Second FAT's sector of entries 128 to 255 is unreadable: entries
    // 128 to ClusterCount + 1 are not compared, and their bytes are reported.
    let sector = Extent::new(28 * SECTOR as u64 + SECTOR as u64, SECTOR as u64);
    let failing = Failing {
        inner: Bytes(image.clone()),
        bad: vec![sector],
    };
    let (found, report) = compare(&failing);
    assert_eq!(found, expected);
    let lost = u64::from(CLUSTER_COUNT) + 2 - 128;
    assert_eq!(report.fat_compared, heap - lost);
    assert_eq!(report.unreadable, [Extent::new(sector.offset, lost * 4)]);

    let source = Bytes(image);
    let mut first = Vec::new();
    let report = ExfatSalvage::probe(&source)
        .fat_copies(&source, &mut |difference| {
            first.push(difference);
            ControlFlow::Break(())
        })
        .unwrap();
    assert!(report.stopped);
    assert_eq!(
        (first.as_slice(), report.fat_differences),
        (&expected[..1], 1)
    );

    // One FAT: nothing to compare, and no second pair to walk.
    let single = Bytes(Builder::new().image);
    let (found, report) = compare(&single);
    assert!(found.is_empty() && report.pairs.len() == 1);
    assert_eq!((report.fat_compared, report.bitmap_compared), (0, 0));
    let adapter = ExfatSalvage::probe(&single);
    assert_eq!(adapter.fat(), Some(FatCopy::First));
    assert!(adapter.with_fat(FatCopy::Second).is_none());
}

#[test]
fn the_inactive_pair_gives_its_own_chains_as_evidence_not_damage() {
    let image = texfat();
    let spec = Spec(&image);
    let source = Bytes(image.clone());
    let active = ExfatSalvage::probe(&source);
    assert_eq!(active.fat(), Some(FatCopy::Second));
    let inactive = active.with_fat(FatCopy::First).unwrap();
    assert_eq!(inactive.fat(), Some(FatCopy::First));

    let mut candidates: Vec<Candidate<EntrySetDetail>> = Vec::new();
    let root = Extent::new(position(3), CLUSTER as u64);
    active
        .find_candidates(&source, root, &mut candidates)
        .unwrap();
    assert_eq!(candidates[0].name.as_deref(), Some("data.bin"));
    // Each pair's walk covers the clusters its own FAT links from cluster 10.
    for (adapter, n, end) in [
        (&active, 1, WalkEnd::Complete),
        (&inactive, 0, WalkEnd::Broken),
    ] {
        let walk = adapter.walk_extents(&source, &candidates[0]).unwrap();
        let chain = spec.chain(n, 10);
        let extent = Extent::new(position(10), (chain.len() * CLUSTER) as u64);
        assert_eq!((walk.extents, walk.end), (vec![extent], end), "FAT {n}");
    }

    // Stale chains come from each pair's FAT and bitmap: free clusters with
    // non-zero entries, which only the First pair holds.
    let bitmap = spec.bitmap(0);
    let free_links: Vec<_> = (2..CLUSTER_COUNT + 2)
        .filter(|cluster| spec.bit(&bitmap, *cluster) == 0 && spec.entry(0, *cluster) != 0)
        .map(|cluster| (cluster, spec.entry(0, cluster)))
        .collect();
    let mut chains: Vec<StaleChain> = Vec::new();
    inactive.stale_chains(&source, &mut chains).unwrap();
    let links: Vec<_> = chains[0]
        .entries
        .iter()
        .map(|e| (e.cluster, e.value))
        .collect();
    assert_eq!((chains.len(), links), (1, free_links));
    assert_eq!(
        chains[0].entries[0].location.offset,
        spec.fat(0) as u64 + 30 * 4
    );
    let mut chains = Vec::new();
    active.stale_chains(&source, &mut chains).unwrap();
    assert!(chains.is_empty());

    // The differences are not damage: making the stale pair a copy of the
    // active one changes nothing the damage report says.
    let mut agreeing = image.clone();
    let (first, second) = (spec.fat(0), spec.fat(1));
    agreeing.copy_within(second..second + 4 * SECTOR, first);
    let bitmaps = (position(2) as usize, position(SECOND_BITMAP) as usize);
    agreeing.copy_within(bitmaps.1..bitmaps.1 + CLUSTER, bitmaps.0);
    let (found, _) = compare(&Bytes(agreeing.clone()));
    assert!(found.is_empty());
    let damage = |image: Vec<u8>| {
        let source = Bytes(image);
        let mut found = Vec::new();
        ExfatSalvage::probe(&source)
            .report_damage(&source, &mut found)
            .unwrap();
        found
    };
    assert_eq!(damage(image), damage(agreeing));
}
