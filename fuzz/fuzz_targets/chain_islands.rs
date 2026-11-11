#![no_main]
//! primitives::chain_islands and compute_stats over an arbitrary FAT and bitmap.
use libfuzzer_sys::fuzz_target;

#[path = "../shared/mod.rs"]
mod shared;

fuzz_target!(|data: &[u8]| {
    let _ = shared::chain_islands(data);
});
