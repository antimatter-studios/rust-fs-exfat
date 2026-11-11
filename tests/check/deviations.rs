//! Spec deviations (#44). Every rule of `docs/spec-rules.md` has a synthetic
//! case here, built from the specification's text rather than from any
//! platform's output: one field of a known image is broken, the deviation must
//! appear at that field's byte offset with the rule's section and severity,
//! and the strict check must accept or reject the image exactly as the
//! table's "Strict layer" column says, with the same verdict it gives
//! without the report.

use super::interface_support;
use super::support::{self, Bytes};
use fs_exfat::check::{check_device, spec_deviations, CheckReport, DeviationReport, Rule};
use fs_exfat::salvage::Severity;
use std::collections::BTreeSet;

const SECTOR: usize = 512;
const BACKUP: usize = 12 * SECTOR;
const FAT: usize = 24 * SECTOR;
/// Cluster 2: the root directory.
const ROOT: usize = 32 * SECTOR;
/// Cluster 3: the Allocation Bitmap.
const BITMAP: usize = 33 * SECTOR;
/// Cluster 4: the Up-case Table.
const UPCASE: usize = 34 * SECTOR;
/// Cluster 8: the `Sub` directory, which holds `Child.bin`.
const SUB: usize = 38 * SECTOR;
/// The root's entries: Allocation Bitmap, Up-case Table, Volume Label, then
/// the sets of `Example.txt`, `Empty.bin` and `Sub`, three entries each.
const LABEL: usize = ROOT + 64;
const EXAMPLE: usize = ROOT + 96;
const EMPTY: usize = ROOT + 192;
const SUB_SET: usize = ROOT + 288;
/// The root's end-of-directory marker, after the `Sub` set.
const ROOT_FREE: usize = ROOT + 384;
/// The entry after `Child.bin`'s set in `Sub`.
const SUB_FREE: usize = SUB + 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Strict {
    Accepts,
    Rejects,
}

struct Case {
    rule: Rule,
    offset: usize,
    strict: Strict,
    edit: fn(&mut Vec<u8>),
}

fn case(rule: Rule, offset: usize, strict: Strict, edit: fn(&mut Vec<u8>)) -> Case {
    Case {
        rule,
        offset,
        strict,
        edit,
    }
}

/// The interface image with a correct PercentInUse: 8 of its 100 clusters
/// are allocated. The backup's PercentInUse is stale (section 3.1.18).
fn image() -> Vec<u8> {
    let mut bytes = interface_support::image();
    bytes[112] = 8;
    bytes
}

/// Applies `edit` to both boot regions and reseals each one's checksum.
fn both_boot(bytes: &mut [u8], edit: impl Fn(&mut [u8])) {
    for base in [0, BACKUP] {
        let region = &mut bytes[base..base + BACKUP];
        edit(region);
        support::seal_boot(region, SECTOR);
    }
}

fn put(bytes: &mut [u8], at: usize, value: &[u8]) {
    bytes[at..at + value.len()].copy_from_slice(value);
}

/// Edits the entry set of `entries` entries at `at` and reseals its checksum.
fn set(bytes: &mut [u8], at: usize, entries: usize, edit: impl Fn(&mut [u8])) {
    let set = &mut bytes[at..at + entries * 32];
    edit(set);
    support::seal_set(set);
}

/// A Volume GUID entry (Table 32) with a valid SetChecksum.
fn guid(value: u8) -> Vec<u8> {
    let mut entry = vec![0u8; 32];
    entry[0] = 0xA0;
    entry[6..22].fill(value);
    support::seal_set(&mut entry);
    entry
}

/// `Child.bin`'s set with `extra` appended as secondaries, resealed.
fn child_with(bytes: &mut [u8], extra: &[u8]) {
    let mut child = support::file_set("Child.bin", 0x20, 9, 7, true);
    child[1] += (extra.len() / 32) as u8;
    child.extend_from_slice(extra);
    support::seal_set(&mut child);
    put(bytes, SUB, &child);
}

