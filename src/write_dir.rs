//! Directory entry sets: finding them by path, encoding and rewriting them,
//! and making room for new ones.
//!
//! A file or directory is a File directory entry (Section 7.4), a Stream
//! Extension (Section 7.6) and one File Name entry per 15 UTF-16 units of its
//! name (Section 7.7), with a SetChecksum over all of them (Section 6.3.3)
//! and a NameHash over the up-cased name (Section 7.6.4). A directory is read
//! whole and checked by the strict parser before anything in it changes.
//!
//! # The directory cache
//!
//! A directory read and checked once is kept in a [`DirCache`], keyed by its
//! first cluster, with an index from each up-cased name to its entry set and
//! the offset before which every entry is in use. Every write into a
//! directory goes through [`WriteVolume::write_set`] or
//! [`WriteVolume::insert`], which update the cached bytes and the index with
//! the bytes they hold for the device, so a path resolves without reading or
//! parsing a directory again and a new set finds its place without scanning
//! the entries before it. A directory that is removed leaves the cache, and a
//! new one replaces whatever its first cluster held. The root directory's
//! bytes in the strict reader are patched the same way.

use super::{Allocation, WriteVolume};
use crate::directory::{self, DirectoryEntry};
use crate::metadata::UpcaseTable;
use crate::timestamp::{Timestamp, Timestamps};
use crate::{metadata, Error, Result};
use fs_core::BlockDevice;
use std::collections::HashMap;

const ENTRY: usize = 32;
const FILE: u8 = 0x85;
const STREAM: u8 = 0xc0;
const NAME: u8 = 0xc1;
const NAME_UNITS_PER_ENTRY: usize = 15;
/// Section 7.6.5: a directory's DataLength may not exceed 256 MiB.
const MAX_DIRECTORY_BYTES: u64 = 256 * 1024 * 1024;
/// The most directory bytes the cache holds before it is emptied, all but
/// the root, between operations.
const DIR_CACHE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) const ATTRIBUTE_DIRECTORY: u16 = 0x10;
/// The attributes a caller may set (Section 7.4.4): ReadOnly, Hidden, System
/// and Archive.
pub const SETTABLE_ATTRIBUTES: u16 = 0x01 | 0x02 | 0x04 | 0x20;
pub const ATTRIBUTE_ARCHIVE: u16 = 0x20;

/// A directory's stream, read whole and checked, with an index of its names.
pub(crate) struct CachedDir {
    allocation: Allocation,
    bytes: Vec<u8>,
    root: bool,
    /// Each name in use, up-cased through the volume's table, and the offset
    /// of its entry set.
    names: HashMap<Vec<u16>, usize>,
    /// No entry before this offset is free.
    free_from: usize,
}

impl CachedDir {
    fn new(
        allocation: Allocation,
        bytes: Vec<u8>,
        root: bool,
        upcase: &UpcaseTable,
        boot: &crate::boot::BootSector,
    ) -> Result<Self> {
        let names = directory::parse_located(&bytes, boot, upcase, root)?
            .into_iter()
            .map(|located| (fold(upcase, located.entry.name_utf16()), located.offset))
            .collect();
        let free_from = bytes
            .chunks_exact(ENTRY)
            .position(|entry| entry[0] & 0x80 == 0)
            .map_or(bytes.len(), |index| index * ENTRY);
        Ok(Self {
            allocation,
            bytes,
            root,
            names,
            free_from,
        })
    }

    /// The set starting at `offset`: the File entry and its secondaries.
    fn set_at(&self, offset: usize) -> &[u8] {
        let length = (1 + usize::from(self.bytes[offset + 1])) * ENTRY;
        &self.bytes[offset..offset + length]
    }
}

/// The directories this mount has read and checked.
#[derive(Default)]
pub(crate) struct DirCache {
    dirs: HashMap<u32, CachedDir>,
    bytes: usize,
}

impl DirCache {
    fn insert(&mut self, first: u32, dir: CachedDir) {
        self.bytes += dir.bytes.len();
        if let Some(old) = self.dirs.insert(first, dir) {
            self.bytes -= old.bytes.len();
        }
    }

    pub(crate) fn remove(&mut self, first: u32) {
        if let Some(old) = self.dirs.remove(&first) {
            self.bytes -= old.bytes.len();
        }
    }

    /// Empties the cache, all but the root, when it holds more than its
    /// limit. Only called between operations.
    pub(crate) fn trim(&mut self) {
        if self.bytes > DIR_CACHE_BYTES {
            self.dirs.retain(|_, dir| dir.root);
            self.bytes = self.dirs.values().map(|dir| dir.bytes.len()).sum();
        }
    }
}

