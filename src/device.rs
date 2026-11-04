use crate::{Error, Result};
use fs_core::{BlockRead, ReadOnlyDevice};

pub struct Reader<Device: BlockRead> {
    device: ReadOnlyDevice<Device>,
    size: u64,
}

impl<Device: BlockRead> Reader<Device> {
    pub fn new(device: Device) -> Self {
        let size = device.size_bytes();
        Self {
            device: ReadOnlyDevice::new(device),
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
