## Validation

| macOS scale oracle detach safety | Ordinary receipt-owned detaches only; bounded retries retain image and receipt on failure; reviewed Rust image creation and lifecycle are guarded | Unreleased | #202 | `tests/oracle/scale.rs`, `scripts/check-image-safety.py`, `tests/scripts/test-image-safety.sh` |
| macOS oracle holder diagnostics | Exhausted shell retries name visible process holders of only the current receipt's block/raw nodes; holder lookup failure cannot turn detach failure into success | Unreleased | #200 | `tests/scripts/test-oracle-holders.sh`, `scripts/check-image-safety.py` |
