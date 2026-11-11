## Interfaces

| FAT-bad allocated clusters | Experimental; an allocated cluster that no allocation consumes and the active FAT marks bad (FFFFFFF7h) is counted as `bad_clusters`, not as unreferenced, matching macOS fsck_exfat | Unreleased (#72) | #72 | `src/check_audit.rs`, `tests/check.rs`, `tests/oracle/scale.rs` |