/// Up-case words (section 7.2.5) stored with a matching TableChecksum.
fn upcase(bytes: &mut [u8], words: &[u16]) {
    let table: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    put(bytes, UPCASE, &table);
    support::put64(bytes, ROOT + 56, table.len() as u64);
    support::put32(bytes, ROOT + 36, fs_exfat::metadata::checksum32(&table));
}

fn deviations(bytes: &[u8]) -> DeviationReport {
    spec_deviations(Bytes(bytes.to_vec()))
}

/// The (rule, offset) pairs of a report.
fn places(report: &DeviationReport) -> BTreeSet<(&'static str, u64)> {
    report
        .found
        .iter()
        .map(|deviation| (deviation.rule.id(), deviation.offset))
        .collect()
}

/// Everything but the deviations, for comparing verdicts.
fn verdict(report: &CheckReport) -> (bool, u64, u64, u64, u64, u64, bool, bool) {
    (
        report.healthy(),
        report.files,
        report.directories,
        report.allocated_clusters,
        report.referenced_clusters,
        report.unreferenced_clusters,
        report.dirty,
        report.media_failure,
    )
}

fn run(cases: &[Case]) {
    let clean = check_device(Bytes(image())).unwrap();
    for case in cases {
        let id = case.rule.id();
        let mut bytes = image();
        (case.edit)(&mut bytes);
        let report = deviations(&bytes);
        let found = report
            .of(case.rule)
            .find(|deviation| deviation.offset == case.offset as u64)
            .unwrap_or_else(|| {
                panic!(
                    "{id}: nothing at byte {}; found {:?}",
                    case.offset,
                    places(&report)
                )
            });
        assert_eq!(found.section, case.rule.section(), "{id}");
        assert_eq!(found.severity, case.rule.severity(), "{id}");
        assert!(found.length > 0, "{id}");
        match (case.strict, check_device(Bytes(bytes))) {
            (Strict::Accepts, Ok(checked)) => {
                assert_eq!(checked.deviations, report, "{id}");
                assert_eq!(verdict(&checked), verdict(&clean), "{id}");
            }
            (Strict::Rejects, Err(_)) => {}
            (expected, result) => panic!("{id}: strict layer {expected:?}, got {result:?}"),
        }
    }
}

