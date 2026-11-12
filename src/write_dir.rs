//! Directory entry sets: finding them by path, encoding and rewriting them,
//! and making room for new ones.
//!
//! A file or directory is a File directory entry (Section 7.4), a Stream
//! Extension (Section 7.6) and one File Name entry per 15 UTF-16 units of its
//! name (Section 7.7), with a SetChecksum over all of them (Section 6.3.3)
//! and a NameHash over the up-cased name (Section 7.6.4). A directory is read
//! whole and checked by the strict parser before anything in it changes.

use super::{Allocation, WriteVolume};
use crate::directory::{self, DirectoryEntry, Located};
use crate::timestamp::{Timestamp, Timestamps};
use crate::{metadata, Error, Result};
use fs_core::BlockDevice;

const ENTRY: usize = 32;
const FILE: u8 = 0x85;
const STREAM: u8 = 0xc0;
const NAME: u8 = 0xc1;
const NAME_UNITS_PER_ENTRY: usize = 15;
/// Section 7.6.5: a directory's DataLength may not exceed 256 MiB.
const MAX_DIRECTORY_BYTES: u64 = 256 * 1024 * 1024;

pub(crate) const ATTRIBUTE_DIRECTORY: u16 = 0x10;
/// The attributes a caller may set (Section 7.4.4): ReadOnly, Hidden, System
/// and Archive.
pub const SETTABLE_ATTRIBUTES: u16 = 0x01 | 0x02 | 0x04 | 0x20;
pub const ATTRIBUTE_ARCHIVE: u16 = 0x20;

/// A directory's stream, read whole.
pub(crate) struct Dir {
    pub(crate) allocation: Allocation,
    pub(crate) clusters: Vec<u32>,
    pub(crate) bytes: Vec<u8>,
    /// The entry set recording this directory in its parent; None for the
    /// root directory, which has none.
    pub(crate) owner: Option<Set>,
}

impl Dir {
    pub(crate) fn is_root(&self) -> bool {
        self.owner.is_none()
    }
}

/// One file entry set: the directory stream that holds it, where, and its
/// bytes.
#[derive(Clone)]
pub(crate) struct Set {
    pub(crate) clusters: Vec<u32>,
    pub(crate) in_root: bool,
    pub(crate) offset: usize,
    pub(crate) raw: Vec<u8>,
}

/// What a path names: the directory that holds (or would hold) its last
/// component, that component, and its entry set when it exists.
pub(crate) struct Target {
    pub(crate) parent: Dir,
    pub(crate) name: Vec<u16>,
    pub(crate) found: Option<(Set, DirectoryEntry)>,
}

/// Checks a path component as a new exFAT file name (Section 7.7.3): 1 to
/// 255 UTF-16 units, none of them a control character or one of
/// `" * / : < > ? \ |`, and not `.` or `..`.
pub(crate) fn file_name(component: &str) -> Result<Vec<u16>> {
    let name: Vec<u16> = component.encode_utf16().collect();
    if name.is_empty() || name.len() > 255 {
        return Err(Error::Invalid("file name length"));
    }
    if name
        .iter()
        .any(|unit| *unit < 0x20 || [34, 42, 47, 58, 60, 62, 63, 92, 124].contains(unit))
    {
        return Err(Error::Invalid("file name character"));
    }
    if component == "." || component == ".." {
        return Err(Error::Invalid("reserved file name"));
    }
    Ok(name)
}

fn components(path: &str) -> Result<Vec<&str>> {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() > 1024 {
        return Err(Error::Unsupported("path exceeds 1024 components"));
    }
    if parts.iter().any(|part| matches!(*part, "." | "..")) {
        return Err(Error::Invalid("write paths take no . or .. components"));
    }
    Ok(parts)
}

