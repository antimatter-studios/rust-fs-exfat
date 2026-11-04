# Feature fragments

Each behavior change adds a Markdown fragment named for its branch. Use a
`## Section` matching docs/features.md and rows with Feature, State, Since,
Tracking and Checked by columns. Since is `Unreleased (#N)` until release.
Run rust-fs-core's features-check.sh through `chore lint`.
