### Added

- `ExfatSalvage::stale_chains` lists free clusters whose FAT entries are non-zero, as exFAT allows, grouped into the chains they link, with each chain's end classified as the end marker, a break at a free, bad or out-of-heap target, or a join with an allocated cluster or an earlier chain.
- `primitives::stale_chains` groups and classifies those entries over caller-supplied bitmap and FAT records, without a full FAT or bitmap slice.
