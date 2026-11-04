# Test contract

`chore build`, `chore fmt`, `chore lint`, `chore test` and `chore package` are
the local and CI gates. Rust 1.95.0 is pinned. The shared lint scripts need
Python 3 with PyYAML 6.0.3 and ripgrep. CI installs those prerequisites explicitly.
The dependency policy is deny.toml; the initial lockfile contains only the
MIT-licensed rust-fs-core 0.4.3 dependency.

Each test tier uses rust-fs-core's tier.sh, refuses skipped and ignored tests,
and checks a minimum executed count. Logs stay in tmp/logs/.

| Tier | Measured on 2026-11-04 | Budget | Executed floor |
|---|---|---|---|
| unit | 56 lines, 2652 bytes on macOS arm64 | 100 lines, 8000 bytes, allowing clean-build paths and diagnostics | 24 |
| oracle | 93 lines, 3123 bytes for two newly formatted images on macOS arm64 | 160 lines, 16000 bytes, allowing formatter/checker diagnostics | 2 |

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

The unit tier and the macOS-only oracle tier are explicit, separate tasks.
`chore test` requires both. Missing tools and non-macOS oracle invocation fail,
naming `chore test:oracle`; Linux CI invokes only `chore test:unit`, while
macOS CI also invokes `chore test:oracle`. No decoder test is ignored.

No directory/file reader or CLI is implemented yet, and mounted-driver
listing/hash differential tests remain pending. This is not yet Milestone 1.

Use a fresh CARGO_TARGET_DIR for each validation pass and run package validation
last. With mixed staticlib/rlib outputs, package verification in the same target
directory was observed to leave stale library artifacts for a subsequent test
build on Rust 1.95.0. A fresh target reproduced the expected updated behavior.

The Linux harness configuration used by other filesystem drivers is not copied:
there is no Linux guest oracle in the current milestone. Windows formatting is
additional evidence only if the Windows harness can provide it.
