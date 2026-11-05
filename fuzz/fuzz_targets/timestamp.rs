#![no_main]
//! Timestamp::decode on the three on-disk fields of one timestamp.
use libfuzzer_sys::fuzz_target;

#[path = "../shared/mod.rs"]
mod shared;

fuzz_target!(|data: &[u8]| {
    let _ = shared::timestamp(data);
});
