#![allow(dead_code)]

use fs_core::BlockRead;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::{Deref, Range};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub const CHUNK: usize = 1 << 20;

pub struct Image {
    pub path: PathBuf,
    file: Mutex<File>,
    length: u64,
}

impl Image {
    pub fn open(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let file = File::open(path).unwrap();
        assert!(
            file.metadata().unwrap().is_file(),
            "{} is not an image file",
            path.display()
        );
        let length = file.metadata().unwrap().len();
        Self {
            path: path.to_owned(),
            file: Mutex::new(file),
            length,
        }
    }

    pub fn read(&self, range: Range<usize>) -> Vec<u8> {
        let length = range.end.checked_sub(range.start).unwrap();
        assert!(
            length <= CHUNK,
            "image range exceeds the 1 MiB buffer limit"
        );
        let mut bytes = vec![0; length];
        self.read_at(range.start as u64, &mut bytes).unwrap();
        bytes
    }

    pub fn byte(&self, offset: usize) -> u8 {
        self.read(offset..offset + 1)[0]
    }

    pub fn word(&self, offset: usize) -> u32 {
        u32::from_le_bytes(self.read(offset..offset + 4).try_into().unwrap())
    }

    pub fn patch(&self, offset: usize, bytes: &[u8]) {
        assert!((offset as u64).checked_add(bytes.len() as u64).unwrap() <= self.length);
        let mut file = OpenOptions::new().write(true).open(&self.path).unwrap();
        file.seek(SeekFrom::Start(offset as u64)).unwrap();
        file.write_all(bytes).unwrap();
    }
}

impl Clone for Image {
    fn clone(&self) -> Self {
        Self::open(&self.path)
    }
}

impl BlockRead for Image {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        let mut file = self.file.lock().unwrap();
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(output)?;
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.length
    }
}

pub fn sparse_copy(source: &Path, destination: &Path) {
    sparse_copy_device(&Image::open(source), destination);
}

pub fn sparse_copy_device(source: &dyn BlockRead, destination: &Path) {
    let mut destination = File::create_new(destination).unwrap();
    let length = source.size_bytes();
    let mut buffer = vec![0; CHUNK];
    let mut offset = 0;
    while offset < length {
        let count = (length - offset).min(CHUNK as u64) as usize;
        source.read_at(offset, &mut buffer[..count]).unwrap();
        for block in buffer[..count].chunks(4096) {
            if block.iter().any(|byte| *byte != 0) {
                destination.seek(SeekFrom::Start(offset)).unwrap();
                destination.write_all(block).unwrap();
            }
            offset += block.len() as u64;
        }
    }
    destination.set_len(length).unwrap();
    destination.sync_all().unwrap();
}

pub struct Snapshot(Image);

impl Snapshot {
    pub fn new(path: &Path) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let target = path.with_file_name(format!(
            ".image-snapshot-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        sparse_copy(path, &target);
        Self(Image::open(target))
    }

    #[track_caller]
    pub fn assert_unchanged(&self, path: &Path) {
        assert_equal(&self.path, path);
    }
}

impl Deref for Snapshot {
    type Target = Image;
    fn deref(&self) -> &Image {
        &self.0
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).unwrap();
    }
}

pub fn compare(left: impl Read, right: impl Read, left_len: u64, right_len: u64) -> io::Result<()> {
    compare_at(left, right, left_len, right_len, 0)
}

fn compare_at(
    mut left: impl Read,
    mut right: impl Read,
    left_len: u64,
    right_len: u64,
    start: u64,
) -> io::Result<()> {
    let mut left_buffer = vec![0; CHUNK];
    let mut right_buffer = vec![0; CHUNK];
    let common = left_len.min(right_len);
    let mut offset = 0;
    while offset < common {
        let count = (common - offset).min(CHUNK as u64) as usize;
        left.read_exact(&mut left_buffer[..count])?;
        right.read_exact(&mut right_buffer[..count])?;
        if left_buffer[..count] == right_buffer[..count] {
            offset += count as u64;
            continue;
        }
        if let Some(index) = left_buffer[..count]
            .iter()
            .zip(&right_buffer[..count])
            .position(|(left, right)| left != right)
        {
            return Err(io::Error::other(format!(
                "first differing byte at offset {} (lengths {} and {})",
                start + offset + index as u64,
                start + left_len,
                start + right_len
            )));
        }
        offset += count as u64;
    }
    if left_len != right_len {
        return Err(io::Error::other(format!(
            "first differing byte at offset {} (lengths {} and {})",
            start + common,
            start + left_len,
            start + right_len
        )));
    }
    Ok(())
}

#[track_caller]
pub fn assert_equal(left: &Path, right: &Path) {
    assert_equal_from(left, right, 0);
}

#[track_caller]
pub fn assert_equal_from(left: &Path, right: &Path, offset: u64) {
    let mut left_file = File::open(left).unwrap();
    let mut right_file = File::open(right).unwrap();
    let left_len = left_file.metadata().unwrap().len();
    let right_len = right_file.metadata().unwrap().len();
    assert!(offset <= left_len.min(right_len));
    left_file.seek(SeekFrom::Start(offset)).unwrap();
    right_file.seek(SeekFrom::Start(offset)).unwrap();
    compare_at(
        left_file,
        right_file,
        left_len - offset,
        right_len - offset,
        offset,
    )
    .unwrap_or_else(|error| {
        panic!(
            "{} versus {}: {error}; range starts at {offset}",
            left.display(),
            right.display()
        )
    });
}

#[track_caller]
pub fn assert_bytes(path: &Path, bytes: &[u8]) {
    let file = File::open(path).unwrap();
    let length = file.metadata().unwrap().len();
    compare(file, bytes, length, bytes.len() as u64).unwrap();
}

pub fn digest(path: &Path) -> String {
    let mut file = File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = vec![0; CHUNK];
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    format!("{:x}", hash.finalize())
}
