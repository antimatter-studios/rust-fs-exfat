## Reading

| Permissive on-disk primitives | Experimental; boot fields, checksums, timestamp strings, full live/deleted entry sets and bitmap lookup remain separate from strict validation | Unreleased (#26) | #26 | `src/primitives.rs`, `tests/primitive_cases/parse.rs` |

## Validation

| Primitive platform differential | Supported on macOS; healthy boot geometry/checksums, every directory record, bitmap location and two-second timestamp strings match formatter/checker and read-only driver evidence at both sector sizes; damaged cases use synthetic fixtures only | Unreleased (#26) | #26 | `tests/oracle/primitives.rs` |