fn boot_cases() -> Vec<Case> {
    use Strict::*;
    vec![
        case(Rule::JumpBoot, 0, Rejects, |b| {
            both_boot(b, |r| r[0] = 0xE9)
        }),
        case(Rule::FileSystemName, 3, Rejects, |b| {
            both_boot(b, |r| r[3..11].copy_from_slice(b"FAT32   "))
        }),
        case(Rule::MustBeZero, 11, Rejects, |b| {
            both_boot(b, |r| r[40] = 1)
        }),
        case(Rule::VolumeLength, 72, Rejects, |b| {
            both_boot(b, |r| support::put64(r, 72, 1024))
        }),
        case(Rule::FatOffset, 80, Rejects, |b| {
            both_boot(b, |r| support::put32(r, 80, 20))
        }),
        case(Rule::FatLength, 84, Rejects, |b| {
            both_boot(b, |r| support::put32(r, 84, 0))
        }),
        case(Rule::ClusterHeapOffset, 88, Rejects, |b| {
            both_boot(b, |r| support::put32(r, 88, 30))
        }),
        // ClusterCount 100 fits where (4096 - 32) / 1 clusters would: the
        // image's own deviation, which the strict layer accepts.
        case(Rule::ClusterCount, 92, Accepts, |_| {}),
        case(Rule::ClusterCount, BACKUP + 92, Accepts, |_| {}),
        case(Rule::RootCluster, 96, Rejects, |b| {
            both_boot(b, |r| support::put32(r, 96, 1))
        }),
        case(Rule::Revision, 104, Rejects, |b| {
            both_boot(b, |r| support::put16(r, 104, 0x0200))
        }),
        case(Rule::ActiveFat, 106, Rejects, |b| support::put16(b, 106, 1)),
        case(Rule::VolumeFlagsReserved, 106, Accepts, |b| {
            support::put16(b, 106, 0x0010)
        }),
        case(Rule::BytesPerSectorShift, 108, Rejects, |b| {
            both_boot(b, |r| r[108] = 13)
        }),
        case(Rule::SectorsPerClusterShift, 109, Rejects, |b| {
            both_boot(b, |r| r[109] = 17)
        }),
        case(Rule::NumberOfFats, 110, Rejects, |b| {
            both_boot(b, |r| r[110] = 3)
        }),
        case(Rule::PercentInUseRange, 112, Rejects, |b| b[112] = 101),
        case(Rule::PercentInUse, 112, Accepts, |b| b[112] = 50),
        case(Rule::BootReserved, 113, Accepts, |b| {
            both_boot(b, |r| r[115] = 1)
        }),
        case(Rule::BootSignature, 510, Rejects, |b| {
            both_boot(b, |r| r[510] = 0)
        }),
        case(Rule::ExtendedBootSignature, 2 * SECTOR - 4, Rejects, |b| {
            both_boot(b, |r| support::put32(r, 2 * SECTOR - 4, 0))
        }),
        case(Rule::NullParameters, 9 * SECTOR + 48 + 16, Accepts, |b| {
            both_boot(b, |r| r[9 * SECTOR + 48 + 20] = 1)
        }),
        case(Rule::OemReserved, 9 * SECTOR + 480, Accepts, |b| {
            both_boot(b, |r| r[9 * SECTOR + 500] = 1)
        }),
        case(Rule::BootReservedSector, 10 * SECTOR, Accepts, |b| {
            both_boot(b, |r| r[10 * SECTOR + 7] = 1)
        }),
        case(Rule::BootChecksum, 11 * SECTOR + 8, Rejects, |b| {
            b[11 * SECTOR + 8] ^= 1
        }),
        case(Rule::BackupBootRegion, BACKUP + 200, Rejects, |b| {
            b[BACKUP + 200] = 0xF4;
            support::seal_boot(&mut b[BACKUP..2 * BACKUP], SECTOR);
        }),
    ]
}

