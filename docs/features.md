# Features

Nothing is released yet, so the tables below still show the initial scaffold. The
current state of each row is a fragment under `features.d/`, and fragments are
applied to this page at release time.

The unreleased Linux write gates are limited to mount state and allocation/free
cycles; see `features.d/test-linux-allocation-oracles.md`. File-data writing,
crash recovery and two-FAT interoperability are not established by those gates.
Interrupted writes are held to the block-cut contract in
`features.d/test-write-cut-matrix-188.md` (other files are safe), which claims
no atomicity, no repair mode and no TexFAT behaviour.

## Safety

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| Device writes | Forbidden by design; no write feature and strict APIs accept only BlockRead, wrapped in ReadOnlyDevice | Initial contract | #1 | `tests/read_only.rs`, `scripts/check-read-only.sh` |

## Reading

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| Strict filesystem reader | Not yet usable | | #1 | |

## Writing

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| Two-FAT writing (TexFAT interoperability) | Independently unverified; mirrored-copy writer checks remain synthetic; Sleuth Kit 4.11.1 lists mirrored copies but fails the ActiveFat-1 content and allocation controls; no suitable TexFAT-capable harness oracle or transactional/crash-recovery guarantee | | #154, #178 | Synthetic writer checks: `tests/write.rs`; [negative guest-only Sleuth Kit evaluation](sleuthkit-evaluation.md); [oracle review](texfat-validation.md) |

## Interfaces

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| C ABI and command-line tools | Not implemented | | #1 | |

## Validation

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| macOS reference images and read-only differential | Not implemented | | #1 | |
