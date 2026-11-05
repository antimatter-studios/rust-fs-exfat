//! Read-only C ABI, mirrored by `include/fs_exfat.h`.
//!
//! Mount handles are owned until unmount. Directory handles own their entries and
//! may outlive the mount. Callers serialize mutation of each directory handle.
//! Callback contexts must remain valid and support concurrent reads until unmount.
//! Pointer arguments must reference valid, correctly aligned memory of the stated
//! length. Buffers cannot overlap handles or callback storage. No write callback
//! or writable mount exists. Every fallible boundary catches Rust panics.

use crate::directory::DirectoryEntry;
use crate::interface;
use crate::path::Node;
use crate::timestamp::Timestamp;
use crate::volume::Volume;
use crate::{Error, Result};
use fs_core::ffi::{ffi_guard_cleanup, ffi_guard_or, fs_core_last_error_message, set_last_error};
use fs_core::{BlockRead, OwnedSlice};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::path::Path;
use std::ptr;

pub type ReadCallback = unsafe extern "C" fn(*mut c_void, u64, *mut u8, usize) -> c_int;

#[repr(C)]
pub struct FsExfatCallbacks {
    pub context: *mut c_void,
    pub size_bytes: u64,
    pub read: Option<ReadCallback>,
}

struct CallbackRead {
    context: usize,
    size: u64,
    read: ReadCallback,
}

impl BlockRead for CallbackRead {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        let code = unsafe {
            (self.read)(
                self.context as *mut c_void,
                offset,
                output.as_mut_ptr(),
                output.len(),
            )
        };
        if code != 0 {
            return Err(fs_core::Error::Custom(format!(
                "read callback failed with status {code}"
            )));
        }
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.size
    }
}

pub struct FsExfat {
    volume: Volume<OwnedSlice>,
}

