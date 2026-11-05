use crate::volume::Volume;
use crate::{Error, Result};
use fs_core::{BlockRead, FileDevice, OwnedSlice, ReadOnlyDevice};
use std::path::Path;
use std::sync::Arc;

pub fn open_device<Device: BlockRead + 'static>(
    device: Device,
    offset: u64,
) -> Result<Volume<OwnedSlice>> {
    if !offset.is_multiple_of(512) || offset >= device.size_bytes() {
        return Err(Error::Invalid(
            "volume offset must be aligned and within the device",
        ));
    }
    let length = device.size_bytes() - offset;
    let device: Arc<dyn BlockRead> = Arc::new(ReadOnlyDevice::new(device));
    Volume::open(OwnedSlice::new(device, offset, length))
}

pub fn open_path(path: &Path, offset: u64) -> Result<Volume<OwnedSlice>> {
    open_device(FileDevice::open(path)?, offset)
}
