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
request. It also fails when a public `parse` or `decode` has no target, when a
target stops calling a salvage verb it lists, and when no seed in a corpus gets past
its decoder's first check.

A crash found by the explorer is a bug. Commit its reproducer to
`fuzz/corpus/<target>/`, so the gate replays it from then on, and fix the decoder.

## Targets

| Target | Decoder |
|---|---|
| `boot_sector` | Strict and primitive `BootSector::parse`, primitive `BootSectorHeader::parse` and `BootRegion::parse`, and strict `boot_checksum` |
| `fat_entry` | `FatEntry::parse` |
| `allocation_bitmap` | Strict `AllocationBitmap::parse` and primitive `RawAllocationBitmap::parse` |
| `upcase_table` | Strict `UpcaseTable::parse` and primitive `RawUpcaseTable::parse` |
| `root_metadata` | Strict `RootMetadata::parse` and primitive `RawRootMetadata::parse` |
| `directory` | `directory::parse`, primitive `EntryType::parse` and `RawEntrySet::parse` |
| `timestamp` | `Timestamp::decode` |
| `timestamps` | `Timestamps::parse` |
| `image` | a synthetic volume opened, walked, read, looked up and checked |
| `entry_sets` | the salvage carver, `primitives::parse_set` and `find_sets`, with and without valid set checksums |
| `chain_islands` | `primitives::chain_islands` and `compute_stats` over an arbitrary FAT and bitmap |
| `salvage` | every salvage adapter verb and exFAT extension over a damaged synthetic volume with unreadable sectors |
| `plan` | arbitrary materialization plans, as JSON or built from bytes, with extents, cluster runs and declared lengths, validated and materialized |

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

The `salvage` input patches the same volume, which it can also give two FATs, and
names sectors that fail to read and a length to cut the source to. Salvage promises
evidence rather than files, so the target holds each result to what its
documentation says about the source's bytes: a candidate is the set the carver finds
at its offset, a walk follows the FAT, a tree's parents are exactly the directories
whose walks hold the record, a stopped listing is a prefix of the whole one, and an
unreadable finding names bytes that do not read, including a FAT word whose
own sector reads while another sector of its 64 KiB read window fails
([#107](https://github.com/antimatter-studios/rust-fs-exfat/issues/107)).
The `plan` target works out from a plan's own words whether it may run: its extents
and cluster runs lie on the source and in the layout, its final length cuts no more
than part of the last step that produces bytes, and its valid length does not pass
the final length. It checks that validation and materialization agree with that, and
that every output byte is the one the plan names, zeros past the valid length, or
zeros flagged unreadable within a sector of one that fails. The `salvage` target
also rebuilds a carved file from its walk's cluster runs, DataLength and
ValidDataLength.

## The corpus

Seeds for the decoders are cut from images macOS `newfs_exfat` formatted and the
macOS driver populated, at both sector sizes. The oracle tier proves each one is
accepted by its decoder, and rewrites them when asked:

```sh
EXFAT_FUZZ_SEEDS_OUT="$PWD/fuzz/corpus" chore test:oracle
```

They hold only the names and contents `scripts/test-oracle.sh` writes. The
`entry_sets` corpus starts from the same directories. The `image` corpus and the
other salvage corpora are synthetic, built in `fuzz/shared/`, and the same command
rewrites them. Every target consumes arbitrary bytes or a synthetic image and never
opens a device or a host path. A fuzzer finds crashes and broken invariants; it is not an independent oracle
of the format.
