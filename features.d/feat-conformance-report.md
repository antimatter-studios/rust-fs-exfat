## Interfaces

| Spec deviations in check output | Experimental; `CheckReport::deviations`, `check::spec_deviations` and the fsck report's `deviations` object list every break of the 85 rules in `docs/spec-rules.md` with byte offset, length, section and severity, apart from the verdict, exit status and strict decision, which they never change; at most 64 are listed per rule and the rest counted; file data chains are left to the strict audit | Unreleased (#44) | #44 | `src/primitive_deviations.rs`, `src/check_deviations.rs`, `src/cli.rs`, `tests/check/deviations.rs`, `tests/cli.rs`, `tests/images.rs` |

## Validation

| Spec deviation platform differential | Supported on macOS; volumes newfs_exfat formatted and the macOS driver wrote report no deviation at both sector sizes | Unreleased (#44) | #44 | `tests/oracle/boot.rs` |
