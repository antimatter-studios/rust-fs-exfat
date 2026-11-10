# Check-only allocation audit

`fsck.exfat` and `check::check_device` audit the complete volume through a
`BlockRead` wrapped in `ReadOnlyDevice`. Neither API has a repair mode or a write
callback. The CLI opens its source read-only, including when using `--offset`.
An audit returns a report only after every directory and allocation has been
visited. Errors never return a partial report or a clean status.

## Memory budget

For a volume with `C` clusters, the audit permits this payload budget:

```text
2 * ceil(C / 8) + 512 MiB directory workspace + 1 MiB fixed buffers
```

The first term consists of the active on-disk allocation bitmap and an ownership
bitset: one bit per cluster each, or 1/4 byte per cluster together (rounded up
separately). The ownership bitset detects cross-links and FAT cycles, including
links above cluster 1,048,576. The FAT is never loaded into a `Vec<u32>`; a 64 KiB
byte cache reads only bounded chunks of the selected active FAT. Directory data
also uses a 64 KiB buffer, and an entry set occupies at most 8 KiB, including sets
split across clusters. No file-content data is needed.

`primitives::FatWindow` and `primitives::StreamCursor` own those fixed-size
buffers. `primitives::walk_allocation` exposes each position before reading its
FAT link and allocates no cycle set; the audit's existing ownership bitmap stops
cycles and cross-links before that read. Primitive entry framing and bitmap
operations share the same raw decoding layer as the strict metadata and recovery
APIs. Audit rejection order, error messages, directory traversal and budget
accounting remain in `check_audit.rs`; a primitive observation is not a clean
verdict.

The exFAT format maximum, `0xfffffff5` = 4,294,967,285 clusters, requires
536,870,911 bytes for each bitmap: 1,073,741,822 bytes together, two bytes less
than 1 GiB. Thus the default ceiling of **1,537 MiB** permits every valid cluster
count, including the maximum; cluster size and logical image size do not increase
the per-cluster cost. Allocator bookkeeping, thread stacks, executable/runtime
memory and caller-owned objects are outside this payload budget, so it is not
an RSS guarantee.

Directory names are folded using the volume upcase table, stored as compact
UTF-16 plus offset/length records, and sorted for exact duplicate detection.
Pending directories store only allocation descriptors, not directory contents
or cluster vectors. Their vector capacities share the 512 MiB workspace; capacity
growth is checked and fallible. There is no fixed entry-count cap. A volume whose
directory traversal needs more workspace refuses with
`allocation audit directory workspace exceeds memory budget`, not a partial
clean bill. The fixed allowance includes the bounded FAT/directory buffers,
entry-set decoding, boot metadata, and up to 128 KiB each for the on-disk and
decoded upcase tables.

The bitmap read is sized from the actual system-stream length, which must equal
`ceil(C / 8)`. Neither bitmap nor upcase loading uses the extraction reader's
64 MiB `max_metadata_bytes` limit. If the bitmap cannot fit a caller's budget,
the audit reports `allocation bitmap exceeds allocation audit memory budget`
(or identifies the ownership bitmap); failed allocation also returns a specific
unsupported-audit error. Invalid stream lengths are inconsistencies instead.

## Library and exit statuses

`check::check_device_with_budget(device, bytes)` can lower the payload allowance;
it cannot enlarge the geometry-dependent maximum above. `check::check(&volume)`
uses the same complete streaming audit, independently of `ReadLimits` chosen for
file extraction. Existing `Volume::open` metadata/extraction limits still apply
when constructing that object: use `check_device` to audit a large directory or
bitmap without first constructing an extraction `Volume`. Existing volume
allocations are caller-owned and additional to audit memory.

The CLI preserves exit statuses: 0 clean, 4 uncorrected inconsistencies, 8 I/O or
unsupported resource limits, and 16 usage. Budget refusal, allocation failure and
any read error abort the audit. Cross-links, invalid chains and malformed entries
abort with an inconsistency; unreferenced allocations and dirty/media-failure
flags produce an explicitly unhealthy report. Nothing clears those flags.

## Regression evidence

All scale images are sparse or streamed files under the worktree's ignored
`tmp/`, never real disks. Large-cluster fixtures expose the full logical volume
through `BlockRead`, backed only through the last system-metadata cluster; reads
of the unused tail return zeros and reads outside the volume fail. This keeps
the 32 MiB cluster-size regression's 32.125 TiB geometry and high-cluster file
without requiring a host filesystem that accepts files that large. CLI and
macOS oracle fixtures still use full-length sparse files. Tests cover the
format-maximum sparse heap and its nearly 512 MiB bitmap, long FAT-linked and
contiguous files, cross-links and lost
allocations above the former cap, a root exceeding 64 MiB with 1,048,577 files,
late corruption after the former entry cap, duplicate names across streamed sets,
exact bitmap/workspace budget refusal, and injected I/O failure.

The macOS oracle independently formats a large image with `newfs_exfat` and
checks it with `fsck_exfat -n`. Additional generated healthy and corrupted scale
images are compared with the same checker, including the million-entry root.
Every attachment comes from that image's own receipt and is detached afterward;
reference checking attaches the image read-only. The format-maximum case is
synthetic boundary evidence, not a macOS formatter claim.
