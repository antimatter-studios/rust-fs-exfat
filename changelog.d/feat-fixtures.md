### Added

- The optional, default-off `test-support` feature exposes in-memory exFAT boot, FAT, bitmap, upcase, live/deleted entry-set and data-placement encoders plus four complete synthetic volume scenarios through `fs_exfat::fixtures`.
- Fixture byte compatibility is pinned to 18 independently captured lengths and SHA-256 digests; healthy encoder forms are checked against macOS formatter/checker images at both sector sizes.
