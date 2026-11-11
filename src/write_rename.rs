//! Renaming and moving, within a directory and across directories.
//!
//! A move writes the entry set under its new name first and removes the old
//! one after a barrier, so a move that stops partway leaves two names for
//! one stream, which a checker can repair, and never none. A replaced target
//! loses its entry set with the same write, and its clusters are freed only
//! after both. exFAT directories hold no entry for their parent, so moving a
//! directory changes nothing inside it.
//!
//! Each directory whose entries change has its LastModified and LastAccessed
//! set in the same phase as the entry set written into it.

use super::dir::{renamed, Set};
use super::WriteVolume;
use crate::timestamp::Timestamp;
use crate::{Error, Result};
use fs_core::BlockDevice;

fn same_set(first: &Set, second: &Set) -> bool {
    first.dir == second.dir && first.offset == second.offset
}

impl<Device: BlockDevice> WriteVolume<Device> {
    /// Renames or moves `from` to `to`. An existing `to` is replaced when
    /// POSIX rename(2) allows it: a file by a file, or an empty directory by
    /// a directory. A directory cannot move below itself. A name that differs
    /// only in case renames the entry in place.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        self.operation(|volume| volume.move_entry(from, to))
    }

    fn move_entry(&mut self, from: &str, to: &str) -> Result<()> {
        let now = Timestamp::now()?;
        let mut source = self.resolve(from)?;
        let (source_set, source_entry) = source.found.take().ok_or(Error::NotFound)?;
        let mut target = self.resolve(to)?;
        if source_entry.is_directory() {
            let first = source_entry.stream().first_cluster();
            if target.ancestors.contains(&first) {
                return Err(Error::Invalid("a directory cannot move below itself"));
            }
        }
        let hash = self.volume.upcase().name_hash(&target.name);
        let raw = renamed(&source_set.raw, &target.name, hash);
        let mut replaced = None;
        if let Some((target_set, target_entry)) = target.found.take() {
            if same_set(&target_set, &source_set) {
                if target_entry.name_utf16() == target.name.as_slice() {
                    return Ok(());
                }
                // A change of case only: the name has as many units, so the
                // set keeps its size and is rewritten where it is.
                let set = Set { raw, ..source_set };
                self.write_set(&set)?;
                return self.touch(&mut target.parent, &now);
            }
            match (source_entry.is_directory(), target_entry.is_directory()) {
                (false, true) => return Err(Error::IsDirectory),
                (true, false) => return Err(Error::NotDirectory),
                (true, true) => {
                    if !self.dir_is_empty(target_set.clone(), &target_entry)? {
                        return Err(Error::NotEmpty);
                    }
                }
                (false, false) => {}
            }
            let directory = target_entry.is_directory();
            replaced = Some((
                target_set,
                self.stream_allocation(&target_entry)?,
                directory,
            ));
        }
        if let Some((target_set, _, _)) = &mut replaced {
            self.unset(target_set)?;
        }
        self.insert(&mut target.parent, raw)?;
        self.touch(&mut target.parent, &now)?;
        self.barrier()?;
        let mut old = source_set;
        self.unset(&mut old)?;
        // The source's directory is touched once: when it is the target's
        // too, its File entry was written above, after any growth.
        let same_parent = match (&source.parent.owner, &target.parent.owner) {
            (None, None) => true,
            (Some(first), Some(second)) => same_set(first, second),
            _ => false,
        };
        if !same_parent {
            self.touch(&mut source.parent, &now)?;
        }
        if let Some((_, allocation, directory)) = replaced {
            if directory {
                self.dirs.remove(allocation.first_cluster());
            }
            self.barrier()?;
            let mut freed = allocation;
            self.shrink_clusters(&mut freed, 0)?;
        }
        Ok(())
    }
}
