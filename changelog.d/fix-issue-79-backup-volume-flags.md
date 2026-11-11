### Fixed

- When the main boot region failed its checksum, salvage took ActiveFat, VolumeDirty and MediaFailure from the backup boot sector, whose VolumeFlags section 3.1.13.1 calls stale; section 4 makes only the Main Boot Sector's VolumeFlags current and the checksum skips VolumeFlags, so salvage now reads them from the main sector whenever it is readable and valid, and reports when it has to fall back on the backup's copy.
