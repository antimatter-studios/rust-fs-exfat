#![no_main]
//! RootMetadata::parse. The root directory says where the bitmap and upcase streams are, before either has been read.
use libfuzzer_sys::fuzz_target;

#[path = "../shared/mod.rs"]
mod shared;

fuzz_target!(|data: &[u8]| {
    let _ = shared::root_metadata(data);
});
