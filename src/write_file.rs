//! Creating files and directories, and writing file data.

use super::dir::{
    new_set, seal, set_attributes, set_stream, set_time, Dir, Set, Stamp, Target,
    ATTRIBUTE_ARCHIVE, ATTRIBUTE_DIRECTORY, SETTABLE_ATTRIBUTES,
};
use super::{Allocation, WriteVolume};
use crate::directory::DirectoryEntry;
use crate::timestamp::{Timestamp, Timestamps};
use crate::{Error, Result};
use fs_core::BlockDevice;

/// The largest write handed to the device at once.
const WRITE_CHUNK: usize = 1 << 20;

/// How a new file or directory starts out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewEntry {
    /// ReadOnly (0x01), Hidden (0x02), System (0x04) and Archive (0x20);
    /// Directory is set by `mkdir` and other bits are refused.
    pub attributes: u16,
    /// Create, LastModified and LastAccessed; `None` takes the current time
    /// in UTC for all three.
    pub times: Option<Timestamps>,
}

impl NewEntry {
    /// A file with Archive set, as a newly written file is, at the current
    /// time.
    pub const FILE: Self = Self {
        attributes: ATTRIBUTE_ARCHIVE,
        times: None,
    };

    /// A directory with no attributes but Directory, at the current time.
    pub const DIRECTORY: Self = Self {
        attributes: 0,
        times: None,
    };
}

fn now_times() -> Result<Timestamps> {
    let now = Timestamp::now()?;
    Ok(Timestamps {
        created: now,
        modified: now,
        accessed: now,
    })
}

/// A regular file's entry set and stream, opened for change.
struct OpenFile {
    set: Set,
    allocation: Allocation,
    clusters: Vec<u32>,
    length: u64,
    valid: u64,
}

impl<Device: BlockDevice> WriteVolume<Device> {
    /// Creates an empty file. Its parent must exist; a name that exists
    /// already, compared through the volume's Up-case Table, is refused.
    pub fn create(&mut self, path: &str) -> Result<()> {
        self.create_with(path, NewEntry::FILE)
    }

    /// Creates an empty file with the given attributes and times.
    pub fn create_with(&mut self, path: &str, new: NewEntry) -> Result<()> {
        self.operation(|volume| volume.make(path, new, false))
    }

    /// Creates an empty directory of one cluster.
    pub fn mkdir(&mut self, path: &str) -> Result<()> {
        self.mkdir_with(path, NewEntry::DIRECTORY)
    }

    /// Creates an empty directory with the given attributes and times.
    pub fn mkdir_with(&mut self, path: &str, new: NewEntry) -> Result<()> {
        self.operation(|volume| volume.make(path, new, true))
    }

