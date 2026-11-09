## Interfaces

| Permissive structure snapshots | Experimental; fixed 512-byte-sector callback loading and saved-copy inspection retain damaged boot reports, partial root chains and declared bitmap lengths; checked snapshot accessors avoid strict-reader policy | Unreleased (#35) | #35 | `src/primitive_structures.rs`, `tests/primitive_cases/structures.rs`, `tests/structures-captured.json`, `tests/structures-edge-captured.json` |
| Named bitmap and boot-field fixtures | Experimental; optional test-support bitmap entries and exact-width filesystem-name, VolumeFlags and boot-signature replacements preserve other bytes and stale checksums | Unreleased (#35) | #35 | `src/fixtures.rs`, `tests/fixtures.rs`, `tests/structures-captured.json` |

## Validation

| Structure snapshot platform differential | Supported on macOS; fixed-sector loading, FAT words and bitmap bits agree with newly formatted 512-byte-sector images; 4096-byte sectors are explicitly unsupported by this loader; named fixture bytes and offsets agree at both sector sizes; damaged cases have captured synthetic-only evidence | Unreleased (#35) | #35 | `tests/oracle/fixtures.rs`, `scripts/test-oracle.sh` |
