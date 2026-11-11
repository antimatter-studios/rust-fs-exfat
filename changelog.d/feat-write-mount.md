### Added

- `fs_exfat::write::WriteVolume` mounts a volume for writing over rust-fs-core's `BlockDevice`, with `open_path` and `open_device` for images, devices and offsets. Mounting checks the volume with the strict reader, refuses one marked dirty or reporting media failures, and sets VolumeDirty in the Main Boot Sector until `unmount` clears it; `flush` writes pending changes and keeps PercentInUse current.
- `WriteVolume::allocate`, `extend`, `shrink`, `free` and `clusters` allocate and free stream clusters in the Allocation Bitmap and FAT, keep a stream contiguous (NoFatChain) when they can, convert it to a FAT chain when it cannot grow in place, never take a cluster the FAT marks bad, and keep both FATs and both bitmaps of a two-FAT volume in step.
- The C ABI gains `fs_exfat_mount_rw`, `fs_exfat_mount_rw_with_callbacks` (with `fs_exfat_rw_callbacks`), `fs_exfat_flush` and `fs_exfat_umount_checked`; every read call works on a writable mount.

### Changed

- The crate is a read/write driver. The strict reader and the salvage adapter still take `BlockRead` only and cannot write.
