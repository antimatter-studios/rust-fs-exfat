### Added

- `WriteVolume::rename` renames and moves files and directories within and across directories, replacing a file with a file or an empty directory with a directory as POSIX rename(2) does, refusing to move a directory below itself, and rewriting a change of case in place.
- `WriteVolume::set_times` (with `SetTimes`) sets Create, LastModified and LastAccessed with their hundredths and UTC offsets, `set_attributes` sets ReadOnly, Hidden, System and Archive, and `set_label` sets or clears the volume label.
- The C ABI gains `fs_exfat_rename`, `fs_exfat_utimens`, `fs_exfat_set_attributes` and `fs_exfat_set_volume_label`.
- `fs.exfat IMAGE mv FROM TO`, `touch [--created|--modified|--accessed UNIX_MS] [--utc-offset MINUTES] PATH`, `attributes --set LIST PATH` and `label LABEL` change an image.
