#![no_main]
//! FatEntry::parse. Each value decides where a chain goes next, and a reserved value must never read as a cluster.
use libfuzzer_sys::fuzz_target;

#[path = "../shared/mod.rs"]
mod shared;

fuzz_target!(|data: &[u8]| {
    let _ = shared::fat_entry(data);
});