pub struct FsExfatDirectory {
    entries: Vec<DirectoryEntry>,
    next: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FsExfatTimestamp {
    pub unix_milliseconds: i64,
    pub year: u16,
    pub millisecond: u16,
    pub utc_offset_minutes: i16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub offset_known: u8,
    pub present: u8,
    pub reserved: [u8; 3],
}

impl From<Timestamp> for FsExfatTimestamp {
    fn from(value: Timestamp) -> Self {
        Self {
            unix_milliseconds: value.unix_milliseconds().unwrap_or(0),
            year: value.year,
            millisecond: value.millisecond,
            utc_offset_minutes: value.utc_offset_minutes.unwrap_or(0),
            month: value.month,
            day: value.day,
            hour: value.hour,
            minute: value.minute,
            second: value.second,
            offset_known: u8::from(value.utc_offset_minutes.is_some()),
            present: 1,
            reserved: [0; 3],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FsExfatStat {
    pub data_length: u64,
    pub valid_data_length: u64,
    pub first_cluster: u32,
    pub attributes: u16,
    pub is_directory: u8,
    pub no_fat_chain: u8,
    pub created: FsExfatTimestamp,
    pub modified: FsExfatTimestamp,
    pub accessed: FsExfatTimestamp,
}

impl From<&DirectoryEntry> for FsExfatStat {
    fn from(entry: &DirectoryEntry) -> Self {
        Self {
            data_length: entry.stream().data_length(),
            valid_data_length: entry.stream().valid_data_length(),
            first_cluster: entry.stream().first_cluster(),
            attributes: entry.attributes(),
            is_directory: u8::from(entry.is_directory()),
            no_fat_chain: u8::from(entry.stream().no_fat_chain()),
            created: entry.timestamps().created.into(),
            modified: entry.timestamps().modified.into(),
            accessed: entry.timestamps().accessed.into(),
        }
    }
}

#[repr(C)]
pub struct FsExfatDirEntry {
    pub stat: FsExfatStat,
    pub name: [c_char; 1021],
    pub reserved: [u8; 3],
}

#[repr(C)]
pub struct FsExfatVolumeInfo {
    pub volume_bytes: u64,
    pub cluster_size: u64,
    pub cluster_count: u32,
    pub volume_serial: u32,
    pub volume_flags: u16,
    pub sector_size: u16,
    pub guid: [u8; 16],
    pub guid_present: u8,
    pub label: [c_char; 45],
    pub reserved: [u8; 6],
}

fn guard<Value: Copy>(fallback: Value, body: impl FnOnce() -> Result<Value>) -> Value {
    ffi_guard_or(fallback, || match body() {
        Ok(value) => value,
        Err(error) => {
            set_last_error(error.to_string());
            fallback
        }
    })
}

unsafe fn string<'value>(pointer: *const c_char) -> Result<&'value str> {
    if pointer.is_null() {
        return Err(Error::Invalid("null string argument"));
    }
    unsafe { CStr::from_ptr(pointer) }
        .to_str()
        .map_err(|_| Error::Invalid("string is not UTF-8"))
}

/// # Safety
/// `path` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_mount(path: *const c_char, offset: u64) -> *mut FsExfat {
    guard(ptr::null_mut(), || {
        let volume = interface::open_path(Path::new(unsafe { string(path)? }), offset)?;
        Ok(Box::into_raw(Box::new(FsExfat { volume })))
    })
}

/// # Safety
/// `config` and its read callback must meet the module's lifetime and read contract.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_mount_with_callbacks(
    config: *const FsExfatCallbacks,
    offset: u64,
) -> *mut FsExfat {
    guard(ptr::null_mut(), || {
        let config =
            unsafe { config.as_ref() }.ok_or(Error::Invalid("null callback configuration"))?;
        let device = CallbackRead {
            context: config.context as usize,
            size: config.size_bytes,
            read: config.read.ok_or(Error::Invalid("missing read callback"))?,
        };
        let volume = interface::open_device(device, offset)?;
        Ok(Box::into_raw(Box::new(FsExfat { volume })))
    })
}

/// # Safety
/// `handle` must be NULL or a live mount, exclusively owned and closed exactly once.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_umount(handle: *mut FsExfat) {
    ffi_guard_cleanup(|| {
        if !handle.is_null() {
            drop(unsafe { Box::from_raw(handle) });
        }
    });
}

/// # Safety
/// The mount must be valid and `output` writable for one volume information record.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_get_volume_info(
    handle: *const FsExfat,
    output: *mut FsExfatVolumeInfo,
) -> c_int {
    guard(-1, || {
        let handle = unsafe { handle.as_ref() }.ok_or(Error::Invalid("null mount"))?;
        if output.is_null() {
            return Err(Error::Invalid("null volume information output"));
        }
        let boot = handle.volume.boot();
        let metadata = handle.volume.metadata();
        let mut value = FsExfatVolumeInfo {
            volume_bytes: boot
                .volume_length
                .checked_mul(boot.bytes_per_sector())
                .ok_or(Error::Bounds)?,
            cluster_size: boot.cluster_size(),
            cluster_count: boot.cluster_count,
            volume_serial: boot.volume_serial,
            volume_flags: boot.volume_flags,
            sector_size: boot.bytes_per_sector() as u16,
            guid: metadata.guid.unwrap_or([0; 16]),
            guid_present: u8::from(metadata.guid.is_some()),
            label: [0; 45],
            reserved: [0; 6],
        };
        if let Some(label) = &metadata.label {
            if label.len() >= value.label.len() || label.contains('\0') {
                return Err(Error::Invalid(
                    "volume label cannot be represented as an ABI string",
                ));
            }
            for (slot, byte) in value.label.iter_mut().zip(label.bytes()) {
                *slot = byte as c_char;
            }
        }
        unsafe { output.write(value) };
        Ok(0)
    })
}

