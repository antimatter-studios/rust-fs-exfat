# Test contract

`chore build`, `chore fmt`, `chore lint`, `chore test` and `chore package` are
the local and CI gates. Rust 1.95.0 is pinned. The shared lint scripts need
Python 3 with PyYAML 6.0.3 and ripgrep. CI installs those prerequisites explicitly.
The dependency policy is deny.toml; the initial lockfile contains only the
MIT-licensed rust-fs-core 0.4.3 dependency. Unicode normalization is a permissive
test-only dependency for the platform's decomposed directory names.

Each test tier uses rust-fs-core's tier.sh, refuses skipped and ignored tests,
and checks a minimum executed count. Logs stay in tmp/logs/.

| Tier | Measured on 2026-11-04 | Budget | Executed floor |
|---|---|---|---|
| unit | Path-layer clean target: 119 lines, 5761 bytes; build-first: 105 lines, 5221 bytes | 150 lines, 12000 bytes, allowing the added path suite and clean-build diagnostics | 49 |
| oracle | 116 lines, 4066 bytes including path-query comparisons | 160 lines, 16000 bytes, allowing formatter/checker diagnostics | 5 |

Boot reports validate both copies and report every differing byte. The macOS
oracle formats fresh 64 MiB images with 512-byte and 4096-byte sectors, checks
them with fsck_exfat -n, and compares decoded geometry with newfs_exfat output.
Each attachment uses the matching hdiutil -blocksize; otherwise fsck_exfat
correctly refuses a 4096-byte filesystem on a 512-byte presented device.
Only the device node in that attachment's own plist is ever used, and every
image is detached before the Rust reader runs. Metadata tests compare the FAT,
bitmap and upcase streams with formatter output. After GUID insertion into the
detached image, the macOS driver mounts it read-only; diskutil supplies an
independent label and GUID comparison. A failing run retains its
image and receipt; a successful run removes its own scratch directory.

Image setup never requests a system-wide filesystem flush. Files are closed,
then hdiutil detach flushes the image device obtained from that attach receipt.
The lint gate tests both accepted file-scoped flushing and rejected global
flush calls before checking the actual runner. This addresses #12.

The unit tier and the macOS-only oracle tier are explicit, separate tasks.
`chore test` requires both. Missing tools and non-macOS oracle invocation fail,
naming `chore test:oracle`; Linux CI invokes only `chore test:unit`, while
macOS CI also invokes `chore test:oracle`. No decoder test is ignored.

Directory sets spanning FAT-linked clusters and contiguous subdirectories are
validated. Fresh images are populated through the macOS driver, then remounted
read-only for complete recursive path/size comparisons, including platform
metadata files. Canonical Unicode equivalence is used only in the oracle because
macOS readdir decomposes names; the reader preserves the stored UTF-16 name.
Every file length, SHA-256 and creation/modification/access timestamp is compared
with the read-only driver. The reduced-valid-length fixture retains a nonzero
allocated tail and uses a 4096-byte boundary. macOS rounded a 513-byte valid
length to a sector boundary in black-box testing; unit tests independently
require byte-exact zero filling at unaligned lengths. The library does not
reproduce the reference discrepancy (tracked in #10). Unknown UTC offsets remain unknown rather
than being converted using the host clock. Lookup uses the volume table without
host case folding or Unicode normalization. Paths are root-relative, with at
most 1024 nonempty components and 255 UTF-16 units per ordinary component.
Missing components are never cancelled lexically by a later parent component.
The C ABI, command-line tools and complete decoder fuzz targets remain pending.
This is not yet Milestone 1.

Use a fresh CARGO_TARGET_DIR for each validation pass and run package validation
last. With mixed staticlib/rlib outputs, package verification in the same target
directory was observed to leave stale library artifacts for a subsequent test
build on Rust 1.95.0. A fresh target reproduced the expected updated behavior.

The Linux harness configuration used by other filesystem drivers is not copied:
there is no Linux guest oracle in the current milestone. Windows formatting is
additional evidence only if the Windows harness can provide it.
