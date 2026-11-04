mod support;

use fs_exfat::metadata::{checksum32, UpcaseTable};
use fs_exfat::path::Node;
use fs_exfat::volume::Volume;
use fs_exfat::Error;
use support::{file_set, metadata_volume, put16, put32, put64, seal_set, Bytes};

fn image() -> Vec<u8> {
    let mut data = metadata_volume();
    data[33 * 512] |= 8;
    let mut root = file_set("Sub", 0x10, 5, 512, true);
    root.extend(file_set("MixedCase.TXT", 0x20, 0, 0, false));
    root.extend(file_set("é.txt", 0x20, 0, 0, false));
    data[32 * 512 + 96..32 * 512 + 96 + root.len()].copy_from_slice(&root);
    let child = file_set("Child.txt", 0x20, 0, 0, false);
    data[35 * 512..35 * 512 + child.len()].copy_from_slice(&child);
    data
}

#[test]
fn root_empty_relative_and_repeated_separators_resolve() {
    let volume = Volume::open(Bytes(image())).unwrap();
    for path in ["", "/", "////", ".", "./", "..", "../../", "/Sub/../"] {
        assert!(matches!(volume.lookup(path).unwrap(), Node::Root), "{path}");
    }
    assert_eq!(
        volume
            .lookup("//sUb///ChIlD.TxT")
            .unwrap()
            .entry()
            .unwrap()
            .name(),
        "Child.txt"
    );
    assert_eq!(volume.read_directory_path("/").unwrap().len(), 3);
    assert_eq!(
        volume.read_directory_path("sub/").unwrap()[0].name(),
        "Child.txt"
    );
}

#[test]
fn missing_components_are_not_lexically_cancelled_by_parent_navigation() {
    let volume = Volume::open(Bytes(image())).unwrap();
    for path in [
        "missing",
        "Sub/missing",
        "missing/../MixedCase.TXT",
        "Sub/missing/../../MixedCase.TXT",
    ] {
        assert!(
            matches!(volume.lookup(path), Err(Error::NotFound)),
            "{path}"
        );
    }
    assert_eq!(
        volume
            .lookup("Sub/../mixedcase.txt")
            .unwrap()
            .entry()
            .unwrap()
            .name(),
        "MixedCase.TXT"
    );
}

#[test]
fn files_cannot_be_traversed_or_used_with_a_trailing_separator() {
    let volume = Volume::open(Bytes(image())).unwrap();
    for path in [
        "MixedCase.TXT/child",
        "MixedCase.TXT/",
        "MixedCase.TXT/.",
        "MixedCase.TXT/..",
    ] {
        assert!(
            matches!(volume.lookup(path), Err(Error::NotDirectory)),
            "{path}"
        );
    }
    assert!(matches!(
        volume.read_directory_path("mixedcase.txt"),
        Err(Error::NotDirectory)
    ));
}

#[test]
fn volume_specific_non_ascii_mappings_override_host_case_rules() {
    let mut data = metadata_volume();
    let mut words = vec![0xffffu16, 97];
    words.extend(65..=90);
    words.extend([0xffff, 9731 - 123, 9730, 0xffff, u16::MAX - 9731]);
    let upcase: Vec<_> = words.into_iter().flat_map(u16::to_le_bytes).collect();
    put32(&mut data, 32 * 512 + 36, checksum32(&upcase));
    put64(&mut data, 32 * 512 + 56, upcase.len() as u64);
    data[34 * 512..34 * 512 + upcase.len()].copy_from_slice(&upcase);
    let table = UpcaseTable::parse(&upcase, checksum32(&upcase)).unwrap();
    let mut set = file_set("☃.txt", 0x20, 0, 0, false);
    put16(
        &mut set,
        36,
        table.name_hash(&"☃.txt".encode_utf16().collect::<Vec<_>>()),
    );
    seal_set(&mut set);
    data[32 * 512 + 96..32 * 512 + 96 + set.len()].copy_from_slice(&set);
    let volume = Volume::open(Bytes(data)).unwrap();
    assert_eq!(
        volume.lookup("☂.TXT").unwrap().entry().unwrap().name(),
        "☃.txt"
    );
}

#[test]
fn query_names_are_not_unicode_normalized_or_expanded() {
    let volume = Volume::open(Bytes(image())).unwrap();
    assert!(volume.lookup("é.TXT").is_ok());
    assert!(matches!(
        volume.lookup("e\u{301}.txt"),
        Err(Error::NotFound)
    ));
    for path in [
        "bad\0name",
        "bad:name",
        "bad\\name",
        &"a".repeat(256),
        &"😀".repeat(128),
    ] {
        assert!(
            matches!(volume.lookup(path), Err(Error::Invalid(_))),
            "{path}"
        );
    }
}

#[test]
fn directory_ancestor_cycles_are_refused_and_component_work_is_bounded() {
    let mut data = image();
    let loop_entry = file_set("Loop", 0x10, 5, 512, true);
    data[35 * 512..35 * 512 + loop_entry.len()].copy_from_slice(&loop_entry);
    let volume = Volume::open(Bytes(data)).unwrap();
    assert!(matches!(volume.lookup("Sub/Loop"), Err(Error::Invalid(_))));
    assert!(matches!(
        volume.lookup(&"./".repeat(1025)),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn lookup_failure_messages_are_distinct_and_actionable() {
    assert_eq!(Error::NotFound.to_string(), "exFAT path not found");
    assert_eq!(
        Error::NotDirectory.to_string(),
        "exFAT path component is not a directory"
    );
}
