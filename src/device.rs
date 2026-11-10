use crate::{Error, Result};
use fs_core::{BlockRead, ReadOnlyDevice};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) struct AlignedDevice<Device: BlockRead> {
    device: ReadOnlyDevice<Device>,
    size: u64,
    alignment: AtomicUsize,
}

impl<Device: BlockRead> AlignedDevice<Device> {
    pub(crate) fn new(device: Device) -> Self {
        Self {
            size: device.size_bytes(),
            device: ReadOnlyDevice::new(device),
            alignment: AtomicUsize::new(512),
        }
    }
}

impl<Device: BlockRead> BlockRead for AlignedDevice<Device> {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        let length = output.len() as u64;
        let bounds = || fs_core::Error::OutOfBounds {
            offset,
            len: length,
            size: self.size,
        };
        if offset.checked_add(output.len() as u64).ok_or_else(bounds)? > self.size {
            return Err(bounds());
        }
        if output.is_empty() {
            return Ok(());
        }
        let mut alignment = self.alignment.load(Ordering::Relaxed);
        let mut position = offset;
        let mut copied = 0;
        let mut block = Vec::new();
        while copied < output.len() {
            let aligned = position / alignment as u64 * alignment as u64;
            if aligned.checked_add(alignment as u64).ok_or_else(bounds)? > self.size {
                return Err(bounds());
            }
            let direct = position == aligned && (output.len() - copied).is_multiple_of(alignment);
            let result = if direct {
                self.device.read_at(position, &mut output[copied..])
            } else {
                if block.len() < alignment {
                    block
                        .try_reserve_exact(alignment - block.len())
                        .map_err(|error| {
                            fs_core::Error::Custom(format!("device alignment buffer: {error}"))
                        })?;
                    block.resize(alignment, 0);
                }
                self.device.read_at(aligned, &mut block[..alignment])
            };
            match result {
                Ok(()) => {
                    self.alignment.fetch_max(alignment, Ordering::Relaxed);
                    if direct {
                        return Ok(());
                    }
                    let start = (position - aligned) as usize;
                    let count = (alignment - start).min(output.len() - copied);
                    output[copied..copied + count].copy_from_slice(&block[start..start + count]);
                    copied += count;
                    position += count as u64;
                }
                Err(fs_core::Error::Io(error))
                    if error.kind() == std::io::ErrorKind::InvalidInput =>
                {
                    let next = alignment.checked_mul(2).filter(|next| {
                        let next = *next as u64;
                        let aligned = position / next * next;
                        aligned
                            .checked_add(next)
                            .is_some_and(|end| end <= self.size)
                    });
                    match next {
                        Some(next) => alignment = next,
                        None => return Err(fs_core::Error::Io(error)),
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.size
    }
}

pub struct Reader<Device: BlockRead> {
    device: AlignedDevice<Device>,
    size: u64,
}

impl<Device: BlockRead> Reader<Device> {
    pub fn new(device: Device) -> Self {
        let size = device.size_bytes();
        Self {
            device: AlignedDevice::new(device),
            size,
        }
    }

    pub fn size_bytes(&self) -> u64 {
        self.size
    }

    pub fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<()> {
        let end = offset
            .checked_add(output.len() as u64)
            .ok_or(Error::Bounds)?;
        if end > self.size {
            return Err(Error::Bounds);
        }
        if output.is_empty() {
            return Ok(());
        }
        let mut position = offset;
        let mut copied = 0;
        let mut sector = [0; 512];
        while copied < output.len() {
            let aligned = position / 512 * 512;
            if aligned.checked_add(512).ok_or(Error::Bounds)? > self.size {
                return Err(Error::Bounds);
            }
            self.device.read_at(aligned, &mut sector)?;
            let start = (position - aligned) as usize;
            let count = (512 - start).min(output.len() - copied);
            output[copied..copied + count].copy_from_slice(&sector[start..start + count]);
            copied += count;
            position += count as u64;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Device {
        size: u64,
        sector: usize,
        calls: AtomicUsize,
    }

    impl BlockRead for Device {
        fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            assert!(offset + output.len() as u64 <= self.size);
            if !offset.is_multiple_of(self.sector as u64)
                || !output.len().is_multiple_of(self.sector)
            {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput).into());
            }
            for (index, byte) in output.iter_mut().enumerate() {
                *byte = ((offset + index as u64) % 251) as u8;
            }
            Ok(())
        }

        fn size_bytes(&self) -> u64 {
            self.size
        }
    }

    fn device(size: u64, sector: usize) -> AlignedDevice<Device> {
        AlignedDevice::new(Device {
            size,
            sector,
            calls: AtomicUsize::new(0),
        })
    }

    #[test]
    fn aligned_adapter_bounds_are_checked_before_io() {
        let device = device(513, 512);
        assert_eq!(device.size_bytes(), 513);
        device.read_at(513, &mut []).unwrap();
        for (offset, length) in [(514, 0), (513, 1), (u64::MAX, 1), (512, 1)] {
            let mut output = vec![0xa5; length];
            assert!(matches!(
                device.read_at(offset, &mut output),
                Err(fs_core::Error::OutOfBounds { .. })
            ));
            assert_eq!(output, vec![0xa5; length]);
        }
        assert_eq!(device.device.inner().calls.load(Ordering::Relaxed), 0);
        let reader = Reader::new(device);
        assert!(matches!(reader.read_at(512, &mut [0]), Err(Error::Bounds)));
    }

    #[test]
    fn aligned_adapter_copies_ranges_across_device_blocks() {
        for sector in [512, 4096] {
            let device = device((sector * 3) as u64, sector);
            let offset = (sector - 3) as u64;
            let mut output = vec![0; sector + 7];
            device.read_at(offset, &mut output).unwrap();
            for (index, byte) in output.into_iter().enumerate() {
                assert_eq!(byte, ((offset + index as u64) % 251) as u8);
            }
            assert_eq!(device.alignment.load(Ordering::Relaxed), sector);
        }
    }

    #[test]
    fn alignment_buffer_allocation_failures_are_reported() {
        let device = device(u64::MAX, 512);
        device
            .alignment
            .store(1usize << (usize::BITS - 1), Ordering::Relaxed);
        let error = device.read_at(0, &mut [0]).unwrap_err();
        assert!(
            matches!(error, fs_core::Error::Custom(message) if message.starts_with("device alignment buffer:"))
        );
        assert!(matches!(
            device.read_at(u64::MAX - 1, &mut [0]),
            Err(fs_core::Error::OutOfBounds { .. })
        ));
        assert_eq!(device.device.inner().calls.load(Ordering::Relaxed), 0);
    }
}
