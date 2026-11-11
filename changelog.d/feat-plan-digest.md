### Added

- `Plan::digest` gives a materialization plan a stable identity: the SHA-256 of `Plan::canonical_bytes`, a documented, versioned encoding of every step, length and metadata field, the same on every platform and release, so a consumer knows when a file must be materialized again.

### Changed

- `sha2` is a dependency of the library rather than only of its tests.
