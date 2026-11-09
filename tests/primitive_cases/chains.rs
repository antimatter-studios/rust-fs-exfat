use fs_exfat::primitives::{
    allocation_class, boot_check, chain_head, chain_islands, cluster_offset, clusters_for,
    compute_predecessors, compute_stats, decode_fat, is_allocated, predecessor, root_chain,
    valid_cluster, AllocationStats, BootCheck, BootSector, ChainGroup, Island, StreamClusters,
    ALLOC_ALLOCATED, ALLOC_FREE_FAT, ALLOC_FREE_ZERO, FAT_BAD, FAT_END, ISLAND_END_BROKEN,
    ISLAND_END_EOF, ISLAND_END_JOINS_ANOTHER,
};
use serde_json::json;

// Synthetic fixtures here deliberately contain damaged geometry, FAT entries,
// chains or boot copies. Healthy cases belong to the independent macOS oracles.
fn damaged_boot() -> BootSector {
    BootSector {
        file_system_name: "EXFAT   ".into(),
        partition_offset: 0,
        volume_length: 0,
        fat_offset: 24,
        fat_length: 1,
        cluster_heap_offset: 32,
        cluster_count: 12,
        root_cluster: 2,
        volume_serial: 0,
        revision: 0x100,
        volume_flags: 0,
        bytes_per_sector_shift: 9,
        sectors_per_cluster_shift: 3,
        number_of_fats: 1,
        percent_in_use: 255,
        signature_ok: false,
    }
}

fn damaged_fat() -> Vec<u32> {
    vec![0, 0, 4, 4, FAT_END, 6, 0, 8, FAT_BAD, 10, 14, 11, 13, 12]
}

fn damaged_bitmap() -> Vec<u8> {
    vec![0b0010_0101, 1]
}

#[test]
fn fat_decoding_pads_truncation_ignores_partial_words_and_preserves_damage_and_extras() {
    let boot = damaged_boot();
    let mut raw: Vec<u8> = [FAT_BAD, 1, 0xffff_fff8, u32::MAX - 1]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    raw.extend_from_slice(&[0x12, 0x34, 0x56]);
    let decoded = decode_fat(&raw, &boot);
    assert_eq!(&decoded[..4], &[FAT_BAD, 1, 0xffff_fff8, u32::MAX - 1]);
    assert_eq!(&decoded[4..], &[0; 10]);
    assert_eq!(decode_fat(&[], &boot), vec![0; 14]);
    raw = (0..20u32).flat_map(u32::to_le_bytes).collect();
    assert_eq!(decode_fat(&raw, &boot), (0..20).collect::<Vec<_>>());
}

#[test]
fn root_chains_return_partial_runs_on_broken_links_cycles_and_invalid_roots() {
    let mut boot = damaged_boot();
    for terminator in [0, 1, FAT_BAD, FAT_END, 14, u32::MAX - 1] {
        let mut fat = vec![0; 14];
        fat[2] = 3;
        fat[3] = terminator;
        assert_eq!(root_chain(&boot, &fat), [2, 3]);
    }
    let mut fat = vec![0; 14];
    fat[2] = 3;
    fat[3] = 4;
    fat[4] = 3;
    assert_eq!(root_chain(&boot, &fat), [2, 3, 4]);
    for root in [0, 1, 14, FAT_BAD, FAT_END] {
        boot.root_cluster = root;
        assert!(root_chain(&boot, &fat).is_empty());
    }
}

