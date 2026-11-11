## Reading

| File streams | Experimental; contiguous and FAT-linked reads zero-fill bytes beyond ValidDataLength | Unreleased | #1 | `src/file.rs`, `tests/file.rs` |
| Timestamps | Experimental; calendar bounds, ten-millisecond increments and signed quarter-hour UTC offsets validated; modified and accessed times the Linux kernel wrote, 1980, 2038, 2107 and centiseconds included, read as the kernel reads them | Unreleased | #1 | `src/timestamp.rs`, `tests/timestamp.rs`, `tests/images.rs`, `tests/kernel.rs`, `tests/third.rs` |

## Validation

| File-content platform differential | Supported on macOS; every file length, SHA-256 and timestamp compared on new images mounted read-only | Unreleased | #1 | `tests/oracle/file.rs` |
