### Fixed
- Raw devices with larger logical sectors, including 4096-byte sectors, are read through aligned, read-only transfers before volume offsets are applied, without changing 512-byte image results.
