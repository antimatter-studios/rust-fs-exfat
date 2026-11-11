### Added

- `ExfatSalvage::upcase_table` reads the volume's Up-case Table, compressed or not, into an `UpcaseReport` whose `same_name` and `upcase` compare names by the volume's own mappings, or by Table 24 alone when the table is damaged.
- `primitives::RawUpcaseTable` reports whether a table uses the compressed form, the byte offset of a faulty identity run or of the first word after the mapping of FFFFh, the bytes that encode the first 128 mappings, and `usable()`; `primitives::mandatory_upcase` applies Table 24 alone.
- The spec deviation report's new `upcase-run` rule (section 7.2.5) places a compressed table's run with no count, a count of 0 or a count past FFFFh at its FFFFh.

### Changed

- Words after the mapping of FFFFh are reported at the first of them, and an Up-case Table's Table 24 deviation spans the words that encode the first 128 mappings, fewer than 256 bytes in a compressed table.

### Fixed

- A compressed Up-case Table whose last identity run covers FFFFh alone, FFFFh followed by a count of 1, was refused as having extra mappings; section 7.2.5 allows it, and macOS fsck_exfat accepts it.
