## Validation

| Decoder fuzz targets | Supported; every public decoder has a cargo-fuzz target over seeds cut from reference images, replayed on stable in every pull request and explored nightly | Unreleased | #1 | `tests/fuzz_decoders.rs`, `fuzz/fuzz_targets/`, `scripts/fuzz-all.sh`, `.github/workflows/fuzz.yml` |
