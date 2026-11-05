#![no_main]
//! BootSector::parse, read before anything is known. Its shifts size every later read and its offsets place every later structure.
use libfuzzer_sys::fuzz_target;

#[path = "../shared/mod.rs"]
mod shared;

fuzz_target!(|data: &[u8]| {
    let _ = shared::boot_sector(data);
});
