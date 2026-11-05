#![no_main]
//! A whole synthetic volume opened, walked, read and checked after patches to its metadata.
use libfuzzer_sys::fuzz_target;

#[path = "../shared/mod.rs"]
mod shared;

fuzz_target!(|data: &[u8]| {
    let _ = shared::image(data);
});
