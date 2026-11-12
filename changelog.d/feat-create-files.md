### Added

- `WriteVolume::create`, `mkdir`, `create_with` and `mkdir_with` make files and directories, with names checked as Section 7.7.3 requires, refused when they collide through the volume's Up-case Table, and recorded with SetChecksum and NameHash; `NewEntry` gives attributes (ReadOnly, Hidden, System, Archive) and times, which otherwise are the current time in UTC.
- `WriteVolume::pwrite` writes into a file at any offset and `write_file` creates a file or replaces its contents; files grow cluster by cluster, stay NoFatChain while contiguous, and read a gap past the old valid length as zeros. A directory that runs out of entries grows by a zeroed cluster.
- `Timestamp::from_unix_milliseconds`, `Timestamp::now` and `Timestamp::encode` build and encode exFAT timestamps with their 10 ms parts and UTC offsets.
- The C ABI gains `fs_exfat_create`, `fs_exfat_mkdir`, `fs_exfat_pwrite`, `fs_exfat_write_file` and `fs_exfat_last_errno`.
- `fs.exfat IMAGE mkdir [-p] PATH` and `fs.exfat IMAGE write [--at BYTES] PATH` (bytes from stdin) write to an image.
