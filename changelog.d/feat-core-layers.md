### Changed

- **The salvage adapter trait, its evidence and plan types, and the
  conformance set come from rust-fs-core 0.5.0.** They are `fs_core::layers`,
  shared by every rust-fs driver. `fs_exfat::salvage` and
  `fs_exfat::conformance` re-export them under their old paths for one
  release. Two types changed shape on the way, for formats with holes and
  hard links:
  - `ExtentWalk::extents` holds `WalkExtent`s with file offsets. A cluster
    chain's walk is `ExtentWalk::contiguous`, and `walk.sources()` gives the
    plain extents.
  - A tree node's parents are `ParentClaim`s, each carrying the entry set's
    name and state.

  Plan digests are unchanged.
