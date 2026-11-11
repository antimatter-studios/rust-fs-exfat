### Added

- `WriteVolume::truncate` shrinks a file, freeing the clusters past its new end after the entry set records it, or grows it with bytes that read as zeros; a shrunk FAT chain whose clusters are consecutive becomes NoFatChain.
- `WriteVolume::unlink` removes a file and `rmdir` an empty directory, clearing InUse on every entry of the set before freeing its clusters; `Error::NotEmpty` reports a directory that still holds an entry.
- The C ABI gains `fs_exfat_truncate`, `fs_exfat_unlink` and `fs_exfat_rmdir`, with ENOTEMPTY from `fs_exfat_last_errno`.
- `fs.exfat IMAGE truncate --size BYTES PATH`, `rm PATH` and `rmdir PATH` change an image.
