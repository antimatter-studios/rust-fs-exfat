## Reading

| Per-sector FAT retries | Experimental; `primitives::FatWindow` retries a failed 64 KiB FAT chunk per 512-byte sector, so salvage walks, damage findings and the rebuilt tree name a FAT word unreadable only when its own sector does not read; synthetic evidence only, since no oracle injects read errors | Unreleased (#107) | #107 | `src/primitive_streaming.rs`, `tests/primitive_cases/audit_streams.rs`, `tests/salvage.rs` |
