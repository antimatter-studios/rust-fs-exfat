use super::{
    checked_fat_entry, decode_fat_entry, ChainPosition, EntryType, FatChainStep, FatEntryReport,
    FatValue, ENTRY_SIZE,
};
use crate::{Error, Result};

const CHUNK_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeState {
    pub active_fat: usize,
    pub dirty: bool,
    pub media_failure: bool,
}

impl VolumeState {
    pub fn from_flags(flags: u16) -> Self {
        Self {
            active_fat: usize::from(flags & 1),
            dirty: flags & 2 != 0,
            media_failure: flags & 4 != 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectoryRecord {
    pub entry_type: EntryType,
    pub length: usize,
}

/// Frames live file and benign primary sets, retaining all other slots singly.
pub fn directory_record(primary: &[u8]) -> Option<DirectoryRecord> {
    let primary = primary.get(..ENTRY_SIZE)?;
    let entry_type = EntryType::parse(primary)?;
    let secondaries = if entry_type.in_use
        && (entry_type.value == 0x85 || entry_type.benign && !entry_type.secondary)
    {
        usize::from(primary[1])
    } else {
        0
    };
    Some(DirectoryRecord {
        entry_type,
        length: (secondaries + 1) * ENTRY_SIZE,
    })
}

/// The unit a failed chunk read is retried in: the smallest exFAT sector.
const RETRY_BYTES: usize = 512;

/// One selected FAT's fixed-size read window; no full FAT snapshot is allocated.
/// A failed chunk read is retried per 512-byte sector, so a bad sector fails
/// only the words it holds, never the rest of its chunk.
pub struct FatWindow {
    start: u64,
    length: u64,
    cluster_count: u32,
    cached: u64,
    /// One bit per retry sector of the cached chunk that did not read.
    failed: u128,
    bytes: [u8; CHUNK_BYTES],
}

const _: () = assert!(CHUNK_BYTES / RETRY_BYTES == u128::BITS as usize);

impl FatWindow {
    pub fn new(start: u64, length: u64, cluster_count: u32) -> Self {
        Self {
            start,
            length,
            cluster_count,
            cached: u64::MAX,
            failed: 0,
            bytes: [0; CHUNK_BYTES],
        }
    }

    /// The cluster's FAT entry. A read error is returned only when the
    /// entry's own sector does not read; that sector is read again first.
    pub fn entry(
        &mut self,
        cluster: u32,
        mut read: impl FnMut(u64, &mut [u8]) -> Result<()>,
    ) -> Result<FatEntryReport> {
        let offset = u64::from(cluster) * 4;
        if offset + 4 > self.length {
            return Err(Error::Bounds);
        }
        let chunk = offset / CHUNK_BYTES as u64 * CHUNK_BYTES as u64;
        let physical = self.start.checked_add(chunk).ok_or(Error::Bounds)?;
        if chunk != self.cached {
            let length = (self.length - chunk).min(CHUNK_BYTES as u64) as usize;
            self.cached = chunk;
            self.failed = 0;
            if read(physical, &mut self.bytes[..length]).is_err() {
                for (index, sector) in self.bytes[..length].chunks_mut(RETRY_BYTES).enumerate() {
                    if read(physical + (index * RETRY_BYTES) as u64, sector).is_err() {
                        sector.fill(0);
                        self.failed |= 1 << index;
                    }
                }
            }
        }
        let position = (offset - chunk) as usize / 4;
        let index = position * 4 / RETRY_BYTES;
        if self.failed & 1 << index != 0 {
            let length = (self.length - chunk).min(CHUNK_BYTES as u64) as usize;
            let range = index * RETRY_BYTES..((index + 1) * RETRY_BYTES).min(length);
            read(physical + range.start as u64, &mut self.bytes[range])?;
            self.failed &= !(1 << index);
        }
        Ok(decode_fat_entry(
            checked_fat_entry(&self.bytes, position).unwrap(),
            self.cluster_count,
        ))
    }
}

/// Walks FAT links or a contiguous claim without allocating cycle state. Each
/// position precedes its FAT read; the caller must stop repeated positions using
/// its own ownership bitmap, so the position's `cycle` flag is always false.
/// `Some(count)` selects contiguous traversal.
pub fn walk_allocation(
    first: u32,
    cluster_count: u32,
    mut contiguous: Option<u64>,
    mut read: impl FnMut(u32) -> Result<FatEntryReport>,
) -> impl Iterator<Item = Result<FatChainStep>> {
    let mut current = (contiguous != Some(0)).then_some(first);
    let mut pending: Option<u32> = None;
    std::iter::from_fn(move || {
        if let Some(cluster) = pending.take() {
            if let Some(remaining) = contiguous.as_mut() {
                *remaining -= 1;
                if *remaining == 0 {
                    return None;
                }
                current = match cluster.checked_add(1) {
                    Some(next) => Some(next),
                    None => return Some(Err(Error::Bounds)),
                };
            } else {
                return Some(read(cluster).map(|entry| {
                    if let FatValue::Next(next) = entry.kind {
                        current = Some(next);
                    }
                    FatChainStep::Entry { cluster, entry }
                }));
            }
        }
        let cluster = current.take()?;
        pending = Some(cluster);
        Some(Ok(FatChainStep::Cluster(ChainPosition {
            cluster,
            valid: (2..u64::from(cluster_count) + 2).contains(&u64::from(cluster)),
            cycle: false,
        })))
    })
}

/// Marks an available bitmap bit and reports whether it was already claimed.
pub fn claim_bitmap_cluster(bitmap: &mut [u8], cluster: u32) -> Option<bool> {
    let index = cluster.checked_sub(2)? as usize;
    let byte = bitmap.get_mut(index / 8)?;
    let mask = 1 << (index % 8);
    let previous = *byte & mask != 0;
    *byte |= mask;
    Some(previous)
}

pub fn bitmap_contains_all(allocated: &[u8], claimed: &[u8]) -> bool {
    claimed
        .iter()
        .enumerate()
        .all(|(index, claims)| claims & !allocated.get(index).copied().unwrap_or(0) == 0)
}

/// Counts available heap bits, excluding storage beyond the declared heap.
pub fn bitmap_count(bitmap: &[u8], cluster_count: u32) -> u64 {
    let full = cluster_count as usize / 8;
    let tail = cluster_count % 8;
    bitmap
        .iter()
        .take(full)
        .map(|byte| u64::from(byte.count_ones()))
        .sum::<u64>()
        + bitmap
            .get(full)
            .map_or(0, |byte| u64::from((byte & ((1 << tail) - 1)).count_ones()))
}

#[derive(Debug)]
pub enum StreamReadError {
    End,
    Link(FatEntryReport),
    Failure(Error),
}

impl From<Error> for StreamReadError {
    fn from(error: Error) -> Self {
        Self::Failure(error)
    }
}

/// Fixed-memory stream traversal. Callers supply checked physical reads and
/// decide how short streams, damaged links and read errors affect their audit.
pub struct StreamCursor {
    length: u64,
    contiguous: bool,
    cluster: u32,
    within: u64,
    position: u64,
    bytes: [u8; CHUNK_BYTES],
    start: usize,
    end: usize,
}

impl StreamCursor {
    pub fn new(first: u32, length: u64, contiguous: bool) -> Self {
        Self {
            length,
            contiguous,
            cluster: first,
            within: 0,
            position: 0,
            bytes: [0; CHUNK_BYTES],
            start: 0,
            end: 0,
        }
    }

    pub fn finished(&self) -> bool {
        self.position == self.length && self.start == self.end
    }

    pub fn read(
        &mut self,
        cluster_size: u64,
        mut next: impl FnMut(u32) -> Result<FatEntryReport>,
        mut read: impl FnMut(u32, u64, &mut [u8]) -> Result<()>,
        mut output: &mut [u8],
    ) -> std::result::Result<(), StreamReadError> {
        if cluster_size == 0 {
            return Err(Error::Bounds.into());
        }
        while !output.is_empty() {
            if self.start == self.end {
                if self.position == self.length {
                    return Err(StreamReadError::End);
                }
                if self.within == cluster_size {
                    self.cluster = if self.contiguous {
                        self.cluster.checked_add(1).ok_or(Error::Bounds)?
                    } else {
                        let entry = next(self.cluster)?;
                        if let FatValue::Next(next) = entry.kind {
                            next
                        } else {
                            return Err(StreamReadError::Link(entry));
                        }
                    };
                    self.within = 0;
                }
                let length = (CHUNK_BYTES as u64)
                    .min(cluster_size - self.within)
                    .min(self.length - self.position) as usize;
                read(self.cluster, self.within, &mut self.bytes[..length])?;
                self.within += length as u64;
                self.position += length as u64;
                self.start = 0;
                self.end = length;
            }
            let count = output.len().min(self.end - self.start);
            output[..count].copy_from_slice(&self.bytes[self.start..self.start + count]);
            self.start += count;
            output = &mut output[count..];
        }
        Ok(())
    }
}
