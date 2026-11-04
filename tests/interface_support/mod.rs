#![allow(dead_code)]

#[path = "../support/mod.rs"]
pub mod base;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub fn image() -> Vec<u8> {
    let mut bytes = base::metadata_volume();
    let mut file = base::file_set("Example.txt", 0x20, 5, 1030, true);
    base::put64(&mut file, 40, 513);
    base::seal_set(&mut file);
    let empty = base::file_set("Empty.bin", 0x20, 0, 0, false);
    let directory = base::file_set("Sub", 0x10, 8, 512, true);
    let root = 32 * 512 + 96;
    for (index, entry) in [file, empty, directory].iter().enumerate() {
        bytes[root + index * 96..root + (index + 1) * 96].copy_from_slice(entry);
    }
    bytes[33 * 512] = 0xff;
    for (index, value) in bytes[35 * 512..38 * 512].iter_mut().enumerate() {
        *value = (index % 251) as u8;
    }
    let child = base::file_set("Child.bin", 0x20, 9, 7, true);
    bytes[38 * 512..38 * 512 + child.len()].copy_from_slice(&child);
    bytes[39 * 512..39 * 512 + 7].copy_from_slice(b"nested\n");
    bytes
}

pub fn expected() -> Vec<u8> {
    (0..1030)
        .map(|index| if index < 513 { (index % 251) as u8 } else { 0 })
        .collect()
}

pub struct Temp {
    pub directory: PathBuf,
    pub image: PathBuf,
}

impl Temp {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp");
        std::fs::create_dir_all(&root).unwrap();
        let directory = root.join(format!(
            "interface-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let image = directory.join("generated.img");
        std::fs::write(&image, image_bytes()).unwrap();
        Self { directory, image }
    }
}

fn image_bytes() -> Vec<u8> {
    image()
}

impl Drop for Temp {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}
