## Reading

| Uninterpreted directory entries | Experimental; an exFAT salvage extension lists every entry the driver does not interpret in the root and in every directory candidate's claim, Vendor Extension and Vendor Allocation entries and unknown benign or critical primary and secondary types, each with its source extent, directory, raw type, InUse bit and the entry set it lies in, that set's primary, validity as it stands and checksum with InUse restored; unreadable directory bytes are listed, never guessed; evidence is synthetic-only, because macOS drops such entries silently | Unreleased (#43) | #43 | `src/primitive_uninterpreted.rs`, `src/salvage_uninterpreted.rs`, `tests/primitive_cases/uninterpreted.rs`, `tests/salvage_cases/uninterpreted.rs` |

## Interfaces

| Uninterpreted entry counts in check output | Experimental; `CheckReport::uninterpreted` and the fsck report's `uninterpreted_entries` count the checked directories' vendor and unknown entries by kind, apart from the healthy verdict and exit status, which they never change; synthetic-only evidence | Unreleased (#43) | #43 | `src/check_audit.rs`, `src/cli.rs`, `tests/check/uninterpreted.rs`, `tests/cli.rs` |
