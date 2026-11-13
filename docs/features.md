# Features

Nothing is released yet, so the tables below still show the initial scaffold. The
current state of each row is a fragment under `features.d/`, and fragments are
applied to this page at release time.

The unreleased Linux write gates are limited to mount state and allocation/free
cycles; see `features.d/test-linux-allocation-oracles.md`. File-data writing,
crash recovery and two-FAT interoperability are not established by those gates.

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
| Two-FAT writing (TexFAT interoperability) | Independently unverified; mirroring both FATs and Allocation Bitmaps is checked only on specification-built images with this crate's own reader; macOS, the Linux kernel and exfatprogs do not implement TexFAT, and no TexFAT-capable harness oracle has been validated; no transactional or crash-recovery guarantee | | #154 | Synthetic only: `tests/write.rs`; [oracle review](texfat-validation.md) |

## Interfaces

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| C ABI and command-line tools | Not implemented | | #1 | |

## Validation

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| macOS reference images and read-only differential | Not implemented | | #1 | |
