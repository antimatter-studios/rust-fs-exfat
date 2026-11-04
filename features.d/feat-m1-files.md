## Reading

| File streams | Experimental; contiguous and FAT-linked reads zero-fill bytes beyond ValidDataLength | Unreleased | #1 | `src/file.rs`, `tests/file.rs` |
| Timestamps | Experimental; calendar bounds, ten-millisecond increments and signed quarter-hour UTC offsets validated | Unreleased | #1 | `src/timestamp.rs`, `tests/timestamp.rs` |

## Validation

| File-content platform differential | Supported on macOS; every file length, SHA-256 and timestamp compared on new images mounted read-only | Unreleased | #1 | `tests/oracle/file.rs` |