fn fat_and_bitmap_cases() -> Vec<Case> {
    use Strict::*;
    vec![
        case(Rule::FatEntry0, FAT + 1, Accepts, |b| {
            support::put32(b, FAT, 0x00FF_FFF8)
        }),
        case(Rule::FatMediaType, FAT, Accepts, |b| {
            support::put32(b, FAT, 0xFFFF_FFF0)
        }),
        case(Rule::FatEntry1, FAT + 4, Accepts, |b| {
            support::put32(b, FAT + 4, 0)
        }),
        // FAT entry 2 is the root directory's only link.
        case(Rule::FatChainValue, FAT + 8, Rejects, |b| {
            support::put32(b, FAT + 8, 0)
        }),
        case(Rule::FatChainEnd, FAT + 8, Rejects, |b| {
            support::put32(b, FAT + 8, 0xFFFF_FFF7)
        }),
        case(Rule::FatChainCycle, FAT + 8, Rejects, |b| {
            support::put32(b, FAT + 8, 2)
        }),
        case(Rule::BitmapEntryCount, ROOT_FREE, Rejects, |b| {
            b.copy_within(ROOT..ROOT + 32, ROOT_FREE)
        }),
        case(Rule::BitmapEntryCount, ROOT, Rejects, |b| b[ROOT] = 0x01),
        case(Rule::BitmapIdentifier, ROOT + 1, Rejects, |b| {
            b[ROOT + 1] = 1
        }),
        case(Rule::BitmapFlagsReserved, ROOT + 1, Rejects, |b| {
            b[ROOT + 1] = 2
        }),
        case(Rule::BitmapLength, ROOT + 24, Rejects, |b| {
            support::put64(b, ROOT + 24, 12)
        }),
        // Bit 4 of byte 12 is cluster 102: past ClusterCount + 1.
        case(Rule::BitmapReserved, BITMAP + 12, Accepts, |b| {
            b[BITMAP + 12] = 0x10
        }),
        // Cluster 8 holds the `Sub` directory.
        case(Rule::BitmapFreeCluster, BITMAP, Rejects, |b| {
            b[BITMAP] = 0xBF
        }),
        case(Rule::UpcaseEntryCount, ROOT_FREE, Rejects, |b| {
            b.copy_within(ROOT + 32..ROOT + 64, ROOT_FREE)
        }),
        case(Rule::UpcaseChecksum, ROOT + 36, Rejects, |b| {
            b[ROOT + 36] ^= 1
        }),
        // 'a' up-cases to 'B': Table 24 says 'A'.
        case(Rule::UpcaseMandatory, UPCASE, Rejects, |b| {
            let mut words = vec![0xFFFF, 97, 66];
            words.extend(66..=90);
            words.extend([0xFFFF, 65413]);
            upcase(b, &words);
        }),
        // The last identity run stops one character short of FFFFh.
        case(Rule::UpcaseCoverage, UPCASE, Rejects, |b| {
            let mut words = vec![0xFFFF, 97];
            words.extend(65..=90);
            words.extend([0xFFFF, 65412]);
            upcase(b, &words);
        }),
        case(Rule::UpcaseCoverage, ROOT + 56, Rejects, |b| {
            support::put64(b, ROOT + 56, 0)
        }),
        // The compressed table's last word starts a run with no count.
        case(Rule::UpcaseRun, UPCASE + 56, Rejects, |b| {
            let mut words = vec![0xFFFF, 97];
            words.extend(65..=90);
            words.push(0xFFFF);
            upcase(b, &words);
        }),
        // Its last run counts one character past FFFFh.
        case(Rule::UpcaseRun, UPCASE + 56, Rejects, |b| {
            let mut words = vec![0xFFFF, 97];
            words.extend(65..=90);
            words.extend([0xFFFF, 65414]);
            upcase(b, &words);
        }),
    ]
}

/// The image's own compressed table up to 'z', 28 words.
fn upcase_start() -> Vec<u16> {
    let mut words = vec![0xFFFF, 97];
    words.extend(65..=90);
    words
}

#[test]
fn compressed_upcase_deviations_point_at_their_words() {
    // A run of one for FFFFh, the last character, breaks no rule.
    let mut bytes = image();
    upcase(
        &mut bytes,
        &[upcase_start(), vec![0xFFFF, 65412, 0xFFFF, 1]].concat(),
    );
    assert_eq!(places(&deviations(&bytes)), places(&deviations(&image())));
    assert!(check_device(Bytes(bytes)).is_ok());

    // A word after the mapping of FFFFh is reported where it lies.
    let mut bytes = image();
    upcase(
        &mut bytes,
        &[upcase_start(), vec![0xFFFF, 65413, 0x41]].concat(),
    );
    let report = deviations(&bytes);
    let found: Vec<_> = report.of(Rule::UpcaseCoverage).collect();
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].offset, found[0].length), (UPCASE as u64 + 60, 2));

    // A run of no characters stands for no mappings and breaks no rule (#75).
    let mut bytes = image();
    upcase(
        &mut bytes,
        &[upcase_start(), vec![0xFFFF, 0, 0xFFFF, 65413]].concat(),
    );
    assert_eq!(places(&deviations(&bytes)), places(&deviations(&image())));
    assert!(check_device(Bytes(bytes)).is_ok());

    // A run past FFFFh before the last one is one deviation, at its FFFFh and
    // count; the table's coverage past it is unknown, so it is not reported
    // as well.
    let mut bytes = image();
    upcase(
        &mut bytes,
        &[upcase_start(), vec![0xFFFF, 65414, 0xFFFF, 65413]].concat(),
    );
    let report = deviations(&bytes);
    let run: Vec<_> = report.of(Rule::UpcaseRun).collect();
    assert_eq!(run.len(), 1);
    assert_eq!((run[0].offset, run[0].length), (UPCASE as u64 + 56, 4));
    assert_eq!(report.count(Rule::UpcaseCoverage), 0);
    // The 123 mappings before it follow Table 24; the rest are unknown.
    assert_eq!(report.count(Rule::UpcaseMandatory), 0);
    assert!(check_device(Bytes(bytes)).is_err());

    // 'a' up-cases to 'B': the deviation spans the 30 words that encode the
    // first 128 mappings, not 256 bytes.
    let mut bytes = image();
    let mut words = vec![0xFFFF, 97, 66];
    words.extend(66..=90);
    words.extend([0xFFFF, 65413]);
    upcase(&mut bytes, &words);
    let report = deviations(&bytes);
    let found: Vec<_> = report.of(Rule::UpcaseMandatory).collect();
    assert_eq!((found[0].offset, found[0].length), (UPCASE as u64, 60));
}

