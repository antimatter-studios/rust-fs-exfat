## Safety

| Raw-device sector alignment | Read-only transfers adapt to device sector alignment before volume slicing; 512-byte and 4096-byte raw/block nodes match macOS file hashes and CLI checks on newly created images | Unreleased (#20) | #20 | `tests/read_only.rs`, `tests/oracle/device.rs`, `scripts/test-image-safety.sh` |
