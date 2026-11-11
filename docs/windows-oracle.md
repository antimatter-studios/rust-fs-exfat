# Spec-built Windows-layout oracle

## Decision and provenance

Issue #22 adds a second fixture kind to `chore test:oracle-macos`: `windows`, alongside
the existing macOS `reference` images. These are **spec-built Windows-layout
fixtures, not Windows-produced images**. No licence-clean published Windows image
was verified, so no external image is redistributed. The builder is test
infrastructure, not a production formatter or a write API.

The layout follows Microsoft's public [exFAT specification](https://learn.microsoft.com/en-us/windows/win32/fileio/exfat-specification):
sections 3.1–3.4 (boot regions and checksum), 4 (FAT), 5 (cluster heap),
7.1 (allocation bitmap), 7.2 (upcase), 7.3 (volume label), and 7.4, 7.6, 7.7
(file entry sets, streams, UTF-16 names and checksums). No formatter implementation
source or downloaded disk image is used.

The only donor bytes are the recommended compressed upcase table from the
current run's freshly formatted macOS image. The builder follows its FAT chain,
requires the 5,836-byte table and specification checksum `E619D30D`, and decodes
it for name hashes. This is format data generated locally by an independent
platform tool, not copied implementation code. Boot sectors, FAT, directory
entries, allocation bitmap and file data are built separately in Python without
the Rust fixture builder or strict driver's encoders.

## Layout

Each raw image is 64 MiB, with either 512- or 4,096-byte logical sectors.
Both boot regions describe 32 KiB clusters (versus the macOS fixtures' 4 KiB),
a FAT at byte 131,072 of length 131,072, and a heap at byte 1,048,576. The boot
checksum is independently recomputed and repeated throughout sector 11 of each
copy. The label is `WINORACLE`; the serial is `57494E31`.

The allocation bitmap occupies cluster 2, the upcase table cluster 5 and the
root directory cluster 9. The root starts with VolumeLabel, AllocationBitmap,
UpcaseTable, in that order. Gaps deliberately prevent assumptions that system
streams or the root immediately follow each other. The files exercise an empty
stream, mixed-case ASCII, a long Unicode name with a surrogate pair, a three-
cluster noncontiguous FAT chain and a four-cluster NoFatChain stream.

These choices exercise Windows-compatible allocation and metadata variation;
they do not claim to reproduce any particular Windows formatter version's
default geometry, boot code or allocation policy. The upcase contents themselves
are the recommended table, not a purported Windows-specific alternative.

## Independent checks and limits

The normal oracle command generates both fixture kinds inside its private
worktree `tmp/` directory. Every Windows-layout image must mount read-only on
macOS, pass `fsck_exfat -n`, and yield a complete name/size/SHA-256 manifest from
the mounted driver. Missing tools, a failed mount or a failed checker fails the
tier; there are no optional branches or ignored tests.

`tests/oracle/windows.rs` runs through the existing `macos_boot` test target. It
compares every decoded boot field with the independent permissive reader and
checks every repeated checksum word in both boot regions. The oracle's FAT-chain
reader and raw directory entries supply system-stream placement, the label,
every allocation bitmap bit, the upcase checksum and all 65,536 mappings for
comparison with the strict reader. The strict complete-tree audit must be healthy.
Every file's name, length and hash must match the macOS read-only manifest.

Passing establishes agreement with macOS on these spec-built layouts, not with
Windows `format`, `chkdsk` or a Windows filesystem driver. A real Windows-produced
fixture and Windows-side listing/hash/checker evidence remain additional work
requiring an available Windows harness or a verified redistributable reference.
