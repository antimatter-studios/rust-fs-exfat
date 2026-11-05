#![no_main]
//! UpcaseTable::parse. Run lengths expand towards the 65536 mappings every name is compared through.
use libfuzzer_sys::fuzz_target;

#[path = "../shared/mod.rs"]
mod shared;

fuzz_target!(|data: &[u8]| {
    let _ = shared::upcase_table(data);
});
