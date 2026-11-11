### Fixed

- `check` counted the clusters a Vendor Allocation (EntryType E1h, section 7.9) owns as unreferenced and exited 4, while macOS fsck_exfat accepts the volume; a valid, unshared cluster allocation of a File set's benign secondary with AllocationPossible set now counts as referenced. An invalid one is left unclaimed as before and does not make `check` reject the volume.
