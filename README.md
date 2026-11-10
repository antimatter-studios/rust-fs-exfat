# rust-fs-exfat

A strict, read-only exFAT reader with safe Rust decoders, licensed under MIT.

**Status: experimental and unreleased.** Boot-region reports, system metadata,
directory iteration, file reads, timestamps, volume-upcase path lookup, a read-only
C ABI and command-line tools are implemented, which completes
[Milestone 1](https://github.com/antimatter-studios/rust-fs-exfat/issues/1).
There is no formatter, repair operation or write feature.

The evidence is limited. Listings and every file's SHA-256 are compared with the
macOS driver on images macOS formatted and populated, at 512 and 4096 bytes per
sector, and `fsck_exfat -n` agrees on them. Nothing has been compared with an image
Windows formatted
([#22](https://github.com/antimatter-studios/rust-fs-exfat/issues/22)), or with a
real disk. Known limits:

- A raw device with 4096-byte sectors cannot be read; its block node can
  ([#20](https://github.com/antimatter-studios/rust-fs-exfat/issues/20)).
- A file over 1,048,576 clusters is refused, not misread
  ([#21](https://github.com/antimatter-studios/rust-fs-exfat/issues/21)).
- The macOS driver keeps original bytes up to the next sector boundary past an
  unaligned ValidDataLength. This reader returns zeros from the exact length, as the
  specification requires, so the two differ on such a file.

## Architecture

The library is `fs_exfat`. Strict entry points accept `fs_core::BlockRead` only;
devices are held behind `ReadOnlyDevice`, never a writable device trait or a
runtime writable flag. Reads passed to a device are aligned to 512 bytes.
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
Pointer conversion is confined to the C ABI module; the decoders and command-line
implementation deny unsafe Rust. The header and ownership rules are documented
in [the C ABI guide](docs/C-ABI.md).

## Command-line tools

The optional `cli` feature builds the family multi-call binary. These commands
use it without relying on tool names installed elsewhere on PATH:

```sh
cargo build --locked --release --features cli
target/release/rust-fs-exfat fs image.dmg ls /
target/release/rust-fs-exfat fs image.dmg stat /file.txt
target/release/rust-fs-exfat fs image.dmg cat /file.txt
target/release/rust-fs-exfat fs image.dmg export /file.txt output.txt --verify
target/release/rust-fs-exfat fsck image.dmg -n
```

The corresponding dotted names are `fs.exfat` and `fsck.exfat`; packaging may
link those names to the same binary. Metadata is JSON by default, with `--text`
for people; `cat` writes raw bytes. `--offset BYTES` selects a filesystem within
an image and must be 512-byte aligned. Export uses `fs_core::cli::transfer`:
it stages locally, refuses source aliases and existing destinations by default,
and verifies staged length and SHA-256 before publication when `--verify` is set.
Only the destination is written; the source always stays read-only.

`fsck.exfat` is check-only, with no repair mode. It checks boot copies, metadata,
entry sets, chain allocation, cross-links and unreferenced allocated clusters.
Dirty/media-failure flags are reported without clearing them. The audit supports
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
additional evidence if a suitable harness is available. No Linux VM is needed
for the macOS oracle contract.

Only Microsoft's public specification and black-box reference tools are format
sources. Never use Linux exFAT or exfatprogs source code.
