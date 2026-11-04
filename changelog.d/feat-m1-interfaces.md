### Added
- A read-only C ABI with file and callback mounts, stat, directory iteration and positioned file reads.
- The optional fs.exfat CLI with ls, stat, cat and verified atomic export through rust-fs-core.
- A check-only fsck.exfat allocation audit with no repair mode.

### Changed
- Image safety checks enforce fresh attachment ownership and permit only the exact read-only metadata query on the current image device.
