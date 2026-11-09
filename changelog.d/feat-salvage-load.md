### Added
- Permissive fixed-sector structure loading and saved-copy inspection expose raw boot regions, FAT, root directory, bitmap geometry and damage diagnostics without strict-reader rejection.
- Checked raw FAT and bitmap accessors return absent-word and clear-bit results for incomplete snapshots without imposing volume geometry.
- Optional test-support fixtures name allocation-bitmap entries and boot-sector filesystem-name, VolumeFlags and boot-signature mutations without resealing checksums.
