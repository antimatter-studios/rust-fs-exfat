# Two-FAT writing: independent validation is missing

The writer's two-FAT output is **independently unverified**. The existing
`a_two_fat_volume_keeps_its_second_fat_and_bitmap_in_step` test in
`tests/write.rs` remains useful synthetic coverage, not TexFAT interoperability
evidence. Its image comes from `tests/write_support`, it compares the two FATs
and Allocation Bitmaps, and the write mount uses this crate's own strict reader.
Agreement between these components does not establish acceptance by another
implementation.

The writer mirrors updates to both copies; it does not use an ActiveFat switch
to commit transactions. Two copies alone do not establish TexFAT transaction
safety or crash recovery. Microsoft's [exFAT specification, section 1.5][spec]
describes TexFAT as transaction-safe semantics using two FATs and bitmaps, with
additional structures. The basic specification is not a complete TexFAT
transaction protocol.

## Oracle suitability review (2026-11-13)

- **Existing harness oracles:** macOS's exFAT driver, the Linux kernel and
  exfatprogs do not implement TexFAT. Their single-FAT writer checks cannot
  validate this two-FAT path; see [the original evidence gap][issue]. Linux
  tools remain guest-only, and no host Linux tool was used for this review.
- **relan/exfat:** this is already the harness's third implementation, but its
  maintainer [explicitly declined removing the two-FAT rejection][relan].
  That discussion was read as documentation only; no implementation source
  was read or copied. It is not a TexFAT-capable alternative to the other
  harness oracles.
- **Sleuth Kit:** its [public API reference][tsk] describes a TexFAT padding
  directory entry, but the [pinned guest-only evaluation](sleuthkit-evaluation.md)
  for [#178][tsk-followup] found that version 4.11.1 is unsuitable as an
  ActiveFat-aware oracle. Mirrored two-FAT images list and extract, but a stale
  inactive First FAT produces incorrect, truncated extraction with exit 0 when
  ActiveFat selects the Second FAT; a stale inactive First Allocation Bitmap
  likewise produces the wrong allocation verdict. The symmetric ActiveFat-0
  controls pass. Its mixed IPL/CPL/GPL package licensing means it remains an
  external CLI oracle candidate, not a linked dependency or source input; no
  implementation source was inspected.
- **FatFs R0.16:** this portable C implementation is a plausible permissively
  licensed candidate, but its exFAT mount path requires exactly one FAT and
  returns `FR_NO_FILESYSTEM` otherwise (`source/ff.c`, lines 3541–3542 in the
  [official R0.16 archive][fatfs]). Its FAT12/16/32 support for two FATs does
  not apply to exFAT. The [published one-clause BSD-style license][license]
  and the archive's matching source header were checked before inspecting
  that restriction. No integration or new dependency was added.
- **Windows CE / Windows Embedded:** Microsoft's specification identifies
  Windows CE as a TexFAT implementation. The harness has no licensed TexFAT
  image, reader tool or reproducible guest setup for it. A potential oracle
  is not a runnable oracle; a generic Windows exFAT reader is not evidence
  of TexFAT support.

No suitable independent TexFAT-capable oracle was established in the current
harness. Sleuth Kit 4.11.1 was run on newly created two-FAT images and rejected
as an ActiveFat-aware oracle on the evidence above. This is not a claim that
no such implementation exists, nor a verdict about every Sleuth Kit version.
The negative capability result does not supply positive interoperability
evidence for this crate's writer.

For reproducibility, the downloaded R0.16 archive's SHA-256 is
`99f7dc1f7e095356e4a9e3dbe29959090d8b948afe2bbc5441e52fdf4b85449e`;
its unmodified `source/ff.c` bytes have SHA-256
`dbeae3d7d22dbc070983e31af897fa5e43d29c726a0a240fe9589ffe2ec0ef1e`.
No third-party source or archive is included in this repository.

## Evidence needed to change the status

Create a new two-FAT image, change it through this crate's writer, unmount
cleanly, and require a reader with documented TexFAT support to accept it and
agree on its files and allocation state. Pin that implementation and its
license/setup in the harness, fail on missing prerequisites, and retain the
synthetic test. Transaction and crash-recovery claims would require additional
tests of those semantics, not merely successful reads of mirrored copies.

[spec]: https://learn.microsoft.com/en-us/windows/win32/fileio/exfat-specification#15-windows-ce-and-texfat
[issue]: https://github.com/antimatter-studios/rust-fs-exfat/issues/154
[relan]: https://github.com/relan/exfat/issues/77
[fatfs]: https://elm-chan.org/fsw/ff/arc/ff16.zip
[license]: https://elm-chan.org/fsw/ff/doc/appnote.html#license
[tsk]: https://sleuthkit.org/sleuthkit/docs/api-docs/4.13.0/structEXFATFS__TEXFAT__DIR__ENTRY.html
[tsk-followup]: https://github.com/antimatter-studios/rust-fs-exfat/issues/178
