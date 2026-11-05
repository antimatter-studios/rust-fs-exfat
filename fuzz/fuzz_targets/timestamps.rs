#![no_main]
//! Timestamps::parse on a file directory entry.
use libfuzzer_sys::fuzz_target;

#[path = "../shared/mod.rs"]
mod shared;

fuzz_target!(|data: &[u8]| {
    let _ = shared::timestamps(data);
});