fn root_cases() -> Vec<Case> {
    use Strict::*;
    vec![
        case(Rule::LabelCount, ROOT_FREE, Rejects, |b| {
            b.copy_within(LABEL..LABEL + 32, ROOT_FREE)
        }),
        case(Rule::LabelLength, LABEL + 1, Rejects, |b| b[LABEL + 1] = 12),
        case(Rule::LabelCharacters, LABEL + 2, Rejects, |b| {
            support::put16(b, LABEL + 2, 0x2A)
        }),
        case(Rule::GuidCount, ROOT_FREE + 32, Rejects, |b| {
            put(b, ROOT_FREE, &guid(1));
            put(b, ROOT_FREE + 32, &guid(2));
        }),
        case(Rule::GuidFields, ROOT_FREE + 4, Rejects, |b| {
            let mut entry = guid(1);
            entry[4] = 1;
            support::seal_set(&mut entry);
            put(b, ROOT_FREE, &entry);
        }),
        case(Rule::GuidNull, ROOT_FREE + 6, Rejects, |b| {
            put(b, ROOT_FREE, &guid(0))
        }),
        case(Rule::GuidOutsideRoot, SUB_FREE, Rejects, |b| {
            put(b, SUB_FREE, &guid(1))
        }),
        case(Rule::SystemEntryReserved, ROOT + 2, Accepts, |b| {
            b[ROOT + 5] = 1
        }),
        case(Rule::SystemEntryReserved, LABEL + 24, Accepts, |b| {
            b[LABEL + 30] = 1
        }),
    ]
}

