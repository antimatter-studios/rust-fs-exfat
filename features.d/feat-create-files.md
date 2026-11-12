## Writing

| Files and directories | Experimental; files and directories created with checked names, case-insensitive collisions refused, SetChecksum and NameHash recorded, directories grown by zeroed clusters, given or current times with hundredths and UTC offsets, ReadOnly, Hidden, System and Archive; macOS `fsck_exfat -n` accepts the result and the read-only macOS driver lists the same names, sizes, times and SHA-256s at 512 and 4096 bytes per sector; the Linux kernel and exfatprogs are not checked yet (#133) | Unreleased (#130) | #130 | `tests/write_files.rs`, `tests/oracle/write.rs` |
| File data | Experimental; writes at any offset and whole-file replacement, NoFatChain while contiguous and FAT chains when fragmented, gaps past the valid length zeroed so ValidDataLength equals DataLength; large, fragmented and gap files match the read-only macOS driver's SHA-256s | Unreleased (#130) | #130 | `tests/write_files.rs`, `tests/oracle/write.rs` |

## Interfaces

| Writing through the C ABI and CLI | Experimental; `fs_exfat_create`, `fs_exfat_mkdir`, `fs_exfat_pwrite`, `fs_exfat_write_file` and `fs_exfat_last_errno`; `fs.exfat mkdir [-p]` and `fs.exfat write [--at]` open the image for writing only for those verbs and leave it clean | Unreleased (#130) | #130 | `tests/capi_write.rs`, `tests/cli_write.rs` |
