//! The compressed Up-case Table (#42), from Microsoft's exFAT specification,
//! section 7.2.5 "Up-case Table": each word is the up-cased form of the next
//! character, except FFFFh, which with the word after it stands for that many
//! identity mappings. Section 7.2.5.1 "Recommended Up-case Table" requires
//! the table, compressed or not, to cover 0000h to FFFFh, and section 7.2.2
//! "TableChecksum Field" sums the table as stored. Every table here is built
//! from those rules, not from a formatter's output.

use fs_exfat::metadata::UpcaseTable;
use fs_exfat::primitives::{checksum32, RawUpcaseTable, UpcaseIssue};

fn bytes(words: &[u16]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

/// A compressed table with Latin-1 mappings: identity runs around a-z → A-Z,
/// à–ö → À–Ö and ø–þ → Ø–Þ, and a run of one for ÷.
fn compressed() -> Vec<u16> {
    let mut words = vec![0xFFFF, 0x61];
    words.extend(0x41..=0x5A);
    words.extend([0xFFFF, 0xE0 - 0x7B]);
    words.extend(0xC0..=0xD6);
    words.extend([0xFFFF, 1]);
    words.extend(0xD8..=0xDE);
    // 0xFF to 0xFFFF: 65281 characters.
    words.extend([0xFFFF, 0xFFFF - 0xFE]);
    words
}

#[test]
fn a_final_identity_run_of_one_covers_ffffh() {
    // The last run stops at FFFEh and a run of one maps FFFFh: complete, with
    // nothing past FFFFh.
    let mut words = compressed();
    words.pop();
    words.extend([0xFFFF - 0xFF, 0xFFFF, 1]);
    let data = bytes(&words);
    let raw = RawUpcaseTable::parse(&data, checksum32(&data));
    assert_eq!(raw.issue, None);
    assert!(raw.complete);
    assert_eq!(raw.mappings[0xFFFF], 0xFFFF);
    assert!(UpcaseTable::parse(&data, checksum32(&data)).is_ok());
    // An uncompressed table's FFFFh at FFFFh is still that character's own
    // mapping, and a word after it is one too many.
    let mut data = bytes(&(0..=0xFFFFu16).collect::<Vec<_>>());
    assert!(RawUpcaseTable::parse(&data, 0).complete);
    data.extend_from_slice(&2u16.to_le_bytes());
    let raw = RawUpcaseTable::parse(&data, 0);
    assert_eq!(raw.issue, Some(UpcaseIssue::ExtraMappings));
}

/// What `compressed` encodes, character by character.
fn latin1_upcase(unit: u16) -> u16 {
    match unit {
        0x61..=0x7A | 0xE0..=0xF6 | 0xF8..=0xFE => unit - 0x20,
        _ => unit,
    }
}

#[test]
fn compressed_and_uncompressed_tables_decode_alike() {
    // Section 7.2.5's own example: the first 97 characters are identity
    // mappings, then 0061h to 0063h map to 0041h to 0043h.
    let example = bytes(&[0xFFFF, 0x61, 0x41, 0x42, 0x43]);
    let raw = RawUpcaseTable::parse(&example, checksum32(&example));
    assert!(raw.compressed);
    assert_eq!(raw.mappings.len(), 100);
    assert_eq!(raw.mappings[0x60], 0x60);
    assert_eq!(raw.mappings[0x61..], [0x41, 0x42, 0x43]);
    assert!(!raw.complete);
    assert!(!raw.usable());

    let packed = bytes(&compressed());
    let plain = bytes(&(0..=0xFFFF).map(latin1_upcase).collect::<Vec<_>>());
    let raw_packed = RawUpcaseTable::parse(&packed, checksum32(&packed));
    let raw_plain = RawUpcaseTable::parse(&plain, checksum32(&plain));
    assert!(raw_packed.compressed && raw_packed.usable());
    assert!(!raw_plain.compressed && raw_plain.usable());
    assert_eq!(raw_packed.mappings, raw_plain.mappings);
    // Runs cover 'a' to 'z' with 30 words; uncompressed, 128 words.
    assert_eq!(raw_packed.mandatory_bytes, 60);
    assert_eq!(raw_plain.mandatory_bytes, 256);
    // The checksum is of the stored form, so the two forms differ in it.
    assert_ne!(checksum32(&packed), checksum32(&plain));
    assert!(UpcaseTable::parse(&packed, checksum32(&plain)).is_err());

    let packed = UpcaseTable::parse(&packed, checksum32(&packed)).unwrap();
    let plain = UpcaseTable::parse(&plain, checksum32(&plain)).unwrap();
    for unit in 0..=0xFFFF {
        assert_eq!(packed.map(unit), plain.map(unit), "{unit:04X}");
    }
    let name: Vec<u16> = "Ünïcödé ÷ ÿ.txt".encode_utf16().collect();
    let upper: Vec<u16> = "üNÏCÖDÉ ÷ ÿ.TXT".encode_utf16().collect();
    assert!(packed.equal(&name, &upper) && plain.equal(&name, &upper));
    assert_eq!(packed.name_hash(&name), plain.name_hash(&upper));
}

#[test]
fn corrupt_compressed_tables_are_located_and_refused() {
    // 'a' to 'z' end at byte 56, where each fault starts.
    let mut start = vec![0xFFFF, 0x61];
    start.extend(0x41..=0x5A);
    for (tail, issue) in [
        (vec![0xFFFF], UpcaseIssue::TruncatedRun),
        // A run of 0 is read as no mappings (#75); one character too many
        // before the final run is still refused.
        (
            vec![0xFFFF, 0xFFFF - 0x79, 0xFFFF, 0xFFFF - 0x7A],
            UpcaseIssue::RunOutsideRange,
        ),
        (vec![0xFFFF, 0xFFFF - 0x79], UpcaseIssue::RunOutsideRange),
        (
            vec![0xFFFF, 0xFFFF - 0x7A, 0x41],
            UpcaseIssue::ExtraMappings,
        ),
    ] {
        let data = bytes(&[start.clone(), tail.clone()].concat());
        let raw = RawUpcaseTable::parse(&data, checksum32(&data));
        assert_eq!(raw.issue, Some(issue), "{tail:?}");
        let at = if issue == UpcaseIssue::ExtraMappings {
            60
        } else {
            56
        };
        assert_eq!(raw.issue_at, Some(at), "{tail:?}");
        assert!(raw.checksum_ok && !raw.usable(), "{tail:?}");
        assert!(UpcaseTable::parse(&data, checksum32(&data)).is_err());
    }
    // A run over 'a' to 'z' maps them to themselves, against Table 24.
    let data = bytes(&[0xFFFF, 0x7B, 0xFFFF, 0xFFFF - 0x7A]);
    let raw = RawUpcaseTable::parse(&data, checksum32(&data));
    assert!(raw.complete && raw.issue.is_none() && !raw.ascii_ok && !raw.usable());
    // The first run ends at 007Ah, so the first 128 mappings take both runs.
    assert_eq!(raw.mandatory_bytes, 8);
    assert!(UpcaseTable::parse(&data, checksum32(&data)).is_err());
}

#[test]
fn zero_count_identity_run_is_read_as_no_mappings() {
    // Section 7.2.5: FFFFh is "followed with the number of identity
    // mappings"; nothing requires that number to be at least one.
    let mut words = vec![0xFFFF, 0];
    words.extend(compressed());
    let data = bytes(&words);
    let raw = RawUpcaseTable::parse(&data, checksum32(&data));
    assert_eq!(raw.issue, None);
    assert!(raw.usable());
    assert!(UpcaseTable::parse(&data, checksum32(&data)).is_ok());
}
