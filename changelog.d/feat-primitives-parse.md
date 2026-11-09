### Added

- The primitive layer exposes permissive boot-sector parsing and accessors, boot and entry-set checksums, timestamp and UTC-offset decoding, allocation-bitmap lookup, raw little-endian helpers, and the complete entry-set metadata and camelCase serialization contract.
- macOS formatter, checker and read-only driver differentials cover healthy primitive parsing at 512- and 4096-byte sector sizes without changing the strict reader's validation rules.
