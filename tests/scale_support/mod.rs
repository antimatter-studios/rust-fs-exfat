#![allow(dead_code)]

use super::base;

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

pub const OLD_CAP: u32 = 1_048_576;
pub const MAX_CLUSTERS: u32 = 0xffff_fff5;

pub struct Image {
    pub directory: PathBuf,
    pub path: PathBuf,
    pub file: File,
    pub heap: u64,
    pub bitmap: u64,
    pub allocated: u64,
    logical_bytes: u64,
}

impl Image {
    pub fn sparse(clusters: u32, files: u32) -> Self {
        let image = Self::with_cluster_size(clusters, files, 512);
        image.file.set_len(image.logical_bytes).unwrap();
        image
    }

    pub fn with_cluster_size(clusters: u32, files: u32, cluster_size: u64) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "tmp/fsck-scale-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("generated.img");
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let fat_sectors = ((u64::from(clusters) + 2) * 4).div_ceil(512);
        let heap_sector = 24 + fat_sectors;
        let logical_bytes = heap_sector * 512 + u64::from(clusters) * cluster_size;
        let root_clusters = (64 + u64::from(files) * 96 + 32).div_ceil(cluster_size);
        let bitmap_first = 2 + root_clusters as u32;
        let bitmap_length = u64::from(clusters).div_ceil(8);
        let bitmap_clusters = bitmap_length.div_ceil(cluster_size);
        let upcase_first = bitmap_first + bitmap_clusters as u32;
        assert!(upcase_first < clusters + 2);
        file.set_len(heap_sector * 512 + u64::from(upcase_first - 1) * cluster_size)
            .unwrap();
        let mut boot = base::volume(9);
        base::put64(
            &mut boot,
            72,
            heap_sector + u64::from(clusters) * (cluster_size / 512),
        );
        base::put32(&mut boot, 84, fat_sectors as u32);
        base::put32(&mut boot, 88, heap_sector as u32);
        base::put32(&mut boot, 92, clusters);
        boot[109] = (cluster_size / 512).ilog2() as u8;
        base::seal_boot(&mut boot, 512);
        boot.copy_within(..12 * 512, 12 * 512);
        file.write_all_at(&boot[..24 * 512], 0).unwrap();
        let heap = heap_sector * 512;
        let bitmap = heap + u64::from(bitmap_first - 2) * cluster_size;
        let mut root = [0; 64];
        root[0] = 0x81;
        base::put32(&mut root, 20, bitmap_first);
        base::put64(&mut root, 24, bitmap_length);
        let upcase = base::upcase_bytes();
        root[32] = 0x82;
        base::put32(&mut root, 36, fs_exfat::metadata::checksum32(&upcase));
        base::put32(&mut root, 52, upcase_first);
        base::put64(&mut root, 56, upcase.len() as u64);
        file.write_all_at(&root, heap).unwrap();
        file.write_all_at(&upcase, heap + u64::from(upcase_first - 2) * cluster_size)
            .unwrap();
        file.seek(SeekFrom::Start(24 * 512)).unwrap();
        let mut writer = BufWriter::with_capacity(64 * 1024, &mut file);
        for cluster in 0..=upcase_first {
            let value = match cluster {
                0 => 0xffff_fff8,
                1 => 0xffff_ffff,
                current
                    if current == bitmap_first - 1
                        || current == upcase_first - 1
                        || current == upcase_first =>
                {
                    0xffff_ffff
                }
                current => current + 1,
            };
            writer.write_all(&value.to_le_bytes()).unwrap();
        }
        writer.flush().unwrap();
        drop(writer);
        let allocated = u64::from(upcase_first - 1);
        file.seek(SeekFrom::Start(bitmap)).unwrap();
        let full_bytes = allocated / 8;
        let chunk = [0xff; 64 * 1024];
        let mut remaining = full_bytes;
        while remaining != 0 {
            let count = remaining.min(chunk.len() as u64) as usize;
            file.write_all(&chunk[..count]).unwrap();
            remaining -= count as u64;
        }
        if allocated % 8 != 0 {
            file.write_all(&[(1u8 << (allocated % 8)) - 1]).unwrap();
        }
        file.seek(SeekFrom::Start(heap + 64)).unwrap();
        let mut writer = BufWriter::with_capacity(64 * 1024, &mut file);
        let mut entry = base::file_set("f0000000", 0x20, 0, 0, false);
        for index in 0..files {
            let name = format!("f{index:07x}");
            let mut hash = 0u16;
            for (position, byte) in name.bytes().enumerate() {
                base::put16(&mut entry, 66 + position * 2, u16::from(byte));
                for folded in [byte.to_ascii_uppercase(), 0] {
                    hash = hash.rotate_right(1).wrapping_add(u16::from(folded));
                }
            }
            base::put16(&mut entry, 36, hash);
            base::seal_set(&mut entry);
            writer.write_all(&entry).unwrap();
        }
        writer.flush().unwrap();
        drop(writer);
        Self {
            directory,
            path,
            file,
            heap,
            bitmap,
            allocated,
            logical_bytes,
        }
    }

    pub fn allocate(&self, cluster: u32) {
        let position = self.bitmap + u64::from(cluster - 2) / 8;
        let mut byte = [0];
        self.file.read_exact_at(&mut byte, position).unwrap();
        byte[0] |= 1 << ((cluster - 2) % 8);
        self.file.write_all_at(&byte, position).unwrap();
    }

    pub fn high_file(&self, slot: u64, cluster: u32) {
        self.allocate(cluster);
        self.file
            .write_all_at(
                &base::file_set(&format!("high{slot}"), 0x20, cluster, 512, true),
                self.heap + 64 + slot * 96,
            )
            .unwrap();
    }

    pub fn long_file(&self, count: u32, contiguous: bool) {
        let first = self.allocated as u32 + 2;
        let last = first + count - 1;
        let mut file = self.file.try_clone().unwrap();
        if !contiguous {
            file.seek(SeekFrom::Start(24 * 512 + u64::from(first) * 4))
                .unwrap();
            let mut writer = BufWriter::with_capacity(64 * 1024, &mut file);
            for cluster in first..=last {
                let value = if cluster == last {
                    u32::MAX
                } else {
                    cluster + 1
                };
                writer.write_all(&value.to_le_bytes()).unwrap();
            }
            writer.flush().unwrap();
        }
        let allocated = u64::from(last - 1);
        file.seek(SeekFrom::Start(self.bitmap)).unwrap();
        let chunk = [0xff; 64 * 1024];
        let mut remaining = allocated / 8;
        while remaining != 0 {
            let length = remaining.min(chunk.len() as u64) as usize;
            file.write_all(&chunk[..length]).unwrap();
            remaining -= length as u64;
        }
        if allocated % 8 != 0 {
            file.write_all(&[(1u8 << (allocated % 8)) - 1]).unwrap();
        }
        self.file
            .write_all_at(
                &base::file_set("long", 0x20, first, u64::from(count) * 512, contiguous),
                self.heap + 64,
            )
            .unwrap();
    }

    pub fn check(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_rust-fs-exfat"))
            .arg("fsck")
            .arg(&self.path)
            .arg("-n")
            .output()
            .unwrap()
    }
}

impl fs_core::BlockRead for Image {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        if offset
            .checked_add(output.len() as u64)
            .is_none_or(|end| end > self.logical_bytes)
        {
            return Err(fs_core::Error::OutOfBounds {
                offset,
                len: output.len() as u64,
                size: self.logical_bytes,
            });
        }
        let available = self.file.metadata()?.len().saturating_sub(offset);
        let length = available.min(output.len() as u64) as usize;
        output.fill(0);
        if length != 0 {
            self.file.read_exact_at(&mut output[..length], offset)?;
        }
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.logical_bytes
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        if std::fs::read_to_string(self.directory.join("attachment-state"))
            .is_ok_and(|state| state == "attached")
        {
            eprintln!(
                "scale oracle: attached image and receipt retained in {}",
                self.directory.display()
            );
            return;
        }
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

pub fn clean(output: Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "status {:?}: {} {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["healthy"], true);
    report
}
