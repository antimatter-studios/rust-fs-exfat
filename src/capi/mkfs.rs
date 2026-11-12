//! The formatter's C ABI, mirrored by `include/fs_exfat.h`. Each entry point
//! formats a whole device, or its first `size_bytes`, and returns 0, or -1
//! with `fs_exfat_last_error` set.
//!
//! A callback device is the writable mount's `fs_exfat_rw_callbacks`; its
//! context must stay valid for the call and support its reads, writes and
//! flushes. Every fallible boundary catches Rust panics.

use super::{
    guard, string, CallbackDevice, CallbackRead, FsExfatRwCallbacks, EINVAL, EIO, EROFS, LAST_ERRNO,
};
use crate::mkfs::{self, FormatError, FormatOptions, Layout};
use crate::{Error, Result};
use fs_core::ffi::set_last_error;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use fs_core::FileDevice;
use std::ffi::{c_char, c_int};
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use std::path::Path;

/// Format options. A zero field takes its default; NULL options take every
/// default.
#[repr(C)]
pub struct FsExfatFormatOptions {
    /// Bytes to format; 0 is the whole device.
    pub size_bytes: u64,
    /// FAT and cluster-heap alignment in bytes; 0 is the cluster size.
    pub alignment: u64,
    /// PartitionOffset in sectors; 0 tells readers to ignore it.
    pub partition_offset: u64,
    /// 512, 1024, 2048 or 4096; 0 is 512.
    pub bytes_per_sector: u32,
    /// A power of two up to 32 MiB; 0 chooses by volume size.
    pub cluster_size: u32,
    pub volume_serial: u32,
    /// Nonzero uses `volume_serial`; zero takes it from the clock.
    pub serial_present: u8,
    /// 0 or 1 for one FAT; 2 for TexFAT's two.
    pub number_of_fats: u8,
    /// Nonzero writes a Volume GUID entry holding `guid`.
    pub guid_present: u8,
    pub reserved: u8,
    pub guid: [u8; 16],
    /// NUL-terminated UTF-8; NULL writes no Volume Label entry.
    pub label: *const c_char,
}

/// # Safety
/// `options` must be NULL or valid, with `label` NULL or a NUL-terminated string.
unsafe fn options(options: *const FsExfatFormatOptions) -> Result<FormatOptions> {
    let Some(options) = (unsafe { options.as_ref() }) else {
        return Ok(FormatOptions::default());
    };
    let nonzero = |value: u32| (value != 0).then_some(value);
    Ok(FormatOptions {
        size_bytes: (options.size_bytes != 0).then_some(options.size_bytes),
        bytes_per_sector: nonzero(options.bytes_per_sector),
        cluster_size: nonzero(options.cluster_size),
        alignment: (options.alignment != 0).then_some(options.alignment),
        number_of_fats: options.number_of_fats.max(1),
        label: if options.label.is_null() {
            None
        } else {
            Some(unsafe { string(options.label)? }.to_owned())
        },
        volume_serial: (options.serial_present != 0).then_some(options.volume_serial),
        guid: (options.guid_present != 0).then_some(options.guid),
        partition_offset: options.partition_offset,
    })
}

/// Runs `body` at the ABI boundary. A refused or failed format sets the
/// thread's last error and errno as any other failure does: EINVAL for a
/// refused layout, EROFS for a read-only device, EIO for any other device
/// failure.
fn run(body: impl FnOnce() -> Result<std::result::Result<Layout, FormatError>>) -> c_int {
    let mut failure = None;
    let code = guard(-1, || match body()? {
        Ok(_) => Ok(0),
        Err(error) => {
            failure = Some(error);
            Ok(-1)
        }
    });
    if let Some(error) = failure {
        let errno = match &error {
            FormatError::Device(fs_core::Error::ReadOnly) => EROFS,
            FormatError::Device(_) => EIO,
            _ => EINVAL,
        };
        LAST_ERRNO.with(|value| value.set(errno));
        set_last_error(error.to_string());
    }
    code
}

/// # Safety
/// `path` must be a NUL-terminated UTF-8 string naming an existing file or
/// device; `options` must be NULL or valid.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_mkfs(
    path: *const c_char,
    options: *const FsExfatFormatOptions,
) -> c_int {
    run(|| {
        let path = Path::new(unsafe { string(path)? });
        let options = unsafe { self::options(options)? };
        let device = FileDevice::open_rw(path)?;
        Ok(mkfs::format(&device, &options))
    })
}

/// # Safety
/// `device` and its callbacks must meet the module's contract; `options`
/// must be NULL or valid.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_mkfs_with_callbacks(
    device: *const FsExfatRwCallbacks,
    options: *const FsExfatFormatOptions,
) -> c_int {
    run(|| {
        let device = unsafe { device.as_ref() }.ok_or(Error::Invalid("null device callbacks"))?;
        let device = CallbackDevice {
            read: CallbackRead {
                context: device.context as usize,
                size: device.size_bytes,
                read: device.read.ok_or(Error::Invalid("missing read callback"))?,
            },
            write: device
                .write
                .ok_or(Error::Invalid("missing write callback"))?,
            flush: device.flush,
        };
        let options = unsafe { self::options(options)? };
        Ok(mkfs::format(&device, &options))
    })
}
