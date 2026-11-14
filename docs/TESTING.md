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

## Shared Linux guest

The Linux tiers pin fs-linux-test-harness v0.7.0 and always join the host's
shared guest. No directory export is needed: each run attaches its own worktree
at `/repo` and its own share at `/share`. A tier or fixture build ends only its
own session; the guest stays up for subsequent runs and other consumers.
The lifecycle reaper handles unfinished boots and abandoned runs without
stopping another consumer's work. `chore vm:down` stops the guest only when
nobody is using it. Guest memory, CPUs, disk and idle deadline are host settings
(`FLTH_SHARED_MEMORY`, `FLTH_SHARED_CPUS`, `FLTH_SHARED_DISK` and
`FLTH_SHARED_DEADLINE`), not consumer `[vm]` settings.
A CI runner has no later run to share the guest with, so each CI job that
boots it ends with `chore vm:down` and then fails if `chore vm:status` still
finds it running.

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

The macOS job uses `tmp/target-worktree` as its Cargo target directory, so the
unit tier also checks the longer executable paths used in local worktrees.
For #182, on 2026-11-13, main `1a9477c` and stack `5952c46` each measured
562 lines / 33,461 bytes with 19-character worktree-local target paths and
all 402 tests passing. These paths add 325 bytes across 25 executable banners
compared with `target`, exceeding the old 33,300-byte cap. The stack's existing
measured 34,300-byte cap covers this case without another budget increase.

