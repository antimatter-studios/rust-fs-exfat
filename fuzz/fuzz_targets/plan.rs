#![no_main]
//! Arbitrary materialization plans, as JSON or built from bytes, validated and materialized.
use libfuzzer_sys::fuzz_target;

#[path = "../shared/mod.rs"]
mod shared;

fuzz_target!(|data: &[u8]| {
    let _ = shared::plan(data);
});
