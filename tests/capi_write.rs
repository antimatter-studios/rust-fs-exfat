//! The writable mounts of the C ABI: by path and through callbacks.

mod write_support;

use fs_exfat::capi::*;
use std::ffi::{c_int, c_void, CStr, CString};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use write_support::{format, Layout};

struct Scratch {
    directory: PathBuf,
    image: PathBuf,
}

impl Scratch {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join(format!(
                "capi-write-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let image = directory.join("volume.img");
        std::fs::write(&image, bytes).unwrap();
        Self { directory, image }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

fn last_error() -> String {
    let message = fs_exfat_last_error();
    if message.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned()
}

fn volume_flags(handle: *const FsExfat) -> u16 {
    let mut information = std::mem::MaybeUninit::uninit();
    assert_eq!(
        unsafe { fs_exfat_get_volume_info(handle, information.as_mut_ptr()) },
        0
    );
    unsafe { information.assume_init() }.volume_flags
}

#[test]
fn a_path_mount_for_writing_is_dirty_until_a_checked_unmount() {
    let (image, _) = format(&Layout::default());
    let scratch = Scratch::new(&image);
    let path = CString::new(scratch.image.to_str().unwrap()).unwrap();
    unsafe {
        let handle = fs_exfat_mount_rw(path.as_ptr(), 0);
        assert!(!handle.is_null(), "{}", last_error());
        assert_eq!(volume_flags(handle), 2);
        assert_eq!(std::fs::read(&scratch.image).unwrap()[106], 2);
        let root = CString::new("/").unwrap();
        let mut metadata = FsExfatStat::default();
        assert_eq!(fs_exfat_stat(handle, root.as_ptr(), &mut metadata), 0);
        assert_eq!(metadata.is_directory, 1);
        assert_eq!(fs_exfat_flush(handle), 0, "{}", last_error());
        assert_eq!(fs_exfat_umount_checked(handle), 0, "{}", last_error());
    }
    assert_eq!(std::fs::read(&scratch.image).unwrap(), image);
}

#[test]
fn a_read_only_mount_refuses_flush_and_closes_cleanly() {
    let (image, _) = format(&Layout::default());
    let scratch = Scratch::new(&image);
    let path = CString::new(scratch.image.to_str().unwrap()).unwrap();
    unsafe {
        let handle = fs_exfat_mount(path.as_ptr(), 0);
        assert!(!handle.is_null());
        assert_eq!(fs_exfat_flush(handle), -1);
        assert!(last_error().contains("read-only") || last_error().contains("ReadOnly"));
        assert_eq!(fs_exfat_flush(ptr::null()), -1);
        assert_eq!(fs_exfat_umount_checked(handle), 0);
        assert_eq!(fs_exfat_umount_checked(ptr::null_mut()), 0);
    }
}

#[test]
fn a_dirty_volume_and_an_unaligned_offset_are_refused() {
    let (mut image, _) = format(&Layout::default());
    image[106] = 2;
    let scratch = Scratch::new(&image);
    let path = CString::new(scratch.image.to_str().unwrap()).unwrap();
    unsafe {
        assert!(fs_exfat_mount_rw(path.as_ptr(), 0).is_null());
        assert!(last_error().contains("dirty"), "{}", last_error());
        assert!(fs_exfat_mount_rw(path.as_ptr(), 1).is_null());
        assert!(fs_exfat_mount_rw(ptr::null(), 0).is_null());
    }
    assert_eq!(std::fs::read(&scratch.image).unwrap(), image);
}

struct Source {
    bytes: Mutex<Vec<u8>>,
    writes: AtomicUsize,
    flushes: AtomicUsize,
}

unsafe extern "C" fn read(
    context: *mut c_void,
    offset: u64,
    output: *mut u8,
    length: usize,
) -> c_int {
    let source = unsafe { &*(context.cast::<Source>()) };
    let bytes = source.bytes.lock().unwrap();
    let Some(data) = bytes.get(offset as usize..offset as usize + length) else {
        return 5;
    };
    unsafe { ptr::copy_nonoverlapping(data.as_ptr(), output, length) };
    0
}

unsafe extern "C" fn write(
    context: *mut c_void,
    offset: u64,
    data: *const u8,
    length: usize,
) -> c_int {
    let source = unsafe { &*(context.cast::<Source>()) };
    if !offset.is_multiple_of(512) || !length.is_multiple_of(512) {
        return 7;
    }
    let mut bytes = source.bytes.lock().unwrap();
    let Some(target) = bytes.get_mut(offset as usize..offset as usize + length) else {
        return 6;
    };
    unsafe { ptr::copy_nonoverlapping(data, target.as_mut_ptr(), length) };
    source.writes.fetch_add(1, Ordering::SeqCst);
    0
}

unsafe extern "C" fn flush(context: *mut c_void) -> c_int {
    let source = unsafe { &*(context.cast::<Source>()) };
    source.flushes.fetch_add(1, Ordering::SeqCst);
    0
}

unsafe extern "C" fn failing_write(_: *mut c_void, _: u64, _: *const u8, _: usize) -> c_int {
    9
}

#[test]
fn callback_mounts_write_whole_sectors_and_flush_through_the_callbacks() {
    let (image, geometry) = format(&Layout::default());
    let offset = 4096u64;
    let mut bytes = vec![0xee; offset as usize];
    bytes.extend_from_slice(&image);
    let source = Source {
        bytes: Mutex::new(bytes.clone()),
        writes: AtomicUsize::new(0),
        flushes: AtomicUsize::new(0),
    };
    let config = FsExfatRwCallbacks {
        context: ptr::from_ref(&source).cast_mut().cast(),
        size_bytes: bytes.len() as u64,
        read: Some(read),
        write: Some(write),
        flush: Some(flush),
    };
    unsafe {
        let handle = fs_exfat_mount_rw_with_callbacks(&config, offset);
        assert!(!handle.is_null(), "{}", last_error());
        assert_eq!(source.bytes.lock().unwrap()[offset as usize + 106], 2);
        assert!(source.flushes.load(Ordering::SeqCst) >= 1);
        assert_eq!(fs_exfat_flush(handle), 0);
        assert_eq!(fs_exfat_umount_checked(handle), 0, "{}", last_error());
    }
    assert!(source.writes.load(Ordering::SeqCst) >= 2);
    assert_eq!(*source.bytes.lock().unwrap(), bytes);
    let _ = geometry;

    let refusing = FsExfatRwCallbacks {
        write: Some(failing_write),
        ..config
    };
    unsafe {
        assert!(fs_exfat_mount_rw_with_callbacks(&refusing, offset).is_null());
        assert!(last_error().contains("status 9"), "{}", last_error());
        let missing = FsExfatRwCallbacks {
            write: None,
            ..config
        };
        assert!(fs_exfat_mount_rw_with_callbacks(&missing, offset).is_null());
        assert!(fs_exfat_mount_rw_with_callbacks(ptr::null(), 0).is_null());
    }
    assert_eq!(*source.bytes.lock().unwrap(), bytes);
}

fn read_back(handle: *const FsExfat, path: &str, length: usize) -> Vec<u8> {
    let path = CString::new(path).unwrap();
    let mut output = vec![0xa5; length + 8];
    let count =
        unsafe { fs_exfat_read_file(handle, path.as_ptr(), 0, output.as_mut_ptr(), output.len()) };
    assert_eq!(count, length as i64, "{}", last_error());
    output.truncate(length);
    output
}

#[test]
fn create_mkdir_pwrite_and_write_file_set_errno_and_persist() {
    let (image, _) = format(&Layout::default());
    let scratch = Scratch::new(&image);
    let path = CString::new(scratch.image.to_str().unwrap()).unwrap();
    let directory = CString::new("/Docs").unwrap();
    let file = CString::new("/Docs/report.txt").unwrap();
    let whole = CString::new("/Docs/whole.bin").unwrap();
    let missing = CString::new("/absent/x").unwrap();
    unsafe {
        let handle = fs_exfat_mount_rw(path.as_ptr(), 0);
        assert!(!handle.is_null(), "{}", last_error());
        assert_eq!(
            fs_exfat_mkdir(handle, directory.as_ptr()),
            0,
            "{}",
            last_error()
        );
        assert_eq!(fs_exfat_last_errno(), 0);
        assert_eq!(fs_exfat_mkdir(handle, directory.as_ptr()), -1);
        assert_eq!(fs_exfat_last_errno(), 17, "EEXIST");
        assert_eq!(fs_exfat_create(handle, missing.as_ptr()), -1);
        assert_eq!(fs_exfat_last_errno(), 2, "ENOENT");
        assert_eq!(fs_exfat_create(handle, file.as_ptr()), 0);
        assert_eq!(
            fs_exfat_pwrite(handle, file.as_ptr(), 3, b"abc".as_ptr(), 3),
            3
        );
        assert_eq!(fs_exfat_pwrite(handle, file.as_ptr(), 0, ptr::null(), 0), 0);
        assert_eq!(
            fs_exfat_pwrite(handle, file.as_ptr(), 0, ptr::null(), 1),
            -1
        );
        assert_eq!(fs_exfat_last_errno(), 22, "EINVAL");
        assert_eq!(
            fs_exfat_pwrite(handle, directory.as_ptr(), 0, b"x".as_ptr(), 1),
            -1
        );
        assert_eq!(fs_exfat_last_errno(), 21, "EISDIR");
        let body = vec![7u8; 70_000];
        assert_eq!(
            fs_exfat_write_file(handle, whole.as_ptr(), body.as_ptr(), body.len()),
            70_000
        );
        assert_eq!(read_back(handle, "/Docs/report.txt", 6), b"\0\0\0abc");
        assert_eq!(read_back(handle, "/docs/WHOLE.bin", 70_000), body);
        assert_eq!(fs_exfat_umount_checked(handle), 0, "{}", last_error());
        let handle = fs_exfat_mount(path.as_ptr(), 0);
        assert_eq!(read_back(handle, "/Docs/whole.bin", 70_000), body);
        assert_eq!(fs_exfat_create(handle, file.as_ptr()), -1);
        assert_eq!(fs_exfat_last_errno(), 30, "EROFS");
        fs_exfat_umount(handle);
    }
}
