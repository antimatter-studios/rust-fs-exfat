## Reading

| Boot-sector bounds outside the specification's range | Experimental; the bounds `primitives::BootSector` derives answer for every field value: the heap holds clusters 2 to `cluster_count + 1` compared in `u64`, sizes and offsets past `u64::MAX` saturate there, and `last_cluster_exclusive` saturates at `u32::MAX`; synthetic evidence only, since no oracle formats a volume outside the specification's range | Unreleased (#117) | #117 | `src/primitives.rs`, `src/primitive_chains.rs`, `src/primitive_copies.rs`, `tests/primitive_cases/chains.rs` |
