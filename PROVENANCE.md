# Provenance

## Complete allocation audits

For #38, the check-only audit streams the active FAT and directories and tracks
ownership in a cluster bitset. The allocation bitmap and upcase table are read
from their validated system-stream lengths, independently of extraction limits.
This is new code based on the public exFAT specification and existing strict
decoders; no other filesystem implementation is an input.

The macOS oracle formats a newly created large sparse image with `newfs_exfat`
and compares check-only results with `fsck_exfat -n`. Additional specification-
built sparse/streamed images cross-validate high-cluster ownership failures and
a million-entry root with the platform checker. The format-maximum boundary and
resource/I/O refusal tests are synthetic evidence only. The budget and exact
per-cluster cost are documented in `docs/fsck-memory.md`.

## TexFAT second FAT and Allocation Bitmap

For #40, the FAT layout, the copy comparisons and the salvage `fat_copies` and
`with_fat` extensions are new code written from Microsoft's public exFAT
specification, sections 1.5 "Windows CE and TexFAT", 2 "Volume Structure"
(Table 3), 3.1.6 "FatOffset Field", 3.1.7 "FatLength Field", 3.1.13.1
"ActiveFat Field", 3.1.16 "NumberOfFats Field", 4 "File Allocation Table
Region", 7.1 "Allocation Bitmap Directory Entry" and 7.1.2.1 "BitmapIdentifier
Field", read on 2026-11-11. No TexFAT document beyond those sections, and no
other filesystem implementation, was an input. macOS does not
implement TexFAT, so the tests build two-FAT images from those sections and
check the results against a reader written in the test from the same sections,
which shares no code with the crate's decoders.

## Compressed Up-case Table

For #42, the decoder, the `upcase-run` deviation rule and the salvage
`upcase_table` extension are new code written from Microsoft's public exFAT
specification, sections 7.2.2 "TableChecksum Field", 7.2.5 "Up-case Table" and
7.2.5.1 "Recommended Up-case Table". No other filesystem implementation was an
input. The tests build their tables from those rules, not from formatter output.
Table 25 was transcribed from the specification's source only for a one-off
comparison: it sums to E619D30Dh, as section 7.2.5.1 says, and macOS
`newfs_exfat` wrote the same 5836 bytes on fresh images at 512 and 4096 bytes
per sector. The table is not stored in this repository.

## Permissive structure snapshots

For #35, on 2026-11-09 the owner authorized the current Captain Salvage
`crates/salvage-core/src/volume.rs`, `src/changes.rs` and synthetic tests as
behavioral inputs. The application worktree was at revision
`71ad79bb63976caa558fd786e43e1b640c27cab9`, with its existing uncommitted
`refactor/exfat-remove` changes. The files read had these SHA-256 digests:

- `volume.rs`: `02ed137a7735778679b69bbc22860da30b36e0fbfc1ee886d07a72dd2587d3d2`.
- `changes.rs`: `39921c5dd08711f17aa0ca9b6a485a6255c26b7575b1af2c44bdeb7ec09ca4bd`.
- Current `tests/synthetic.rs`: `bffe9110c35726c061ac1ad64ea96f8f51c8edc77d2df31a87f9aca221439380`.

Before the driver implementation, a standalone Rust program called the actual
application's `Volume::read_structures` and `Volume::from_structures`. It used
the original synthetic builder definitions extracted unchanged from that
revision, whose complete source-file digest is recorded below for #33.
The two private snapshot accessors from the current `changes.rs` were extracted
unchanged and executed in the same program. No expected output was generated
by the new driver APIs.

`tests/structures-captured.json` records raw lengths and SHA-256 digests, exact
read offsets/order/lengths, decoded geometry, boot reports, bitmap minimums,
diagnostic text and snapshot boundary results. Its SHA-256 is
`80c51ee0faa199d861cfaef0034f573658231f60dbf2f71ae7467a1ef1357c10`.
The cases include damaged signatures/checksums, an unusable backup name, a
fragmented root, a zero-length FAT, absent/undersized/oversized bitmap entries,
rounding at 201 clusters and an injected failure at each of the five reads.
The fixture captures preserve the bitmap entry and raw mutations at the
filesystem name (3..11), both VolumeFlags bytes (106..108) and boot signature
(510..512), without resealing boot checksums.

Additional application captures in `tests/structures-edge-captured.json`
(SHA-256 `57ec636a9af37b19a70b3d70a2ff67c623cbca54988125c05620c23fdc68281d`)
pin saved-copy errors and acceptance of a partial checksum sector containing
only its first stored word. Backup geometry remains raw rather than imposing
the main sector-size rule on it. New representability checks return diagnostics
where legacy unchecked arithmetic or slicing would panic; they add no arbitrary
volume-size limit. Out-of-snapshot FAT words return `None`; bitmap bits return
`false`, without strict cluster/value validation. Reserved bitmap clusters below
two also return `false` instead of underflowing.

