mod interface_support;

#[path = "support/image.rs"]
mod image_support;

use image_support::Snapshot;

use fs_exfat::capi::*;
use std::ffi::{c_int, c_void, CStr, CString};
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

struct Source {
    bytes: Vec<u8>,
    calls: Mutex<Vec<(u64, usize)>>,
    fail: AtomicBool,
}

unsafe extern "C" fn read(
    context: *mut c_void,
    offset: u64,
    output: *mut u8,
    length: usize,
) -> c_int {
    let source = unsafe { &*(context.cast::<Source>()) };
    if source.fail.load(Ordering::Relaxed)
        || !offset.is_multiple_of(512)
        || !length.is_multiple_of(512)
    {
        return 11;
    }
    source.calls.lock().unwrap().push((offset, length));
    let Some(bytes) = source.bytes.get(offset as usize..offset as usize + length) else {
        return 12;
    };
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), output, length) };
    0
}

fn source() -> Source {
    Source {
        bytes: interface_support::image(),
        calls: Mutex::new(Vec::new()),
        fail: AtomicBool::new(false),
    }
}

unsafe fn mount(source: &Source) -> *mut FsExfat {
    let config = FsExfatCallbacks {
        context: ptr::from_ref(source).cast_mut().cast(),
        size_bytes: source.bytes.len() as u64,
        read: Some(read),
    };
    let handle = unsafe { fs_exfat_mount_with_callbacks(&config, 0) };
    assert!(!handle.is_null());
    handle
}

#[test]
fn callback_mount_stat_read_and_alignment() {
    let source = source();
    let path = CString::new("/eXAMPLE.TXT").unwrap();
    unsafe {
        let handle = mount(&source);
        let mut information = MaybeUninit::uninit();
        assert_eq!(
            fs_exfat_get_volume_info(handle, information.as_mut_ptr()),
            0
        );
        let information: FsExfatVolumeInfo = information.assume_init();
        assert_eq!(
            CStr::from_ptr(information.label.as_ptr()).to_str().unwrap(),
            "M1TEST"
        );
        assert_eq!(
            (
                information.sector_size,
                information.cluster_count,
                information.guid_present
            ),
            (512, 100, 0)
        );
        assert_eq!(fs_exfat_get_volume_info(handle, ptr::null_mut()), -1);
        let mut metadata = FsExfatStat::default();
        assert_eq!(fs_exfat_stat(handle, path.as_ptr(), &mut metadata), 0);
        assert_eq!(
            (metadata.data_length, metadata.valid_data_length),
            (1030, 513)
        );
        assert_eq!(
            (
                metadata.created.present,
                metadata.created.offset_known,
                metadata.created.year
            ),
            (1, 1, 2026)
        );
        let mut output = vec![0xaa; 1200];
        assert_eq!(
            fs_exfat_read_file(handle, path.as_ptr(), 0, output.as_mut_ptr(), output.len()),
            1030
        );
        assert_eq!(&output[..1030], interface_support::expected());
        assert_eq!(&output[1030..], &[0xaa; 170]);
        assert_eq!(
            fs_exfat_read_file(handle, path.as_ptr(), u64::MAX, output.as_mut_ptr(), 1),
            0
        );
        assert_eq!(
            fs_exfat_read_file(handle, path.as_ptr(), 0, ptr::null_mut(), 0),
            0
        );
        fs_exfat_umount(handle);
    }
    assert!(source
        .calls
        .lock()
        .unwrap()
        .iter()
        .all(|(offset, length)| offset % 512 == 0 && length % 512 == 0));
}

