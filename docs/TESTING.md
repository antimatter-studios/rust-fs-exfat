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

## CI

Every job in `.github/workflows/ci.yml` runs chore tasks, so a green local task is
the same evidence as the job that runs it; the workflow's header names each job's
tasks, and `ci-ok`, the one required check, fails when any job failed, was cancelled
or was skipped. `unit` runs `chore test:unit` with no fixture images present, then
`chore lint`; `fixtures` builds the kernel-made images in the harness VM on x86_64
KVM and hands them on as an artifact; `images` runs `chore test:images` from that
artifact; `test` runs `chore test:oracle`, `chore test:kernel` and
`chore test:third` with every Linux tool in the guest and the runner's own
exfatprogs made unusable; `suite-in-vm` runs `chore test:vm`; `test-arm64` runs
lint, the unit tier and `chore test:images` on aarch64, which has no KVM;
`oracle-macos` runs the unit, images and macOS oracle tiers; `package` runs `chore test:package`; `semver`, `msrv` and
`provenance` run `chore check:semver`, `chore check:msrv` and
`chore check:provenance`; `cli` runs `chore cli:install`, puts the staged tools first
on PATH and runs `chore test:cli` against them as installed; `scripts` runs
`chore test:scripts`, the shell tests in `tests/scripts/`, which hold the workflows to
their headers. A job's tier logs are uploaded as `tier-logs-<job>` once its first
tier has run.

