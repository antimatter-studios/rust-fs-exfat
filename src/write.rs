//! The write side of the driver.
//!
//! Everything that changes a volume lives under this module and takes
//! `fs_core::BlockDevice`. The strict reader ([`crate::volume`]) and the
//! salvage adapter ([`crate::salvage`]) take only `BlockRead`, so they stay
//! read-only by type; `scripts/check-read-only.sh` keeps the writable surface
//! out of them.
//!
//! A [`WriteVolume`] is a mount for writing. Mounting validates the volume
//! with the strict reader, refuses one whose VolumeDirty or MediaFailure flag
//! is set, and sets VolumeDirty before anything else is written; a clean
//! [`WriteVolume::unmount`] clears it last. Reads on the mount go through
//! [`WriteVolume::reader`], the same strict reader running over the device
//! with the mount's held writes laid over it.
//!
//! # Update order
//!
//! Section 3.1.13.2 of the specification asks an implementation to set
//! VolumeDirty before it updates metadata and to clear it afterwards. Within
//! that window every operation writes in an order that leaves the volume
//! consistent, or at worst holding clusters that are allocated and not
//! referenced, if it stops partway:
//!
//! - growing: the Allocation Bitmap and FAT first, then the directory entry
//!   that refers to the new clusters;
//! - shrinking and removing: the directory entry first, then the FAT and the
//!   Allocation Bitmap.
//!
//! Each phase is held in a sector cache and written at a barrier, so nothing
//! of a later phase reaches the device before an earlier one.
//!
//! # Boot regions
//!
//! VolumeDirty and PercentInUse are written to the Main Boot region only.
//! Section 3.2 asks implementations not to modify the Backup Boot region
//! except in infrequent circumstances, and the macOS driver leaves it alone
//! when it marks a volume dirty. Both fields are outside the boot checksum
//! (Section 3.4), which is recomputed after each change and rewritten only if
//! it differs.

#[path = "write_alloc.rs"]
mod alloc;
#[path = "write_cache.rs"]
mod cache;
#[path = "write_dir.rs"]
mod dir;
#[path = "write_file.rs"]
mod file;
#[path = "write_meta.rs"]
mod meta;
#[path = "write_remove.rs"]
mod remove;
#[path = "write_rename.rs"]
mod rename;

pub use alloc::Allocation;
pub use cache::CacheView;
pub use dir::{ATTRIBUTE_ARCHIVE, SETTABLE_ATTRIBUTES};
pub use file::NewEntry;
pub use meta::SetTimes;

use crate::volume::{ReadLimits, Volume};
use crate::{primitives, Error, Result};
use cache::Cache;
use fs_core::{BlockDevice, FileDevice, OwnedRwSlice};
use std::path::Path;
use std::sync::Arc;

const VOLUME_DIRTY: u16 = 2;
const MEDIA_FAILURE: u16 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Mounted,
    /// An operation failed after it had written something. The volume stays
    /// marked dirty and the mount refuses further writes.
    Failed,
    Unmounted,
}

/// A volume mounted for writing.
pub struct WriteVolume<Device: BlockDevice> {
    cache: Arc<Cache<Device>>,
    volume: Volume<CacheView<Device>>,
    fats: Vec<u64>,
    bitmaps: Vec<Vec<u32>>,
    allocated: u64,
    next_free: u32,
    state: State,
    /// Set when the root directory's stream changed, so the strict reader
    /// reads it again at the end of the operation.
    root_changed: bool,
}

impl<Device: BlockDevice> WriteVolume<Device> {
    /// Mounts a volume for writing with the default read limits.
    pub fn mount(device: Device) -> Result<Self> {
        Self::mount_with_limits(device, ReadLimits::default())
    }

    /// Mounts a volume for writing. The volume is checked by the strict
    /// reader first, and VolumeDirty is set and flushed before this returns.
    pub fn mount_with_limits(device: Device, limits: ReadLimits) -> Result<Self> {
        if !device.is_writable() {
            return Err(Error::Device(fs_core::Error::ReadOnly));
        }
        let cache = Arc::new(Cache::new(device));
        let volume = Volume::open_with_limits(CacheView::new(cache.clone()), limits)?;
        let boot = volume.boot().clone();
        // Section 3.1.13.2: only an implementation that resolves metadata
        // inconsistencies may clear a VolumeDirty it found at mount. This one
        // does not, so it does not write to such a volume at all.
        if boot.volume_flags & VOLUME_DIRTY != 0 {
            return Err(Error::Unsupported(
                "volume is marked dirty; check and repair it before writing",
            ));
        }
        if boot.volume_flags & MEDIA_FAILURE != 0 {
            return Err(Error::Unsupported("volume reports media failures"));
        }
        cache.set_sector(boot.bytes_per_sector())?;
        let fats = (0..u64::from(boot.number_of_fats))
            .map(|index| {
                (u64::from(boot.fat_offset) + index * u64::from(boot.fat_length))
                    * boot.bytes_per_sector()
            })
            .collect();
        let mut bitmaps = Vec::new();
        for stream in volume
            .metadata()
            .bitmaps
            .iter()
            .take(usize::from(boot.number_of_fats))
        {
            let stream = stream.ok_or(Error::Invalid("missing allocation bitmap"))?;
            bitmaps.push(volume.cluster_chain(stream.first_cluster)?);
        }
        let allocated = volume.bitmap().allocated_count();
        let mut mounted = Self {
            cache,
            volume,
            fats,
            bitmaps,
            allocated,
            next_free: 2,
            state: State::Unmounted,
            root_changed: false,
        };
        mounted.write_boot_state(true)?;
        mounted.cache.sync()?;
        mounted.state = State::Mounted;
        Ok(mounted)
    }

