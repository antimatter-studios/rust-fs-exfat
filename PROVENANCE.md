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

Synthetic tests prove bounds and device alignment only. The reference-formatter,
check-only filesystem checker and mounted-driver differential tests required by
issue #1 are pending; this record does not claim independent format validation.
