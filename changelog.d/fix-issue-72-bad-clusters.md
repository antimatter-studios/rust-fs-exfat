### Fixed

- `check` counted an allocated cluster that the active FAT marks bad (FFFFFFF7h) and no allocation consumes as unreferenced, and exited 4; section 7.1.5.1 names a bad cluster as a reason for a set bitmap bit and macOS fsck_exfat accepts it, so such clusters are now reported as `bad_clusters` and do not make the volume unhealthy.
