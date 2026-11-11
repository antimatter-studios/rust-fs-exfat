//! C ABI, mirrored by `include/fs_exfat.h`.
//!
//! Mount handles are owned until unmount. Directory handles own their entries and
//! may outlive the mount. Callers serialize mutation of each directory handle.
//! Callback contexts must remain valid and support concurrent reads until unmount.
//! Pointer arguments must reference valid, correctly aligned memory of the stated
//! length. Buffers cannot overlap handles or callback storage. A read-only mount
//! never writes; a mount made by a `_rw` constructor writes through
//! `fs_exfat::write` and serializes its calls with a lock. Every fallible
//! boundary catches Rust panics.

use crate::boot::BootSector;
use crate::directory::DirectoryEntry;
use crate::interface;
use crate::metadata::RootMetadata;
use crate::path::Node;
use crate::timestamp::Timestamp;
use crate::volume::Volume;
use crate::write::{self, WriteVolume};
use crate::{Error, Result};
use fs_core::ffi::{ffi_guard_cleanup, ffi_guard_or, fs_core_last_error_message, set_last_error};
use fs_core::{BlockDevice, BlockRead, OwnedRwSlice, OwnedSlice};
use std::cell::Cell;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::path::Path;
use std::ptr;
use std::sync::{Mutex, MutexGuard};

const ENOENT: c_int = 2;
const EIO: c_int = 5;
const EEXIST: c_int = 17;
const ENOTDIR: c_int = 20;
const EISDIR: c_int = 21;
const EINVAL: c_int = 22;
const ENOSPC: c_int = 28;
const EROFS: c_int = 30;
#[cfg(target_os = "linux")]
const ENOTEMPTY: c_int = 39;
#[cfg(not(target_os = "linux"))]
const ENOTEMPTY: c_int = 66;
#[cfg(target_os = "linux")]
const ENOTSUP: c_int = 95;
#[cfg(not(target_os = "linux"))]
const ENOTSUP: c_int = 45;

thread_local! {
    static LAST_ERRNO: Cell<c_int> = const { Cell::new(0) };
}

/// The errno value that matches an error, for callers such as filesystem
/// bridges that report one.
fn errno_of(error: &Error) -> c_int {
    match error {
        Error::NotFound => ENOENT,
        Error::NotDirectory => ENOTDIR,
        Error::Exists => EEXIST,
        Error::IsDirectory => EISDIR,
        Error::NotEmpty => ENOTEMPTY,
        Error::NoSpace => ENOSPC,
        Error::Device(fs_core::Error::ReadOnly) => EROFS,
        Error::Device(_) => EIO,
        Error::Unsupported(_) => ENOTSUP,
        Error::Invalid(_) | Error::Bounds => EINVAL,
    }
}

pub type ReadCallback = unsafe extern "C" fn(*mut c_void, u64, *mut u8, usize) -> c_int;
pub type WriteCallback = unsafe extern "C" fn(*mut c_void, u64, *const u8, usize) -> c_int;
pub type FlushCallback = unsafe extern "C" fn(*mut c_void) -> c_int;

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

/// Callbacks for a writable mount. `flush` may be NULL when the source has
/// nothing to make durable.
#[repr(C)]
pub struct FsExfatRwCallbacks {
    pub context: *mut c_void,
    pub size_bytes: u64,
    pub read: Option<ReadCallback>,
    pub write: Option<WriteCallback>,
    pub flush: Option<FlushCallback>,
}

struct CallbackDevice {
    read: CallbackRead,
    write: WriteCallback,
    flush: Option<FlushCallback>,
}

impl BlockRead for CallbackDevice {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        self.read.read_at(offset, output)
    }

    fn size_bytes(&self) -> u64 {
        self.read.size
    }
}

impl BlockDevice for CallbackDevice {
    fn write_at(&self, offset: u64, data: &[u8]) -> fs_core::Result<()> {
        let code = unsafe {
            (self.write)(
                self.read.context as *mut c_void,
                offset,
                data.as_ptr(),
                data.len(),
            )
        };
        if code != 0 {
            return Err(fs_core::Error::Custom(format!(
                "write callback failed with status {code}"
            )));
        }
        Ok(())
    }

