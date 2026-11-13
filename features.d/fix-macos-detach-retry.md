## Validation

| macOS oracle detach retries | All shell-runner detach paths retry the receipt-owned image device at most five times with one-second delays; exhausted retries retain failure evidence even if EXIT cleanup succeeds | Unreleased (#176) | #176 | `tests/scripts/test-image-safety.sh`, `scripts/check-image-safety.py`, `chore test:oracle-macos` |
