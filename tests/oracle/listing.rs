//! The tree this crate's reader sees, in the format the macOS oracle lists
//! the read-only mounted driver's view, so that `scripts/test-oracle.sh`
//! can compare the two with `diff`.

use fs_exfat::timestamp::Timestamp;
use fs_exfat::volume::Volume;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

/// How the macOS driver shows exFAT attributes in st_flags, as observed on
/// an image made for the purpose: ReadOnly as UF_IMMUTABLE, Hidden as
/// UF_HIDDEN, and a file's clear Archive bit as SF_ARCHIVED (a directory's
/// Archive bit is not shown).
fn macos_flags(attributes: u16) -> u32 {
    let directory = attributes & 0x10 != 0;
    let mut flags = 0;
    if attributes & 0x01 != 0 {
        flags |= 0x2;
    }
    if attributes & 0x02 != 0 {
        flags |= 0x8000;
    }
    if attributes & 0x20 == 0 && !directory {
        flags |= 0x10000;
    }
    flags
}

pub fn listing<Device: fs_core::BlockRead>(volume: &Volume<Device>) -> String {
    fn walk<Device: fs_core::BlockRead>(
        volume: &Volume<Device>,
        prefix: &str,
        entries: Vec<fs_exfat::directory::DirectoryEntry>,
        records: &mut Vec<String>,
    ) {
        for entry in entries {
            let path: String = format!("{prefix}{}", entry.name()).nfc().collect();
            let times = entry.timestamps();
            let milliseconds = |time: &Timestamp| {
                time.unix_milliseconds()
                    .expect("every entry on the written image has a UTC offset")
            };
            let times = format!(
                "{}\t{}\t{}",
                milliseconds(&times.modified),
                milliseconds(&times.accessed),
                milliseconds(&times.created),
            );
            if entry.is_directory() {
                records.push(format!(
                    "d\t0\t-\t{:x}\t{times}\t{path}\n",
                    macos_flags(entry.attributes())
                ));
                walk(
                    volume,
                    &format!("{path}/"),
                    volume.read_directory(&entry).unwrap(),
                    records,
                );
                continue;
            }
            let file = volume.open_file(&entry).unwrap();
            let mut hash = Sha256::new();
            let mut buffer = vec![0; 1 << 20];
            let mut offset = 0;
            loop {
                let count = file.read_at(offset, &mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
                offset += count as u64;
            }
            records.push(format!(
                "f\t{}\t{:x}\t{:x}\t{times}\t{path}\n",
                file.len(),
                hash.finalize(),
                macos_flags(entry.attributes()),
            ));
        }
    }
    let mut records = Vec::new();
    walk(volume, "", volume.root_directory().unwrap(), &mut records);
    records.sort();
    records.concat()
}
