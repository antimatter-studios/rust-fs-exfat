use crate::device::AlignedDevice;
use crate::volume::Volume;
use crate::{Error, Result};
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use fs_core::FileDevice;
use fs_core::{BlockRead, OwnedSlice};
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
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
    let device: Arc<dyn BlockRead> = Arc::new(AlignedDevice::new(device));
    Volume::open(OwnedSlice::new(device, offset, length))
}

/// Opens a volume from a file or device node. Not on the browser build
/// (wasm32-unknown-unknown), which has no files: use [`open_device`].
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn open_path(path: &Path, offset: u64) -> Result<Volume<OwnedSlice>> {
    open_device(FileDevice::open(path)?, offset)
}
