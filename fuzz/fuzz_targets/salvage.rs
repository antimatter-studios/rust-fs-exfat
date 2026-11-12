#![no_main]
//! Every salvage adapter verb and exFAT extension over a damaged synthetic volume with unreadable sectors.
use libfuzzer_sys::fuzz_target;

#[path = "../shared/mod.rs"]
mod shared;

fuzz_target!(|data: &[u8]| {
    let _ = shared::salvage(data);
});
