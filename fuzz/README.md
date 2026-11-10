# Fuzzing

Two halves over one corpus, as in the rest of the family.

**The explorer** is this directory: cargo-fuzz targets on the nightly toolchain,
in a package of their own that `cargo package` leaves out. `chore fuzz -- 30` runs
every target for 30 seconds, one process at a time, and fails on a crash, a target
with no corpus, a run that failed to start and a run that executed almost nothing.
Each target's output is in `tmp/logs/fuzz-<target>.log`. The `fuzz` workflow does
this nightly and on demand, and keeps any reproducer as an artifact.

**The gate** is `tests/fuzz_decoders.rs`: the same harness on the pinned toolchain,
replaying and deterministically mutating the corpus in the unit tier of every pull
request. It also fails when a public `parse` or `decode` has no target, and when no
seed in a corpus gets past its decoder's first check.

A crash found by the explorer is a bug. Commit its reproducer to
`fuzz/corpus/<target>/`, so the gate replays it from then on, and fix the decoder.

## Targets

| Target | Decoder |
|---|---|
| `boot_sector` | Strict and primitive `BootSector::parse`, primitive `BootSectorHeader::parse` and `BootRegion::parse`, and strict `boot_checksum` |
| `fat_entry` | `FatEntry::parse` |
| `allocation_bitmap` | `AllocationBitmap::parse` |
| `upcase_table` | `UpcaseTable::parse` |
| `root_metadata` | `RootMetadata::parse` |
| `directory` | `directory::parse`, primitive `EntryType::parse` and `RawEntrySet::parse` |
| `timestamp` | `Timestamp::decode` |
| `timestamps` | `Timestamps::parse` |
| `image` | a synthetic volume opened, walked, read, looked up and checked |

`DirectoryEntry` and `Stream` have no target of their own because they have no
decoder of their own: their fields are private and only `directory::parse` builds
them. Both targets that produce them hold every entry to what its accessors
promise. The code that follows a `Stream` to its clusters and bytes is reached
through `image`, which is also the only way to `BootRegions::read`, `check` and
`Volume::lookup`, since they need a device.

The `directory` and `image` targets recompute set checksums and name hashes and
try each input both ways, because a mutated input otherwise stops at the first
comparison. The `image` input is a list of patches to a volume built in memory, not
a whole image: a volume is at least 1 MiB, and random changes across that much empty
space would rarely reach anything a decoder reads.

## The corpus

Seeds for the decoders are cut from images macOS `newfs_exfat` formatted and the
macOS driver populated, at both sector sizes. The oracle tier proves each one is
accepted by its decoder, and rewrites them when asked:

```sh
EXFAT_FUZZ_SEEDS_OUT="$PWD/fuzz/corpus" chore test:oracle
```

They hold only the names and contents `scripts/test-oracle.sh` writes. Every target
consumes arbitrary bytes or a synthetic image and never opens a device or a host
path. A fuzzer finds crashes and broken invariants; it is not an independent oracle
of the format.
