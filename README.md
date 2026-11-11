# rust-fs-exfat

An exFAT filesystem driver that reads and writes, with safe Rust decoders and a
read-only salvage adapter, licensed under MIT.

**Status: experimental and unreleased.** Boot-region reports, system metadata,
directory iteration, file reads, timestamps, volume-upcase path lookup, a
C ABI and command-line tools are implemented, which completes
[Milestone 1](https://github.com/antimatter-studios/rust-fs-exfat/issues/1).
Writing is being built ([#135](https://github.com/antimatter-studios/rust-fs-exfat/issues/135)):
a volume can be mounted for writing, files and directories created, written,
truncated and removed. Renaming and changing times, attributes or the label are
not implemented yet, and there is no formatter or repair operation.

The evidence is limited. Listings and every file's SHA-256 are compared with the
macOS driver on images macOS formatted and populated, at 512 and 4096 bytes per
sector, and `fsck_exfat -n` agrees on them. Nothing has been compared with an image
Windows formatted
([#22](https://github.com/antimatter-studios/rust-fs-exfat/issues/22)), or with a
real disk. Known limits:

- A raw device with 4096-byte sectors cannot be read; its block node can
  ([#20](https://github.com/antimatter-studios/rust-fs-exfat/issues/20)).
- The macOS driver keeps original bytes up to the next sector boundary past an
  unaligned ValidDataLength. This reader returns zeros from the exact length, as the
  specification requires, so the two differ on such a file.

## Architecture

The library is `fs_exfat`. Strict entry points accept `fs_core::BlockRead` only;
devices are held behind `ReadOnlyDevice`, never a writable device trait or a
runtime writable flag. Reads passed to a device are aligned to 512 bytes.

Writing is a separate API, `fs_exfat::write`, over rust-fs-core's `BlockDevice`.
The salvage adapter and the strict reader cannot reach it: they take
`BlockRead`, and `scripts/check-read-only.sh` keeps every writable device type
out of every module but the writer and the C ABI. `WriteVolume::mount` checks
the volume with the strict reader, refuses one already marked dirty or
reporting media failures, and sets VolumeDirty in the Main Boot Sector before
it writes anything else; `unmount` clears it last. The Backup Boot region is
left alone, as Section 3.2 of the specification asks and the macOS driver does.
PercentInUse is kept current, and the boot checksum is recomputed when either
field changes. Metadata writes are held in a sector cache and written at
barriers, so each operation updates the Allocation Bitmap and FAT before the
directory entry that refers to new clusters, and the directory entry before
the clusters it released. `WriteVolume::reader` is the strict reader over the
mount, seeing the writes made so far. Allocation keeps a stream contiguous
(NoFatChain) when it can, converts it to a FAT chain when it cannot grow in
place, never takes a cluster the FAT marks bad, and writes both FATs and both
Allocation Bitmaps of a two-FAT volume.

`create`, `mkdir`, `pwrite` and `write_file` make files and directories and
write their data. New names are checked as Section 7.7.3 requires and compared
with existing ones through the volume's Up-case Table; each entry set carries
its SetChecksum and a NameHash over the up-cased name. A directory that runs
out of entries grows by a zeroed cluster, and its parent records the new
length. Writes allocate as a file grows, keep it contiguous (NoFatChain) while
they can, and zero any gap between the old ValidDataLength and the write, so
DataLength and ValidDataLength stay equal. New entries take the current time
in UTC unless `create_with` and `mkdir_with` give times, with hundredths and
UTC offsets, and ReadOnly, Hidden, System or Archive. On the macOS reference
volumes, a tree written beside macOS's own files (large, fragmented and
non-ASCII-named files, directories grown past a cluster, gaps and given times)
passes `fsck_exfat -n`, and the read-only macOS driver lists the same names,
sizes, times and SHA-256s.

`truncate` shrinks a file, recording the new length before it frees the
clusters past it, or grows it with bytes that read as zeros: ValidDataLength
stays where it was and the rest of its cluster is zeroed on the device, since
some readers return what a cluster holds past it. A shrunk FAT chain whose
remaining clusters are consecutive is recorded as NoFatChain. `unlink` and
`rmdir` clear InUse on every entry of the set before they free its clusters;
`rmdir` refuses a directory holding any entry in use, and `unlink` a
directory. On the macOS reference volumes, files macOS made and files this
crate made are truncated both ways and removed, named through the volume's
case mapping, and the result passes `fsck_exfat -n` and reads back through the
read-only macOS driver as this crate records it.
See [features](docs/features.md) and [provenance](PROVENANCE.md).
Directory entry types, primary/stream/name fields, UTF-16 names, entry-set
checksums and name hashes are decoded by `fs_exfat::primitives`. Its
`RawEntrySet` retains damaged values and validity flags; strict directory
validation and recovery's `parse_set`/`find_sets` apply their distinct policies
to the same decoded fields.
`fs_exfat::primitives` exposes raw bitmap, upcase-table and root-system-entry
decoders with independent validity observations. Strict metadata APIs delegate
decoding to these primitives and retain rejection policy and diagnostics.
Strict FAT lookups, chain traversal and cluster read mapping delegate to
`fs_exfat::primitives`. Its lazy chain reports retain raw FAT words and damage
flags without requiring a full FAT snapshot. Volume and file APIs retain
allocation, system-stream overlap, read-budget and ValidDataLength policy.
The allocation audit also uses primitive FAT windows, constant-memory stream
walks, entry framing and bitmap operations. Audit verdicts, ownership rejection,
directory workspace budgeting and the complete-report requirement stay in
`check.rs` and its `check_audit.rs` implementation.
`fs_exfat::salvage` defines the format-neutral salvage adapter trait of the
[driver convention](docs/convention.md) and implements it for exFAT on the
primitives alone. It needs no strict mount, takes sources only as `BlockRead`,
and returns candidates, extent walks, parent claims and damage as serializable
evidence rather than trusted files. A file entry set that runs past its
cluster into a distant one, as in a directory that grew after other data was
written, is read across the clusters the FAT links. Plans materialize into a
caller's `io::Write`, never into the source. A plan names source extents, zeros,
literal bytes and cluster runs, which map through the adapter's `cluster_layout`,
and may declare a final length, a valid length past which output is zeros, and
the file's name and times. `plan_reader` executes a plan as a positioned reader,
so any offset of the output can be read on demand. `Plan::digest` is a stable
SHA-256 of a plan's documented canonical encoding, so a caller knows when a file
must be materialized again. On the macOS reference images, every verb agrees
with the read-only macOS driver's listing, hierarchy and file contents. As an
exFAT extension outside the common trait, `ExfatSalvage::stale_chains` lists
free clusters whose FAT entries are non-zero, which the specification allows,
with the chains they still link. `ExfatSalvage::uninterpreted_entries`, another
such extension, lists the
directory entries the driver does not interpret, Vendor Extension and Vendor
Allocation entries and unknown primary and secondary types, which the
specification lets a reader skip, with each one's offset, type, InUse bit and
whether it lies in a valid entry set. `ExfatSalvage::upcase_table` reads the
volume's Up-case Table, compressed or not, so salvage compares names by the
volume's own case mappings. On a two-FAT (TexFAT) volume,
`ExfatSalvage::fat_copies` lists every cluster the two FATs or the two
Allocation Bitmaps record differently, with its byte offset in each copy, and
`ExfatSalvage::with_fat` walks claims through the inactive pair, which the
specification calls stale rather than invalid.
`ExfatSalvage::cross_links` lists each cluster that two or more FAT entries name,
or that two or more of the caller's candidates claim, with the FAT entries and
their chains' heads, and the candidate records, that reach it.
Pointer conversion is confined to the C ABI module; the decoders and command-line
implementation deny unsafe Rust. The header and ownership rules are documented
in [the C ABI guide](docs/C-ABI.md).

File reads and chain walks are bounded by the volume's cluster count, not a fixed
million-cluster cap. Contiguous streams retain only their cluster range; opening
them still validates every allocation bit and rejects system-cluster overlap.
FAT-linked streams retain a four-byte index per cluster and use constant-space
cycle detection. `ReadLimits::max_metadata_bytes` bounds each metadata buffer and
each chain index independently, including reserved vector capacity; its default
64 MiB permits up to 16,777,216 FAT-linked clusters, further bounded by the volume
and any explicit `max_chain_clusters`. The default cluster cap is the volume's
cluster count (`usize::MAX` before opening). Memory or cluster limits and
allocation failures return explicit errors, never truncated files or chains.
These are per-allocation limits, not a total process-memory ceiling. Simultaneously
open files retain separate indexes; the allocation audit has its own total budget.
Both stream layouts above 1,048,576 clusters are checked with macOS `fsck_exfat -n`
and complete streamed SHA-256 comparisons against its read-only filesystem driver.

## Command-line tools

The optional `cli` feature builds the family multi-call binary. These commands
use it without relying on tool names installed elsewhere on PATH:

```sh
cargo build --locked --release --features cli
target/release/rust-fs-exfat fs image.dmg ls /
target/release/rust-fs-exfat fs image.dmg stat /file.txt
target/release/rust-fs-exfat fs image.dmg cat /file.txt
target/release/rust-fs-exfat fs image.dmg export /file.txt output.txt --verify
target/release/rust-fs-exfat fs image.dmg mkdir -p /backup/2026
target/release/rust-fs-exfat fs image.dmg write /backup/2026/notes.txt < notes.txt
target/release/rust-fs-exfat fs image.dmg truncate --size 0 /backup/2026/notes.txt
target/release/rust-fs-exfat fs image.dmg rm /backup/2026/notes.txt
target/release/rust-fs-exfat fs image.dmg rmdir /backup/2026
target/release/rust-fs-exfat fsck image.dmg -n
```

The corresponding dotted names are `fs.exfat` and `fsck.exfat`; packaging may
link those names to the same binary. Metadata is JSON by default, with `--text`
for people; `cat` writes raw bytes. `--offset BYTES` selects a filesystem within
an image and must be 512-byte aligned. Export uses `fs_core::cli::transfer`:
it stages locally, refuses source aliases and existing destinations by default,
and verifies staged length and SHA-256 before publication when `--verify` is set.
Only the destination is written; the source always stays read-only. `mkdir`,
`write`, `truncate`, `rm` and `rmdir` are the only verbs that open the image
for writing: the volume is
marked dirty while they run and clean when they finish, and one already marked
dirty is refused.

`fsck.exfat` is check-only, with no repair mode. It checks boot copies, metadata,
entry sets, chain allocation, cross-links and unreferenced allocated clusters.
Dirty/media-failure flags are reported without clearing them. Vendor and unknown
directory entries the audit does not interpret are counted by kind in
`uninterpreted_entries`; they never change the verdict. The audit supports
every valid exFAT cluster count, without a fixed cluster or entry cap. Allocation
and ownership bitmaps use 1/4 byte per cluster together; FAT and directories are
streamed in 64 KiB chunks. The total audit memory ceiling is 1,537 MiB, including
budgeted directory workspace. Insufficient memory, exhausted workspace or an
incomplete read fails explicitly, never with a partial clean bill. See the
[audit memory contract](docs/fsck-memory.md) for exact bounds and library APIs.
Exit statuses are 0 clean, 4 uncorrected
inconsistencies, 8 I/O or unsupported audit limits, and 16 usage errors.

## Building and testing

Rust 1.95.0 is pinned. Install `chore` 0.19.0 and check out rust-fs-core v0.4.3
beside this checkout for the shared development scripts. The library dependency
itself comes from crates.io.

```sh
chore build
chore lint
chore test
chore package
chore staticlib
chore artifact
chore fuzz -- 30
```

`chore test` runs unit, documentation and macOS image-oracle tests, and the check
that package verification leaves nothing a later test links. `chore fuzz -- 30` runs
every decoder's [fuzz target](fuzz/README.md) for 30 seconds and needs nightly Rust
and cargo-fuzz. Linux CI runs
the explicitly host-independent `chore test:unit` tier; the oracle tier fails on
a non-macOS host rather than skipping.
Tests never silently skip missing prerequisites. Test output is retained in
`tmp/logs/`, through rust-fs-core's shared budgeted runner.

## Validation contract

Synthetic images test malformed inputs, not format correctness. Reference
fixtures must be generated by macOS `newfs_exfat` on newly created disk images,
checked with `fsck_exfat -n`, and compared with the macOS filesystem driver
mounted read-only, including every path and file's SHA-256. Those oracle tests
belong on a macOS CI job. Boot and metadata formatter/checker tests and read-only
mounted label/GUID, complete directory-listing and every-file SHA-256/timestamp
comparisons are implemented. Windows formatting is
additional evidence if a suitable harness is available. The oracle also checks
[spec-built Windows-layout fixtures](docs/windows-oracle.md), not images produced
by Windows, at both sector sizes against the read-only macOS driver and checker.
No Linux VM is needed
for the macOS oracle contract.

Only Microsoft's public specification and black-box reference tools are format
sources. Never use Linux exFAT or exfatprogs source code.
