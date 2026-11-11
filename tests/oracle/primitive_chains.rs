use super::{chain, find_bitmap_entry, formatter_value, oracle_directory, reference, walk};
use fs_exfat::primitives::{
    allocation_class, boot_check, chain_head, chain_islands, cluster_offset, clusters_for,
    compute_predecessors, compute_stats, decode_fat, is_allocated, predecessor, root_chain,
    valid_cluster, BootSector, ChainGroup, ALLOC_ALLOCATED, ALLOC_FREE_FAT, ALLOC_FREE_ZERO,
    BOOT_REGION_SECTORS, FAT_END, FIRST_CLUSTER, ISLAND_END_EOF,
};
use fs_exfat::salvage::{ExfatSalvage, StaleChain, StaleEnd};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use unicode_normalization::UnicodeNormalization;

fn raw_fat(image: &[u8], boot: &BootSector) -> Vec<u32> {
    let start = (u64::from(boot.fat_offset) * boot.bytes_per_sector()) as usize;
    let length = (u64::from(boot.fat_length) * boot.bytes_per_sector()) as usize;
    image[start..start + length]
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
        .collect()
}

fn raw_chain(fat: &[u32], first: u32) -> Vec<u32> {
    let mut current = first;
    let mut result = Vec::new();
    while current != FAT_END {
        assert!(current >= FIRST_CLUSTER && (current as usize) < fat.len());
        assert!(!result.contains(&current), "cycle in macOS reference image");
        result.push(current);
        current = fat[current as usize];
    }
    result
}

fn raw_offset(boot: &BootSector, cluster: u32) -> usize {
    (u64::from(boot.cluster_heap_offset) * boot.bytes_per_sector()
        + u64::from(cluster - FIRST_CLUSTER) * boot.cluster_size()) as usize
}

fn bitmap(image: &[u8], boot: &BootSector) -> Vec<u8> {
    let root = chain(image, boot, boot.root_cluster, None);
    let (first, length) = find_bitmap_entry(&root).unwrap();
    let start = raw_offset(boot, first);
    image[start..start + length as usize].to_vec()
}

fn bitmap_bit(bitmap: &[u8], cluster: u32) -> bool {
    let index = (cluster - FIRST_CLUSTER) as usize;
    bitmap[index / 8] & (1 << (index % 8)) != 0
}

#[test]
fn primitive_fat_root_geometry_predecessors_and_boot_checks_match_formatted_images() {
    for sector in [512, 4096] {
        let (image, boot) = reference(sector);
        let expected_fat = raw_fat(&image, &boot);
        let start = (u64::from(boot.fat_offset) * sector) as usize;
        let fat = decode_fat(&image[start..start + expected_fat.len() * 4], &boot);
        assert_eq!(fat, expected_fat);
        let expected_root = raw_chain(&expected_fat, formatter_value(sector, "Root start") as u32);
        assert_eq!(root_chain(&boot, &fat), expected_root);
        let predecessors = compute_predecessors(&boot, &fat);
        let mut incoming = BTreeMap::new();
        for cluster in FIRST_CLUSTER..boot.last_cluster_exclusive() {
            let next = expected_fat[cluster as usize];
            if (FIRST_CLUSTER..boot.last_cluster_exclusive()).contains(&next) {
                incoming.entry(next).or_insert(cluster);
            }
            assert!(valid_cluster(&boot, cluster));
            assert_eq!(
                cluster_offset(&boot, cluster) as usize,
                raw_offset(&boot, cluster)
            );
        }
        for cluster in FIRST_CLUSTER..boot.last_cluster_exclusive() {
            assert_eq!(
                predecessor(&predecessors, cluster),
                incoming.get(&cluster).copied()
            );
        }
        for &cluster in &expected_root {
            assert_eq!(chain_head(&predecessors, cluster), boot.root_cluster);
        }
        for cluster in [0, 1, boot.last_cluster_exclusive(), FAT_END] {
            assert!(!valid_cluster(&boot, cluster));
        }
        let size = (BOOT_REGION_SECTORS * sector) as usize;
        let check = boot_check(&image[..size], &image[size..size * 2], sector as usize);
        assert_eq!(check.main_stored, check.main_computed);
        assert_eq!(check.backup_stored, check.backup_computed);
        assert_eq!(check.main_stored, check.backup_stored);
        assert_eq!(
            check.regions_identical,
            image[..size] == image[size..size * 2]
        );
    }
}

