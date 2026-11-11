//! The browser build (wasm32-unknown-unknown), RUN under wasm-bindgen-test in
//! Node by `chore test:wasm`. std on that target compiles calls such as
//! `SystemTime::now` that panic when made, so only running the code finds
//! them. The test opens the sample volume the CLI and interface tests use
//! (tests/interface_support) from memory, walks every directory, reads every
//! file, and checks the result against the walk the native build makes of
//! the same bytes.
//!
//! The same walk runs as an ordinary test on every other target, so the
//! expected listing below is checked natively too.

mod interface_support;

use fs_core::BlockRead;
use fs_exfat::interface::open_device;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use wasm_bindgen_test::wasm_bindgen_test as test;

struct Memory(Vec<u8>);

impl BlockRead for Memory {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        let start = usize::try_from(offset).map_err(|_| fs_core::Error::Custom("offset".into()))?;
        let data = self
            .0
            .get(start..start + output.len())
            .ok_or(fs_core::Error::ShortRead {
                offset,
                want: output.len(),
                got: 0,
            })?;
        output.copy_from_slice(data);
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.0.len() as u64
    }
}

/// FNV-1a over a file's bytes: enough to tell two readings apart, and needs
/// nothing the browser build lacks.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Every path in the sample volume with its size and content hash, or "dir".
fn walk() -> Vec<String> {
    let volume =
        open_device(Memory(interface_support::image()), 0).expect("the sample volume opens");
    let mut out = Vec::new();
    let mut pending = vec![(String::new(), volume.root_directory().expect("root lists"))];
    while let Some((prefix, entries)) = pending.pop() {
        for entry in entries {
            let path = format!("{prefix}/{}", entry.name());
            if entry.is_directory() {
                out.push(format!("{path} dir"));
                let children = volume.read_directory(&entry).expect("directory lists");
                pending.push((path, children));
            } else {
                let file = volume.open_file(&entry).expect("file opens");
                let mut bytes = vec![0; usize::try_from(file.len()).expect("fits")];
                let mut done = 0;
                while done < bytes.len() {
                    let read = file
                        .read_at(done as u64, &mut bytes[done..])
                        .expect("file reads");
                    assert!(read > 0, "{path}: read stopped at {done}");
                    done += read;
                }
                out.push(format!("{path} {} {:016x}", bytes.len(), fnv1a(&bytes)));
            }
        }
    }
    out.sort();
    out
}

/// The native walk of tests/interface_support::image(). Example.txt holds 513
/// bytes of data and is 1030 long (ValidDataLength 513), so its hash also
/// covers the zeros read past ValidDataLength.
const EXPECTED: &[&str] = &[
    "/Empty.bin 0 cbf29ce484222325",
    "/Example.txt 1030 77e5a3ec3a620322",
    "/Sub dir",
    "/Sub/Child.bin 7 c061bc3d7219fe00",
];

#[test]
fn the_sample_volume_reads_the_same_on_every_target() {
    assert_eq!(walk(), EXPECTED);
}