    fn flush(&self) -> fs_core::Result<()> {
        let Some(flush) = self.flush else {
            return Ok(());
        };
        let code = unsafe { flush(self.read.context as *mut c_void) };
        if code != 0 {
            return Err(fs_core::Error::Custom(format!(
                "flush callback failed with status {code}"
            )));
        }
        Ok(())
    }

    fn is_writable(&self) -> bool {
        true
    }
}

/// What the read calls need from a mount, whichever kind it is.
trait Reading {
    fn boot(&self) -> &BootSector;
    fn metadata(&self) -> &RootMetadata;
    fn lookup(&self, path: &str) -> Result<Node>;
    fn read_directory_path(&self, path: &str) -> Result<Vec<DirectoryEntry>>;
    fn read_file(&self, entry: &DirectoryEntry, offset: u64, output: &mut [u8]) -> Result<usize>;
}

impl<Device: BlockRead> Reading for Volume<Device> {
    fn boot(&self) -> &BootSector {
        Volume::boot(self)
    }

    fn metadata(&self) -> &RootMetadata {
        Volume::metadata(self)
    }

    fn lookup(&self, path: &str) -> Result<Node> {
        Volume::lookup(self, path)
    }

    fn read_directory_path(&self, path: &str) -> Result<Vec<DirectoryEntry>> {
        Volume::read_directory_path(self, path)
    }

    fn read_file(&self, entry: &DirectoryEntry, offset: u64, output: &mut [u8]) -> Result<usize> {
        self.open_file(entry)?.read_at(offset, output)
    }
}

enum Mount {
    ReadOnly(Volume<OwnedSlice>),
    Writable(Mutex<WriteVolume<OwnedRwSlice>>),
}

pub struct FsExfat {
    mount: Mount,
}

impl FsExfat {
    fn read<Value>(&self, body: impl FnOnce(&dyn Reading) -> Result<Value>) -> Result<Value> {
        match &self.mount {
            Mount::ReadOnly(volume) => body(volume),
            Mount::Writable(volume) => body(lock(volume).reader()),
        }
    }

    fn writable(&self) -> Result<MutexGuard<'_, WriteVolume<OwnedRwSlice>>> {
        match &self.mount {
            Mount::ReadOnly(_) => Err(Error::Device(fs_core::Error::ReadOnly)),
            Mount::Writable(volume) => Ok(lock(volume)),
        }
    }
}

fn lock<Value>(mutex: &Mutex<Value>) -> MutexGuard<'_, Value> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn handle_of(mount: Mount) -> *mut FsExfat {
    Box::into_raw(Box::new(FsExfat { mount }))
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
    // A panic leaves EIO behind; every returning path overwrites it.
    LAST_ERRNO.with(|errno| errno.set(EIO));
    ffi_guard_or(fallback, || match body() {
        Ok(value) => {
            LAST_ERRNO.with(|errno| errno.set(0));
            value
        }
        Err(error) => {
            LAST_ERRNO.with(|errno| errno.set(errno_of(&error)));
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
        Ok(handle_of(Mount::ReadOnly(volume)))
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
        Ok(handle_of(Mount::ReadOnly(volume)))
    })
}

/// Opens a UTF-8 image or device path read-write and mounts the volume at a
/// 512-byte-aligned `offset` for writing. VolumeDirty is set until unmount.
///
/// # Safety
/// `path` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_mount_rw(path: *const c_char, offset: u64) -> *mut FsExfat {
    guard(ptr::null_mut(), || {
        let volume = write::open_path(Path::new(unsafe { string(path)? }), offset)?;
        Ok(handle_of(Mount::Writable(Mutex::new(volume))))
    })
}