/// A name up-cased through the volume's table, as names are compared.
fn fold(upcase: &UpcaseTable, name: &[u16]) -> Vec<u16> {
    name.iter().map(|unit| upcase.map(*unit)).collect()
}

/// The name a File entry set in use holds (Section 7.7.3).
fn set_name(raw: &[u8]) -> Vec<u16> {
    let length = usize::from(raw[ENTRY + 3]);
    raw[2 * ENTRY..]
        .chunks_exact(ENTRY)
        .take(length.div_ceil(NAME_UNITS_PER_ENTRY))
        .flat_map(|entry| {
            entry[2..]
                .chunks_exact(2)
                .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
        })
        .take(length)
        .collect()
}

/// A directory: the key of its cached stream, and the entry set that records
/// it in its parent.
pub(crate) struct Dir {
    /// The directory's first cluster.
    pub(crate) first: u32,
    /// The entry set recording this directory in its parent; None for the
    /// root directory, which has none.
    pub(crate) owner: Option<Set>,
}

/// One file entry set: the directory that holds it, where, and its bytes.
#[derive(Clone)]
pub(crate) struct Set {
    /// The first cluster of the directory that holds the set.
    pub(crate) dir: u32,
    pub(crate) offset: usize,
    pub(crate) raw: Vec<u8>,
}