#[test]
fn allocation_helpers_keep_nonzero_damage_and_unchecked_bitmap_and_offset_semantics() {
    let boot = damaged_boot();
    let fat = damaged_fat();
    let bitmap = damaged_bitmap();
    assert_eq!(
        (ALLOC_ALLOCATED, ALLOC_FREE_FAT, ALLOC_FREE_ZERO),
        (1, 2, 3)
    );
    for cluster in 0..=15 {
        assert_eq!(valid_cluster(&boot, cluster), (2..14).contains(&cluster));
    }
    assert!(is_allocated(&bitmap, 2));
    assert!(!is_allocated(&bitmap, 3));
    assert!(is_allocated(&bitmap, 10));
    assert_eq!(allocation_class(&fat, &bitmap, 2), ALLOC_ALLOCATED);
    assert_eq!(allocation_class(&fat, &bitmap, 6), ALLOC_FREE_ZERO);
    assert_eq!(allocation_class(&fat, &bitmap, 8), ALLOC_FREE_FAT);
    assert_eq!(allocation_class(&fat, &bitmap, 13), ALLOC_FREE_FAT);
    assert_eq!(allocation_class(&[0; 14], &bitmap, 2), ALLOC_ALLOCATED);
    assert!(is_allocated(&[0, 0xff], 17));
    assert_eq!(cluster_offset(&boot, 2), 16384);
    assert_eq!(cluster_offset(&boot, 13), 61440);
    assert_eq!(cluster_offset(&boot, 14), 65536);
    assert!(std::panic::catch_unwind(|| is_allocated(&[], 2)).is_err());
    assert!(std::panic::catch_unwind(|| root_chain(&boot, &[])).is_err());
}

#[test]
fn contiguous_claims_keep_needed_and_clip_at_the_heap_without_consulting_the_fat() {
    let boot = damaged_boot();
    for (first, length, needed) in [(2, 0, 0), (0, 1, 1), (14, 4096, 1), (2, 49153, 13)] {
        let stream = clusters_for(&boot, &[], first, length, true);
        assert!(stream.clusters.is_empty());
        assert_eq!(stream.needed, needed);
    }
    let stream: StreamClusters = clusters_for(&boot, &[], 12, 12289, true);
    assert_eq!(stream.clusters, [12, 13]);
    assert_eq!(stream.needed, 4);
    assert_eq!(clusters_for(&boot, &[], 2, 1, true).clusters, [2]);
}

#[test]
fn fat_streams_keep_short_claims_and_stop_at_damage_or_a_repeated_cluster() {
    let boot = damaged_boot();
    for terminator in [0, 1, FAT_BAD, FAT_END, 14, u32::MAX - 1, 2] {
        let mut fat = vec![0; 14];
        fat[2] = 3;
        fat[3] = terminator;
        let stream = clusters_for(&boot, &fat, 2, 12288, false);
        assert_eq!(stream.clusters, [2, 3]);
        assert_eq!(stream.needed, 3);
        assert_eq!(clusters_for(&boot, &fat, 2, 1, false).clusters, [2]);
    }
    assert!(clusters_for(&boot, &[], 2, 49153, false)
        .clusters
        .is_empty());
    assert!(clusters_for(&boot, &[], 1, 4096, false).clusters.is_empty());
    assert!(clusters_for(&boot, &[], 2, 0, false).clusters.is_empty());
}

#[test]
fn predecessors_choose_the_lowest_cross_link_and_backward_walks_keep_cycle_identity() {
    let boot = damaged_boot();
    let predecessors = compute_predecessors(&boot, &damaged_fat());
    assert_eq!(predecessors.len(), 14);
    assert_eq!(predecessor(&predecessors, 4), Some(2));
    assert_eq!(predecessor(&predecessors, 14 - 1), Some(12));
    assert_eq!(predecessor(&predecessors, 2), None);
    assert_eq!(predecessor(&predecessors, 0), None);
    assert_eq!(chain_head(&predecessors, 4), 2);
    assert_eq!(chain_head(&predecessors, 8), 7);
    assert_eq!(chain_head(&predecessors, 11), 11);
    assert_eq!(chain_head(&predecessors, 12), 12);
    assert_eq!(chain_head(&predecessors, 13), 13);
    assert!(std::panic::catch_unwind(|| predecessor(&[], 2)).is_err());
}

#[test]
fn islands_keep_partial_tails_joins_allocation_counts_and_omit_headless_cycles() {
    let islands = chain_islands(&damaged_boot(), &damaged_fat(), &damaged_bitmap());
    let observed: Vec<_> = islands
        .iter()
        .map(|island| {
            (
                island.head,
                island.tail,
                island.length,
                island.allocated,
                island.end,
            )
        })
        .collect();
    assert_eq!(
        observed,
        [
            (2, 4, 2, 2, ISLAND_END_EOF),
            (3, 3, 1, 0, ISLAND_END_JOINS_ANOTHER),
            (5, 6, 2, 0, ISLAND_END_BROKEN),
            (7, 8, 2, 1, ISLAND_END_BROKEN),
            (9, 10, 2, 1, ISLAND_END_BROKEN),
        ]
    );
    for marker in [0, 1, FAT_BAD, 14, u32::MAX - 1] {
        let mut fat = vec![marker; 14];
        fat[2] = 2;
        assert!(chain_islands(&damaged_boot(), &fat, &[0, 0]).is_empty());
    }
}