/// Mounts the volume at `offset` for writing through caller callbacks.
///
/// # Safety
/// `config` and its callbacks must meet the module's lifetime contract; the
/// context must accept concurrent reads and serialized writes until unmount.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_mount_rw_with_callbacks(
    config: *const FsExfatRwCallbacks,
    offset: u64,
) -> *mut FsExfat {
    guard(ptr::null_mut(), || {
        let config =
            unsafe { config.as_ref() }.ok_or(Error::Invalid("null callback configuration"))?;
        let device = CallbackDevice {
            read: CallbackRead {
                context: config.context as usize,
                size: config.size_bytes,
                read: config.read.ok_or(Error::Invalid("missing read callback"))?,
            },
            write: config
                .write
                .ok_or(Error::Invalid("missing write callback"))?,
            flush: config.flush,
        };
        let volume = write::open_device(device, offset)?;
        Ok(handle_of(Mount::Writable(Mutex::new(volume))))
    })
}

/// Writes every pending change of a writable mount and flushes its source.
/// Returns 0, or -1 with a diagnostic; a read-only mount is an error.
///
/// # Safety
/// `handle` must be a live mount.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_flush(handle: *const FsExfat) -> c_int {
    guard(-1, || {
        let handle = unsafe { handle.as_ref() }.ok_or(Error::Invalid("null mount"))?;
        handle.writable()?.flush()?;
        Ok(0)
    })
}

/// Unmounts and reports whether the unmount was clean: 0 when a writable
/// mount was flushed and VolumeDirty cleared (and always for a read-only
/// mount), -1 with a diagnostic otherwise. The handle is closed either way.
///
/// # Safety
/// `handle` must be NULL or a live mount, exclusively owned and closed exactly once.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_umount_checked(handle: *mut FsExfat) -> c_int {
    guard(-1, || {
        if handle.is_null() {
            return Ok(0);
        }
        let handle = unsafe { Box::from_raw(handle) };
        match handle.mount {
            Mount::ReadOnly(_) => Ok(0),
            Mount::Writable(volume) => {
                volume
                    .into_inner()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .unmount()?;
                Ok(0)
            }
        }
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
        let value = handle.read(|volume| volume_info(volume.boot(), volume.metadata()))?;
        unsafe { output.write(value) };
        Ok(0)
    })
}

fn volume_info(boot: &BootSector, metadata: &RootMetadata) -> Result<FsExfatVolumeInfo> {
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
    Ok(value)
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
        let path = unsafe { string(path)? };
        let (node, root_cluster) =
            handle.read(|volume| Ok((volume.lookup(path)?, volume.boot().root_cluster)))?;
        let value = match node {
            Node::Root => FsExfatStat {
                first_cluster: root_cluster,
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
        let path = unsafe { string(path)? };
        let entries = handle.read(|volume| volume.read_directory_path(path))?;
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
        let path = unsafe { string(path)? };
        let bytes = if length == 0 {
            &mut []
        } else {
            unsafe { std::slice::from_raw_parts_mut(output, length) }
        };
        let count = handle.read(|volume| {
            let entry = match volume.lookup(path)? {
                Node::Entry(entry) if !entry.is_directory() => entry,
                _ => return Err(Error::Invalid("path is a directory")),
            };
            volume.read_file(&entry, offset, bytes)
        })?;
        Ok(count as i64)
    })
}

#[no_mangle]
pub extern "C" fn fs_exfat_last_error() -> *const c_char {
    fs_core_last_error_message()
}

/// The errno value for the last fallible call on this thread: 0 after
/// success, ENOENT, ENOTDIR, EEXIST, EISDIR, ENOTEMPTY, ENOSPC, EROFS,
/// ENOTSUP or EINVAL for the matching failure, and EIO for device errors and
/// panics.
#[no_mangle]
pub extern "C" fn fs_exfat_last_errno() -> c_int {
    LAST_ERRNO.with(Cell::get)
}

/// # Safety
/// The mount must be live and `path` a NUL-terminated UTF-8 string.
unsafe fn write_call<Value>(
    handle: *const FsExfat,
    path: *const c_char,
    body: impl FnOnce(&mut WriteVolume<OwnedRwSlice>, &str) -> Result<Value>,
) -> Result<Value> {
    let handle = unsafe { handle.as_ref() }.ok_or(Error::Invalid("null mount"))?;
    let path = unsafe { string(path)? };
    let mut volume = handle.writable()?;
    body(&mut volume, path)
}

