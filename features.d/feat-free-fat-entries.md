## Reading

| Stale FAT chains on free clusters | Experimental; an exFAT salvage extension lists every free cluster with a non-zero FAT entry, its value and FAT word, grouped into the chains the entries link with their cluster extents, and classifies each chain's end as the end marker, a break at a free, bad or out-of-heap target, a join with an allocated cluster or an earlier chain, a loop, or an unreadable record; unreadable or missing bitmap and FAT bytes are counted as unclassified, never guessed; non-empty listings are synthetic-only evidence, because the macOS driver clears freed FAT entries | Unreleased (#41) | #41 | `src/primitive_stale.rs`, `src/salvage_stale.rs`, `tests/salvage_cases/stale_chains.rs`, `tests/primitive_cases/chains.rs` |

## Validation

| Stale FAT chain platform differential | Supported on macOS; listings of newly formatted, driver-populated images match the free clusters with non-zero FAT entries read from the raw bytes at both sector sizes, and freeing a driver-written fragmented file by its InUse and bitmap bits alone lists the FAT chain macOS wrote, whose clusters hash to the driver's own read | Unreleased (#41) | #41 | `tests/oracle/primitive_chains.rs` |
