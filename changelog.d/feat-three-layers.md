### Added

- `fs_exfat::layers::ExfatVolume` implements rust-fs-core's shared `VolumeAPI`, `OpenVolume`, `PrimitiveAPI` and `FileAPI` layers: a volume opens strictly for files or with only a usable boot geometry for salvage, reports its geometry, both boot regions and a quick health verdict, lists its on-disk structures, binds the salvage verbs to its source, and browses and reads files with names compared through the volume's Up-case Table.
