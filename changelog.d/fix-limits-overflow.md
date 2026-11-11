### Fixed

- `primitives::parse_set` and `find_sets` panicked in debug builds, and in release builds wrongly refused sets, when a set's checksum failed and the `Limits` given held a cluster count of `FFFFFFFEh` or more, or a cluster count and cluster size whose product passes `u64::MAX`; the plausibility filter now works out the heap's last cluster and its size without overflow, so every `Limits` value gets a defined answer (#112).
