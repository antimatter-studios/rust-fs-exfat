//! Renaming and moving, within a directory and across directories.
//!
//! A move writes the entry set under its new name first and removes the old
//! one after a barrier, so a move that stops partway leaves two names for
//! one stream, which a checker can repair, and never none. A replaced target
//! loses its entry set with the same write, and its clusters are freed only
//! after both. exFAT directories hold no entry for their parent, so moving a
//! directory changes nothing inside it.

use super::dir::{renamed, Set};
use super::WriteVolume;
use crate::{Error, Result};
use fs_core::BlockDevice;

const ENTRY: usize = 32;

fn same_set(first: &Set, second: &Set) -> bool {
    first.clusters.first() == second.clusters.first() && first.offset == second.offset
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
        let source = self.resolve(from)?;
        let (source_set, source_entry) = source.found.ok_or(Error::NotFound)?;
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
                return self.write_set(&set);
            }
            match (source_entry.is_directory(), target_entry.is_directory()) {
                (false, true) => return Err(Error::IsDirectory),
                (true, false) => return Err(Error::NotDirectory),
                (true, true) => {
                    let (_, clusters) = self.stream_allocation(&target_entry)?;
                    let bytes =
                        self.read_clusters(&clusters, target_entry.stream().data_length())?;
                    let in_use = bytes
                        .chunks_exact(ENTRY)
                        .take_while(|entry| entry[0] != 0)
                        .any(|entry| entry[0] & 0x80 != 0);
                    if in_use {
                        return Err(Error::NotEmpty);
                    }
                }
                (false, false) => {}
            }
            replaced = Some((target_set, self.stream_allocation(&target_entry)?.0));
        }
        if let Some((target_set, _)) = &mut replaced {
            self.unset(target_set)?;
            let start = target_set.offset;
            target.parent.bytes[start..start + target_set.raw.len()]
                .copy_from_slice(&target_set.raw);
        }
        self.insert(&mut target.parent, raw)?;
        self.barrier()?;
        let mut old = source_set;
        self.unset(&mut old)?;
        if let Some((_, allocation)) = replaced {
            self.barrier()?;
            let mut freed = allocation;
            self.shrink_clusters(&mut freed, 0)?;
        }
        Ok(())
    }
}
