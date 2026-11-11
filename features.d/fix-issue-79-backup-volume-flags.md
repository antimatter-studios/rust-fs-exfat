## Reading

| Current VolumeFlags in salvage | Experimental; salvage takes ActiveFat, VolumeDirty and MediaFailure from the Main Boot Sector even when its region fails the checksum (sections 3.1.13.1, 3.4 and 4), and uses the backup's stale copy only when the main sector is unreadable or invalidated by its JumpBoot, FileSystemName or BootSignature, reporting that fallback as damage | Unreleased (#79) | #79 | `src/salvage_exfat.rs`, `tests/salvage_cases/copies.rs` |