fn directory_cases() -> Vec<Case> {
    use Strict::*;
    vec![
        case(Rule::EntryType80, ROOT_FREE, Rejects, |b| {
            b[ROOT_FREE] = 0x80
        }),
        // An unused entry after the root's end-of-directory marker.
        case(Rule::EndOfDirectory, ROOT_FREE + 32, Accepts, |b| {
            b[ROOT_FREE + 32] = 0x05
        }),
        case(Rule::SetInUse, EXAMPLE + 64, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| s[64] = 0x41)
        }),
        // SecondaryCount 3 reaches `Empty.bin`'s File entry.
        case(Rule::SetSecondary, EMPTY, Rejects, |b| b[EXAMPLE + 1] = 3),
        case(Rule::OrphanSecondary, ROOT_FREE, Rejects, |b| {
            b[ROOT_FREE] = 0xC0
        }),
        case(Rule::SetChecksum, EXAMPLE + 2, Rejects, |b| {
            b[EXAMPLE + 2] ^= 1
        }),
        // Unused entries, then a File entry in `Sub`'s last slot that
        // declares two more entries than the directory holds.
        case(Rule::SetTruncated, SUB + 15 * 32 + 1, Rejects, |b| {
            for slot in 3..15 {
                b[SUB + slot * 32] = 0x05;
            }
            let file = support::file_set("Late", 0x20, 0, 0, false);
            put(b, SUB + 15 * 32, &file[..32]);
        }),
        case(Rule::RootCriticalPrimary, ROOT_FREE, Rejects, |b| {
            b[ROOT_FREE] = 0x84
        }),
        case(Rule::CriticalPrimaryOutsideRoot, SUB_FREE, Rejects, |b| {
            b.copy_within(LABEL..LABEL + 32, SUB_FREE)
        }),
        // A benign primary that revision 1.00 does not define, with a valid
        // set: skipped by the strict layer, as section 6.3.1.2.2 allows.
        case(Rule::RevisionEntryType, SUB_FREE, Accepts, |b| {
            let mut entry = vec![0u8; 32];
            entry[0] = 0xA5;
            support::seal_set(&mut entry);
            put(b, SUB_FREE, &entry);
        }),
        case(Rule::RevisionEntryType, SUB + 96, Accepts, |b| {
            let mut entry = vec![0u8; 32];
            entry[0] = 0xE7;
            child_with(b, &entry);
        }),
    ]
}

