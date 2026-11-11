## Safety

| Raw-device block alignment for writes | The write path's sector cache doubles its transfer size each time the device refuses a transfer with `InvalidInput`, and writes a partly covered block whole after reading it with the held sectors laid over it; a 512-byte-sector volume is written through a memory device that accepts only 4096-byte transfers and read back; a raw-node write oracle is not in the macOS tier yet (#159) | Unreleased (#152) | #152 | `tests/write.rs`, `src/write_cache.rs` |
