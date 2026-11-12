### Added

- `mkfs::format` lays down a new, empty exFAT volume on a `fs_core::BlockDevice`: both boot regions with their extended boot sectors, OEM parameters and checksum, the FAT (two for TexFAT), the Allocation Bitmap, the recommended compressed Up-case Table of section 7.2.5.1 and a root directory with an optional Volume Label and Volume GUID; `mkfs::plan` returns the same layout without a device.
- The cluster size defaults by volume size (4 KiB below 256 MiB, 32 KiB below 32 GiB, then 128 KiB, raised beyond 2^24 - 2 clusters), the FAT and cluster heap start on the cluster size, and sector size, cluster size, alignment, FAT count, label, serial number, GUID, partition offset and size can each be overridden through `mkfs::FormatOptions`.
- A later format keeps the OEM Parameters of a valid exFAT boot region already on the device, as section 3.3 requires, and writes the main boot sector last, after a flush.
- `mkfs.exfat` (`rust-fs-exfat mkfs`) formats an image file or device, creating an image file of `--size` bytes first, and reports the layout the strict reader reads back; `--dry-run` writes nothing.
- The C ABI's `fs_exfat_mkfs` and `fs_exfat_mkfs_with_callbacks` format a path or a read/write/flush callback device with `fs_exfat_format_options`.
- `mkfs::FormatError` says why a format did not happen: `Refused` names a layout the specification or the device does not allow, and nothing is written; `Device` carries a failed read, write or flush.