The macOS oracle checks the loader against formatter-produced 512-byte-sector
structures, raw FAT words and allocation bits. It explicitly confirms the
fixed-sector loader rejects a 4096-byte-sector image rather than interpreting
it as 512-byte sectors. Named bitmap-entry bytes and boot-field boundaries are
checked against newly formatted images at both sector sizes. Damaged structures
have captured compatibility evidence, not platform acceptance evidence.

The application's existing 171 tests and unchanged `scripts/parity.sh` pass.
They also pass in an isolated copy wired to the new loader, inspection,
checked-accessor and fixture APIs; all three existing parity references remain
byte-identical. The original application worktree and references are untouched.

## Test-support encoders

For #33, on 2026-11-09 the owner authorized the supplied `tests/synthetic.rs`
as an additional source. Its SHA-256 is
`94e4f89d9cfa9e7546fe5a50b63639a61379ce9a2933cd81cb32e87cb44cd82e`,
at source revision `71ad79bb63976caa558fd786e43e1b640c27cab9`.
`src/fixtures.rs` preserves its fixed geometry, boot copies, reserved FAT words,
bitmap bits, raw system entries, live/deleted entry sets, timestamps, deterministic
data, placement methods and four named volume scenarios. The checksum-sector
loop is factored into `seal_boot`; public names and `Builder::image` permit
callers to move inline scenarios without their own on-disk encoders.

Before implementing the module, the original builder definitions were extracted
unchanged into a standalone Rust program and executed once, using their existing
driver primitive for the boot checksum. The 18 lengths and SHA-256 digests in
`tests/fixtures-captured.sha256` are those outputs, not values generated by the
new module. They include every complete volume, both boot regions, live/deleted
and multi-slot Unicode names, dates, resealing, system entries, noise and combined
FAT/bitmap/directory/data placement. `tests/fixtures.rs` checks those captures.

The supplied file has no real upcase-table payload encoder: its volume builders
declare a 100-byte upcase file with no checksum and leave the bytes zeroed.
That behavior is deliberately preserved. New `upcase_table` and `upcase_entry`
helpers encode supplied words and the actual table checksum without changing
legacy images. Application sessions, scans, databases, engine calls, file-format
sample generators and end-to-end assertions are not format encoders and are not
incorporated.

`tests/oracle/fixtures.rs` checks healthy forms at both platform sector sizes:
boot checksum sectors against both macOS boot copies, FAT words, bitmap bit
numbering, cross-cluster placement, bitmap/upcase entry bytes, the compressed
platform upcase table and its checksum, and live/deleted entry-set checksums and
date fields. Directory reconstruction retains platform fields the legacy encoder
does not supply: full attribute flags, timestamps/timezones, name hashes and
distinct valid lengths. The resulting newly created images pass `fsck_exfat -n`.
The four intentionally damaged scenarios, missing extended boot trailers and
placeholder upcase tables have no healthy form; captured hashes prove byte
compatibility, not acceptance by the platform checker.

