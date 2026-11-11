#![allow(dead_code)]

//! A writable in-memory device and a spec-built empty volume for the write
//! tests. The volume is laid out from the specification (Sections 2 to 7):
//! boot regions, one or two FATs, Allocation Bitmaps, a compressed Up-case
//! Table and a root directory. It is self-built, so it proves only that the
//! writer agrees with this crate's reader; `tests/oracle/write.rs` checks the
//! writer on volumes macOS formatted.

use fs_core::{BlockDevice, BlockRead};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Memory {
    pub bytes: Arc<Mutex<Vec<u8>>>,
    /// Writes left before every later write fails; `u64::MAX` never fails.
    pub writes_left: Arc<AtomicU64>,
    pub writable: bool,
    /// The only transfer alignment the device accepts, as a raw device with
    /// larger logical blocks does: anything else fails with InvalidInput.
    pub block: u64,
    /// Every read the device accepted: calls, then bytes.
    pub reads: Arc<(AtomicU64, AtomicU64)>,
}

impl Memory {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Arc::new(Mutex::new(bytes)),
            writes_left: Arc::new(AtomicU64::new(u64::MAX)),
            writable: true,
            block: 512,
            reads: Arc::new((AtomicU64::new(0), AtomicU64::new(0))),
        }
    }

    /// Read calls and bytes so far.
    pub fn read_count(&self) -> (u64, u64) {
        (
            self.reads.0.load(Ordering::SeqCst),
            self.reads.1.load(Ordering::SeqCst),
        )
    }

    /// The same device, refusing transfers not aligned to `block` bytes.
    pub fn with_block(mut self, block: u64) -> Self {
        self.block = block;
        self
    }

    fn aligned(&self, offset: u64, length: usize) -> fs_core::Result<()> {
        if !offset.is_multiple_of(self.block) || !(length as u64).is_multiple_of(self.block) {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput).into());
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.bytes.lock().unwrap().clone()
    }
}

impl BlockRead for Memory {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        assert_eq!(offset % 512, 0, "reads are whole sectors");
        assert_eq!(output.len() % 512, 0, "reads are whole sectors");
        self.aligned(offset, output.len())?;
        let bytes = self.bytes.lock().unwrap();
        let start = offset as usize;
        let source = bytes
            .get(start..start + output.len())
            .ok_or(fs_core::Error::OutOfBounds {
                offset,
                len: output.len() as u64,
                size: bytes.len() as u64,
            })?;
        output.copy_from_slice(source);
        self.reads.0.fetch_add(1, Ordering::SeqCst);
        self.reads
            .1
            .fetch_add(output.len() as u64, Ordering::SeqCst);
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.bytes.lock().unwrap().len() as u64
    }
}

impl BlockDevice for Memory {
    fn write_at(&self, offset: u64, data: &[u8]) -> fs_core::Result<()> {
        if !self.writable {
            return Err(fs_core::Error::ReadOnly);
        }
        self.aligned(offset, data.len())?;
        let left = self.writes_left.load(Ordering::SeqCst);
        if left == 0 {
            return Err(fs_core::Error::Custom("injected write failure".into()));
        }
        if left != u64::MAX {
            self.writes_left.store(left - 1, Ordering::SeqCst);
        }
        assert_eq!(offset % 512, 0, "writes are whole sectors");
        assert_eq!(data.len() % 512, 0, "writes are whole sectors");
        let mut bytes = self.bytes.lock().unwrap();
        let start = offset as usize;
        bytes
            .get_mut(start..start + data.len())
            .ok_or(fs_core::Error::OutOfBounds {
                offset,
                len: data.len() as u64,
                size: 0,
            })?
            .copy_from_slice(data);
        Ok(())
    }

    fn is_writable(&self) -> bool {
        self.writable
    }
}

pub struct Layout {
    pub sector_shift: u8,
    pub cluster_shift: u8,
    pub bytes: u64,
    pub fats: u8,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sector_shift: 9,
            cluster_shift: 3,
            bytes: 8 * 1024 * 1024,
            fats: 1,
        }
    }
}

pub struct Geometry {
    pub sector: usize,
    pub cluster: usize,
    pub fat_offset: usize,
    pub fat_length: usize,
    pub heap_offset: usize,
    pub cluster_count: u32,
    pub fats: u8,
    pub bitmaps: Vec<u32>,
    pub upcase: u32,
    pub root: u32,
}

impl Geometry {
    pub fn cluster_offset(&self, cluster: u32) -> usize {
        self.heap_offset * self.sector + (cluster as usize - 2) * self.cluster
    }

    pub fn fat(&self, image: &[u8], copy: usize, cluster: u32) -> u32 {
        let position =
            (self.fat_offset + copy * self.fat_length) * self.sector + cluster as usize * 4;
        u32::from_le_bytes(image[position..position + 4].try_into().unwrap())
    }

    pub fn allocated(&self, image: &[u8], copy: usize, cluster: u32) -> bool {
        let index = (cluster - 2) as usize;
        image[self.cluster_offset(self.bitmaps[copy]) + index / 8] & (1 << (index % 8)) != 0
    }

    pub fn fat_bytes<'a>(&self, image: &'a [u8], copy: usize) -> &'a [u8] {
        let start = (self.fat_offset + copy * self.fat_length) * self.sector;
        &image[start..start + (self.cluster_count as usize + 2) * 4]
    }

    pub fn bitmap_bytes<'a>(&self, image: &'a [u8], copy: usize) -> &'a [u8] {
        let start = self.cluster_offset(self.bitmaps[copy]);
        &image[start..start + (self.cluster_count as usize).div_ceil(8)]
    }
}

