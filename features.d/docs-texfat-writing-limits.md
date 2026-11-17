## Writing

| Two-FAT writing (TexFAT interoperability) | Independently unverified; mirrored-copy writer checks remain synthetic; Sleuth Kit 4.11.1 lists mirrored copies but fails the ActiveFat-1 content and allocation controls; no suitable TexFAT-capable harness oracle or transactional/crash-recovery guarantee | Unreleased (#154, #178) | #154, #178 | Synthetic writer checks: `tests/write.rs`; [negative guest-only Sleuth Kit evaluation](../docs/sleuthkit-evaluation.md); [oracle review](../docs/texfat-validation.md) |
