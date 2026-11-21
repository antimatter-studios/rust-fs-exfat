### Fixed
- A volume with 1024, 2048 or 4096-byte sectors is read by `fs_exfat::primitives::read_structures` and `inspect_structures`, which refused it with `unsupported sector size`; the boot regions, FAT, root directory and bitmap are placed from the sector size in the boot sector, not from 512 bytes.
