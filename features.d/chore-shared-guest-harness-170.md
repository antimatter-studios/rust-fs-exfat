## Validation

| Shared Linux guest lifecycle | Linux tiers pin fs-linux-test-harness v0.7.0, keep per-run sessions, and leave the shared guest running; a script contract checks the pin, host-owned resource configuration and non-destructive fixture and `test:vm` timeout cleanup; fixtures, `test:oracle`, `test:kernel`, `test:third` and `test:vm` pass on a v0.7.0 shared guest | Unreleased (#170) | #170 | `tests/scripts/test-shared-guest.sh`, `chore test:scripts`, `chore fixtures`, `chore test:oracle`, `chore test:kernel`, `chore test:third`, `chore test:vm` |