#[test]
fn primitive_allocation_islands_and_statistics_match_the_formatter_fat_and_bitmap() {
    for sector in [512, 4096] {
        let (image, boot) = reference(sector);
        let fat = raw_fat(&image, &boot);
        let bitmap = bitmap(&image, &boot);
        let range = FIRST_CLUSTER..boot.last_cluster_exclusive();
        let allocated = range
            .clone()
            .filter(|&cluster| bitmap_bit(&bitmap, cluster))
            .count() as u64;
        let free_fat = range
            .clone()
            .filter(|&cluster| !bitmap_bit(&bitmap, cluster) && fat[cluster as usize] != 0)
            .count() as u64;
        for cluster in range.clone() {
            let expected = if bitmap_bit(&bitmap, cluster) {
                ALLOC_ALLOCATED
            } else if fat[cluster as usize] == 0 {
                ALLOC_FREE_ZERO
            } else {
                ALLOC_FREE_FAT
            };
            assert_eq!(is_allocated(&bitmap, cluster), bitmap_bit(&bitmap, cluster));
            assert_eq!(allocation_class(&fat, &bitmap, cluster), expected);
        }
        let targeted: BTreeSet<_> = range.clone().map(|cluster| fat[cluster as usize]).collect();
        let chains: Vec<_> = range
            .filter(|&cluster| fat[cluster as usize] != 0 && !targeted.contains(&cluster))
            .map(|head| raw_chain(&fat, head))
            .collect();
        assert!(chains.len() >= 3);
        let islands = chain_islands(&boot, &fat, &bitmap);
        assert_eq!(islands.len(), chains.len());
        for (island, expected) in islands.iter().zip(&chains) {
            assert_eq!(island.head, expected[0]);
            assert_eq!(island.tail, *expected.last().unwrap());
            assert_eq!(island.length, expected.len() as u64);
            assert_eq!(
                island.allocated,
                expected
                    .iter()
                    .filter(|&&cluster| bitmap_bit(&bitmap, cluster))
                    .count() as u64
            );
            assert_eq!(island.end, ISLAND_END_EOF);
        }
        let stats = compute_stats(&boot, &fat, &bitmap);
        assert_eq!(stats.allocated, allocated);
        assert_eq!(stats.free_with_fat, free_fat);
        assert_eq!(
            stats.free_zero_fat,
            u64::from(boot.cluster_count) - allocated - free_fat
        );
        assert_eq!(stats.cross_links, 0);
        for (group, class) in [
            (&stats.free_chains, 0),
            (&stats.allocated_chains, 1),
            (&stats.mixed_chains, 2),
        ] {
            let lengths: Vec<_> = chains
                .iter()
                .filter_map(|chain| {
                    let count = chain
                        .iter()
                        .filter(|&&cluster| bitmap_bit(&bitmap, cluster))
                        .count();
                    let observed = if count == 0 {
                        0
                    } else if count == chain.len() {
                        1
                    } else {
                        2
                    };
                    (class == observed).then_some(chain.len() as u64)
                })
                .collect();
            let expected = ChainGroup {
                chains: lengths.len() as u64,
                clusters: lengths.iter().sum(),
                longest: lengths.into_iter().max().unwrap_or(0),
            };
            assert_eq!(
                serde_json::to_value(group).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
        }
    }
}

#[test]
fn primitive_stream_clusters_recover_every_driver_file_and_directory_at_both_sector_sizes() {
    for sector in [512, 4096] {
        let (image, boot) = reference(sector);
        let fat = raw_fat(&image, &boot);
        let root = chain(&image, &boot, boot.root_cluster, None);
        let mut entries = BTreeMap::new();
        walk(&image, &boot, &root, "", &mut entries);
        let listing =
            std::fs::read_to_string(oracle_directory().join(format!("files-{sector}.txt")))
                .unwrap();
        let expected_files: BTreeMap<String, (u64, String)> = listing
            .lines()
            .map(|line| {
                let fields: Vec<_> = line.splitn(6, '\t').collect();
                (
                    fields[5].nfc().collect(),
                    (fields[0].parse().unwrap(), fields[1].into()),
                )
            })
            .collect();
        let mut files = 0;
        for (path, entry) in entries {
            let stream = clusters_for(
                &boot,
                &fat,
                entry.first_cluster,
                entry.length,
                entry.no_fat_chain,
            );
            let needed = entry.length.div_ceil(boot.cluster_size());
            assert_eq!(stream.needed, needed);
            let expected = if needed == 0 {
                Vec::new()
            } else if entry.no_fat_chain {
                (entry.first_cluster..entry.first_cluster + needed as u32).collect()
            } else {
                raw_chain(&fat, entry.first_cluster)
            };
            assert_eq!(stream.clusters, expected, "{path}");
            assert_eq!(stream.clusters.len() as u64, needed, "{path}");
            let mut bytes: Vec<u8> = stream
                .clusters
                .iter()
                .flat_map(|&cluster| {
                    let start = raw_offset(&boot, cluster);
                    image[start..start + boot.cluster_size() as usize]
                        .iter()
                        .copied()
                })
                .collect();
            bytes.truncate(entry.length as usize);
            if entry.is_dir {
                assert_eq!(
                    bytes,
                    chain(
                        &image,
                        &boot,
                        entry.first_cluster,
                        entry.no_fat_chain.then_some(entry.length)
                    )
                );
            } else {
                bytes[entry.valid_length as usize..].fill(0);
                let expected = &expected_files[&path];
                assert_eq!(bytes.len() as u64, expected.0, "{path}");
                assert_eq!(
                    format!("{:x}", Sha256::digest(&bytes)),
                    expected.1,
                    "{path}"
                );
                files += 1;
            }
        }
        assert_eq!(files, expected_files.len());
        assert!(files > 180);
    }
}

/// Free clusters with non-zero FAT entries, read straight from the bytes.
fn free_fat_clusters(image: &[u8], boot: &BootSector) -> BTreeSet<u32> {
    let fat = raw_fat(image, boot);
    let bitmap = bitmap(image, boot);
    (FIRST_CLUSTER..boot.last_cluster_exclusive())
        .filter(|&cluster| !bitmap_bit(&bitmap, cluster) && fat[cluster as usize] != 0)
        .collect()
}

fn stale_listing(image: &[u8]) -> Vec<StaleChain> {
    let source = crate::support::Bytes(image.to_vec());
    let mut chains = Vec::new();
    let report = ExfatSalvage::probe(&source)
        .stale_chains(&source, &mut chains)
        .unwrap();
    assert_eq!(report.unclassified, 0);
    assert!(report.unreadable.is_empty() && !report.stopped);
    chains
}

fn listed(chains: &[StaleChain]) -> BTreeSet<u32> {
    chains
        .iter()
        .flat_map(|chain| chain.entries.iter().map(|entry| entry.cluster))
        .collect()
}

/// macOS clears a freed cluster's FAT entry, so its images hold none to list.
/// The specification does not require that (sections 7.1 and 8.1): an
/// implementation may free a file by clearing only its InUse bits and bitmap
/// bits. Doing exactly that to the macOS-written fragmented.bin must list the
/// FAT chain macOS wrote, and its clusters must still hold the bytes the
/// read-only macOS driver hashed.
#[test]
fn stale_chains_match_the_formatter_fat_and_a_freed_driver_chain() {
    for sector in [512, 4096] {
        let (image, boot) = reference(sector);
        assert_eq!(
            listed(&stale_listing(&image)),
            free_fat_clusters(&image, &boot)
        );

        let fat = raw_fat(&image, &boot);
        let root = chain(&image, &boot, boot.root_cluster, None);
        let mut entries = BTreeMap::new();
        walk(&image, &boot, &root, "", &mut entries);
        let file = &entries["fragmented.bin"];
        assert!(!file.no_fat_chain);
        let clusters = raw_chain(&fat, file.first_cluster);
        assert!(clusters.windows(2).any(|pair| pair[1] != pair[0] + 1));

        let mut freed = image.clone();
        let root_at = |position: usize| {
            let cluster =
                raw_chain(&fat, boot.root_cluster)[position / boot.cluster_size() as usize];
            raw_offset(&boot, cluster) + position % boot.cluster_size() as usize
        };
        for slot in 0..file.slots {
            freed[root_at(file.offset + slot * 32)] &= 0x7f;
        }
        let (first, _) = find_bitmap_entry(&root).unwrap();
        for &cluster in &clusters {
            let index = (cluster - FIRST_CLUSTER) as usize;
            freed[raw_offset(&boot, first) + index / 8] &= !(1 << (index % 8));
        }
        let chains = stale_listing(&freed);
        assert_eq!(listed(&chains), free_fat_clusters(&freed, &boot));
        let chain = chains
            .iter()
            .find(|chain| chain.head() == Some(clusters[0]))
            .expect("the freed chain is listed from its first cluster");
        let links: Vec<_> = chain
            .entries
            .iter()
            .map(|entry| (entry.cluster, entry.value))
            .collect();
        let mut expected: Vec<_> = clusters.windows(2).map(|pair| (pair[0], pair[1])).collect();
        expected.push((*clusters.last().unwrap(), FAT_END));
        assert_eq!(links, expected);
        assert_eq!(chain.end, StaleEnd::End);

        let mut bytes: Vec<u8> = chain
            .extents
            .iter()
            .flat_map(|extent| {
                freed[extent.offset as usize..(extent.offset + extent.length) as usize].to_vec()
            })
            .collect();
        bytes.truncate(file.length as usize);
        let hashes =
            std::fs::read_to_string(oracle_directory().join(format!("files-{sector}.txt")))
                .unwrap();
        let expected = hashes
            .lines()
            .find(|line| line.ends_with("\tfragmented.bin"))
            .and_then(|line| line.split('\t').nth(1))
            .unwrap();
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected);
    }
}