/// # Safety
/// The mount and path must be valid; `output` must be writable for one stat record.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_stat(
    handle: *const FsExfat,
    path: *const c_char,
    output: *mut FsExfatStat,
) -> c_int {
    guard(-1, || {
        let handle = unsafe { handle.as_ref() }.ok_or(Error::Invalid("null mount"))?;
        if output.is_null() {
            return Err(Error::Invalid("null stat output"));
        }
        let node = handle.volume.lookup(unsafe { string(path)? })?;
        let value = match node {
            Node::Root => FsExfatStat {
                first_cluster: handle.volume.boot().root_cluster,
                attributes: 0x10,
                is_directory: 1,
                ..FsExfatStat::default()
            },
            Node::Entry(entry) => (&entry).into(),
        };
        unsafe { output.write(value) };
        Ok(0)
    })
}

/// # Safety
/// The mount and NUL-terminated UTF-8 path must be valid for this call.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_dir_open(
    handle: *const FsExfat,
    path: *const c_char,
) -> *mut FsExfatDirectory {
    guard(ptr::null_mut(), || {
        let handle = unsafe { handle.as_ref() }.ok_or(Error::Invalid("null mount"))?;
        let entries = handle
            .volume
            .read_directory_path(unsafe { string(path)? })?;
        Ok(Box::into_raw(Box::new(FsExfatDirectory {
            entries,
            next: 0,
        })))
    })
}

/// # Safety
/// The directory must be exclusively borrowed and `output` writable for one record.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_dir_next(
    handle: *mut FsExfatDirectory,
    output: *mut FsExfatDirEntry,
) -> c_int {
    guard(-1, || {
        let directory = unsafe { handle.as_mut() }.ok_or(Error::Invalid("null directory"))?;
        if output.is_null() {
            return Err(Error::Invalid("null directory output"));
        }
        let Some(entry) = directory.entries.get(directory.next) else {
            return Ok(0);
        };
        let mut value = FsExfatDirEntry {
            stat: entry.into(),
            name: [0; 1021],
            reserved: [0; 3],
        };
        if entry.name().len() >= value.name.len() {
            return Err(Error::Invalid("directory name exceeds ABI buffer"));
        }
        for (slot, byte) in value.name.iter_mut().zip(entry.name().bytes()) {
            *slot = byte as c_char;
        }
        unsafe { output.write(value) };
        directory.next += 1;
        Ok(1)
    })
}

/// # Safety
/// `handle` must be NULL or an exclusively owned live directory, closed once.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_dir_close(handle: *mut FsExfatDirectory) {
    ffi_guard_cleanup(|| {
        if !handle.is_null() {
            drop(unsafe { Box::from_raw(handle) });
        }
    });
}

/// # Safety
/// The mount and path must be valid; `output` must cover `length` writable bytes.
/// A NULL output is permitted only for a zero-length request.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_read_file(
    handle: *const FsExfat,
    path: *const c_char,
    offset: u64,
    output: *mut u8,
    length: usize,
) -> i64 {
    guard(-1, || {
        let handle = unsafe { handle.as_ref() }.ok_or(Error::Invalid("null mount"))?;
        if (output.is_null() && length != 0) || length > isize::MAX as usize {
            return Err(Error::Invalid("invalid read output range"));
        }
        let entry = match handle.volume.lookup(unsafe { string(path)? })? {
            Node::Entry(entry) if !entry.is_directory() => entry,
            _ => return Err(Error::Invalid("path is a directory")),
        };
        let file = handle.volume.open_file(&entry)?;
        let bytes = if length == 0 {
            &mut []
        } else {
            unsafe { std::slice::from_raw_parts_mut(output, length) }
        };
        Ok(file.read_at(offset, bytes)? as i64)
    })
}

#[no_mangle]
pub extern "C" fn fs_exfat_last_error() -> *const c_char {
    fs_core_last_error_message()
}

#[cfg(test)]
mod tests {
    #[test]
    fn panic_boundary_returns_a_diagnostic() {
        assert_eq!(super::guard(-1, || panic!("ABI boundary test")), -1);
        let message = unsafe { std::ffi::CStr::from_ptr(super::fs_exfat_last_error()) };
        assert!(message.to_str().unwrap().contains("ABI boundary test"));
    }
}
