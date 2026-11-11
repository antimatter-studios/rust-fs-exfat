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

| Tier | Measured output | Budget | Executed floor |
|---|---|---|---|
| unit | 2026-11-11 compressed Up-case Table, cached target: 384 lines, 22750 bytes, 280 tests (before it: 378 lines, 22328 bytes, 274 tests) | 384 lines, 22822 bytes, allowing timing digits | 150 |
| oracle | 2026-11-10: 223 lines, 11070 bytes, 25 tests including large-volume and million-entry comparisons | 270 lines, 16000 bytes, allowing formatter/checker diagnostics | 25 |
| package | 4 lines, 207 bytes: one line per check | 20 lines, 1000 bytes, allowing a failure's assertion lines | Not counted: it prints no test count |
| fuzz | 10 lines, 538 bytes: one verdict per target | 40 lines, 4000 bytes, allowing an error line per target | A target's run fails below one input per second |

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

The Linux harness configuration used by other filesystem drivers is not copied:
there is no Linux guest oracle in the current milestone. Windows formatting is
additional evidence only if the Windows harness can provide it.

## Fuzzing

Every public decoder has a cargo-fuzz target; `fuzz/README.md` lists them and says
why `DirectoryEntry` and `Stream` need none of their own. `chore fuzz -- SECONDS` is
the explorer: it runs on nightly, is not a required check, and fails on a crash, on
a target with no corpus, on a run that failed to start and on a run that executed
fewer inputs than it had seconds. `.github/workflows/fuzz.yml` runs it nightly and
keeps reproducers. `scripts/test-fuzz-all.sh`, in `chore lint`, proves each of those
verdicts against a stub.

`tests/fuzz_decoders.rs` is the gate, in the unit tier: the same harness on the
pinned toolchain, deterministic, replaying and mutating the corpus. It fails when a
public `parse` or `decode` has no target and when no seed gets past a decoder's first
check. The seeds are cut from reference images by `tests/oracle/fuzz_seeds.rs`, which
the oracle tier runs. Fuzzing uses in-memory inputs only and needs no device.
