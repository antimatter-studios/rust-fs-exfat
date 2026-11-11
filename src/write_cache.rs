//! A sector cache that holds metadata writes back until a barrier.
//!
//! Metadata updates (FAT entries, Allocation Bitmap bytes, directory entries)
//! are small and arrive in bursts, so they are gathered here as whole volume
//! sectors and written in ascending order at the next [`Cache::flush_dirty`].
//! The write path calls that between the phases of an operation, which is
//! what gives the update order its meaning: everything before a barrier is on
//! the device before anything after it is written.
//!
//! File data is not held: [`Cache::write_through`] writes it at once, and
//! patches any held sector it overlaps so that a later flush cannot write
//! older bytes over it. Reads see the held sectors over the device's bytes,
//! so the strict reader that runs over [`CacheView`] reads what the volume
//! will hold once the cache is flushed.

use crate::{Error, Result};
use fs_core::{BlockDevice, BlockRead};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

struct State {
    sector: u64,
    dirty: BTreeMap<u64, Vec<u8>>,
    writes: u64,
}

pub(crate) struct Cache<Device: BlockDevice> {
    device: Device,
    size: u64,
    state: Mutex<State>,
}

impl<Device: BlockDevice> Cache<Device> {
    pub(crate) fn new(device: Device) -> Self {
        Self {
            size: device.size_bytes(),
            device,
            state: Mutex::new(State {
                sector: 512,
                dirty: BTreeMap::new(),
                writes: 0,
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Sets the sector size that writes are gathered in. Only valid while
    /// nothing is held.
    pub(crate) fn set_sector(&self, sector: u64) -> Result<()> {
        let mut state = self.state();
        if !state.dirty.is_empty() || !sector.is_power_of_two() || sector < 512 {
            return Err(Error::Invalid("cache sector size"));
        }
        state.sector = sector;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn device(&self) -> &Device {
        &self.device
    }

    /// Counts every write accepted, held or immediate, so an operation can
    /// tell whether it changed anything before it failed.
    pub(crate) fn writes(&self) -> u64 {
        self.state().writes
    }

    #[cfg(test)]
    pub(crate) fn has_dirty(&self) -> bool {
        !self.state().dirty.is_empty()
    }

    fn bounds(&self, offset: u64, length: usize) -> Result<u64> {
        let end = offset.checked_add(length as u64).ok_or(Error::Bounds)?;
        if end > self.size {
            return Err(Error::Bounds);
        }
        Ok(end)
    }

    fn read_locked(&self, state: &State, offset: u64, output: &mut [u8]) -> Result<()> {
        let end = self.bounds(offset, output.len())?;
        if output.is_empty() {
            return Ok(());
        }
        // The device sees whole sectors only, as it does for writes; a raw
        // device refuses anything else.
        let aligned_start = offset / state.sector * state.sector;
        let aligned_end = end.div_ceil(state.sector) * state.sector;
        if aligned_start == offset && aligned_end == end {
            self.device.read_at(offset, output)?;
        } else {
            if aligned_end > self.size {
                return Err(Error::Bounds);
            }
            let mut buffer = vec![0; (aligned_end - aligned_start) as usize];
            self.device.read_at(aligned_start, &mut buffer)?;
            output.copy_from_slice(
                &buffer[(offset - aligned_start) as usize..(end - aligned_start) as usize],
            );
        }
        let first = offset / state.sector;
        let last = (end - 1) / state.sector;
        for (index, bytes) in state.dirty.range(first..=last) {
            let start = index * state.sector;
            let from = start.max(offset);
            let to = (start + state.sector).min(end);
            output[(from - offset) as usize..(to - offset) as usize]
                .copy_from_slice(&bytes[(from - start) as usize..(to - start) as usize]);
        }
        Ok(())
    }

    pub(crate) fn read(&self, offset: u64, output: &mut [u8]) -> Result<()> {
        let state = self.state();
        self.read_locked(&state, offset, output)
    }

    /// Holds `data` at `offset` until the next flush, reading any sector it
    /// only partly covers first.
    pub(crate) fn write(&self, offset: u64, data: &[u8]) -> Result<()> {
        let end = self.bounds(offset, data.len())?;
        let mut state = self.state();
        state.writes += 1;
        let sector = state.sector;
        let mut position = offset;
        while position < end {
            let index = position / sector;
            let start = index * sector;
            if !state.dirty.contains_key(&index) {
                if start + sector > self.size {
                    return Err(Error::Bounds);
                }
                let mut bytes = vec![0; sector as usize];
                self.read_locked(&state, start, &mut bytes)?;
                state.dirty.insert(index, bytes);
            }
            let to = (start + sector).min(end);
            let bytes = state.dirty.get_mut(&index).expect("inserted above");
            bytes[(position - start) as usize..(to - start) as usize]
                .copy_from_slice(&data[(position - offset) as usize..(to - offset) as usize]);
            position = to;
        }
        Ok(())
    }

    /// Writes `data` to the device now, in whole sectors, and updates any
    /// held sector it covers so a later flush writes the same bytes.
    pub(crate) fn write_through(&self, offset: u64, data: &[u8]) -> Result<()> {
        let end = self.bounds(offset, data.len())?;
        if data.is_empty() {
            return Ok(());
        }
        let mut state = self.state();
        state.writes += 1;
        let sector = state.sector;
        let first = offset / sector * sector;
        let last = end.div_ceil(sector) * sector;
        if last > self.size {
            return Err(Error::Bounds);
        }
        let aligned = first == offset && last == end;
        let staged;
        let bytes: &[u8] = if aligned {
            data
        } else {
            let mut buffer = vec![0; (last - first) as usize];
            if first < offset {
                self.read_locked(&state, first, &mut buffer[..sector as usize])?;
            }
            if last > end {
                let tail = buffer.len() - sector as usize;
                self.read_locked(&state, last - sector, &mut buffer[tail..])?;
            }
            buffer[(offset - first) as usize..(end - first) as usize].copy_from_slice(data);
            staged = buffer;
            &staged
        };
        for (index, held) in state.dirty.range_mut(first / sector..last / sector) {
            let start = (index * sector - first) as usize;
            held.copy_from_slice(&bytes[start..start + sector as usize]);
        }
        self.device.write_at(first, bytes)?;
        Ok(())
    }

    /// Writes every held sector in ascending order, joining runs of
    /// adjacent sectors into one device write.
    pub(crate) fn flush_dirty(&self) -> Result<()> {
        let mut state = self.state();
        let sector = state.sector;
        let mut run: Option<(u64, Vec<u8>)> = None;
        let held = std::mem::take(&mut state.dirty);
        let mut result = Ok(());
        for (index, bytes) in &held {
            let offset = index * sector;
            match &mut run {
                Some((start, buffer)) if *start + buffer.len() as u64 == offset => {
                    buffer.extend_from_slice(bytes);
                }
                _ => {
                    if let Some((start, buffer)) = run.take() {
                        result = result.and(self.device.write_at(start, &buffer));
                    }
                    run = Some((offset, bytes.clone()));
                }
            }
        }
        if let Some((start, buffer)) = run {
            result = result.and(self.device.write_at(start, &buffer));
        }
        if result.is_err() {
            // Nothing is known about which sectors reached the device, so all
            // of them stay held and the next flush tries them again.
            state.dirty = held;
        }
        Ok(result?)
    }

    pub(crate) fn sync(&self) -> Result<()> {
        self.flush_dirty()?;
        Ok(self.device.flush()?)
    }
}

/// A read-only view of a writable mount: the device's bytes with every held
/// write laid over them. The strict reader runs over it, so the read side of
/// a writable mount is the same validated code as a read-only mount.
pub struct CacheView<Device: BlockDevice> {
    cache: Arc<Cache<Device>>,
}

impl<Device: BlockDevice> CacheView<Device> {
    pub(crate) fn new(cache: Arc<Cache<Device>>) -> Self {
        Self { cache }
    }
}

impl<Device: BlockDevice> BlockRead for CacheView<Device> {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        self.cache
            .read(offset, output)
            .map_err(|error| match error {
                Error::Device(error) => error,
                Error::Bounds => fs_core::Error::OutOfBounds {
                    offset,
                    len: output.len() as u64,
                    size: self.cache.size,
                },
                other => fs_core::Error::Custom(other.to_string()),
            })
    }

    fn size_bytes(&self) -> u64 {
        self.cache.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Memory {
        bytes: Mutex<Vec<u8>>,
        writes: Mutex<Vec<(u64, usize)>>,
    }

    impl BlockRead for Memory {
        fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
            assert_eq!(offset % 512, 0, "reads are whole sectors");
            assert_eq!(output.len() % 512, 0, "reads are whole sectors");
            let bytes = self.bytes.lock().unwrap();
            output.copy_from_slice(&bytes[offset as usize..offset as usize + output.len()]);
            Ok(())
        }

        fn size_bytes(&self) -> u64 {
            self.bytes.lock().unwrap().len() as u64
        }
    }

    impl BlockDevice for Memory {
        fn write_at(&self, offset: u64, data: &[u8]) -> fs_core::Result<()> {
            assert_eq!(offset % 512, 0, "writes are whole sectors");
            assert_eq!(data.len() % 512, 0, "writes are whole sectors");
            self.writes.lock().unwrap().push((offset, data.len()));
            self.bytes.lock().unwrap()[offset as usize..offset as usize + data.len()]
                .copy_from_slice(data);
            Ok(())
        }

        fn is_writable(&self) -> bool {
            true
        }
    }

    fn cache() -> Cache<Memory> {
        Cache::new(Memory {
            bytes: Mutex::new((0..8192).map(|index| index as u8).collect()),
            writes: Mutex::new(Vec::new()),
        })
    }

    #[test]
    fn held_writes_are_read_back_and_reach_the_device_only_on_flush() {
        let cache = cache();
        cache.write(510, &[0xaa; 4]).unwrap();
        let mut output = [0; 8];
        cache.read(508, &mut output).unwrap();
        assert_eq!(output, [252, 253, 0xaa, 0xaa, 0xaa, 0xaa, 2, 3]);
        assert!(cache.device().writes.lock().unwrap().is_empty());
        cache.flush_dirty().unwrap();
        assert_eq!(*cache.device().writes.lock().unwrap(), [(0, 1024)]);
        assert!(!cache.has_dirty());
        assert_eq!(cache.device().bytes.lock().unwrap()[511], 0xaa);
    }

    #[test]
    fn immediate_writes_update_held_sectors_so_a_flush_cannot_undo_them() {
        let cache = cache();
        cache.write(1024, &[1; 8]).unwrap();
        cache.write_through(1020, &[2; 600]).unwrap();
        cache.flush_dirty().unwrap();
        let bytes = cache.device().bytes.lock().unwrap();
        assert_eq!(&bytes[1019..1021], &[251, 2]);
        assert!(bytes[1020..1620].iter().all(|byte| *byte == 2));
        assert_eq!(bytes[1620], (1620 % 256) as u8);
    }

    #[test]
    fn writes_past_the_device_are_refused_before_any_io() {
        let cache = cache();
        assert!(matches!(cache.write(8190, &[0; 4]), Err(Error::Bounds)));
        assert!(matches!(
            cache.write_through(u64::MAX, &[0]),
            Err(Error::Bounds)
        ));
        assert!(cache.device().writes.lock().unwrap().is_empty());
    }
}