    /// The strict reader over this mount, seeing every write made so far.
    pub fn reader(&self) -> &Volume<CacheView<Device>> {
        &self.volume
    }

    /// Clusters the Allocation Bitmap records as free.
    pub fn free_clusters(&self) -> u64 {
        u64::from(self.volume.boot().cluster_count) - self.allocated
    }

    /// PercentInUse as this mount computes it: allocated clusters as a
    /// percentage of the cluster heap, rounded down (Section 3.1.15).
    pub fn percent_in_use(&self) -> u8 {
        (self.allocated * 100 / u64::from(self.volume.boot().cluster_count)) as u8
    }

    /// Writes every held change, updates PercentInUse, and flushes the device.
    pub fn flush(&mut self) -> Result<()> {
        self.writable()?;
        self.commit()?;
        self.cache.sync()
    }

    /// Writes every held change, flushes the device, and clears VolumeDirty.
    ///
    /// A mount on which an operation failed partway is flushed but left
    /// marked dirty, and this reports that.
    pub fn unmount(mut self) -> Result<()> {
        self.finish()
    }

    fn finish(&mut self) -> Result<()> {
        match self.state {
            State::Unmounted => Ok(()),
            State::Failed => {
                self.state = State::Unmounted;
                self.cache.sync()?;
                Err(Error::Invalid(
                    "an earlier write failed partway; the volume is left marked dirty",
                ))
            }
            State::Mounted => {
                self.commit()?;
                self.cache.sync()?;
                self.write_boot_state(false)?;
                self.cache.sync()?;
                self.state = State::Unmounted;
                Ok(())
            }
        }
    }

    fn writable(&self) -> Result<()> {
        match self.state {
            State::Mounted => Ok(()),
            State::Failed => Err(Error::Invalid(
                "an earlier write failed partway; the mount takes no more writes",
            )),
            State::Unmounted => Err(Error::Invalid("volume is not mounted for writing")),
        }
    }

    /// Runs one operation. A failure after the operation wrote anything
    /// leaves the mount failed, because the volume may then be inconsistent.
    pub(crate) fn operation<Value>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<Value>,
    ) -> Result<Value> {
        self.writable()?;
        let before = self.cache.writes();
        match body(self).and_then(|value| self.commit().map(|()| value)) {
            Ok(value) => Ok(value),
            Err(error) => {
                if self.cache.writes() != before {
                    self.state = State::Failed;
                    let _ = self.cache.flush_dirty();
                }
                Err(error)
            }
        }
    }

    /// Writes everything held so far before anything that follows.
    pub(crate) fn barrier(&self) -> Result<()> {
        self.cache.flush_dirty()
    }

    fn commit(&mut self) -> Result<()> {
        self.cache.flush_dirty()?;
        if self.root_changed {
            self.volume.reload_root()?;
            self.root_changed = false;
        }
        if self.percent_in_use() != self.volume.boot().percent_in_use {
            self.write_boot_state(true)?;
        }
        Ok(())
    }

    /// Writes VolumeDirty and PercentInUse into the Main Boot Sector at once,
    /// and the Main Boot Checksum if it no longer matches.
    fn write_boot_state(&mut self, dirty: bool) -> Result<()> {
        let sector = self.volume.boot().bytes_per_sector() as usize;
        let mut region = vec![0; sector * 12];
        self.cache.read(0, &mut region)?;
        let mut flags = u16::from_le_bytes([region[106], region[107]]) & !VOLUME_DIRTY;
        if dirty {
            flags |= VOLUME_DIRTY;
        }
        let percent = self.percent_in_use();
        region[106..108].copy_from_slice(&flags.to_le_bytes());
        region[112] = percent;
        self.cache.write_through(0, &region[..sector])?;
        let checksum = primitives::boot_checksum(&region, sector);
        if primitives::stored_boot_checksum(&region, sector) != checksum {
            let words: Vec<u8> = std::iter::repeat_n(checksum.to_le_bytes(), sector / 4)
                .flatten()
                .collect();
            self.cache.write_through(sector as u64 * 11, &words)?;
        }
        let boot = self.volume.boot_mut();
        boot.volume_flags = flags;
        boot.percent_in_use = percent;
        Ok(())
    }
}

impl<Device: BlockDevice> Drop for WriteVolume<Device> {
    /// A mount dropped without [`WriteVolume::unmount`] is unmounted here,
    /// and any error is lost; call `unmount` to see it. A mount dropped while
    /// its thread panics is flushed but left marked dirty, because the panic
    /// may have stopped an update partway.
    fn drop(&mut self) {
        if std::thread::panicking() && self.state == State::Mounted {
            self.state = State::Failed;
        }
        let _ = self.finish();
    }
}

/// Mounts the volume at a 512-byte-aligned `offset` within `device` for
/// writing.
pub fn open_device<Device: BlockDevice + 'static>(
    device: Device,
    offset: u64,
) -> Result<WriteVolume<OwnedRwSlice>> {
    if !offset.is_multiple_of(512) || offset >= device.size_bytes() {
        return Err(Error::Invalid(
            "volume offset must be aligned and within the device",
        ));
    }
    let length = device.size_bytes() - offset;
    let device: Arc<dyn BlockDevice> = Arc::new(device);
    WriteVolume::mount(OwnedRwSlice::new(device, offset, length))
}

/// Opens an image or device file read-write and mounts the volume at
/// `offset` within it for writing.
pub fn open_path(path: &Path, offset: u64) -> Result<WriteVolume<OwnedRwSlice>> {
    open_device(FileDevice::open_rw(path)?, offset)
}