| Tier | Measured output | Budget | Executed floor |
|---|---|---|---|
| unit | 2026-11-11 the full pipeline on main (#134), local macOS: 436 lines, 26410 bytes, 321 tests, after `chore build` | 437 lines, 26527 bytes, allowing one slow-test notice | 150 |
| images | CI run 43105817721, ubuntu, cold build: 95 lines, 3328 bytes; 2026-11-11 (#122), local macOS, cold build: 53 lines, 2025 bytes, 7 tests over 14 fixtures; 14 lines warm | 98 lines, 3500 bytes | 7 |
| oracle | CI run 43105817721, x86_64 guest, cold build: 152 lines, 7274 bytes; 2026-11-11 exfatprogs in the guest (#123), local macOS, cold build: 107 lines, 5718 bytes, 2 tests and one `[oracle vm]` line per fixture | 156 lines, 7500 bytes | 2 |
| kernel | 2026-11-11 (#124), local macOS HVF guest, cold build: 104 lines, 5590 bytes, 3 tests | 135 lines, 7300 bytes | 3 |
| third | 2026-11-11 relan/exfat (#125), local macOS HVF guest, warm: 95 lines, 7084 bytes, 3 tests; a cold build adds about 38 lines, 1165 bytes | 175 lines, 10700 bytes | 3 |
| vm | 2026-11-11 the whole suite in the guest (#126), local macOS HVF guest, warm: 625 lines, 38742 bytes, 411 tests with the write layers above it; on main the suite lists 337 tests (335 and 2 doc tests); CI's fresh guest adds the toolchain install and a whole build, about 60 lines, 3.5 KB | 860 lines, 51000 bytes | 337 |
| oracle-macos | 2026-11-11 the full pipeline on main (#134), local macOS: 258 lines, 10947 bytes, 40 tests including large-volume and million-entry comparisons | 310 lines, 16000 bytes, allowing formatter/checker diagnostics | 25 |
| wasm | CI run 43105817721, ubuntu, cold wasm32 build: 84 lines, 2946 bytes; 2026-11-11 the browser build under Node, local macOS, cold wasm32 build: 41 lines, 1476 bytes, 1 test (7 lines warm) | 86 lines, 3100 bytes | 1 |
| package | 4 lines, 207 bytes: one line per check | 20 lines, 1000 bytes, allowing a failure's assertion lines | Not counted: it prints no test count |
| fuzz | 10 lines, 538 bytes: one verdict per target | 40 lines, 4000 bytes, allowing an error line per target | A target's run fails below one input per second |
| scripts | 2026-11-11, six files: 23 lines, 1305 bytes, mostly test-fuzz-all.sh's line per check | 30 lines, 1700 bytes, room for two more files | Each file fails itself; `set -e` stops at the first |
| cli | 2026-11-11, three files: 15 lines, 666 bytes, three of them doctor's staged paths | 20 lines, 900 bytes, room for one more file | 60 of the 64 checks |
| semver | 2026-11-11, passing against main: 14 lines, 631 bytes, fixed rather than per item | 40 lines, 4000 bytes; a failing run lists every broken item and is not held to it | cargo-semver-checks prints its own check count |

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
`chore test` requires both; on a Mac it runs the Linux tiers in the guest
(`chore test:vm`) and the macOS oracle on the host. Missing tools and non-macOS
oracle invocation fail, naming `chore test:oracle-macos`. CI's `unit` (x86_64) and
`test-arm64` (aarch64) jobs run `chore test:unit` on Linux; `oracle-macos` runs it
and then `chore test:oracle-macos` on macOS. No decoder test is ignored.

Directory sets spanning FAT-linked clusters and contiguous subdirectories are
validated. Fresh images are populated through the macOS driver, then remounted
read-only for complete recursive path/size comparisons, including platform
metadata files. Canonical Unicode equivalence is used only in the oracle because
macOS readdir decomposes names; the reader preserves the stored UTF-16 name.
Every file length, SHA-256 and creation/modification/access timestamp is compared
with the read-only driver. The reduced-valid-length fixture retains a nonzero
allocated tail and uses a 4096-byte boundary, because the macOS driver rounds an
unaligned valid length up to the next sector boundary: of a 513-byte valid length
it kept 1024 original bytes at 512 bytes per sector and 4096 at 4096, and
fsck_exfat -n accepts both images. The specification requires zeros from the
valid length itself, so the reader differs from the driver here on purpose. An
oracle test exercises both on each reference image: the reader must be byte-exact,
and the driver must keep exactly 513 bytes or round as described, so a change in
the platform is told apart from a regression in the reader. Unknown UTC offsets remain unknown rather
than being converted using the host clock. Lookup uses the volume table without
host case folding or Unicode normalization. Paths are root-relative, with at
most 1024 nonempty components and 255 UTF-16 units per ordinary component.
Missing components are never cancelled lexically by a later parent component.
The C ABI and CLI are exercised on the same independent images. Complete ABI and
CLI listings and every file's SHA-256 match the mounted driver. The C compiler
also links the static library and reads a reference-created file on each sector
size. Check-only fsck agrees with the reference checker on those clean images;
scale tests additionally compare high-cluster cross-links, lost clusters and a
million-entry root with the platform checker. Synthetic tests cover the format
maximum, dirty state, resource refusal and injected I/O failure. The scale images
are sparse or streamed files under this worktree's ignored `tmp/`; see the
[audit memory contract](fsck-memory.md). CLI tests cover verified extraction,
source protection and repair refusal.

The writer is checked against the same platform. After each reference image
has been populated and checked, the oracle copies it, and `tests/oracle/write.rs`
changes the copy through `fs_exfat::write` and records the tree and every
file's SHA-256 as this crate's reader then sees it. The copy is attached
read-only for `fsck_exfat -n`, then mounted read-only, and the macOS driver's
listing must equal the record. The guard admits the written image only after
its producer has run, and only read-only attachments of it.

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

`chore package` verifies in `package-verify` under the current target directory.
Verification builds the packaged copy under the same crate hash as the working
tree's build, so in a shared directory a later test, after an edit to the library,
linked the packaged library and tested old code. `chore test:package` edits a copy
of the tree after `chore package` and fails unless a test links the edited library.
A bare `cargo package` still shares the directory, so use the task.

## The Linux oracles

rust-fs-ext4's test contract is the template, and fs-linux-test-harness the
guest: `chore siblings`, `chore tools` and `chore vm:*` set it up, and
`scripts/vm-setup.sh` provisions a Debian 12 guest with exfatprogs, the kernel's
exfat module and relan/exfat, built at the commit `tests/walk_support` pins.

`chore fixtures` formats fourteen images with `mkfs.exfat` and writes them through
the kernel's driver on a loop mount: an empty volume, the same tree at three
geometries, 32 MiB clusters on 4096-byte sectors, fragmented files and
directories, 30,000 entries in one directory, deleted files, a full volume, and
damaged copies of the tree and the fragmented image. Each image has a manifest
saying how it was made and the kernel's listing, every path, size, mode, time and
SHA-256, captured from a read-only mount in the guest; a damaged copy also has a
record of every byte range its damage changed, located by `dump.exfat` and the
kernel, never by this crate.

`chore test:images` reads them with no VM. `chore test:oracle` asks exfatprogs;
`fsck.exfat -n` cannot see file content, so the overwritten-data copy passes it and
shows in one file's hash instead. `chore test:kernel` mounts each image read-only
and compares the kernel's walk with the strict reader and with salvage, and
proves a dropped last cluster would be caught. `chore test:third` reads each image
three ways; relan/exfat's own departures from the specification (whole-second
times, a day late from 2100-03-01, no read-only attribute in its FUSE modes, no
up-case checksum in its checker) are listed exactly in `tests/third.rs`, and
anything else fails naming the path, the field and all three values.

Two deviation-report findings are expected on every kernel-made image and
allowed by rule, severity and offset only: mkfs.exfat 1.2.0 fills the OEM
Parameters sector's reserved bytes with FFh, and the kernel never updates
PercentInUse.

Windows formatting is additional evidence only if the Windows harness can
provide it.

## Fuzzing

Every public decoder has a cargo-fuzz target; `fuzz/README.md` lists them and says
why `DirectoryEntry` and `Stream` need none of their own. `chore fuzz -- SECONDS` is
the explorer: it runs on nightly, is not a required check, and fails on a crash, on
a target with no corpus, on a run that failed to start and on a run that executed
fewer inputs than it had seconds. `.github/workflows/fuzz.yml` runs it nightly and
keeps reproducers. `tests/scripts/test-fuzz-all.sh`, in `chore test:scripts`, proves
each of those verdicts against a stub on every pull request.

`tests/fuzz_decoders.rs` is the gate, in the unit tier: the same harness on the
pinned toolchain, deterministic, replaying and mutating the corpus. It fails when a
public `parse` or `decode` has no target and when no seed gets past a decoder's first
check. The seeds are cut from reference images by `tests/oracle/fuzz_seeds.rs`, which
the oracle tier runs. Fuzzing uses in-memory inputs only and needs no device.
