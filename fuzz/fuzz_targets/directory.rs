#![no_main]
//! directory::parse, which is what builds every DirectoryEntry and Stream: neither has a decoder of its own.
use libfuzzer_sys::fuzz_target;

#[path = "../shared/mod.rs"]
mod shared;

fuzz_target!(|data: &[u8]| {
    let _ = shared::directory(data);
});
