## Safety

| Device writes | Only through `fs_exfat::write`, over `BlockDevice`; the strict reader and the salvage adapter accept only BlockRead, wrapped in ReadOnlyDevice, and `scripts/check-read-only.sh` keeps writable device types out of every other module but the C ABI | Unreleased (#129) | #135 | `tests/read_only.rs`, `scripts/check-read-only.sh` |

## Writing

| Write mount and volume state | Experimental; VolumeDirty set in the Main Boot Sector while mounted and cleared on a clean unmount, dirty and media-failure volumes refused, PercentInUse kept current, boot checksum recomputed; a failed or panicking write leaves the volume dirty; macOS `fsck_exfat -n` accepts the result at 512 and 4096 bytes per sector; the Linux kernel and exfatprogs are not checked yet (#133) | Unreleased (#129) | #129 | `tests/write.rs`, `tests/capi_write.rs`, `tests/oracle/write.rs` |
| Cluster allocation | Experimental; contiguous (NoFatChain) streams grown in place, converted to FAT chains when blocked and recognised again when a chain shrinks to a run; bad clusters skipped; both FATs and bitmaps of a two-FAT volume kept in step (synthetic evidence only: macOS has no TexFAT); allocations around a volume macOS populated pass `fsck_exfat -n` and leave its tree as the macOS driver reads it | Unreleased (#129) | #129 | `tests/write.rs`, `tests/oracle/write.rs` |
