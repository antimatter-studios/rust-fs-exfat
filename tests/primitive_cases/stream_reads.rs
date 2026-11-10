use fs_exfat::primitives::{
    cluster_reads, contiguous_clusters, decode_fat_entry, read_fat_entry, walk_fat_chain,
    ChainPosition, ClusterRead, FatChainStep, FatValue, FAT_BAD, FAT_END,
};
use fs_exfat::{Error, Result};
use std::cell::Cell;

fn walk(tail: u32) -> Vec<FatChainStep> {
    walk_fat_chain(2, 12, |cluster| {
        Ok(decode_fat_entry(if cluster == 2 { 3 } else { tail }, 12))
    })
    .collect::<Result<_>>()
    .unwrap()
}

#[test]
fn raw_fat_entries_keep_reserved_values_and_validity() {
    for (value, kind, valid) in [
        (0, FatValue::Free, true),
        (1, FatValue::Reserved, false),
        (2, FatValue::Next(2), true),
        (13, FatValue::Next(13), true),
        (14, FatValue::Reserved, false),
        (FAT_BAD, FatValue::Bad, true),
        (0xffff_fff8, FatValue::Reserved, false),
        (FAT_END, FatValue::End, true),
    ] {
        let entry = read_fat_entry(4096, 7, 12, |offset, output| {
            assert_eq!(offset, 4096 + 7 * 4);
            output.copy_from_slice(&value.to_le_bytes());
            Ok(())
        })
        .unwrap();
        assert_eq!(entry.value, value);
        assert_eq!(entry.kind, kind);
        assert_eq!(entry.valid, valid);
        assert_eq!(entry, decode_fat_entry(value, 12));
    }
    assert_eq!(
        decode_fat_entry(u32::MAX - 2, u32::MAX).kind,
        FatValue::Next(u32::MAX - 2)
    );
}

#[test]
fn fat_walks_expose_cycles_without_rejecting_or_rereading_them() {
    let reads = Cell::new(0);
    let steps = walk_fat_chain(2, 12, |cluster| {
        reads.set(reads.get() + 1);
        Ok(decode_fat_entry(
            if cluster == 4 { 3 } else { cluster + 1 },
            12,
        ))
    })
    .collect::<Result<Vec<_>>>()
    .unwrap();
    assert_eq!(reads.get(), 3);
    assert_eq!(steps.len(), 7);
    for (index, cluster) in [2, 3, 4].into_iter().enumerate() {
        assert_eq!(
            steps[index * 2],
            FatChainStep::Cluster(ChainPosition {
                cluster,
                valid: true,
                cycle: false
            })
        );
        let FatChainStep::Entry {
            cluster: source,
            entry,
        } = steps[index * 2 + 1]
        else {
            panic!("missing raw FAT entry")
        };
        assert_eq!(source, cluster);
        assert_eq!(entry.value, if cluster == 4 { 3 } else { cluster + 1 });
        assert!(entry.valid);
    }
    assert_eq!(
        steps[6],
        FatChainStep::Cluster(ChainPosition {
            cluster: 3,
            valid: true,
            cycle: true
        })
    );
}

#[test]
fn fat_walks_expose_out_of_range_entries_and_roots() {
    for value in [1, 14, 0xffff_fff8] {
        let steps = walk(value);
        assert_eq!(steps.len(), 4);
        assert_eq!(
            steps[3],
            FatChainStep::Entry {
                cluster: 3,
                entry: decode_fat_entry(value, 12)
            }
        );
        let FatChainStep::Entry { entry, .. } = steps[3] else {
            unreachable!()
        };
        assert_eq!(entry.value, value);
        assert!(!entry.valid);
    }
    for cluster in [0, 1, 14, FAT_BAD, FAT_END] {
        let steps = walk_fat_chain(cluster, 12, |_| panic!("invalid cluster was read"))
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            steps,
            [FatChainStep::Cluster(ChainPosition {
                cluster,
                valid: false,
                cycle: false
            })]
        );
    }
}

#[test]
fn fat_walks_preserve_broken_tails_and_end_markers() {
    for (value, kind) in [
        (0, FatValue::Free),
        (FAT_BAD, FatValue::Bad),
        (FAT_END, FatValue::End),
    ] {
        let steps = walk(value);
        assert_eq!(steps.len(), 4);
        let FatChainStep::Entry { cluster, entry } = steps[3] else {
            panic!("missing tail")
        };
        assert_eq!(cluster, 3);
        assert_eq!(entry.value, value);
        assert_eq!(entry.kind, kind);
        assert!(entry.valid);
    }
    let steps = walk(2);
    assert_eq!(
        steps.last(),
        Some(&FatChainStep::Cluster(ChainPosition {
            cluster: 2,
            valid: true,
            cycle: true
        }))
    );
}

#[test]
fn contiguous_claims_and_read_chunks_keep_raw_boundaries() {
    assert_eq!(contiguous_clusters(13, 3), 13..16);
    assert_eq!(contiguous_clusters(0, 0), 0..0);
    assert_eq!(
        contiguous_clusters(u32::MAX, 2),
        u64::from(u32::MAX)..u64::from(u32::MAX) + 2
    );
    assert_eq!(
        cluster_reads(400, 700, 512).collect::<Vec<_>>(),
        [
            ClusterRead {
                cluster_index: 0,
                within: 400,
                output: 0..112
            },
            ClusterRead {
                cluster_index: 1,
                within: 0,
                output: 112..624
            },
            ClusterRead {
                cluster_index: 2,
                within: 0,
                output: 624..700
            },
        ]
    );
    assert_eq!(
        cluster_reads(1024, 1, 512).collect::<Vec<_>>(),
        [ClusterRead {
            cluster_index: 2,
            within: 0,
            output: 0..1
        }]
    );
    assert_eq!(cluster_reads(0, 0, 512).count(), 0);
    assert_eq!(cluster_reads(0, 512, 0).count(), 0);
}

#[test]
fn fat_walks_are_lazy_and_io_failures_are_not_damage() {
    let reads = Cell::new(0);
    let mut steps = walk_fat_chain(2, 12, |_| {
        reads.set(reads.get() + 1);
        Err(Error::Invalid("injected read failure"))
    });
    assert!(matches!(steps.next(), Some(Ok(FatChainStep::Cluster(_)))));
    assert_eq!(reads.get(), 0);
    assert!(matches!(
        steps.next(),
        Some(Err(Error::Invalid("injected read failure")))
    ));
    assert_eq!(reads.get(), 1);
    assert!(steps.next().is_none());
    assert!(steps.next().is_none());
    assert!(matches!(
        read_fat_entry(u64::MAX, 2, 12, |_, _| panic!("overflow was read")),
        Err(Error::Bounds)
    ));
    assert!(matches!(
        read_fat_entry(0, 2, 12, |_, _| Err(Error::Bounds)),
        Err(Error::Bounds)
    ));
}
