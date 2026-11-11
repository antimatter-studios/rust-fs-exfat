## Reading

| Up-case Tables longer than 128 KiB | Experimental; the strict reader, the allocation audit and the deviation report accept a compressed Up-case Table of any even, nonzero DataLength that maps exactly 0000h to FFFFh (sections 7.2.4, 7.2.5 and 7.2.5.1), bounded by the cluster chain, `max_metadata_bytes` and the audit memory budget; salvage reads it too (#91) | Unreleased (#76) | #76 | `src/primitives.rs`, `src/volume.rs`, `src/check_audit.rs`, `src/check_deviations.rs`, `tests/primitive_cases/upcase.rs`, `tests/check/deviations.rs` |
