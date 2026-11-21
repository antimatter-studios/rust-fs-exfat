#[path = "../support/image.rs"]
mod image_support;

use fs_exfat::capi::*;
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::path::PathBuf;
use std::process::Command;
use unicode_normalization::UnicodeNormalization;

unsafe fn abi_listing(
    handle: *const FsExfat,
    path: &str,
    records: &mut BTreeMap<String, (String, u64)>,
) {
    let requested = CString::new(path).unwrap();
    let directory = unsafe { fs_exfat_dir_open(handle, requested.as_ptr()) };
    assert!(!directory.is_null());
    let mut children = Vec::new();
    loop {
        let mut entry = std::mem::MaybeUninit::uninit();
        let result = unsafe { fs_exfat_dir_next(directory, entry.as_mut_ptr()) };
        assert!(result >= 0);
        if result == 0 {
            break;
        }
        let entry: FsExfatDirEntry = unsafe { entry.assume_init() };
        let name = unsafe { CStr::from_ptr(entry.name.as_ptr()) }
            .to_str()
            .unwrap();
        let full = format!(
            "{}{name}",
            if path == "/" {
                String::new()
            } else {
                format!("{path}/")
            }
        );
        let kind = if entry.stat.is_directory != 0 {
            "d"
        } else {
            "f"
        };
        let size = if kind == "d" {
            0
        } else {
            entry.stat.data_length
        };
        assert!(records.insert(full.clone(), (kind.into(), size)).is_none());
        if kind == "d" {
            children.push(full);
        }
    }
    unsafe { fs_exfat_dir_close(directory) };
    for child in children {
        unsafe { abi_listing(handle, &child, records) };
    }
}

#[test]
fn interfaces_match_the_reference_driver_and_check_only_oracle() {
    let directory = PathBuf::from(
        std::env::var_os("EXFAT_ORACLE_DIR")
            .expect("run chore test:oracle-macos to generate independent macOS images"),
    );
    let binary = env!("CARGO_BIN_EXE_rust-fs-exfat");
    for sector in [512, 4096] {
        let image = directory.join(format!("reference-{sector}.dmg"));
        assert!(std::fs::symlink_metadata(&image)
            .unwrap()
            .file_type()
            .is_file());
        let before = image_support::Snapshot::new(&image);
        let requested_image = CString::new(image.to_str().unwrap()).unwrap();
        let handle = unsafe { fs_exfat_mount(requested_image.as_ptr(), 0) };
        assert!(!handle.is_null());
        let checked = Command::new(binary)
            .arg("fsck")
            .arg(&image)
            .arg("-n")
            .output()
            .unwrap();
        assert!(
            checked.status.success(),
            "{}",
            String::from_utf8_lossy(&checked.stderr)
        );
        let listing =
            std::fs::read_to_string(directory.join(format!("listing-{sector}.txt"))).unwrap();
        let mut expected = BTreeMap::new();
        for line in listing.lines() {
            let mut fields = line.splitn(3, '\t');
            let kind = fields.next().unwrap();
            let size: u64 = fields.next().unwrap().parse().unwrap();
            let path: String = fields.next().unwrap().nfc().collect();
            expected.insert(path.clone(), (kind.to_owned(), size));
            let metadata = Command::new(binary)
                .arg("fs")
                .arg(&image)
                .args(["stat", &path])
                .output()
                .unwrap();
            assert!(
                metadata.status.success(),
                "{path}: {}",
                String::from_utf8_lossy(&metadata.stderr)
            );
            let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout).unwrap();
            assert_eq!(
                metadata["type"],
                if kind == "d" { "directory" } else { "file" }
            );
            if kind == "f" {
                assert_eq!(metadata["size"], size);
            }
            let requested = CString::new(path.as_str()).unwrap();
            let mut abi_metadata = FsExfatStat::default();
            assert_eq!(
                unsafe { fs_exfat_stat(handle, requested.as_ptr(), &mut abi_metadata) },
                0
            );
            assert_eq!(abi_metadata.is_directory != 0, kind == "d");
            if kind == "f" {
                assert_eq!(abi_metadata.data_length, size);
            }
        }
        let mut observed = BTreeMap::new();
        unsafe { abi_listing(handle, "/", &mut observed) };
        assert_eq!(observed, expected);
        let mut cli_observed = BTreeMap::new();
        let directories = std::iter::once("/".to_owned()).chain(
            expected
                .iter()
                .filter(|(_, (kind, _))| kind == "d")
                .map(|(path, _)| path.clone()),
        );
        for path in directories {
            let result = Command::new(binary)
                .arg("fs")
                .arg(&image)
                .args(["ls", &path])
                .output()
                .unwrap();
            assert!(result.status.success());
            let entries: Vec<serde_json::Value> = serde_json::from_slice(&result.stdout).unwrap();
            for entry in entries {
                let name = entry["name"].as_str().unwrap();
                let full = format!(
                    "{}{name}",
                    if path == "/" {
                        String::new()
                    } else {
                        format!("{path}/")
                    }
                );
                let kind = if entry["type"] == "directory" {
                    "d"
                } else {
                    "f"
                };
                let size = if kind == "d" {
                    0
                } else {
                    entry["size"].as_u64().unwrap()
                };
                assert!(cli_observed.insert(full, (kind.into(), size)).is_none());
            }
        }
        assert_eq!(cli_observed, expected);
        let files = std::fs::read_to_string(directory.join(format!("files-{sector}.txt"))).unwrap();
        use sha2::{Digest, Sha256};
        for line in files.lines() {
            let fields: Vec<_> = line.splitn(6, '\t').collect();
            let path: String = fields[5].nfc().collect();
            let result = Command::new(binary)
                .arg("fs")
                .arg(&image)
                .args(["cat", &path])
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{path}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(
                format!("{:x}", Sha256::digest(&result.stdout)),
                fields[1],
                "{path}"
            );
            let requested = CString::new(path.as_str()).unwrap();
            let mut digest = Sha256::new();
            let mut offset = 0;
            let mut buffer = vec![0; 65536];
            loop {
                let count = unsafe {
                    fs_exfat_read_file(
                        handle,
                        requested.as_ptr(),
                        offset,
                        buffer.as_mut_ptr(),
                        buffer.len(),
                    )
                };
                assert!(count >= 0);
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count as usize]);
                offset += count as u64;
            }
            assert_eq!(format!("{:x}", digest.finalize()), fields[1], "ABI {path}");
        }
        let destination = directory.join(format!("export-{sector}.bin"));
        let result = Command::new(binary)
            .arg("fs")
            .arg(&image)
            .args(["export", "ASCII.txt"])
            .arg(&destination)
            .arg("--verify")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap()["verified"],
            true
        );
        image_support::assert_bytes(&destination, b"contents of ASCII.txt\n");
        before.assert_unchanged(&image);
        unsafe { fs_exfat_umount(handle) };
    }
}
