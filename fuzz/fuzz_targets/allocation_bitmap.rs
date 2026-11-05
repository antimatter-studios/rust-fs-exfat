#![no_main]
//! AllocationBitmap::parse, with a cluster count that disagrees with the length and with one that agrees.
use libfuzzer_sys::fuzz_target;

#[path = "../shared/mod.rs"]
mod shared;

fuzz_target!(|data: &[u8]| {
    let _ = shared::allocation_bitmap(data);
});
