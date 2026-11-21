### Changed

- The crate depends on rust-fs-core 0.4.5, whose `FileDevice` flushes macOS raw character nodes with `fsync` when they refuse `F_FULLFSYNC`. `write::open_path` on a `/dev/rdiskN` node now mounts, writes and unmounts cleanly, as the macOS raw-node oracle shows.
