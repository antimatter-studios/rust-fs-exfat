#[path = "../support/mod.rs"]
mod support;

use fs_exfat::path::Node;
use fs_exfat::volume::Volume;
use fs_exfat::Error;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use support::Bytes;

#[test]
fn case_insensitive_lookup_and_failures_match_the_read_only_macos_driver() {
    let directory =
        PathBuf::from(std::env::var("EXFAT_ORACLE_DIR").expect("run chore test:oracle on macOS"));
    for sector in [512, 4096] {
        let image = directory.join(format!("reference-{sector}.dmg"));
        assert!(std::fs::symlink_metadata(&image)
            .unwrap()
            .file_type()
            .is_file());
        let volume = Volume::open(Bytes(std::fs::read(image).unwrap())).unwrap();
        let expected =
            std::fs::read_to_string(directory.join(format!("lookup-{sector}.txt"))).unwrap();
        assert_eq!(expected.lines().count(), 11);
        for line in expected.lines() {
            let (query, expected) = line.split_once('\t').unwrap();
            let observed = match volume.lookup(query) {
                Ok(Node::Root) => "directory".to_owned(),
                Ok(Node::Entry(entry)) if entry.is_directory() => "directory".to_owned(),
                Ok(Node::Entry(entry)) => {
                    let file = volume.open_file(&entry).unwrap();
                    let mut hash = Sha256::new();
                    let mut offset = 0;
                    let mut buffer = [0; 1001];
                    loop {
                        let count = file.read_at(offset, &mut buffer).unwrap();
                        if count == 0 {
                            break;
                        }
                        hash.update(&buffer[..count]);
                        offset += count as u64;
                    }
                    format!("file\t{}\t{:x}", file.len(), hash.finalize())
                }
                Err(Error::NotFound) => "missing".to_owned(),
                Err(Error::NotDirectory) => "not-directory".to_owned(),
                Err(error) => panic!("{query}: {error}"),
            };
            assert_eq!(observed, expected, "{query} on {sector}-byte image");
        }
    }
}
