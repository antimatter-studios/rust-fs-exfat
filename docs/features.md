# Features

Nothing is released yet, so the tables below still show the initial scaffold. The
current state of each row is a fragment under `features.d/`, and fragments are
applied to this page at release time.

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

## Interfaces

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| C ABI and command-line tools | Not implemented | | #1 | |

## Validation

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| macOS reference images and read-only differential | Not implemented | | #1 | |