This MIT-licensed crate is a clean-room implementation of Microsoft's public
[exFAT specification](https://learn.microsoft.com/en-us/windows/win32/fileio/exfat-specification).
No Linux exFAT, exfatprogs or other copyleft implementation source is an input.
Reference programs are used only as black-box tools on generated disk images.

The repository layout and development conventions follow rust-fs-ext4 (MIT).
Filesystem-specific source, historical provenance and test fixtures are not
copied from that template. rust-fs-core 0.4.3 (MIT) supplies the read-only device
traits and shared development scripts. Serde (MIT/Apache-2.0) preserves the
primitive boot-sector and entry-set types' camelCase serialization contract.

## Owner-authorized starting code

On 2026-11-04 the owner authorized MIT reuse of three standalone files written
from Microsoft's specification: `lens-core/src/exfat.rs`,
`lens-core/src/volume.rs` and `lens-core/tests/synthetic.rs`. Only those exact
files may be inspected. Their enclosing project, captures and disk-derived
artifacts are not inputs to this repository.

The first two files were screened for private disk/path markers and reviewed.
The synthetic test file contained a private-path marker and was not opened or
copied. No source from these files is incorporated in the scaffold. Subsequent
decoder layers will record any reused algorithms here, including their limits
and independent reference tests. Recovery scans and salvage code are out of
scope for Milestone 1.

## Validation

The owner authorized the complete permissive on-disk parser for the primitive
layer in #26. The supplied `src/exfat.rs` has SHA-256
`3dabdfb21626dbdcbb42da5b8e034516299aab962c9262e2f786eef4b2cbf347`.
Its boot fields and accessors, checksums, timestamp strings, entry-set fields,
bitmap lookup and byte helpers are preserved independently of the strict reader.
Boot parse failures use this crate's existing `Error::Invalid` type. Serde keeps
the original camelCase field names; public checksum and byte helpers retain their
unchecked slice preconditions. No stricter geometry, calendar-day, checksum,
name or bitmap validation is introduced into these primitives.

`tests/oracle/primitives.rs` compares healthy boot geometry and both boot checksums
with macOS formatter/checker output, every live directory entry with read-only
driver listings, bitmap locations with formatter output, and timestamp strings
with driver stat timestamps at their preserved two-second precision. These tests
run on both 512- and 4096-byte-sector images in `chore test:oracle` without using
the strict decoder as an oracle. Synthetic primitive fixtures are limited to
permissive and malformed cases, not evidence for healthy format correctness.

The owner authorized the format-level volume algorithms for #27. The supplied
`src/volume.rs` has SHA-256
`225483f08ce15b5172f36abe353765159adfb4159b5596e467b55a26df787ab0`.
FAT decoding, root and stream walks, allocation classes, predecessor indexing,
chain heads, islands, allocation statistics and boot-copy checks are preserved
as pure borrowed-slice functions. Existing primitive boot geometry, byte readers,
checksums and format constants are reused. Device access, saved-structure models,
hashing and application reports are not incorporated.

`tests/oracle/primitive_chains.rs` compares healthy FAT entries, root and stream
chains, offsets, allocation bits, predecessor indexes, islands and statistics
with macOS-created images, including every read-only driver file hash. Both
512- and 4096-byte-sector images pass the platform checker in the shared oracle
harness. Damaged-only fixtures preserve partial walks, out-of-range FAT values,
convergent predecessors, headless cycles, raw bitmap preconditions and boot-copy
mismatches; no strict-reader validation is added.

The stale FAT chain listing (#41) is new code derived from specification
sections 4.1.3, 5.1, 7.1 and 8.1: only the Allocation Bitmap records
allocation, no FAT value marks a cluster free, and freeing updates the FAT only
"if necessary", so a free cluster may keep any entry. The specification does
not say whether implementations clear such entries. On a newly formatted image,
the macOS driver zeroed the FAT entries of a deleted fragmented file and of a
truncated file's freed tail, so its images hold no such chains.
`tests/oracle/primitive_chains.rs` therefore compares the listing with the raw
bytes of the macOS reference images, then frees the driver-written fragmented
file by its InUse and bitmap bits alone and checks that the listing returns the
FAT chain macOS wrote, whose clusters hash to the driver's own read. Ends,
breaks, joins, loops and unreadable records are synthetic-only evidence built
from the specification.

Salvage's reading of entry sets split across distant directory clusters
(#99) is new code derived from specification sections 6 and 6.3.2: a
directory's entries lie over its cluster chain, and a primary's secondary
entries immediately follow it. `tests/oracle/salvage_scan.rs` rebuilds every
file the read-only macOS driver returns from salvage candidates alone on both
macOS reference images, whose root directories macOS fragmented; sets split
across three or more distant clusters, and splits with no FAT link, are
synthetic-only evidence.

The uninterpreted directory entry listing and counts (#43) are new code derived
from specification sections 6.2.1, 6.3.1.2.2, 6.3.2, 6.3.3, 6.4.1.2, 7, 7.8,
7.9, 7.10 and 8.2: the EntryType ranges and InUse bit, the revision 1.00 entry
types and their codes, the Vendor Extension (E0h) and Vendor Allocation (E1h)
layouts, TexFAT Padding (A1h) treated as an unrecognized benign primary, and
the rules that let a reader ignore unrecognized benign entries while an
unrecognized critical primary in the root invalidates the volume. macOS drops
such entries silently, so no oracle compares them; vendor entries, unknown
types, unused entries, set validity and unreadable directory sectors are
synthetic-only evidence built from the specification.

Image setup uses only the device in its own attachment receipt. There is no
system-wide flush; detaching that image completes its writes. The safety guard
has positive and negative checks and gates lint and the image-safety workflow.
The same guard runs before image setup. It rejects other disk-management verbs,
foreign device/image bindings, stale receipts, global flushes and process killing.
The only permitted disk-management query is the exact read-only
`/usr/sbin/diskutil info -plist "$device"` on the current attachment's own node;
it independently supplies the mounted label, GUID and read-only state. The
reviewed receipt parser and failure cleanup are pinned by source fingerprints;
changes to those small ownership boundaries require deliberate guard review.

The C ABI and CLI are new interface code, not reused filesystem implementations.
Both use BlockRead-only volume entry points and ReadOnlyDevice. The C ABI's
limited pointer conversion is isolated from safe decoders and catches Rust
panics with rust-fs-core's boundary helpers. CLI extraction delegates staging,
source-alias refusal and SHA-256 verification to rust-fs-core. The check-only
allocation audit has no repair or write callback.

Path equality applies the decoded volume upcase table to individual UTF-16
units, as required by specification sections 7.2 and 7.6. It does not use host
Unicode case folding, normalization or multi-character expansions. A synthetic
nonstandard table proves that the volume, not the host, determines equality.
Read-only macOS queries independently cover ASCII/non-ASCII case variants,
directory navigation, content hashes and missing/non-directory errors.

Directory sets follow specification sections 6 and 7.4, 7.6–7.9. No directory
decoder implementation is copied from another filesystem. Synthetic tests cover
checksums, order, counts, invalid names and cluster-boundary splits. The macOS
driver creates Unicode, long-name, nested and many-entry directories on newly
formatted images; a subsequent read-only mount supplies every path and file
size for differential comparison. macOS readdir decomposes accented names even
when their on-disk form is composed; the oracle compares canonical-equivalent
paths in NFC. The library preserves on-disk names and does not normalize them.
File streams and timestamps follow specification sections 7.4 and 7.6. The
read-only macOS oracle compares every file length, SHA-256, creation, modification
and access timestamp. A driver-created file has its ValidDataLength reduced in
the detached image; its allocated tail remains nonzero while both readers must
return zeroes. The fixture edit and its checksum are specification-derived.
The main file differential uses a 4096-byte-aligned valid length: the macOS
reference rounds an unaligned 513-byte valid length up to the next sector
boundary. The library does not emulate that discrepancy; synthetic tests retain
byte-exact unaligned boundaries, and the oracle tier exercises the reference's
rounding on each image, so a change in either is seen (#10).
Unknown UTC offsets are represented explicitly, not guessed from the host clock.

FAT entries, allocation bitmap indexing, compressed and uncompressed upcase
tables, labels and Volume GUIDs are derived from specification sections 4.1
and 7.1–7.5. No salvage statistics or island-walking code is imported from the
authorized starting code. Synthetic system streams cover malformed metadata;
the macOS oracle compares FAT-linked system streams with independent formatter
output and its recommended upcase checksum. A specification-built optional
GUID entry is inserted into each newly formatted image while detached. The
macOS driver then mounts the image read-only, and diskutil independently reports
the label and GUID for comparison. The checker accepts these images but does not
reject a damaged GUID checksum; checksum rejection is synthetic evidence only.

The boot-sector offsets and rotating-add checksum follow specification sections
3.1 and 3.4. Their starting point is the owner's authorized `src/exfat.rs`, whose
SHA-256 was `787958c49773eaed2c15b54340293af74be341382b63755aef7a0db3c985434d`.
The strict reader adds complete geometry checks, every repeated checksum word,
extended signatures, bounded reads, independent reports for both boot copies,
and an explicit comparison of the copies. It does not reuse the starting code's
salvage scanner, fixed-sector assumption or unchecked geometry.

Synthetic tests test malformed boot structures. `chore test:oracle` creates new
64 MiB images with macOS hdiutil, formats them with newfs_exfat at 512 and 4096
bytes per sector, checks them with fsck_exfat -n, and compares both decoded boot
copies with the formatter's geometry. Device nodes come only from each attach
command's own plist output. Images are detached before the Rust reader runs.
Mounted-driver listing/hash differentials cover the Rust reader, C ABI and CLI;
synthetic corruption tests are not independent format evidence. Fuzz seeds are cut
from these reference images, and fuzzing is not independent format evidence either.

The Windows-layout fixture kind is specification-built, not Windows-produced.
`scripts/build-windows-oracle.py` is a test-only builder derived from Microsoft's
public specification, without any third-party formatter implementation or image.
It reuses only the recommended compressed upcase table from this run's newfs_exfat
image, checking its length and specification checksum before relocating it.
The macOS read-only driver and fsck_exfat independently validate these fixtures;
the existing permissive primitive reader and raw oracle decoders compare their
boot geometry, both checksums, every bitmap bit and all 65,536 upcase mappings
with the strict driver. See `docs/windows-oracle.md` for layout and limitations.
