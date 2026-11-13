## Writing

| Two-FAT writing (TexFAT interoperability) | Independently unverified; mirroring both FATs and Allocation Bitmaps is checked only on specification-built images with this crate's own reader; macOS, the Linux kernel and exfatprogs do not implement TexFAT, and no TexFAT-capable harness oracle has been validated; no transactional or crash-recovery guarantee | Unreleased (#154) | #154 | Synthetic only: `tests/write.rs`; [oracle review](../docs/texfat-validation.md) |
