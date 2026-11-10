use fs_exfat::interface::open_path;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Command;
use unicode_normalization::UnicodeNormalization;

#[test]
fn raw_and_block_devices_match_the_read_only_macos_driver() {
    let directory =
        PathBuf::from(std::env::var("EXFAT_ORACLE_DIR").expect("run chore test:oracle on macOS"));
    let block = PathBuf::from(
        std::env::var("EXFAT_ORACLE_DEVICE").expect("the oracle requires its live attach receipt"),
    );
    let sector: u64 = std::env::var("EXFAT_ORACLE_SECTOR")
        .expect("the oracle requires its attached sector size")
        .parse()
        .unwrap();
    let name = block.file_name().unwrap().to_str().unwrap();
    let number = name.strip_prefix("disk").unwrap();
    assert!(!number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()));
    let raw = block.with_file_name(format!("rdisk{number}"));
    let block_volume = open_path(&block, 0).unwrap();
    let raw_volume = open_path(&raw, 0).unwrap();
    assert_eq!(raw_volume.boot().bytes_per_sector(), sector);
    assert_eq!(block_volume.boot().bytes_per_sector(), sector);
    assert_eq!(raw_volume.metadata().label, block_volume.metadata().label);
    let files = std::fs::read_to_string(directory.join(format!("files-{sector}.txt"))).unwrap();
    for line in files.lines() {
        let fields: Vec<_> = line.splitn(6, '\t').collect();
        let path: String = fields[5].nfc().collect();
        for volume in [&raw_volume, &block_volume] {
            let entry = volume.lookup(&path).unwrap();
            let file = volume.open_file(entry.entry().unwrap()).unwrap();
            assert_eq!(file.len(), fields[0].parse::<u64>().unwrap(), "{path}");
            let mut digest = Sha256::new();
            let mut output = [0; 65536];
            let mut offset = 0;
            loop {
                let count = file.read_at(offset, &mut output).unwrap();
                if count == 0 {
                    break;
                }
                digest.update(&output[..count]);
                offset += count as u64;
            }
            assert_eq!(format!("{:x}", digest.finalize()), fields[1], "{path}");
        }
    }
    let binary = env!("CARGO_BIN_EXE_rust-fs-exfat");
    for arguments in [
        vec!["fs", "ls", "/"],
        vec!["fs", "cat", "ASCII.txt"],
        vec!["fsck", "-n"],
    ] {
        let outputs: Vec<_> = [&block, &raw]
            .into_iter()
            .map(|device| {
                let result = Command::new(binary)
                    .arg(arguments[0])
                    .arg(device)
                    .args(&arguments[1..])
                    .output()
                    .unwrap();
                assert!(
                    result.status.success(),
                    "{}: {}",
                    device.display(),
                    String::from_utf8_lossy(&result.stderr)
                );
                result.stdout
            })
            .collect();
        assert_eq!(outputs[0], outputs[1], "{arguments:?}");
    }
}
