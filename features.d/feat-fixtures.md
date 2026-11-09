## Interfaces

| Test-support image builders | Experimental; optional and off by default, pure in-memory encoders and four byte-preserved damaged-volume scenarios, without device writes or a formatter | Unreleased (#33) | #33 | `src/fixtures.rs`, `tests/fixtures.rs`, `tests/fixtures-captured.sha256` |

## Validation

| Fixture encoder platform differential | Supported on macOS; healthy boot checksums, FAT words, bitmap bits, upcase words/checksum, entry-set fields/dates and placement agree at both sector sizes, and reencoded healthy directories pass the checker; damaged scenarios and legacy omissions have synthetic-only compatibility evidence | Unreleased (#33) | #33 | `tests/oracle/fixtures.rs`, `scripts/test-oracle.sh` |
