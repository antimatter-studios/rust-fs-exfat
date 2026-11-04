# Test contract

`chore build`, `chore fmt`, `chore lint`, `chore test` and `chore package` are
the local and CI gates. Rust 1.95.0 is pinned. The shared lint scripts need
Python 3 with PyYAML 6.0.3 and ripgrep. CI installs those prerequisites explicitly.
The dependency policy is deny.toml; the initial lockfile contains only the
MIT-licensed rust-fs-core 0.4.3 dependency.

Each test tier uses rust-fs-core's tier.sh, refuses skipped and ignored tests,
and checks a minimum executed count. Logs stay in tmp/logs/.

| Tier | Measured on 2026-11-04 | Budget | Executed floor |
|---|---|---|---|
| unit | 23 lines, 862 bytes on macOS arm64 | 100 lines, 8000 bytes, allowing clean-build paths and diagnostics | 4 |

The scaffold has no format decoder, reference image tests or CLI. The unit
tests do not satisfy the independent oracle acceptance criteria for Milestone
1. Formatter/checker and mounted-driver differential tests will run on macOS;
Linux host unit tests must not claim to have run those tests.

The Linux harness configuration used by other filesystem drivers is not copied:
there is no Linux guest oracle in the current milestone. Windows formatting is
additional evidence only if the Windows harness can provide it.
