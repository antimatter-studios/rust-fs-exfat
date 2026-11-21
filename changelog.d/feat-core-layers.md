### Changed

- **The salvage adapter trait, its evidence and plan types, and the
  conformance set come from rust-fs-core 0.5.0.** They are `fs_core::layers`,
  shared by every rust-fs driver. The old `fs_exfat::salvage` and
  `fs_exfat::conformance` APIs keep their original shapes and are deprecated
  for one release. New callers should import from `fs_core::layers`.
  In the shared API:
  - `ExtentWalk::extents` holds `WalkExtent`s with file offsets. A cluster
    chain's walk is `ExtentWalk::contiguous`, and `walk.sources()` gives the
    plain extents.
  - A tree node's parents are `ParentClaim`s, each carrying the entry set's
    name and state.

  Plan digests are unchanged.
