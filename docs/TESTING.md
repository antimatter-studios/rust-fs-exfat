# Test contract

`chore build`, `chore fmt`, `chore lint`, `chore test` and `chore package` are
the local and CI gates. Rust 1.95.0 is pinned. The shared lint scripts need
Python 3 with PyYAML 6.0.3 and ripgrep. CI installs those prerequisites explicitly.
The dependency policy is deny.toml. rust-fs-core 0.4.3 supplies the optional CLI
plumbing; all dependencies must remain permissively licensed. Unicode
normalization and JSON parsing are test-only dependencies for platform names and
structured CLI output. A C compiler is required by the unit header check and
the oracle's static-library link test.

Each test tier uses rust-fs-core's tier.sh, refuses skipped and ignored tests,
and checks a minimum executed count. Logs stay in tmp/logs/.

| Tier | Measured on 2026-11-04 | Budget | Executed floor |
|---|---|---|---|
| unit | Interface-layer cold target: 180 lines, 8109 bytes; build-first: 141 lines, 6873 bytes, with 67 tests | 200 lines, 12000 bytes, allowing clean-build diagnostics | 67 |
| oracle | 128 lines, 4647 bytes including ABI/CLI comparisons and the linked C caller | 160 lines, 16000 bytes, allowing formatter/checker diagnostics | 6 |

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
The C ABI and CLI are exercised on the same independent images. Complete ABI and
CLI listings and every file's SHA-256 match the mounted driver. The C compiler
also links the static library and reads a reference-created file on each sector
size. Check-only fsck agrees with the reference checker on those clean images;
synthetic tests cover cross-links, lost clusters, dirty state and explicit audit
limits. CLI tests cover verified extraction, source protection and repair refusal.
Complete decoder fuzz targets and the final audit remain pending. This is not
yet Milestone 1.

Before any image operation, the safety guard checks the reviewed receipt parser,
cleanup, fresh image binding and attach/query/detach ordering. Only the exact
read-only diskutil info query on that attachment's own device is accepted; other
verbs, device reassignment, stale/foreign receipts, global flushes and process
killing fail. The guard's negative regression was observed failing in CI before
the correction. These checks are source-policy tests, not a sandbox for arbitrary
untrusted shell programs.

On a host without PyYAML, `chore lint` stops at `ci-gate.sh` with status 2.
Run the remaining read-only/provenance guards, format and all-feature Clippy
steps explicitly; report the missing YAML gate. CI supplies PyYAML and still
runs the complete lint task. Do not install host tools implicitly.

Use a fresh CARGO_TARGET_DIR for each validation pass and run package validation
last. With mixed staticlib/rlib outputs, package verification in the same target
directory was observed to leave stale library artifacts for a subsequent test
build on Rust 1.95.0. A fresh target reproduced the expected updated behavior.

The Linux harness configuration used by other filesystem drivers is not copied:
there is no Linux guest oracle in the current milestone. Windows formatting is
additional evidence only if the Windows harness can provide it.
