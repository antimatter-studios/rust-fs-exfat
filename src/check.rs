use crate::boot::{BootDifference, BootRegions};
use crate::volume::Volume;
use crate::{Error, Result};
use fs_core::BlockRead;
use std::collections::HashSet;

#[derive(Debug)]
pub struct CheckReport {
    pub files: u64,
    pub directories: u64,
    pub allocated_clusters: u64,
    pub referenced_clusters: u64,
    pub unreferenced_clusters: u64,
    pub dirty: bool,
    pub media_failure: bool,
    pub boot_differences: Vec<BootDifference>,
}

impl CheckReport {
    pub fn healthy(&self) -> bool {
        !self.dirty && !self.media_failure && self.unreferenced_clusters == 0
    }
}

pub fn check<Device: BlockRead>(volume: &Volume<Device>) -> Result<CheckReport> {
    let boot = volume.boot();
    let limit = volume.limits().max_chain_clusters;
    if u64::from(boot.cluster_count) > limit as u64 {
        return Err(Error::Unsupported(
            "check exceeds the allocation audit limit",
        ));
    }
    let mut referenced = HashSet::new();
    let mut claim = |clusters: Vec<u32>| -> Result<()> {
        for cluster in clusters {
            if !referenced.insert(cluster) {
                return Err(Error::Invalid("cross-linked allocation"));
            }
        }
        Ok(())
    };
    claim(volume.cluster_chain(boot.root_cluster)?)?;
    for bitmap in volume.metadata().bitmaps.iter().flatten() {
        claim(volume.cluster_chain(bitmap.first_cluster)?)?;
    }
    claim(volume.cluster_chain(volume.metadata().upcase.first_cluster)?)?;
    let mut pending = vec![volume.root_directory()?];
    let mut files = 0u64;
    let mut directories = 1u64;
    while let Some(entries) = pending.pop() {
        for entry in entries {
            if files + directories >= limit as u64 {
                return Err(Error::Unsupported(
                    "check exceeds the directory entry limit",
                ));
            }
            claim(volume.stream_clusters(entry.stream())?)?;
            if entry.is_directory() {
                directories += 1;
                pending.push(volume.read_directory(&entry)?);
            } else {
                files += 1;
            }
        }
    }
    let mut allocated_clusters = 0;
    for cluster in 2..boot.cluster_count + 2 {
        allocated_clusters += u64::from(volume.bitmap().allocated(cluster)?);
    }
    let referenced_clusters = referenced.len() as u64;
    Ok(CheckReport {
        files,
        directories,
        allocated_clusters,
        referenced_clusters,
        unreferenced_clusters: allocated_clusters - referenced_clusters,
        dirty: boot.volume_flags & 2 != 0,
        media_failure: boot.volume_flags & 4 != 0,
        boot_differences: BootRegions::read(&volume.reader)?.differences,
    })
}