#[test]
fn directories_own_entries_and_report_end_separately() {
    let source = source();
    unsafe {
        let handle = mount(&source);
        let path = CString::new("/").unwrap();
        let directory = fs_exfat_dir_open(handle, path.as_ptr());
        assert!(!directory.is_null());
        fs_exfat_umount(handle);
        let mut names = Vec::new();
        loop {
            let mut entry = MaybeUninit::uninit();
            let result = fs_exfat_dir_next(directory, entry.as_mut_ptr());
            assert!(result >= 0);
            if result == 0 {
                break;
            }
            let entry: FsExfatDirEntry = entry.assume_init();
            names.push(
                CStr::from_ptr(entry.name.as_ptr())
                    .to_str()
                    .unwrap()
                    .to_owned(),
            );
        }
        assert_eq!(names, ["Example.txt", "Empty.bin", "Sub"]);
        assert_eq!(fs_exfat_dir_next(directory, ptr::null_mut()), -1);
        fs_exfat_dir_close(directory);
        assert!(!fs_exfat_last_error().is_null());
    }
}

#[test]
fn invalid_arguments_fail_without_crossing_the_boundary() {
    unsafe {
        assert!(fs_exfat_mount(ptr::null(), 0).is_null());
        assert!(!fs_exfat_last_error().is_null());
        assert!(fs_exfat_mount_with_callbacks(ptr::null(), 0).is_null());
        let config = FsExfatCallbacks {
            context: ptr::null_mut(),
            size_bytes: 1,
            read: None,
        };
        assert!(fs_exfat_mount_with_callbacks(&config, 0).is_null());
        assert_eq!(fs_exfat_stat(ptr::null(), ptr::null(), ptr::null_mut()), -1);
        assert!(fs_exfat_dir_open(ptr::null(), ptr::null()).is_null());
        assert_eq!(fs_exfat_dir_next(ptr::null_mut(), ptr::null_mut()), -1);
        assert_eq!(
            fs_exfat_read_file(ptr::null(), ptr::null(), 0, ptr::null_mut(), 0),
            -1
        );
        fs_exfat_umount(ptr::null_mut());
        fs_exfat_dir_close(ptr::null_mut());
    }
}

#[test]
fn callback_failure_and_success_preserve_the_error_contract() {
    let source = source();
    unsafe {
        let handle = mount(&source);
        let file = CString::new("Example.txt").unwrap();
        let mut output = [0; 1];
        source.fail.store(true, Ordering::Relaxed);
        assert_eq!(
            fs_exfat_read_file(handle, file.as_ptr(), 0, output.as_mut_ptr(), 1),
            -1
        );
        assert!(CStr::from_ptr(fs_exfat_last_error())
            .to_str()
            .unwrap()
            .contains("status 11"));
        source.fail.store(false, Ordering::Relaxed);
        assert_eq!(
            fs_exfat_read_file(handle, file.as_ptr(), 0, output.as_mut_ptr(), 1),
            1
        );
        assert!(fs_exfat_last_error().is_null());
        fs_exfat_umount(handle);
    }
}

#[test]
fn file_mount_offsets_null_outputs_and_invalid_utf8() {
    let fixture = interface_support::Temp::new();
    let original = Snapshot::new(&fixture.image);
    let path = CString::new(fixture.image.to_str().unwrap()).unwrap();
    unsafe {
        assert!(fs_exfat_mount(path.as_ptr(), 1).is_null());
        let handle = fs_exfat_mount(path.as_ptr(), 0);
        assert!(!handle.is_null());
        let file = CString::new("Example.txt").unwrap();
        assert_eq!(fs_exfat_stat(handle, file.as_ptr(), ptr::null_mut()), -1);
        assert_eq!(
            fs_exfat_read_file(handle, file.as_ptr(), 0, ptr::null_mut(), 1),
            -1
        );
        assert_eq!(
            fs_exfat_read_file(handle, file.as_ptr(), 0, ptr::dangling_mut(), usize::MAX),
            -1
        );
        let mut metadata = FsExfatStat::default();
        let invalid = [0xffu8, 0];
        assert_eq!(
            fs_exfat_stat(handle, invalid.as_ptr().cast(), &mut metadata),
            -1
        );
        let root = CString::new("/").unwrap();
        assert_eq!(fs_exfat_stat(handle, root.as_ptr(), &mut metadata), 0);
        assert_eq!((metadata.is_directory, metadata.created.present), (1, 0));
        assert_eq!(
            fs_exfat_read_file(handle, root.as_ptr(), 0, ptr::null_mut(), 0),
            -1
        );
        fs_exfat_umount(handle);
    }
    original.assert_unchanged(&fixture.image);
}

