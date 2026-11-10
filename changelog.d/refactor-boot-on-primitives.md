### Added

- Permissive boot headers and complete boot-region diagnostics expose raw fields and identity, checksum-repetition, and extended-signature validity without rejecting damaged structures.

### Changed

- Strict boot parsing delegates all on-disk decoding and checksum arithmetic to primitives while preserving validation order, error messages, and accepted inputs.