| Tier | Measured output | Budget | Executed floor |
|---|---|---|---|
| unit | 2026-11-13 (#178), 3ed71ee: 406 passing tests, 567 lines / 33768 bytes with three slow-test notices; earlier c60274f (#173), 402 tests: local macOS 564 lines / 33931 bytes, CI Linux 560 / 32902 and macOS 560 / 32903 | 570 lines, 34300 bytes; three lines above the 406-test measurement for timing diagnostics, with the byte cap retained | 150 |
| images | CI run 43105817721, ubuntu, cold build: 95 lines, 3328 bytes; 2026-11-11 (#122), local macOS, cold build: 53 lines, 2025 bytes, 7 tests over 14 fixtures; 14 lines warm | 98 lines, 3500 bytes | 7 |
| oracle | 2026-11-13 (#139), macOS host and HVF guest, incremental build including #129 and #130: 107 lines, 7943 bytes, 6 tests; the historical CI cold/warm difference adds 83 lines, 2721 bytes: 190 lines, 10664 bytes cold; the formatter alone adds 15 lines, 1475 bytes; 2026-11-14 (#185), the same host, release build with 39 Compiling lines: 149 lines, 9566 bytes, 7 tests; the write-kind checkpoints add 10 lines, 1135 bytes: 200 lines, 11799 bytes cold; 2026-11-14 (#188), the same host, warm release build with one Compiling line: 123 lines, 9379 bytes, 8 tests; the block-cut matrix adds 12 lines, 967 bytes: 212 lines, 12766 bytes cold | 220 lines, 13150 bytes | 8 |
| kernel | 2026-11-13 (#139), macOS host and HVF guest, incremental build including #129 and #130: 114 lines, 8718 bytes, 7 tests; the historical cold/warm difference adds 39 lines, 1232 bytes: 153 lines, 9950 bytes cold; the formatter alone adds 15 lines, 1639 bytes; 2026-11-14 (#186), the same host, release build with 1 Compiling line: 136 lines, 12319 bytes, 9 tests; the write matrix adds 24 lines, 3834 bytes: 177 lines, 13784 bytes cold | 186 lines, 14150 bytes | 9 |
| third | 2026-11-11 relan/exfat (#125), local macOS HVF guest, warm: 95 lines, 7084 bytes, 3 tests; a cold build adds about 38 lines, 1165 bytes | 175 lines, 10700 bytes | 3 |
| vm | 2026-11-11 the whole suite in the guest (#126), local macOS HVF guest, warm: 625 lines, 38742 bytes, 411 tests with the write layers; CI's fresh guest adds the toolchain install and a whole build, about 60 lines, 3.5 KB | 860 lines, 51000 bytes | 388 |
| oracle-macos | 2026-11-13 detach safety gates (#202, #200), local macOS: 318 lines, 14123 bytes, 50 tests. The change adds 13 lines: 9 image-safety verdicts, 3 detach regressions and 1 retained-evidence notice. A further 3 Compiling lines depend on the cache. The same Mac measured 302 lines before the change, and CI on main measured 303 lines, 12925 bytes at most. 2026-11-14 (#185), the same Mac: 379 lines, 16734 bytes, 52 tests; 30 write-kind checkpoint verdicts, their 30 detach notices and 2 test lines add 62 lines, 2611 bytes | 385 lines, 18700 bytes, allowing formatter/checker diagnostics | 25 |
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

The formatter's own tests read every layout back with the strict reader, its
check and its deviation report, which must all be clean; they prove only that
the formatter and the reader agree. `tests/oracle/mkfs.rs` is the independent
evidence. It formats sparse images of 1 MiB, 64 MiB, 300 MiB, 6 GiB and 40 GiB
at 512 and 4096 bytes per sector, then requires `fsck_exfat -n` to find each
clean, the macOS driver to mount it read-write and take files and a directory,
`fsck_exfat -n` to find it clean again, and the reader to return what the driver
wrote. It formats the same sizes with `newfs_exfat`: at the default alignment
only FatOffset, FatLength, ClusterHeapOffset and ClusterCount differ, with the
bitmap length and boot checksum that follow from them, because `newfs_exfat`
aligns to 64 KiB or more where this formatter aligns to the cluster size. At
`newfs_exfat`'s 64 KiB alignment a 64 MiB volume is byte-identical at both
sector sizes. On a two-FAT volume the only difference is FatEntry[3] in each
FAT, which `newfs_exfat` leaves at 0 and section 4.1.3 requires to be FFFFFFFFh.

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

The #130 write direction runs in the same `test:oracle` and `test:kernel`
tiers. It reuses `tests/guest_support/write_image.rs` to format fresh 64 MiB
volumes and seed a tree with the kernel at 512-byte sectors / 4 KiB clusters
and 4096-byte sectors / 128 KiB clusters. `tests/write_support/created.rs`
then mirrors `tests/oracle/write.rs`'s create/mkdir/pwrite/write_file tree,
checking that the kernel's seed remains unchanged. The entry count and append
size scale with the cluster size so the tests
require directory growth and FAT-chain fragmentation at both geometries.
The tree includes a 3 MiB file, non-ASCII and long names, zero-filled gaps,
replacement, an empty read-only file, and given file/directory times with
hundredths and UTC offsets. After a clean unmount, the guest's `fsck.exfat -n`
must accept it and the read-only Linux mount must agree with the strict
reader on every path, size, read-only attribute, mtime, atime and SHA-256.
GNU `stat` also checks every creation time through the kernel; an unavailable
birth time fails rather than weakening the comparison. Missing fixtures or
guest access fail the tiers. This does not cover the rest of #133: relan/exfat
in both directions, crash consistency, or write fuzzing.

Two deviation-report findings are expected on every kernel-made image and
allowed by rule, severity and offset only: mkfs.exfat 1.2.0 fills the OEM
Parameters sector's reserved bytes with FFh, and the kernel never updates
PercentInUse.

For #129, both Linux tiers also create fresh 64 MiB volumes in the guest, at
512-byte sectors with 4 KiB clusters and 4096-byte sectors with 128 KiB clusters.
`tests/guest_support/write_image.rs` formats them with exfatprogs and seeds a
directory and a multi-cluster sentinel file with the kernel, checks them, and
returns only detached images. This producer and its check-only fsck helper are
reusable by later write-direction tests. Missing guest tools fail naming
`chore vm:provision`; a missing harness or host prerequisite fails naming
`chore siblings` or `chore tools`.

`tests/allocation_support` mounts each image through the writer and exercises
NoFatChain growth, blocked growth into a FAT chain, shrinking back to a run,
allocation from isolated holes, a full heap and freeing everything. It checks
the persisted bitmap and FAT, VolumeDirty, PercentInUse and boot checksum while
mounted, and that the backup boot region is unchanged. After unmount, the FAT,
bitmap, directories and data must equal their original bytes. The oracle tier
then requires `fsck.exfat -n` to accept the result; the kernel tier requires it
too, then mounts read-only and compares the kernel's entire tree and hashes
before and after the allocation cycle, and against the strict reader.
Unattached allocations are freed before either final oracle runs. These tests
do not exercise file-data writing (#130), crash recovery (#133), or TexFAT;
two-FAT allocation still has synthetic-only evidence.

For #185, both checkers also see a clean, unmounted checkpoint after every
kind of write on its own. `tests/write_support/kinds.rs` runs each kind on a
mount of its own, in order: allocation, creation, truncation (grow past
cluster boundaries, shrink to an unaligned length, to zero, and a fragmented
chain shrunk into its first run, cut mid-chain and grown again), unlink,
rmdir, rename (case-only, across directories, replacing a file and an empty
directory), timestamps, attributes, and a volume label set to eleven
non-ASCII units and then cleared. After every unmount it checks VolumeDirty
is clear and the strict reader opens the volume, and it saves the image as a
checkpoint named for the kind. Whatever the image held before must read back
unchanged at the end. `chore test:oracle` runs it on the guest-formatted
images at both geometries above and gives every checkpoint to the guest's
`fsck.exfat -n` in one guest call per geometry. `chore test:oracle-macos` runs
it on a fresh copy of each macOS-populated reference volume, and
`scripts/test-oracle.sh` attaches every listed checkpoint read-only and
unmounted for `fsck_exfat -n`. A refused checkpoint fails the tier, which
retains the checkpoint image named for its kind. These are checker verdicts
on clean unmounts only: they establish nothing about crash recovery or
TexFAT interoperability.

For #186, `chore test:kernel` runs the same kinds as an explicit matrix in
both directions at both geometries. This crate to the kernel: after each kind
in `tests/write_support/kinds.rs`, the checkpoint must pass `fsck.exfat -n`,
and the kernel's read-only walk of every checkpoint, in one guest call per
geometry, must agree with the strict reader on the whole tree, every size,
SHA-256, ReadOnly flag, modified, accessed and created time. The kernel to
this crate: `tests/write_support/kernel_kinds.rs` makes the equivalent kinds
on a read-write loop mount in the guest, with kernel-written sentinel content,
and saves an unmounted checkpoint after each with the kernel's own listing and
created times. The strict reader must agree with that listing, must find the
original kernel-made sentinel unchanged, and must see what each kind was meant
to leave. Three platform differences are scoped exactly and named in the
evidence line. Linux 6.1 shows a file grown by `truncate` only up to its
ValidDataLength (#207), so those files, and only those, are compared with
that prefix view. rename(2) to a case-only variant of the same name leaves
the name unchanged on Linux, which the test asserts. The kernel has no label
or Hidden/System/Archive interface, and no ReadOnly bit on directories, so
exfatprogs' `exfatlabel` sets and clears the label in the kernel direction.
The label in the crate direction is left to the #185 checkers. Allocation
without an entry has no kernel interface. No crash safety or TexFAT behaviour
is inferred.

### Block-cut crash matrix (#188)

The writer's recovery contract, decided by the owner on #188 (option (b),
"other files are safe"): after a write is cut off at any block boundary,

- every file and directory the interrupted operation was not changing is
  intact: the same names, sizes, attributes, times and bytes as before the
  operation;
- the file or directory being changed may be left in its old state,
  partially updated, or missing, but must not corrupt anything else;
- the volume may be left dirty, and the only inconsistencies allowed are
  ones `fsck.exfat` repairs without touching unrelated files, such as
  clusters marked allocated that nothing references (leaked space).

Nothing more is claimed: no operation is atomic or transactional, nothing
is journalled, there is no production repair mode (this crate's
`fsck.exfat` stays check-only), and no TexFAT behaviour is established
(#154).

The model is a device that persists writes in the order the writer issues
them, one whole volume sector at a time, and loses power after some number
of sectors. `CutDevice` in `tests/write_support/cuts.rs` is that device: it
counts sectors rather than calls, so a multi-sector write is cut inside
itself, and it refuses every write after the cut. Reordering inside a
device between flushes, and a sector torn part-way, are not modelled.

`tests/write_support/cuts.rs` holds a bounded script of 22 steps, each one
writer operation on a mount of its own: allocate, extend and free; mkdir at
the root and nested; create an empty file; write_file three files; pwrite
to append, to overwrite in place and to grow a file into a FAT chain;
truncate to grow and to shrink; rename across directories, case-only,
replacing a file, and of a directory with contents; set times; set
attributes; set the label; unlink a file with clusters and an empty one;
and rmdir. One uncut run counts the sectors each step writes. Then, for
every cut position from zero sectors to all of them, the script is replayed
onto a fresh copy of a guest-formatted image (8 MiB, with the kernel-made
`/keep/sentinel.bin`) through a `CutDevice` that stops there; the replay
must stop inside the step the count predicts, after exactly that many
sectors. Before the interrupted step, the strict reader records a manifest
of every path the step does not change (the step's own paths and, for a
directory, everything under it are left out; their parents' own times may
change).

`chore test:oracle` runs the matrix at both geometries: 512-byte sectors
with 4 KiB clusters, and 4096-byte sectors with 128 KiB clusters. Every cut image crosses the share as the
sectors in which it differs from the cut before it, in one file,
`deltas.bin`, and `tests/guest_support/write_cuts.rs` rebuilds them in order
on the guest's disk. exfatprogs' `fsck.exfat -n` judges each image as the
cut left it; one it refuses is repaired on a private copy with
`fsck.exfat -y`, which must report success, and the repaired copy must then
pass `fsck.exfat -n`. The sectors the repair changed come back, and the
host reads the judged image (as cut, or repaired) with the strict reader:
every path in the manifest must be there with the same fields and bytes, no
other path may appear beside them, and this crate's allocation audit must
find no stream using a free or shared cluster. A cut at a step boundary,
and the uncut end, must pass `fsck.exfat -n` as they stand. A control
changes one byte of the sentinel's data in the final image and requires the
same check to refuse it, so the matrix cannot pass vacuously.

On 2026-11-14 the matrix judged 167 cut positions at 512-byte sectors and
335 at 4096-byte sectors, the uncut end included. `fsck.exfat -n` (exfatprogs 1.2.0) refused two at each
geometry, both inside a rename, where the new entry and the old one briefly
share clusters; `fsck.exfat -y` truncated the renamed entry, a path the
step was changing, and every other file was intact. 67 and 176 cuts leave
leaked clusters. exfatprogs 1.2.0 neither reports nor reclaims those, so
they are counted by this crate's audit, not by the checker. No cut broke
the contract. A cut that does is a writer ordering defect: it fails the
tier, names the step, the sectors of it written and the byte offset of the
first lost sector, and retains the fresh image, `deltas.bin`, the checker's
report and the first failing images beside them; it is filed red-first and
fixed in the write order, never by weakening the contract.

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