fn file_cases() -> Vec<Case> {
    use Strict::*;
    let stream = EXAMPLE + 32;
    let name = EXAMPLE + 64;
    vec![
        case(Rule::FileStream, stream, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| s[32] = 0xC1)
        }),
        case(Rule::FileNames, name, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| s[64] = 0xC0)
        }),
        case(Rule::VendorOrder, stream, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| s[32] = 0xE0)
        }),
        case(Rule::FileAttributesReserved, EXAMPLE + 4, Accepts, |b| {
            set(b, EXAMPLE, 3, |s| s[4] |= 0x40)
        }),
        case(Rule::FileReserved, EXAMPLE + 25, Accepts, |b| {
            set(b, EXAMPLE, 3, |s| s[26] = 1)
        }),
        case(Rule::Timestamp, EXAMPLE + 8, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| {
                support::put32(s, 8, (46 << 25) | (13 << 21) | (4 << 16))
            })
        }),
        case(Rule::Timestamp, EXAMPLE + 12, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| {
                support::put32(s, 12, (46 << 25) | (2 << 21) | (30 << 16))
            })
        }),
        case(Rule::TenMsIncrement, EXAMPLE + 20, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| s[20] = 200)
        }),
        case(Rule::UtcOffset, EXAMPLE + 22, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| s[22] = 0x05)
        }),
        case(Rule::StreamAllocationPossible, stream + 1, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| s[33] = 0x02)
        }),
        case(Rule::StreamReserved, stream + 2, Accepts, |b| {
            set(b, EXAMPLE, 3, |s| s[34] = 1)
        }),
        case(Rule::StreamReserved, stream + 1, Accepts, |b| {
            set(b, EXAMPLE, 3, |s| s[33] |= 0x80)
        }),
        case(Rule::NameLength, stream + 3, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| s[35] = 0)
        }),
        case(Rule::NameHash, stream + 4, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| s[36] ^= 1)
        }),
        case(Rule::ValidDataLength, stream + 8, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| support::put64(s, 40, 2000))
        }),
        // A directory whose DataLength is not whole clusters.
        case(Rule::DirectoryLength, SUB_SET + 32 + 24, Rejects, |b| {
            set(b, SUB_SET, 3, |s| {
                support::put64(s, 40, 1000);
                support::put64(s, 56, 1000);
            })
        }),
        case(Rule::FirstCluster, stream + 20, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| support::put32(s, 52, 300))
        }),
        case(Rule::DataLength, EMPTY + 32 + 24, Rejects, |b| {
            set(b, EMPTY, 3, |s| support::put64(s, 56, 10))
        }),
        case(Rule::NoFatChain, EMPTY + 32 + 1, Rejects, |b| {
            set(b, EMPTY, 3, |s| s[33] = 0x03)
        }),
        // Three contiguous clusters from cluster 100, the heap's last.
        case(Rule::ContiguousRange, stream + 24, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| support::put32(s, 52, 100))
        }),
        case(Rule::NameEntryFlags, name + 1, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| s[65] = 1)
        }),
        case(Rule::NameFlagsReserved, name + 1, Accepts, |b| {
            set(b, EXAMPLE, 3, |s| s[65] = 4)
        }),
        case(Rule::NameCharacters, name + 2, Rejects, |b| {
            set(b, EXAMPLE, 3, |s| support::put16(s, 66, 0x3F))
        }),
        case(Rule::NameReserved, SUB_FREE + 66, Rejects, |b| {
            put(b, SUB_FREE, &support::file_set("..", 0x20, 0, 0, false))
        }),
        // "Example.txt" has 11 characters; the twelfth slot is unused.
        case(Rule::NameUnusedZero, name + 2 + 22, Accepts, |b| {
            set(b, EXAMPLE, 3, |s| support::put16(s, 64 + 2 + 22, 0x78))
        }),
        case(Rule::NameDuplicate, ROOT_FREE, Rejects, |b| {
            put(
                b,
                ROOT_FREE,
                &support::file_set("EXAMPLE.txt", 0x20, 0, 0, false),
            )
        }),
        case(Rule::VendorGuid, SUB + 96 + 2, Accepts, |b| {
            let mut entry = vec![0u8; 32];
            entry[0] = 0xE0;
            child_with(b, &entry);
        }),
        case(Rule::VendorFlags, SUB + 96 + 1, Accepts, |b| {
            let mut entry = vec![0x5Au8; 32];
            entry[0] = 0xE0;
            entry[1] = 0x01;
            child_with(b, &entry);
        }),
        // A Vendor Allocation outside the heap: the strict layer does not
        // interpret vendor entries, so it accepts what section 6.4.3 forbids.
        case(Rule::FirstCluster, SUB + 96 + 20, Accepts, |b| {
            vendor_allocation(b, 0x01, 500, 512)
        }),
        case(Rule::DataLength, SUB + 96 + 24, Accepts, |b| {
            vendor_allocation(b, 0x01, 0, 10)
        }),
        case(Rule::NoFatChain, SUB + 96 + 1, Accepts, |b| {
            vendor_allocation(b, 0x03, 0, 0)
        }),
        case(Rule::ContiguousRange, SUB + 96 + 24, Accepts, |b| {
            vendor_allocation(b, 0x03, 100, 1536)
        }),
    ]
}

/// A Vendor Allocation entry (Table 37) after `Child.bin`'s name.
fn vendor_allocation(bytes: &mut [u8], flags: u8, first: u32, length: u64) {
    let mut entry = vec![0u8; 32];
    entry[0] = 0xE1;
    entry[1] = flags;
    entry[2..18].fill(0x5A);
    support::put32(&mut entry, 20, first);
    support::put64(&mut entry, 24, length);
    child_with(bytes, &entry);
}

#[test]
fn the_image_deviates_only_where_its_fixture_is_not_a_formatter() {
    let report = deviations(&image());
    let expected: BTreeSet<_> = [("cluster-count", 92), ("cluster-count", BACKUP as u64 + 92)]
        .into_iter()
        .collect();
    assert_eq!(places(&report), expected);
    assert!(report.complete, "{:?}", report.skipped);
    assert!(report.omitted.is_empty());
}

#[test]
fn boot_region_rules() {
    run(&boot_cases());
}