/// What a path names: the directory that holds (or would hold) its last
/// component, that component, and its entry set when it exists.
pub(crate) struct Target {
    pub(crate) parent: Dir,
    pub(crate) name: Vec<u16>,
    pub(crate) found: Option<(Set, DirectoryEntry)>,
    /// The first cluster of every directory from the root to the parent.
    pub(crate) ancestors: Vec<u32>,
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

/// The same set with a new name: its File Name entries replaced, its
/// SecondaryCount, NameLength and NameHash updated, and every other
/// secondary entry kept after the names.
pub(crate) fn renamed(raw: &[u8], name: &[u16], hash: u16) -> Vec<u8> {
    let old_names = usize::from(raw[ENTRY + 3]).div_ceil(NAME_UNITS_PER_ENTRY);
    let others = &raw[(2 + old_names) * ENTRY..];
    let names = name.len().div_ceil(NAME_UNITS_PER_ENTRY);
    let mut set = Vec::with_capacity((2 + names) * ENTRY + others.len());
    set.extend_from_slice(&raw[..2 * ENTRY]);
    set[1] = (1 + names + others.len() / ENTRY) as u8;
    set[ENTRY + 3] = name.len() as u8;
    put16(&mut set, ENTRY + 4, hash);
    for chunk in name.chunks(NAME_UNITS_PER_ENTRY) {
        let mut entry = [0u8; ENTRY];
        entry[0] = NAME;
        for (position, unit) in chunk.iter().enumerate() {
            put16(&mut entry, 2 + position * 2, *unit);
        }
        set.extend_from_slice(&entry);
    }
    set.extend_from_slice(others);
    seal(&mut set);
    set
}

/// The first run of `count` entries not in use at or after `from` (Section
/// 6.2: InUse clear, or the end-of-directory marker and everything after it).
fn free_run(bytes: &[u8], from: usize, count: usize) -> Option<usize> {
    let mut start = from;
    let mut length = 0;
    for (index, entry) in bytes[from..].chunks_exact(ENTRY).enumerate() {
        let index = index + from / ENTRY;
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

    /// The root directory, cached from the strict reader's copy, which the
    /// mount checked.
    pub(crate) fn root_dir(&mut self) -> Result<Dir> {
        let root = self.volume.boot().root_cluster;
        if !self.dirs.dirs.contains_key(&root) {
            let clusters = self.fat_chain(root)?;
            let allocation = Allocation::from_parts(
                root,
                *clusters.last().expect("a chain holds its first cluster"),
                clusters.len() as u32,
                false,
            );
            self.load_chain(&allocation)?;
            let bytes = self.volume.root.clone();
            if bytes.len() as u64
                != u64::from(allocation.count()) * self.volume.boot().cluster_size()
            {
                return Err(Error::Invalid(
                    "root directory length differs from its chain",
                ));
            }
            let dir = CachedDir::new(
                allocation,
                bytes,
                true,
                self.volume.upcase(),
                self.volume.boot(),
            )?;
            self.dirs.insert(root, dir);
        }
        Ok(Dir {
            first: root,
            owner: None,
        })
    }

    /// The stream's allocation, with its clusters checked against the FAT
    /// and the Allocation Bitmap and loaded into the chain cache.
    pub(crate) fn stream_allocation(&mut self, entry: &DirectoryEntry) -> Result<Allocation> {
        let stream = entry.stream();
        let count = stream
            .data_length()
            .div_ceil(self.volume.boot().cluster_size());
        let count = u32::try_from(count).map_err(|_| Error::Bounds)?;
        if count == 0 {
            return Ok(Allocation::EMPTY);
        }
        let first = stream.first_cluster();
        let allocation = if stream.no_fat_chain() {
            let last = first
                .checked_add(count - 1)
                .ok_or(Error::Invalid("contiguous stream exceeds heap"))?;
            Allocation::from_parts(first, last, count, true)
        } else if let Some(cached) = self.cached_chain(first, count) {
            cached
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
        self.load_chain(&allocation)?;
        Ok(allocation)
    }

    /// A directory, from the cache or read and checked by the strict parser.
    fn open_dir(&mut self, set: Set, entry: &DirectoryEntry) -> Result<Dir> {
        let first = entry.stream().first_cluster();
        let length = entry.stream().data_length();
        let cached = self
            .dirs
            .dirs
            .get(&first)
            .is_some_and(|dir| !dir.root && dir.bytes.len() as u64 == length);
        if !cached {
            let allocation = self.stream_allocation(entry)?;
            let bytes = self.read_stream(self.chain(&allocation)?, length)?;
            let dir = CachedDir::new(
                allocation,
                bytes,
                false,
                self.volume.upcase(),
                self.volume.boot(),
            )?;
            self.dirs.insert(first, dir);
        }
        Ok(Dir {
            first,
            owner: Some(set),
        })
    }

    fn cached(&self, first: u32) -> Result<&CachedDir> {
        self.dirs
            .dirs
            .get(&first)
            .ok_or(Error::Invalid("directory is not loaded"))
    }

    /// Whether a directory holds no entry in use before its end marker.
    pub(crate) fn dir_is_empty(&mut self, set: Set, entry: &DirectoryEntry) -> Result<bool> {
        let dir = self.open_dir(set, entry)?;
        Ok(!self
            .cached(dir.first)?
            .bytes
            .chunks_exact(ENTRY)
            .take_while(|entry| entry[0] != 0)
            .any(|entry| entry[0] & 0x80 != 0))
    }

    /// Whether a root directory entry of `kind` is in use, and where.
    pub(crate) fn root_entry(&mut self, kind: u8) -> Result<(Dir, Option<usize>)> {
        let root = self.root_dir()?;
        let offset = self
            .cached(root.first)?
            .bytes
            .chunks_exact(ENTRY)
            .take_while(|candidate| candidate[0] != 0)
            .position(|candidate| candidate[0] == kind)
            .map(|index| index * ENTRY);
        Ok((root, offset))
    }

    fn find(&self, dir: &Dir, name: &[u16]) -> Result<Option<(Set, DirectoryEntry)>> {
        let cached = self.cached(dir.first)?;
        let Some(&offset) = cached.names.get(&fold(self.volume.upcase(), name)) else {
            return Ok(None);
        };
        let raw = cached.set_at(offset);
        let entry = directory::parse_located(raw, self.volume.boot(), self.volume.upcase(), false)?
            .into_iter()
            .next()
            .ok_or(Error::Invalid("indexed entry set is not in use"))?
            .entry;
        let set = Set {
            dir: dir.first,
            offset,
            raw: raw.to_vec(),
        };
        Ok(Some((set, entry)))
    }

    /// Resolves every component of `path` but the last, which may be absent.
    pub(crate) fn resolve(&mut self, path: &str) -> Result<Target> {
        let parts = components(path)?;
        let (last, parents) = parts
            .split_last()
            .ok_or(Error::Invalid("the root directory has no entry set"))?;
        let mut dir = self.root_dir()?;
        let mut ancestors = vec![self.volume.boot().root_cluster];
        for part in parents {
            let name: Vec<u16> = part.encode_utf16().collect();
            let (set, entry) = self.find(&dir, &name)?.ok_or(Error::NotFound)?;
            if !entry.is_directory() {
                return Err(Error::NotDirectory);
            }
            ancestors.push(entry.stream().first_cluster());
            dir = self.open_dir(set, &entry)?;
        }
        let name = file_name(last)?;
        let found = self.find(&dir, &name)?;
        Ok(Target {
            parent: dir,
            name,
            found,
            ancestors,
        })
    }

    /// Holds the set's bytes for writing at its place in its directory, and
    /// records them in the cached directory and its name index.
    pub(crate) fn write_set(&mut self, set: &Set) -> Result<()> {
        let allocation = self.cached(set.dir)?.allocation;
        self.load_chain(&allocation)?;
        let clusters = self.chain(&allocation)?;
        let cluster_size = self.volume.boot().cluster_size() as usize;
        let mut position = set.offset;
        let end = set.offset + set.raw.len();
        if end > clusters.len() * cluster_size {
            return Err(Error::Invalid("entry set outside its directory"));
        }
        while position < end {
            let cluster = clusters[position / cluster_size];
            let to = end.min((position / cluster_size + 1) * cluster_size);
            let offset =
                self.volume.boot().cluster_offset(cluster)? + (position % cluster_size) as u64;
            self.cache
                .write(offset, &set.raw[position - set.offset..to - set.offset])?;
            position = to;
        }
        let upcase = &self.volume.upcase;
        let cached = self.dirs.dirs.get_mut(&set.dir).expect("looked up above");
        if cached.bytes[set.offset] == FILE {
            let old = fold(upcase, &set_name(cached.set_at(set.offset)));
            if cached.names.get(&old) == Some(&set.offset) {
                cached.names.remove(&old);
            }
        }
        cached.bytes[set.offset..end].copy_from_slice(&set.raw);
        if set.raw[0] == FILE {
            cached
                .names
                .insert(fold(upcase, &set_name(&set.raw)), set.offset);
        }
        if set
            .raw
            .chunks_exact(ENTRY)
            .any(|entry| entry[0] & 0x80 == 0)
        {
            cached.free_from = cached.free_from.min(set.offset);
        }
        if cached.root {
            self.volume.root[set.offset..end].copy_from_slice(&set.raw);
        }
        Ok(())
    }

    /// Records a change to a directory's entries in the directory's own File
    /// entry: LastModified and LastAccessed become `now` (Sections 7.4.6 and
    /// 7.4.7). The root directory has no File entry, so nothing changes for
    /// it. The set is held with the rest of the operation's phase.
    pub(crate) fn touch(&mut self, dir: &mut Dir, now: &Timestamp) -> Result<()> {
        let Some(owner) = &mut dir.owner else {
            return Ok(());
        };
        set_time(&mut owner.raw, Stamp::Modified, now)?;
        set_time(&mut owner.raw, Stamp::Accessed, now)?;
        seal(&mut owner.raw);
        let owner = owner.clone();
        self.write_set(&owner)
    }

    /// Adds a cluster of unused entries to the end of a directory and
    /// records the new length in its parent.
    fn grow(&mut self, dir: &mut Dir) -> Result<()> {
        let cluster_size = self.volume.boot().cluster_size();
        let cached = self.cached(dir.first)?;
        if cached.bytes.len() as u64 + cluster_size > MAX_DIRECTORY_BYTES {
            return Err(Error::NoSpace);
        }
        let mut allocation = cached.allocation;
        self.extend_clusters(&mut allocation, 1)?;
        let added = allocation.last_cluster();
        // A new directory cluster is all end-of-directory entries before
        // anything refers to it.
        self.cache.write_through(
            self.volume.boot().cluster_offset(added)?,
            &vec![0; cluster_size as usize],
        )?;
        self.barrier()?;
        let cached = self.dirs.dirs.get_mut(&dir.first).expect("looked up above");
        cached.allocation = allocation;
        cached
            .bytes
            .resize(cached.bytes.len() + cluster_size as usize, 0);
        let length = cached.bytes.len();
        self.dirs.bytes += cluster_size as usize;
        if cached.root {
            self.volume.root.resize(length, 0);
        }
        if let Some(owner) = &mut dir.owner {
            let length = length as u64;
            set_stream(&mut owner.raw, &allocation, length, length);
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
            let cached = self.cached(dir.first)?;
            if let Some(offset) = free_run(&cached.bytes, cached.free_from, count) {
                break offset;
            }
            self.grow(dir)?;
        };
        let set = Set {
            dir: dir.first,
            offset,
            raw,
        };
        self.write_set(&set)?;
        let cached = self.dirs.dirs.get_mut(&dir.first).expect("written above");
        if cached.free_from == offset {
            cached.free_from = offset + set.raw.len();
        }
        Ok(set)
    }

    /// Caches a new, empty directory's single cluster of end-of-directory
    /// entries, replacing anything its first cluster held.
    pub(crate) fn cache_new_dir(&mut self, allocation: Allocation) -> Result<()> {
        let bytes = vec![0; self.volume.boot().cluster_size() as usize];
        let dir = CachedDir::new(
            allocation,
            bytes,
            false,
            self.volume.upcase(),
            self.volume.boot(),
        )?;
        self.dirs.insert(allocation.first_cluster(), dir);
        Ok(())
    }
}
