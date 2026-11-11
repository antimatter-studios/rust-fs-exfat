//! The Up-case Table extension (#42) over volumes built from Microsoft's
//! exFAT specification: the Up-case Table entry (section 7.2, Table 23), its
//! TableChecksum (section 7.2.2) and both forms of the table (section 7.2.5),
//! compressed with identity runs and uncompressed.

use super::support::Bytes;
use super::{damage, has, named, position, scan, volume, Failing};
use fs_exfat::fixtures::{upcase_entry, upcase_table, Builder, ROOT_CLUSTER};
use fs_exfat::primitives::FAT_END;
use fs_exfat::salvage::{DamageKind, ExfatSalvage, Extent, UpcaseReport};

/// Clusters 100 to 131 hold an uncompressed table, 32 clusters of 4 KiB.
const PLAIN: u32 = 100;

/// a–z → A–Z, é → É and ☃ → ☂, as a compressed table: identity runs, then
/// the words that differ.
fn compressed() -> Vec<u16> {
    let mut words = vec![0xFFFF, 0x61];
    words.extend(0x41..=0x5A);
    words.extend([0xFFFF, 0xE9 - 0x7B, 0xC9, 0xFFFF, 0x2603 - 0xEA, 0x2602]);
    words.extend([0xFFFF, 0xFFFF - 0x2603]);
    words
}

fn uncompressed() -> Vec<u16> {
    (0..=0xFFFF)
        .map(|unit| match unit {
            0x61..=0x7A => unit - 0x20,
            0xE9 => 0xC9,
            0x2603 => 0x2602,
            _ => unit,
        })
        .collect()
}

/// The fixture volume with `words` as its table, in one cluster from 4 or
/// in a FAT chain from cluster 100, behind a checksummed Up-case Table entry.
fn with_table(words: &[u16]) -> Builder {
    let mut builder = volume();
    let table = upcase_table(words);
    let first = if table.len() > 4096 { PLAIN } else { 4 };
    let clusters = table.len().div_ceil(4096) as u32;
    for cluster in first..first + clusters {
        builder.allocate(cluster);
        let next = if cluster + 1 == first + clusters {
            FAT_END
        } else {
            cluster + 1
        };
        builder.fat(cluster, next);
    }
    builder.put(first, 0, &table);
    builder.put(ROOT_CLUSTER, 32, &upcase_entry(first, &table));
    builder
}

fn report(builder: &Builder) -> UpcaseReport {
    let source = Bytes(builder.image.clone());
    ExfatSalvage::probe(&source).upcase_table(&source).unwrap()
}

#[test]
fn both_forms_decode_alike_and_drive_name_lookups() {
    let packed = report(&with_table(&compressed()));
    let plain = report(&with_table(&uncompressed()));
    let entry = Extent::new(position(ROOT_CLUSTER) + 32, 32);
    assert_eq!(packed.entry, Some(entry));
    assert!(packed.compressed && packed.usable);
    assert_eq!(packed.extents, [Extent::new(position(4), 72)]);
    assert!(!plain.compressed && plain.usable);
    assert_eq!(plain.extents, [Extent::new(position(PLAIN), 131072)]);
    assert_eq!(packed.mappings, plain.mappings);
    for report in [&packed, &plain] {
        assert_eq!(report.upcase(0xE9), 0xC9);
        assert!(report.same_name("☃ é.TXT", "☂ É.txt"));
        assert!(!report.same_name("é", "e"));
        assert!(!report.same_name("é", "ée"));
    }
    // A lookup over the candidates a scan found, by the volume's table.
    let source = Bytes(with_table(&compressed()).image);
    let (_, found) = scan(&source);
    let matched: Vec<_> = found
        .iter()
        .filter(|candidate| {
            candidate
                .name
                .as_deref()
                .is_some_and(|name| packed.same_name(name, "KEEP.MP3"))
        })
        .collect();
    assert_eq!(matched, [named(&found, "keep.mp3")]);
}

#[test]
fn a_damaged_table_is_reported_and_lookups_fall_back_to_table_24() {
    // The run after 'z' counts past FFFFh: malformed damage, and the
    // report holds no mappings.
    let mut words = compressed();
    words[29] = 0xFFFF;
    let builder = with_table(&words);
    let found = damage(&Bytes(builder.image.clone()));
    let table = Extent::new(position(4), 72);
    assert!(has(&found, DamageKind::Malformed, table, "upcase table"));
    let fallback = report(&builder);
    assert!(fallback.compressed && !fallback.usable && fallback.mappings.is_empty());
    // Table 24 still up-cases a–z; outside it, names may match (section 7.2.5).
    assert_eq!(fallback.upcase(0xE9), 0xE9);
    assert!(fallback.same_name("keep.MP3", "KEEP.mp3"));
    assert!(!fallback.same_name("keep.mp3", "kept.mp3"));
    assert!(fallback.same_name("é", "ü"));

    // A wrong TableChecksum, or an unreadable table, leaves it unused.
    let mut builder = with_table(&compressed());
    builder.image[position(ROOT_CLUSTER) as usize + 36] ^= 1;
    assert!(!report(&builder).usable);
    let source = Failing {
        inner: Bytes(with_table(&compressed()).image),
        bad: vec![Extent::new(position(4), 512)],
    };
    let unread = ExfatSalvage::probe(&source).upcase_table(&source).unwrap();
    assert!(!unread.usable);
    assert_eq!(unread.unreadable, [Extent::new(position(4), 72)]);

    // No Up-case Table entry: nothing to read.
    let mut builder = volume();
    builder.image[position(ROOT_CLUSTER) as usize + 32] = 0x02;
    let missing = report(&builder);
    assert_eq!(missing.entry, None);
    assert!(!missing.usable && missing.extents.is_empty());
}
