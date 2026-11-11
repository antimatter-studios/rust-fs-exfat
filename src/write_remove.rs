//! Removing files and directories.
//!
//! Section 6.2 marks a directory entry unused by clearing InUse in its
//! EntryType. Removal clears it on every entry of the set, writes the set,
//! and only then frees the stream's clusters, so a removal that stops
//! partway leaves allocated, unreferenced clusters and never a set that
//! names free ones. The rest of each entry is left as it was. The parent
//! directory's LastModified and LastAccessed are set with the same write.

use super::WriteVolume;
use crate::timestamp::Timestamp;
use crate::{Error, Result};
use fs_core::BlockDevice;

const ENTRY: usize = 32;
const IN_USE: u8 = 0x80;

impl<Device: BlockDevice> WriteVolume<Device> {
    /// Removes a file and frees its clusters. A directory is refused.
    pub fn unlink(&mut self, path: &str) -> Result<()> {
        self.operation(|volume| volume.remove(path, false))
    }

    /// Removes an empty directory and frees its clusters. A file, a
    /// directory that holds any entry in use, and the root are refused.
    pub fn rmdir(&mut self, path: &str) -> Result<()> {
        self.operation(|volume| volume.remove(path, true))
    }

    fn remove(&mut self, path: &str, directory: bool) -> Result<()> {
        let now = Timestamp::now()?;
        let mut target = self.resolve(path)?;
        let (mut set, entry) = target.found.take().ok_or(Error::NotFound)?;
        match (directory, entry.is_directory()) {
            (false, true) => return Err(Error::IsDirectory),
            (true, false) => return Err(Error::NotDirectory),
            _ => {}
        }
        let allocation = self.stream_allocation(&entry)?;
        if directory && !self.dir_is_empty(set.clone(), &entry)? {
            return Err(Error::NotEmpty);
        }
        self.unset(&mut set)?;
        self.touch(&mut target.parent, &now)?;
        self.barrier()?;
        if directory {
            self.dirs.remove(allocation.first_cluster());
        }
        let mut freed = allocation;
        self.shrink_clusters(&mut freed, 0)
    }

    /// Clears InUse on every entry of a set and holds it for writing.
    pub(crate) fn unset(&mut self, set: &mut super::dir::Set) -> Result<()> {
        for entry in set.raw.chunks_exact_mut(ENTRY) {
            entry[0] &= !IN_USE;
        }
        self.write_set(set)
    }
}
