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
The mounted-driver listing/hash differential is still pending; boot validation
alone does not satisfy the whole milestone.