    /// Writes `data` into an existing file at `offset`, growing it as
    /// needed. Bytes between the old valid length and `offset` read as
    /// zeros. LastModified and LastAccessed become the current time, and
    /// Archive is set.
    pub fn pwrite(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<()> {
        let now = Timestamp::now()?;
        self.operation(|volume| {
            let target = volume.resolve(path)?;
            let mut file = volume.open_for_write(target)?;
            volume.write_into(&mut file, offset, data, &now)
        })
    }

    /// Makes `path` a file holding exactly `data`, creating it if it is
    /// missing and replacing its contents if it is a file.
    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<()> {
        let now = Timestamp::now()?;
        self.operation(|volume| {
            let target = volume.resolve(path)?;
            if target.found.is_none() {
                volume.make_in(target, NewEntry::FILE, false)?;
                let target = volume.resolve(path)?;
                let mut file = volume.open_for_write(target)?;
                return volume.write_into(&mut file, 0, data, &now);
            }
            let mut file = volume.open_for_write(target)?;
            volume.release(&mut file)?;
            volume.write_into(&mut file, 0, data, &now)
        })
    }

    fn make(&mut self, path: &str, new: NewEntry, directory: bool) -> Result<()> {
        let target = self.resolve(path)?;
        self.make_in(target, new, directory)
    }

    fn make_in(&mut self, target: Target, new: NewEntry, directory: bool) -> Result<()> {
        if new.attributes & !SETTABLE_ATTRIBUTES != 0 {
            return Err(Error::Invalid(
                "attributes other than ReadOnly, Hidden, System and Archive",
            ));
        }
        if target.found.is_some() {
            return Err(Error::Exists);
        }
        let times = match new.times {
            Some(times) => times,
            None => now_times()?,
        };
        let hash = self.volume.upcase().name_hash(&target.name);
        let mut attributes = new.attributes;
        if directory {
            attributes |= ATTRIBUTE_DIRECTORY;
        }
        let mut raw = new_set(&target.name, hash, attributes, &times)?;
        if directory {
            // The new directory's cluster is allocated and holds only
            // end-of-directory entries before its entry set refers to it.
            let allocation = self.allocate_clusters(1, None)?;
            let cluster_size = self.volume.boot().cluster_size();
            self.cache.write_through(
                self.volume
                    .boot()
                    .cluster_offset(allocation.first_cluster())?,
                &vec![0; cluster_size as usize],
            )?;
            self.barrier()?;
            set_stream(&mut raw, &allocation, cluster_size, cluster_size);
            seal(&mut raw);
        }
        let mut parent: Dir = target.parent;
        self.insert(&mut parent, raw)?;
        Ok(())
    }

    fn open_for_write(&self, target: Target) -> Result<OpenFile> {
        let (set, entry): (Set, DirectoryEntry) = target.found.ok_or(Error::NotFound)?;
        if entry.is_directory() {
            return Err(Error::IsDirectory);
        }
        let (allocation, clusters) = self.stream_allocation(&entry)?;
        Ok(OpenFile {
            set,
            allocation,
            clusters,
            length: entry.stream().data_length(),
            valid: entry.stream().valid_data_length(),
        })
    }

    /// Empties a file: its entry set first, then its clusters.
    fn release(&mut self, file: &mut OpenFile) -> Result<()> {
        if file.length == 0 && file.allocation.is_empty() {
            return Ok(());
        }
        let old = file.allocation;
        set_stream(&mut file.set.raw, &Allocation::EMPTY, 0, 0);
        seal(&mut file.set.raw);
        self.write_set(&file.set)?;
        self.barrier()?;
        let mut freed = old;
        self.shrink_clusters(&mut freed, 0)?;
        self.barrier()?;
        file.allocation = Allocation::EMPTY;
        file.clusters.clear();
        file.length = 0;
        file.valid = 0;
        Ok(())
    }

    fn write_into(
        &mut self,
        file: &mut OpenFile,
        offset: u64,
        data: &[u8],
        now: &Timestamp,
    ) -> Result<()> {
        // An empty write changes no length, as pwrite(2) does not.
        if !data.is_empty() {
            let end = offset.checked_add(data.len() as u64).ok_or(Error::Bounds)?;
            let cluster_size = self.volume.boot().cluster_size();
            let length = file.length.max(end);
            let needed =
                u32::try_from(length.div_ceil(cluster_size)).map_err(|_| Error::NoSpace)?;
            if needed > file.allocation.count() {
                let more = needed - file.allocation.count();
                self.extend_clusters(&mut file.allocation, more)?;
                self.barrier()?;
                file.clusters = self.clusters(&file.allocation)?;
            }
            if offset > file.valid {
                self.zero_range(&file.clusters, file.valid, offset)?;
            }
            self.write_range(&file.clusters, offset, data)?;
            file.length = length;
            file.valid = file.valid.max(end);
        }
        let raw = &mut file.set.raw;
        set_stream(raw, &file.allocation, file.valid, file.length);
        set_time(raw, Stamp::Modified, now)?;
        set_time(raw, Stamp::Accessed, now)?;
        let attributes = super::dir::attributes(raw) | ATTRIBUTE_ARCHIVE;
        set_attributes(raw, attributes);
        seal(raw);
        let set = file.set.clone();
        self.write_set(&set)
    }

    fn zero_range(&mut self, clusters: &[u32], from: u64, to: u64) -> Result<()> {
        let zeros = vec![0; WRITE_CHUNK.min((to - from) as usize)];
        let mut position = from;
        while position < to {
            let count = ((to - position) as usize).min(zeros.len());
            self.write_range(clusters, position, &zeros[..count])?;
            position += count as u64;
        }
        Ok(())
    }

    /// Writes `data` at a stream offset, joining runs of consecutive
    /// clusters into single device writes.
    fn write_range(&mut self, clusters: &[u32], offset: u64, data: &[u8]) -> Result<()> {
        let cluster_size = self.volume.boot().cluster_size();
        let mut written = 0usize;
        while written < data.len() {
            let position = offset + written as u64;
            let index = usize::try_from(position / cluster_size).map_err(|_| Error::Bounds)?;
            let first = *clusters
                .get(index)
                .ok_or(Error::Invalid("write past the file's clusters"))?;
            let mut run = 1;
            while clusters.get(index + run) == Some(&(first + run as u32)) {
                run += 1;
            }
            let run_end = (index + run) as u64 * cluster_size;
            let count = ((run_end - position) as usize)
                .min(data.len() - written)
                .min(WRITE_CHUNK);
            let device =
                self.volume.boot().cluster_offset(first)? + position - index as u64 * cluster_size;
            self.cache
                .write_through(device, &data[written..written + count])?;
            written += count;
        }
        Ok(())
    }
}