#[test]
fn public_c_header_compiles_with_the_documented_layout() {
    assert_eq!(std::mem::size_of::<FsExfatTimestamp>(), 24);
    assert_eq!(std::mem::size_of::<FsExfatStat>(), 96);
    assert_eq!(std::mem::size_of::<FsExfatDirEntry>(), 1120);
    assert_eq!(std::mem::size_of::<FsExfatVolumeInfo>(), 96);
    let result = std::process::Command::new("cc")
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-fsyntax-only",
            "-Iinclude",
            "tests/capi_smoke.c",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("a C compiler is required by chore test:unit");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn unknown_utc_offsets_remain_explicit_in_the_c_abi() {
    let mut source = source();
    let root = 32 * 512 + 96;
    source.bytes[root + 22] = 0;
    interface_support::base::seal_set(&mut source.bytes[root..root + 96]);
    unsafe {
        let handle = mount(&source);
        let path = CString::new("Example.txt").unwrap();
        let mut metadata = FsExfatStat::default();
        assert_eq!(fs_exfat_stat(handle, path.as_ptr(), &mut metadata), 0);
        assert_eq!(
            (metadata.created.present, metadata.created.offset_known),
            (1, 0)
        );
        assert_eq!(metadata.created.unix_milliseconds, 0);
        assert_eq!(metadata.modified.offset_known, 1);
        fs_exfat_umount(handle);
    }
}

struct Disk {
    bytes: Mutex<Vec<u8>>,
    fail_writes: AtomicBool,
    flushes: Mutex<u32>,
}

unsafe extern "C" fn disk_read(
    context: *mut c_void,
    offset: u64,
    output: *mut u8,
    length: usize,
) -> c_int {
    let disk = unsafe { &*(context.cast::<Disk>()) };
    let bytes = disk.bytes.lock().unwrap();
    let Some(source) = bytes.get(offset as usize..offset as usize + length) else {
        return 12;
    };
    unsafe { ptr::copy_nonoverlapping(source.as_ptr(), output, length) };
    0
}

unsafe extern "C" fn disk_write(
    context: *mut c_void,
    offset: u64,
    input: *const u8,
    length: usize,
) -> c_int {
    let disk = unsafe { &*(context.cast::<Disk>()) };
    if disk.fail_writes.load(Ordering::Relaxed) {
        return 13;
    }
    let mut bytes = disk.bytes.lock().unwrap();
    let Some(target) = bytes.get_mut(offset as usize..offset as usize + length) else {
        return 12;
    };
    unsafe { ptr::copy_nonoverlapping(input, target.as_mut_ptr(), length) };
    0
}

unsafe extern "C" fn disk_flush(context: *mut c_void) -> c_int {
    let disk = unsafe { &*(context.cast::<Disk>()) };
    *disk.flushes.lock().unwrap() += 1;
    0
}

fn format_options(label: &CStr) -> FsExfatFormatOptions {
    FsExfatFormatOptions {
        size_bytes: 0,
        alignment: 0,
        partition_offset: 0,
        bytes_per_sector: 4096,
        cluster_size: 0,
        volume_serial: 0x1234_abcd,
        serial_present: 1,
        number_of_fats: 0,
        guid_present: 1,
        reserved: 0,
        guid: *b"0123456789abcdef",
        label: label.as_ptr(),
    }
}

#[test]
fn the_formatter_writes_through_callbacks_and_reports_failures() {
    assert_eq!(std::mem::size_of::<FsExfatFormatOptions>(), 64);
    let disk = Disk {
        bytes: Mutex::new(vec![0; 8 << 20]),
        fail_writes: AtomicBool::new(false),
        flushes: Mutex::new(0),
    };
    let mut device = FsExfatRwCallbacks {
        context: ptr::from_ref(&disk).cast_mut().cast(),
        size_bytes: 8 << 20,
        read: Some(disk_read),
        write: Some(disk_write),
        flush: Some(disk_flush),
    };
    let label = CString::new("CABI").unwrap();
    let options = format_options(&label);
    unsafe {
        assert_eq!(fs_exfat_mkfs_with_callbacks(&device, &options), 0);
        assert_eq!(*disk.flushes.lock().unwrap(), 2);
        let source = Source {
            bytes: disk.bytes.lock().unwrap().clone(),
            calls: Mutex::new(Vec::new()),
            fail: AtomicBool::new(false),
        };
        let handle = mount(&source);
        let mut info = MaybeUninit::<FsExfatVolumeInfo>::uninit();
        assert_eq!(fs_exfat_get_volume_info(handle, info.as_mut_ptr()), 0);
        let info = info.assume_init();
        assert_eq!(
            CStr::from_ptr(info.label.as_ptr()).to_str().unwrap(),
            "CABI"
        );
        assert_eq!((info.sector_size, info.cluster_size), (4096, 4096));
        assert_eq!(info.volume_serial, 0x1234_abcd);
        assert_eq!((info.guid_present, info.guid), (1, *b"0123456789abcdef"));
        fs_exfat_umount(handle);

        assert_eq!(fs_exfat_mkfs_with_callbacks(&device, ptr::null()), 0);
        assert_eq!(fs_exfat_last_errno(), 0);
        disk.fail_writes.store(true, Ordering::Relaxed);
        assert_eq!(fs_exfat_mkfs_with_callbacks(&device, &options), -1);
        let message = CStr::from_ptr(fs_exfat_last_error()).to_str().unwrap();
        assert!(
            message.contains("write callback failed with status 13"),
            "{message}"
        );
        assert_eq!(fs_exfat_last_errno(), 5);
        disk.fail_writes.store(false, Ordering::Relaxed);
        let long = CString::new("a label too long").unwrap();
        assert_eq!(
            fs_exfat_mkfs_with_callbacks(&device, &format_options(&long)),
            -1
        );
        let message = CStr::from_ptr(fs_exfat_last_error()).to_str().unwrap();
        assert!(message.contains("UTF-16 code units"), "{message}");
        assert_eq!(fs_exfat_last_errno(), 22);
        device.write = None;
        assert_eq!(fs_exfat_mkfs_with_callbacks(&device, &options), -1);
        assert_eq!(fs_exfat_mkfs_with_callbacks(ptr::null(), &options), -1);
    }
}

#[test]
fn the_formatter_formats_a_file_by_path() {
    let fixture = interface_support::Temp::new();
    let target = fixture.directory.join("formatted.img");
    std::fs::File::create(&target)
        .unwrap()
        .set_len(16 << 20)
        .unwrap();
    let path = CString::new(target.to_str().unwrap()).unwrap();
    let label = CString::new("FILE").unwrap();
    unsafe {
        assert_eq!(fs_exfat_mkfs(path.as_ptr(), &format_options(&label)), 0);
        let handle = fs_exfat_mount(path.as_ptr(), 0);
        assert!(!handle.is_null());
        let mut info = MaybeUninit::<FsExfatVolumeInfo>::uninit();
        assert_eq!(fs_exfat_get_volume_info(handle, info.as_mut_ptr()), 0);
        assert_eq!(
            CStr::from_ptr(info.assume_init().label.as_ptr())
                .to_str()
                .unwrap(),
            "FILE"
        );
        fs_exfat_umount(handle);
        let missing = CString::new(fixture.directory.join("absent.img").to_str().unwrap()).unwrap();
        assert_eq!(fs_exfat_mkfs(missing.as_ptr(), ptr::null()), -1);
        assert_eq!(fs_exfat_mkfs(ptr::null(), ptr::null()), -1);
    }
}
