## Reading

| Compressed Up-case Table | Experimental; the primitive decoder expands both forms of section 7.2.5, identity runs included, reports whether a table is compressed, where a faulty run or an extra word lies and how many bytes encode the first 128 mappings, and reads a final run of one as FFFFh's mapping; the strict reader still refuses a table that fails its checksum, encoding, coverage or Table 24; an exFAT salvage extension reads the table for name lookups and falls back to Table 24 when it is unusable; macOS newfs_exfat writes the compressed recommended table, and both readers expand it alike | Unreleased (#42) | #42 | `src/primitives.rs`, `src/salvage_upcase.rs`, `tests/primitive_cases/upcase.rs`, `tests/salvage_cases/upcase.rs` |

## Interfaces

| Compressed Up-case Table deviations | Experimental; the spec deviation report adds the `upcase-run` rule, at the FFFFh of a run with no count, a count of 0 or a count past FFFFh, reports words after the mapping of FFFFh at the first of them, and spans an Up-case Table's Table 24 deviation over the words that encode the first 128 mappings; synthetic cases plus a macOS fsck_exfat comparison | Unreleased (#42) | #42 | `src/check_deviations.rs`, `tests/check/deviations.rs` |

## Validation

| Compressed Up-case Table platform oracle | Supported on macOS; the table newfs_exfat writes is 5836 bytes, compressed, with the TableChecksum section 7.2.5.1 gives the recommended table, and the strict reader and salvage expand it to the same mappings | Unreleased (#42) | #42 | `tests/oracle/metadata.rs` |