fn put16(raw: &mut [u8], offset: usize, value: u16) {
    raw[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(raw: &mut [u8], offset: usize, value: u32) {
    raw[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(raw: &mut [u8], offset: usize, value: u64) {
    raw[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Which of the File entry's three timestamps (Sections 7.4.5 to 7.4.7).
#[derive(Clone, Copy)]
pub(crate) enum Stamp {
    Created,
    Modified,
    Accessed,
}

pub(crate) fn set_time(raw: &mut [u8], stamp: Stamp, time: &Timestamp) -> Result<()> {
    let (value, increment, offset) = time.encode()?;
    let (field, increment_field, offset_field) = match stamp {
        Stamp::Created => (8, Some(20), 22),
        Stamp::Modified => (12, Some(21), 23),
        Stamp::Accessed => (16, None, 24),
    };
    put32(raw, field, value);
    if let Some(position) = increment_field {
        raw[position] = increment;
    }
    raw[offset_field] = offset;
    Ok(())
}

pub(crate) fn set_times(raw: &mut [u8], times: &Timestamps) -> Result<()> {
    set_time(raw, Stamp::Created, &times.created)?;
    set_time(raw, Stamp::Modified, &times.modified)?;
    set_time(raw, Stamp::Accessed, &times.accessed)
}

pub(crate) fn attributes(raw: &[u8]) -> u16 {
    u16::from_le_bytes([raw[4], raw[5]])
}

pub(crate) fn set_attributes(raw: &mut [u8], value: u16) {
    put16(raw, 4, value);
}

/// Records a stream's clusters and lengths in the Stream Extension
/// (Section 7.6): AllocationPossible is always set, and NoFatChain only for
/// a non-empty contiguous allocation.
pub(crate) fn set_stream(raw: &mut [u8], allocation: &Allocation, valid: u64, length: u64) {
    let stream = ENTRY;
    let no_fat_chain = allocation.no_fat_chain() && !allocation.is_empty();
    raw[stream + 1] = 1 | if no_fat_chain { 2 } else { 0 };
    put64(raw, stream + 8, valid);
    put32(raw, stream + 20, allocation.first_cluster());
    put64(raw, stream + 24, length);
}

/// Recomputes SetChecksum over the whole set (Section 6.3.3).
pub(crate) fn seal(raw: &mut [u8]) {
    let checksum = metadata::set_checksum(raw);
    put16(raw, 2, checksum);
}

/// A new File, Stream Extension and File Name set for `name`, with an empty
/// stream.
pub(crate) fn new_set(
    name: &[u16],
    hash: u16,
    attributes: u16,
    times: &Timestamps,
) -> Result<Vec<u8>> {
    let names = name.len().div_ceil(NAME_UNITS_PER_ENTRY);
    let mut raw = vec![0u8; (2 + names) * ENTRY];
    raw[0] = FILE;
    raw[1] = (1 + names) as u8;
    set_attributes(&mut raw, attributes);
    set_times(&mut raw, times)?;
    raw[ENTRY] = STREAM;
    raw[ENTRY + 3] = name.len() as u8;
    put16(&mut raw, ENTRY + 4, hash);
    set_stream(&mut raw, &Allocation::EMPTY, 0, 0);
    for (index, chunk) in name.chunks(NAME_UNITS_PER_ENTRY).enumerate() {
        let entry = (2 + index) * ENTRY;
        raw[entry] = NAME;
        for (position, unit) in chunk.iter().enumerate() {
            put16(&mut raw, entry + 2 + position * 2, *unit);
        }
    }
    seal(&mut raw);
    Ok(raw)
}

/// The first run of `count` entries not in use (Section 6.2: InUse clear, or
/// the end-of-directory marker and everything after it).
fn free_run(bytes: &[u8], count: usize) -> Option<usize> {
    let mut start = 0;
    let mut length = 0;
    for (index, entry) in bytes.chunks_exact(ENTRY).enumerate() {
        if entry[0] & 0x80 == 0 {
            if length == 0 {
                start = index * ENTRY;
            }
            length += 1;
            if length == count {
                return Some(start);
            }
        } else {
            length = 0;
        }
    }
    None
}

impl<Device: BlockDevice> WriteVolume<Device> {
    pub(crate) fn read_clusters(&self, clusters: &[u32], length: u64) -> Result<Vec<u8>> {
        self.read_stream(clusters, length)
    }

    fn read_stream(&self, clusters: &[u32], length: u64) -> Result<Vec<u8>> {
        let length = usize::try_from(length).map_err(|_| Error::Bounds)?;
        let cluster_size = self.volume.boot().cluster_size() as usize;
        let mut bytes = vec![0; length];
        for (index, chunk) in bytes.chunks_mut(cluster_size).enumerate() {
            let cluster = *clusters
                .get(index)
                .ok_or(Error::Invalid("short directory chain"))?;
            self.cache
                .read(self.volume.boot().cluster_offset(cluster)?, chunk)?;
        }
        Ok(bytes)
    }

    pub(crate) fn root_dir(&self) -> Result<Dir> {
        let root = self.volume.boot().root_cluster;
        let clusters = self.fat_chain(root)?;
        let allocation = Allocation::from_parts(
            root,
            *clusters.last().expect("a chain holds its first cluster"),
            clusters.len() as u32,
            false,
        );
        let length = clusters.len() as u64 * self.volume.boot().cluster_size();
        let bytes = self.read_stream(&clusters, length)?;
        Ok(Dir {
            allocation,
            clusters,
            bytes,
            owner: None,
        })
    }

    /// The stream's allocation and clusters, checked against the FAT and
    /// the Allocation Bitmap.
    pub(crate) fn stream_allocation(
        &self,
        entry: &DirectoryEntry,
    ) -> Result<(Allocation, Vec<u32>)> {
        let stream = entry.stream();
        let count = stream
            .data_length()
            .div_ceil(self.volume.boot().cluster_size());
        let count = u32::try_from(count).map_err(|_| Error::Bounds)?;
        if count == 0 {
            return Ok((Allocation::EMPTY, Vec::new()));
        }
        let first = stream.first_cluster();
        let allocation = if stream.no_fat_chain() {
            let last = first
                .checked_add(count - 1)
                .ok_or(Error::Invalid("contiguous stream exceeds heap"))?;
            Allocation::from_parts(first, last, count, true)
        } else {
            let clusters = self.fat_chain(first)?;
            let last = *clusters.last().expect("a chain holds its first cluster");
            Allocation::from_parts(first, last, clusters.len() as u32, false)
        };
        if allocation.count() != count {
            return Err(Error::Invalid(
                "stream chain length differs from allocation",
            ));
        }
        let clusters = self.clusters(&allocation)?;
        Ok((allocation, clusters))
    }

    fn open_dir(&self, set: Set, entry: &DirectoryEntry) -> Result<Dir> {
        let (allocation, clusters) = self.stream_allocation(entry)?;
        let bytes = self.read_stream(&clusters, entry.stream().data_length())?;
        Ok(Dir {
            allocation,
            clusters,
            bytes,
            owner: Some(set),
        })
    }

    pub(crate) fn entries(&self, dir: &Dir) -> Result<Vec<Located>> {
        directory::parse_located(
            &dir.bytes,
            self.volume.boot(),
            self.volume.upcase(),
            dir.is_root(),
        )
    }

    fn find(&self, dir: &Dir, name: &[u16]) -> Result<Option<(Set, DirectoryEntry)>> {
        let upcase = self.volume.upcase();
        for located in self.entries(dir)? {
            if upcase.equal(located.entry.name_utf16(), name) {
                let set = Set {
                    clusters: dir.clusters.clone(),
                    in_root: dir.is_root(),
                    offset: located.offset,
                    raw: dir.bytes[located.offset..located.offset + located.length].to_vec(),
                };
                return Ok(Some((set, located.entry)));
            }
        }
        Ok(None)
    }

    /// Resolves every component of `path` but the last, which may be absent.
    pub(crate) fn resolve(&self, path: &str) -> Result<Target> {
        let parts = components(path)?;
        let (last, parents) = parts
            .split_last()
            .ok_or(Error::Invalid("the root directory has no entry set"))?;
        let mut dir = self.root_dir()?;
        for part in parents {
            let name: Vec<u16> = part.encode_utf16().collect();
            let (set, entry) = self.find(&dir, &name)?.ok_or(Error::NotFound)?;
            if !entry.is_directory() {
                return Err(Error::NotDirectory);
            }
            dir = self.open_dir(set, &entry)?;
        }
        let name = file_name(last)?;
        let found = self.find(&dir, &name)?;
        Ok(Target {
            parent: dir,
            name,
            found,
        })
    }

    /// Holds the set's bytes for writing at its place in its directory.
    pub(crate) fn write_set(&mut self, set: &Set) -> Result<()> {
        let cluster_size = self.volume.boot().cluster_size() as usize;
        let mut position = set.offset;
        let end = set.offset + set.raw.len();
        while position < end {
            let cluster = *set
                .clusters
                .get(position / cluster_size)
                .ok_or(Error::Invalid("entry set outside its directory"))?;
            let to = end.min((position / cluster_size + 1) * cluster_size);
            let offset =
                self.volume.boot().cluster_offset(cluster)? + (position % cluster_size) as u64;
            self.cache
                .write(offset, &set.raw[position - set.offset..to - set.offset])?;
            position = to;
        }
        if set.in_root {
            self.root_changed = true;
        }
        Ok(())
    }

    /// Adds a cluster of unused entries to the end of a directory and
    /// records the new length in its parent.
    fn grow(&mut self, dir: &mut Dir) -> Result<()> {
        let cluster_size = self.volume.boot().cluster_size();
        if dir.bytes.len() as u64 + cluster_size > MAX_DIRECTORY_BYTES {
            return Err(Error::NoSpace);
        }
        self.extend_clusters(&mut dir.allocation, 1)?;
        let added = dir.allocation.last_cluster();
        // A new directory cluster is all end-of-directory entries before
        // anything refers to it.
        self.cache.write_through(
            self.volume.boot().cluster_offset(added)?,
            &vec![0; cluster_size as usize],
        )?;
        self.barrier()?;
        dir.clusters = self.clusters(&dir.allocation)?;
        dir.bytes.resize(dir.bytes.len() + cluster_size as usize, 0);
        if let Some(owner) = &mut dir.owner {
            let length = dir.bytes.len() as u64;
            set_stream(&mut owner.raw, &dir.allocation, length, length);
            seal(&mut owner.raw);
            let owner = owner.clone();
            self.write_set(&owner)?;
            self.barrier()?;
        }
        Ok(())
    }

    /// Places a new set in the first free run of `dir`, growing it as needed.
    pub(crate) fn insert(&mut self, dir: &mut Dir, raw: Vec<u8>) -> Result<Set> {
        let count = raw.len() / ENTRY;
        let offset = loop {
            if let Some(offset) = free_run(&dir.bytes, count) {
                break offset;
            }
            self.grow(dir)?;
        };
        dir.bytes[offset..offset + raw.len()].copy_from_slice(&raw);
        let set = Set {
            clusters: dir.clusters.clone(),
            in_root: dir.is_root(),
            offset,
            raw,
        };
        self.write_set(&set)?;
        Ok(set)
    }
}