fn put16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// The Up-case Table compressed as Section 7.2.5.1 allows: identity for
/// everything except a-z, which map to A-Z.
pub fn upcase_table() -> Vec<u8> {
    let mut words: Vec<u16> = (0..97).collect();
    words.extend(65..=90);
    words.extend([0xffff, (65536 - 123) as u16]);
    words.into_iter().flat_map(u16::to_le_bytes).collect()
}

pub fn checksum32(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |sum, byte| {
        sum.rotate_right(1).wrapping_add(u32::from(*byte))
    })
}

pub fn seal_boot(region: &mut [u8], sector: usize) {
    let mut checksum = 0u32;
    for (index, byte) in region[..sector * 11].iter().enumerate() {
        if ![106, 107, 112].contains(&index) {
            checksum = checksum.rotate_right(1).wrapping_add(u32::from(*byte));
        }
    }
    for position in (sector * 11..sector * 12).step_by(4) {
        put32(region, position, checksum);
    }
}

/// An empty volume with the label "WRITER".
pub fn format(layout: &Layout) -> (Vec<u8>, Geometry) {
    let sector = 1usize << layout.sector_shift;
    let cluster = sector << layout.cluster_shift;
    let sectors = layout.bytes as usize / sector;
    let fat_offset = 32;
    let mut cluster_count = ((sectors - fat_offset) / (cluster / sector)) as u32;
    let (fat_length, heap_offset) = loop {
        let fat_length = ((cluster_count as usize + 2) * 4).div_ceil(sector);
        let heap = fat_offset + fat_length * usize::from(layout.fats);
        let heap = heap.div_ceil(cluster / sector) * (cluster / sector);
        let fits = ((sectors - heap) / (cluster / sector)) as u32;
        if fits >= cluster_count {
            break (fat_length, heap);
        }
        cluster_count = fits;
    };
    let bitmap_clusters = (cluster_count as usize).div_ceil(8).div_ceil(cluster) as u32;
    let mut next = 2;
    let mut bitmaps = Vec::new();
    for _ in 0..layout.fats {
        bitmaps.push(next);
        next += bitmap_clusters;
    }
    let upcase = next;
    let root = upcase + 1;
    let geometry = Geometry {
        sector,
        cluster,
        fat_offset,
        fat_length,
        heap_offset,
        cluster_count,
        fats: layout.fats,
        bitmaps: bitmaps.clone(),
        upcase,
        root,
    };
    let mut image = vec![0u8; layout.bytes as usize];
    let boot = &mut image[..sector];
    boot[..3].copy_from_slice(&[0xeb, 0x76, 0x90]);
    boot[3..11].copy_from_slice(b"EXFAT   ");
    put64(boot, 72, sectors as u64);
    put32(boot, 80, fat_offset as u32);
    put32(boot, 84, fat_length as u32);
    put32(boot, 88, heap_offset as u32);
    put32(boot, 92, cluster_count);
    put32(boot, 96, root);
    put32(boot, 100, 0x5eed_1234);
    put16(boot, 104, 0x0100);
    boot[108] = layout.sector_shift;
    boot[109] = layout.cluster_shift;
    boot[110] = layout.fats;
    boot[111] = 0x80;
    put16(boot, 510, 0xaa55);
    for index in 1..=8 {
        put32(&mut image, (index + 1) * sector - 4, 0xaa55_0000);
    }
    seal_boot(&mut image[..sector * 12], sector);
    image.copy_within(..sector * 12, sector * 12);
    let mut system = Vec::new();
    for first in &bitmaps {
        system.push((*first, bitmap_clusters));
    }
    system.push((upcase, 1));
    system.push((root, 1));
    for copy in 0..usize::from(layout.fats) {
        let base = (fat_offset + copy * fat_length) * sector;
        put32(&mut image, base, 0xffff_fff8);
        put32(&mut image, base + 4, 0xffff_ffff);
        for (first, count) in &system {
            for cluster in *first..first + count {
                let value = if cluster + 1 == first + count {
                    0xffff_ffff
                } else {
                    cluster + 1
                };
                put32(&mut image, base + cluster as usize * 4, value);
            }
        }
    }
    for copy in 0..usize::from(layout.fats) {
        for (first, count) in &system {
            for cluster in *first..first + count {
                let index = (cluster - 2) as usize;
                image[geometry.cluster_offset(bitmaps[copy]) + index / 8] |= 1 << (index % 8);
            }
        }
    }
    let table = upcase_table();
    let at = geometry.cluster_offset(upcase);
    image[at..at + table.len()].copy_from_slice(&table);
    let mut position = geometry.cluster_offset(root);
    image[position] = 0x83;
    image[position + 1] = 6;
    for (index, unit) in "WRITER".encode_utf16().enumerate() {
        put16(&mut image, position + 2 + index * 2, unit);
    }
    position += 32;
    for (copy, first) in bitmaps.iter().enumerate() {
        image[position] = 0x81;
        image[position + 1] = copy as u8;
        put32(&mut image, position + 20, *first);
        put64(
            &mut image,
            position + 24,
            (cluster_count as u64).div_ceil(8),
        );
        position += 32;
    }
    image[position] = 0x82;
    put32(&mut image, position + 4, checksum32(&table));
    put32(&mut image, position + 20, upcase);
    put64(&mut image, position + 24, table.len() as u64);
    (image, geometry)
}
