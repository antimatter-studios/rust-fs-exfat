#![no_main]
//! primitives::parse_set and find_sets, the salvage carver, with and without valid set checksums.
use libfuzzer_sys::fuzz_target;

#[path = "../shared/mod.rs"]
mod shared;

fuzz_target!(|data: &[u8]| {
    let _ = shared::entry_sets(data);
});