#[test]
fn allocation_statistics_count_cross_link_targets_and_exclude_headless_cycles_from_groups() {
    let stats = compute_stats(&damaged_boot(), &damaged_fat(), &damaged_bitmap());
    assert_eq!(
        serde_json::to_value(stats).unwrap(),
        json!({
            "allocated": 4, "freeWithFat": 7, "freeZeroFat": 1, "crossLinks": 1,
            "freeChains": {"chains": 2, "clusters": 3, "longest": 2},
            "allocatedChains": {"chains": 1, "clusters": 2, "longest": 2},
            "mixedChains": {"chains": 2, "clusters": 4, "longest": 2}
        })
    );
    let mut boot = damaged_boot();
    boot.cluster_count = 300;
    let mut fat = vec![301; 302];
    fat[301] = FAT_END;
    let stats = compute_stats(&boot, &fat, &[0; 38]);
    assert_eq!(stats.cross_links, 1);
    assert_eq!(stats.free_with_fat, 300);
    assert_eq!(stats.free_chains.chains, 299);
    assert_eq!(stats.free_chains.clusters, 300);
    assert_eq!(stats.free_chains.longest, 2);
    assert_eq!(chain_islands(&boot, &fat, &[0; 38]).len(), 299);
}

#[test]
fn boot_copy_checks_report_bad_checksums_and_compare_even_mutable_bytes_verbatim() {
    let main = vec![0; 12 * 512];
    let mut backup = main.clone();
    backup[106] = 2;
    let check = boot_check(&main, &backup, 512);
    assert_eq!((check.main_stored, check.main_computed), (0, 0));
    assert_eq!((check.backup_stored, check.backup_computed), (0, 0));
    assert!(!check.regions_identical);
    backup[11 * 512..11 * 512 + 4].copy_from_slice(&123u32.to_le_bytes());
    let check = boot_check(&main, &backup, 512);
    assert_eq!((check.backup_stored, check.backup_computed), (123, 0));
    backup[0] = 1;
    assert_ne!(boot_check(&main, &backup, 512).backup_computed, 0);
    assert!(boot_check(&main, &main, 512).regions_identical);
    assert!(std::panic::catch_unwind(|| boot_check(&[], &backup, 512)).is_err());
}

#[test]
fn chain_and_boot_value_types_keep_defaults_cloning_and_camel_case_fields() {
    let group = ChainGroup {
        chains: 1,
        clusters: 3,
        longest: 3,
    };
    assert_eq!(
        serde_json::to_value(group.clone()).unwrap(),
        json!({"chains": 1, "clusters": 3, "longest": 3})
    );
    let stats = AllocationStats::default();
    assert_eq!(stats.clone().allocated, 0);
    assert_eq!(stats.free_chains.chains, 0);
    assert_eq!(ChainGroup::default().longest, 0);
    let check = BootCheck {
        main_stored: 1,
        main_computed: 2,
        backup_stored: 3,
        backup_computed: 4,
        regions_identical: false,
    };
    assert_eq!(
        serde_json::to_value(check.clone()).unwrap(),
        json!({"mainStored": 1, "mainComputed": 2, "backupStored": 3, "backupComputed": 4, "regionsIdentical": false})
    );
    let island = Island {
        head: 2,
        tail: 4,
        length: 3,
        allocated: 1,
        end: ISLAND_END_BROKEN,
    };
    assert_eq!(
        serde_json::to_value(island.clone()).unwrap(),
        json!({"head": 2, "tail": 4, "length": 3, "allocated": 1, "end": "broken"})
    );
    assert_eq!(
        (ISLAND_END_EOF, ISLAND_END_JOINS_ANOTHER),
        ("eof", "joins-another")
    );
}