#[test]
fn fat_bitmap_and_upcase_rules() {
    run(&fat_and_bitmap_cases());
    // A root chain 2 -> 10 -> 2 is found to loop only after cluster 2 comes
    // round again; its entries are still checked once, so no name repeats.
    let mut bytes = image();
    support::put32(&mut bytes, FAT + 8, 10);
    support::put32(&mut bytes, FAT + 40, 2);
    let report = deviations(&bytes);
    assert_eq!(report.count(Rule::FatChainCycle), 1);
    assert_eq!(report.count(Rule::NameDuplicate), 0);
}

#[test]
fn root_entry_rules() {
    run(&root_cases());
}

#[test]
fn directory_and_entry_set_rules() {
    run(&directory_cases());
}

#[test]
fn file_entry_set_rules() {
    run(&file_cases());
}

/// One row of `docs/spec-rules.md`: rule, section, severity, strict layer.
fn documented() -> Vec<(String, String, String, String)> {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/spec-rules.md"))
        .unwrap();
    text.lines()
        .filter(|line| line.starts_with("| `"))
        .map(|line| {
            let cells: Vec<_> = line.split('|').map(str::trim).collect();
            (
                cells[1].trim_matches('`').to_owned(),
                cells[2].to_owned(),
                cells[3].to_owned(),
                cells[5].to_owned(),
            )
        })
        .collect()
}

#[test]
fn every_documented_rule_is_in_code_and_has_a_case() {
    let rows = documented();
    let ids: Vec<_> = rows.iter().map(|row| row.0.as_str()).collect();
    let code: Vec<_> = Rule::ALL.iter().map(|rule| rule.id()).collect();
    assert_eq!(ids, code);
    let cases: Vec<Case> = [
        boot_cases(),
        fat_and_bitmap_cases(),
        root_cases(),
        directory_cases(),
        file_cases(),
    ]
    .into_iter()
    .flatten()
    .collect();
    for (rule, (id, section, severity, strict)) in Rule::ALL.iter().zip(&rows) {
        assert_eq!(rule.section(), section, "{id}");
        let named = match rule.severity() {
            Severity::Notice => "notice",
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        assert_eq!(named, severity, "{id}");
        let verdicts: Vec<_> = cases
            .iter()
            .filter(|case| case.rule == *rule)
            .map(|case| case.strict)
            .collect();
        assert!(!verdicts.is_empty(), "{id} has no synthetic case");
        if strict.starts_with("Rejects") {
            assert!(verdicts.contains(&Strict::Rejects), "{id}: {strict}");
        }
        if strict.starts_with("Accepts") || strict.contains("accepts") {
            assert!(verdicts.contains(&Strict::Accepts), "{id}: {strict}");
        }
    }
}

#[test]
fn the_strict_layer_rejects_some_volumes_the_specification_allows() {
    // Revision 1.01 (section 3.1.12: implementations should mount any minor
    // revision) and a bitmap longer than ClusterCount needs (Table 22's
    // Reserved bits): no deviation, yet the strict check refuses both.
    let cases: [fn(&mut Vec<u8>); 2] = [
        |b| both_boot(b, |r| support::put16(r, 104, 0x0101)),
        |b| support::put64(b, ROOT + 24, 14),
    ];
    let clean = places(&deviations(&image()));
    for edit in cases {
        let mut bytes = image();
        edit(&mut bytes);
        assert_eq!(places(&deviations(&bytes)), clean);
        assert!(check_device(Bytes(bytes)).is_err());
    }
}

#[test]
fn deviations_beyond_the_listing_limit_are_counted() {
    let mut report = DeviationReport::default();
    for offset in 0..70 {
        report.report(Rule::EndOfDirectory, offset * 32, 32, String::new);
    }
    assert_eq!(
        report.found.len(),
        fs_exfat::primitives::MAX_LISTED_PER_RULE
    );
    assert_eq!(report.omitted.get(&Rule::EndOfDirectory), Some(&6));
    assert_eq!(report.count(Rule::EndOfDirectory), 70);
}
