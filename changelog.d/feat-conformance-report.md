### Added

- `CheckReport::deviations` lists every place the bytes break a rule of the exFAT specification, each with its volume byte offset, length, specification section and severity, apart from the healthy verdict, the exit status and the strict accept or reject decision, which it never changes.
- `check::spec_deviations` lists the same deviations for any volume, including one the strict check rejects, and never fails: what it cannot read or hold is named in `skipped`.
- The fsck report's new `deviations` object holds the listed deviations, per-rule counts past the listing limit, and whether every part was checked.
- `docs/spec-rules.md` lists the rules, one per row with its section and severity, what the strict reader does with each, and what macOS `fsck_exfat` does.
