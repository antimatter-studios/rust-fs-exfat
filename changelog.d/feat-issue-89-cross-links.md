### Added

- `ExfatSalvage::cross_links` lists every cluster that two or more FAT entries name, or that two or more of the caller's candidates claim, with the cluster's byte offset and its claimants: each FAT entry that names it with the head of that entry's chain, and the record offset of each candidate whose claim includes it. `CrossLink`, `FatLink` and `CrossLinkReport` carry the results; before this, salvage gave cross-links only as a count in `AllocationStats::cross_links`.
