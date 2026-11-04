# Provenance

This MIT-licensed crate is a clean-room implementation of Microsoft's public
[exFAT specification](https://learn.microsoft.com/en-us/windows/win32/fileio/exfat-specification).
No Linux exFAT, exfatprogs or other copyleft implementation source is an input.
Reference programs are used only as black-box tools on generated disk images.

The repository layout and development conventions follow rust-fs-ext4 (MIT).
Filesystem-specific source, historical provenance and test fixtures are not
copied from that template. rust-fs-core 0.4.3 (MIT) supplies the read-only device
traits and shared development scripts. There are no other library dependencies.

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
reference was observed to round an unaligned 513-byte valid length up to a
512-byte sector boundary. The library does not emulate that discrepancy;
synthetic tests retain byte-exact unaligned boundaries. The reference discrepancy
is tracked in #10.
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
synthetic corruption tests are not independent format evidence. Complete decoder
fuzz targets and the final milestone audit remain pending.