/// # Safety
/// `data` must cover `length` readable bytes; NULL is allowed when it is 0.
unsafe fn input<'data>(data: *const u8, length: usize) -> Result<&'data [u8]> {
    if length == 0 {
        return Ok(&[]);
    }
    if data.is_null() || length > isize::MAX as usize {
        return Err(Error::Invalid("invalid input buffer"));
    }
    Ok(unsafe { std::slice::from_raw_parts(data, length) })
}

/// Creates an empty file with Archive set at the current time. Returns 0, or
/// -1 with a diagnostic and errno (EEXIST when the name is taken).
///
/// # Safety
/// The mount must be live and `path` a NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_create(handle: *const FsExfat, path: *const c_char) -> c_int {
    guard(-1, || {
        unsafe { write_call(handle, path, |volume, path| volume.create(path)) }?;
        Ok(0)
    })
}

/// Creates an empty directory. Returns 0, or -1 with a diagnostic and errno.
///
/// # Safety
/// The mount must be live and `path` a NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_mkdir(handle: *const FsExfat, path: *const c_char) -> c_int {
    guard(-1, || {
        unsafe { write_call(handle, path, |volume, path| volume.mkdir(path)) }?;
        Ok(0)
    })
}

/// Writes `length` bytes into an existing file at `offset`, growing it as
/// needed; a gap past the old end reads as zeros. Returns the bytes written,
/// or -1 with a diagnostic and errno.
///
/// # Safety
/// The mount must be live, `path` a NUL-terminated UTF-8 string, and `data`
/// readable for `length` bytes.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_pwrite(
    handle: *const FsExfat,
    path: *const c_char,
    offset: u64,
    data: *const u8,
    length: usize,
) -> i64 {
    guard(-1, || {
        let data = unsafe { input(data, length)? };
        unsafe {
            write_call(handle, path, |volume, path| {
                volume.pwrite(path, offset, data)
            })
        }?;
        Ok(length as i64)
    })
}

/// Sets a file's length: shrinking frees clusters past the end, growing
/// reads as zeros. Returns 0, or -1 with a diagnostic and errno.
///
/// # Safety
/// The mount must be live and `path` a NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_truncate(
    handle: *const FsExfat,
    path: *const c_char,
    length: u64,
) -> c_int {
    guard(-1, || {
        unsafe { write_call(handle, path, |volume, path| volume.truncate(path, length)) }?;
        Ok(0)
    })
}

/// Removes a file. Returns 0, or -1 with a diagnostic and errno (EISDIR for
/// a directory).
///
/// # Safety
/// The mount must be live and `path` a NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_unlink(handle: *const FsExfat, path: *const c_char) -> c_int {
    guard(-1, || {
        unsafe { write_call(handle, path, |volume, path| volume.unlink(path)) }?;
        Ok(0)
    })
}

/// Removes an empty directory. Returns 0, or -1 with a diagnostic and errno
/// (ENOTEMPTY when it holds anything, ENOTDIR for a file).
///
/// # Safety
/// The mount must be live and `path` a NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_rmdir(handle: *const FsExfat, path: *const c_char) -> c_int {
    guard(-1, || {
        unsafe { write_call(handle, path, |volume, path| volume.rmdir(path)) }?;
        Ok(0)
    })
}

/// Makes `path` a file holding exactly `length` bytes of `data`, creating it
/// or replacing its contents. Returns the new size, or -1 with a diagnostic
/// and errno.
///
/// # Safety
/// The mount must be live, `path` a NUL-terminated UTF-8 string, and `data`
/// readable for `length` bytes.
#[no_mangle]
pub unsafe extern "C" fn fs_exfat_write_file(
    handle: *const FsExfat,
    path: *const c_char,
    data: *const u8,
    length: usize,
) -> i64 {
    guard(-1, || {
        let data = unsafe { input(data, length)? };
        unsafe { write_call(handle, path, |volume, path| volume.write_file(path, data)) }?;
        Ok(length as i64)
    })
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
