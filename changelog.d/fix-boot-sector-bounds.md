### Fixed

- `primitives::BootSector::last_cluster_exclusive`, and the public `valid_cluster`, `decode_fat`, `root_chain`, `clusters_for`, `compute_predecessors`, `compute_stats` and `chain_islands` built on it, panicked in debug builds and gave wrong bounds in release builds for a parsed boot sector with a cluster count of `FFFFFFFEh` or more; the heap's end is now worked out in `u64`, so cluster `FFFFFFFFh` is in the heap for those counts, and an end-of-chain FAT entry is still never a link (#117).
- `BootSector::bytes_per_sector`, `cluster_size` and `heap_offset`, `primitives::cluster_offset` and `FatLayout::from_boot` overflowed for shifts whose sizes pass `u64::MAX`; each now saturates at `u64::MAX`, and `last_cluster_exclusive` saturates at `u32::MAX` (#117).
