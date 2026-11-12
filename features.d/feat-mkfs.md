## Safety

| Formatter writes | The formatter writes the device directly, not through the writer: `src/mkfs.rs`, its C ABI and its CLI command take a writable device, and `scripts/check-read-only.sh` admits them beside the write modules and refuses a writable surface anywhere else; strict APIs still accept only BlockRead | Unreleased (#132) | #132 | `scripts/check-read-only.sh`, `tests/mkfs.rs` |

## Interfaces

| Formatter | Experimental; `mkfs::format` creates an empty volume at 512, 1024, 2048 or 4096 bytes per sector, with one FAT or TexFAT's two, the recommended up-case table, an optional label and GUID, cluster size and alignment chosen by volume size or given; exposed as `mkfs.exfat` and the C ABI's `fs_exfat_mkfs`; macOS `fsck_exfat -n` finds every volume clean and the macOS driver mounts, writes and reads it back, from 1 MiB to 40 GiB at both platform sector sizes; at newfs_exfat's alignment a 64 MiB volume is byte-identical to newfs_exfat's; not yet checked by a Linux kernel or exfatprogs | Unreleased (#132) | #132 | `src/mkfs.rs`, `tests/mkfs.rs`, `tests/oracle/mkfs.rs`, `tests/capi.rs`, `tests/cli.rs` |
