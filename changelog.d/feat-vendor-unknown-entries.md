### Added

- `ExfatSalvage::uninterpreted_entries` lists every directory entry the driver does not interpret, Vendor Extension and Vendor Allocation entries and unknown primary and secondary types, in the root and every directory candidate, with each entry's location, raw type, InUse bit and whether it lies in a valid entry set.
- `primitives::uninterpreted_entries` walks directory bytes for those entries, and `UninterpretedKind` classifies an EntryType by the specification's type ranges.
- `CheckReport::uninterpreted` and the fsck report's `uninterpreted_entries` object count them by kind without changing the verdict or exit status.
