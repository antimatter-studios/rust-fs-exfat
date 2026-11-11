//! Rules of Microsoft's exFAT specification, checked against raw bytes.
//!
//! `docs/spec-rules.md` lists every rule with its section, its severity and
//! what the strict reader does with a violation; [`Rule`] is that table in
//! code. The evaluators here only observe. They decode fields and report each
//! one that breaks a rule as a [`Deviation`], with its byte offset, and never
//! reject anything. Callers supply volume byte offsets, so a structure that
//! spans clusters is reported where its bytes really lie.

use super::{boot_checksum, entry_set_checksum, name_hash, u16_at, u32_at, u64_at, ENTRY_SIZE};
use crate::salvage::Severity;
use serde::{Serialize, Serializer};
use std::collections::{BTreeMap, HashSet};
use std::ops::ControlFlow;

macro_rules! rules {
    ($($variant:ident $id:literal $section:literal $severity:ident;)*) => {
        /// A rule of the specification, named by its identifier in
        /// `docs/spec-rules.md`.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Rule {
            $($variant,)*
        }

        impl Rule {
            /// Every rule, in the order `docs/spec-rules.md` lists them.
            pub const ALL: &'static [Rule] = &[$(Rule::$variant,)*];

            /// The identifier, which is a deviation's `rule` value.
            pub fn id(self) -> &'static str {
                match self {
                    $(Rule::$variant => $id,)*
                }
            }

            /// The specification sections the rule comes from.
            pub fn section(self) -> &'static str {
                match self {
                    $(Rule::$variant => $section,)*
                }
            }

            /// How serious a violation is (`docs/spec-rules.md`, "Severity").
            pub fn severity(self) -> Severity {
                match self {
                    $(Rule::$variant => Severity::$severity,)*
                }
            }
        }
    };
}

rules! {
    JumpBoot "jump-boot" "3.1.1" Error;
    FileSystemName "file-system-name" "3.1.2" Error;
    MustBeZero "must-be-zero" "3.1.3" Error;
    VolumeLength "volume-length" "3.1.5" Error;
    FatOffset "fat-offset" "3.1.6" Error;
    FatLength "fat-length" "3.1.7" Error;
    ClusterHeapOffset "cluster-heap-offset" "3.1.8" Error;
    ClusterCount "cluster-count" "3.1.9" Warning;
    RootCluster "root-cluster" "3.1.10" Error;
    Revision "revision" "3.1.12" Error;
    ActiveFat "active-fat" "3.1.13.1" Error;
    VolumeFlagsReserved "volume-flags-reserved" "3.1.13 (Table 5)" Notice;
    BytesPerSectorShift "bytes-per-sector-shift" "3.1.14" Error;
    SectorsPerClusterShift "sectors-per-cluster-shift" "3.1.15" Error;
    NumberOfFats "number-of-fats" "3.1.16" Error;
    PercentInUseRange "percent-in-use-range" "3.1.18" Error;
    PercentInUse "percent-in-use" "3.1.18" Warning;
    BootReserved "boot-reserved" "3.1 (Table 4)" Notice;
    BootSignature "boot-signature" "3.1.20" Error;
    ExtendedBootSignature "extended-boot-signature" "3.2.2" Error;
    NullParameters "null-parameters" "3.3.3" Notice;
    OemReserved "oem-reserved" "3.3 (Table 7)" Notice;
    BootReservedSector "boot-reserved-sector" "2 (Table 3)" Notice;
    BootChecksum "boot-checksum" "3.4" Error;
    BackupBootRegion "backup-boot-region" "3, 3.1" Warning;
    FatEntry0 "fat-entry-0" "4.1.1" Warning;
    FatMediaType "fat-media-type" "4.1.1" Notice;
    FatEntry1 "fat-entry-1" "4.1.2" Warning;
    FatChainValue "fat-chain-value" "4.1.3" Error;
    FatChainEnd "fat-chain-end" "4.1.3" Error;
    FatChainCycle "fat-chain-cycle" "4.1.3" Error;
    BitmapEntryCount "bitmap-entry-count" "7.1" Error;
    BitmapIdentifier "bitmap-identifier" "7.1.2.1" Error;
    BitmapFlagsReserved "bitmap-flags-reserved" "7.1.2 (Table 21)" Notice;
    BitmapLength "bitmap-length" "7.1.5 (Table 22)" Error;
    BitmapReserved "bitmap-reserved" "7.1.5 (Table 22)" Notice;
    BitmapFreeCluster "bitmap-free-cluster" "7.1.5.1" Error;
    UpcaseEntryCount "upcase-entry-count" "7.2" Error;
    UpcaseChecksum "upcase-checksum" "7.2.2" Error;
    UpcaseMandatory "upcase-mandatory" "7.2.5 (Table 24)" Error;
    UpcaseCoverage "upcase-coverage" "7.2.5, 7.2.5.1" Error;
    LabelCount "label-count" "7.3" Error;
    LabelLength "label-length" "7.3.2" Error;
    LabelCharacters "label-characters" "7.3.3" Error;
    GuidCount "guid-count" "7.5" Error;
    GuidFields "guid-fields" "7.5.2, 7.5.4.1" Error;
    GuidNull "guid-null" "7.5.5" Error;
    GuidOutsideRoot "guid-outside-root" "7.5" Warning;
    SystemEntryReserved "system-entry-reserved" "7.1, 7.2, 7.3, 7.5 (Tables 20, 23, 26, 32)" Notice;
    EntryType80 "entry-type-80" "6.2.1" Error;
    EndOfDirectory "end-of-directory" "6.2.1" Warning;
    SetInUse "set-in-use" "6.2.1" Error;
    SetSecondary "set-secondary" "6.3" Error;
    OrphanSecondary "orphan-secondary" "6.3, 6.4" Error;
    SetChecksum "set-checksum" "6.3.3" Error;
    SetTruncated "set-truncated" "6.3.2" Error;
    RootCriticalPrimary "root-critical-primary" "8.2, 6.3.1.2.1" Error;
    CriticalPrimaryOutsideRoot "critical-primary-outside-root" "6.3.1.2.1, 8.2" Error;
    RevisionEntryType "revision-entry-type" "3.1.12, 7" Warning;
    FileStream "file-stream" "7.4, 7.6" Error;
    FileNames "file-names" "7.4, 7.7" Error;
    VendorOrder "vendor-order" "7.8, 7.9" Error;
    FileAttributesReserved "file-attributes-reserved" "7.4.4 (Table 28)" Notice;
    FileReserved "file-reserved" "7.4 (Table 27)" Notice;
    Timestamp "timestamp" "7.4.8" Error;
    TenMsIncrement "ten-ms-increment" "7.4.9" Error;
    UtcOffset "utc-offset" "7.4.10.2" Error;
    StreamAllocationPossible "stream-allocation-possible" "7.6.2.1" Error;
    StreamReserved "stream-reserved" "7.6 (Table 33), 7.6.2" Notice;
    NameLength "name-length" "7.6.3" Error;
    NameHash "name-hash" "7.6.4" Error;
    ValidDataLength "valid-data-length" "7.6.5" Error;
    DirectoryLength "directory-length" "7.6.7" Error;
    FirstCluster "first-cluster" "6.2.2, 6.4.3" Error;
    DataLength "data-length" "6.2.3, 6.4.4" Error;
    NoFatChain "no-fat-chain" "6.3.4.2, 6.4.3, 6.4.4" Error;
    ContiguousRange "contiguous-range" "6.3.4.2" Error;
    NameEntryFlags "name-entry-flags" "7.7.2.1, 6.3.4.2" Error;
    NameFlagsReserved "name-flags-reserved" "7.7.2" Notice;
    NameCharacters "name-characters" "7.7.3 (Table 35)" Error;
    NameReserved "name-reserved" "7.7.3" Error;
    NameUnusedZero "name-unused-zero" "7.7.3" Notice;
    NameDuplicate "name-duplicate" "7.7" Error;
    VendorGuid "vendor-guid" "7.8.3, 7.9.3" Warning;
    VendorFlags "vendor-flags" "7.8.2.1, 7.9.2.1" Warning;
}

impl Serialize for Rule {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.id())
    }
}

/// One place where the bytes break a rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Deviation {
    pub rule: Rule,
    /// The rule's specification sections.
    pub section: &'static str,
    pub severity: Severity,
    /// Volume byte offset of the deviating field.
    pub offset: u64,
    /// Length in bytes of the deviating field.
    pub length: u64,
    /// What was observed, such as the stored and the required values.
    pub detail: String,
}

/// At most this many deviations of one rule are listed; the rest are counted.
pub const MAX_LISTED_PER_RULE: usize = 64;

/// The deviations found, apart from any accept or reject decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviationReport {
    /// Listed deviations, in the order they were found.
    pub found: Vec<Deviation>,
    /// Deviations past [`MAX_LISTED_PER_RULE`] of one rule, counted by rule.
    pub omitted: BTreeMap<Rule, u64>,
    /// Every part of the scan ran; false when [`Self::skipped`] names any.
    pub complete: bool,
    /// Parts not checked, with the reason: unreadable or unusable structures,
    /// or a memory budget. At most [`MAX_LISTED_PER_RULE`] are named.
    pub skipped: Vec<String>,
    /// Parts not checked past those named in [`Self::skipped`].
    pub skipped_omitted: u64,
    #[serde(skip)]
    listed: Vec<usize>,
}

impl Default for DeviationReport {
    fn default() -> Self {
        Self {
            found: Vec::new(),
            omitted: BTreeMap::new(),
            complete: true,
            skipped: Vec::new(),
            skipped_omitted: 0,
            listed: vec![0; Rule::ALL.len()],
        }
    }
}

impl DeviationReport {
    /// Records a deviation of `rule` at `offset`. `detail` runs only when the
    /// deviation is listed.
    pub fn report(
        &mut self,
        rule: Rule,
        offset: u64,
        length: u64,
        detail: impl FnOnce() -> String,
    ) {
        let listed = &mut self.listed[rule as usize];
        if *listed >= MAX_LISTED_PER_RULE {
            *self.omitted.entry(rule).or_default() += 1;
            return;
        }
        *listed += 1;
        self.found.push(Deviation {
            rule,
            section: rule.section(),
            severity: rule.severity(),
            offset,
            length,
            detail: detail(),
        });
    }

    /// Records a part of the scan that could not run.
    pub fn skip(&mut self, what: impl Into<String>) {
        self.complete = false;
        if self.skipped.len() < MAX_LISTED_PER_RULE {
            self.skipped.push(what.into());
        } else {
            self.skipped_omitted += 1;
        }
    }

    /// Deviations of `rule`, listed and omitted.
    pub fn count(&self, rule: Rule) -> u64 {
        self.listed[rule as usize] as u64 + self.omitted.get(&rule).copied().unwrap_or(0)
    }

    /// The listed deviations of `rule`.
    pub fn of(&self, rule: Rule) -> impl Iterator<Item = &Deviation> {
        self.found.iter().filter(move |found| found.rule == rule)
    }

    /// Whether nothing deviates. Skipped parts are not deviations.
    pub fn is_empty(&self) -> bool {
        self.found.is_empty() && self.omitted.is_empty()
    }
}

const JUMP_BOOT: [u8; 3] = [0xEB, 0x76, 0x90];
const EXTENDED_BOOT_SIGNATURE: u32 = 0xAA55_0000;
const MAX_CLUSTER_COUNT: u64 = (1 << 32) - 11;
const MAX_DIRECTORY_BYTES: u64 = 256 * 1024 * 1024;
const PARAMETERS_OFFSET: usize = 9;
const PARAMETERS: usize = 10;
const PARAMETERS_LENGTH: usize = 48;
const OEM_RESERVED: usize = 480;
const RESERVED_SECTOR: usize = 10;
const CHECKSUM_SECTOR: usize = 11;
const VOLUME_FLAGS: usize = 106;
const PERCENT_IN_USE: usize = 112;
const ATTRIBUTE_DIRECTORY: u16 = 0x10;
const ATTRIBUTES_DEFINED: u16 = 0x37;
const MEDIA_TYPE: u8 = 0xF8;

fn nonzero(bytes: &[u8]) -> Option<usize> {
    bytes.iter().position(|byte| *byte != 0)
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Table 35: control codes and `" * / : < > ? \ |`.
pub fn invalid_name_character(unit: u16) -> bool {
    unit < 0x20 || [0x22, 0x2A, 0x2F, 0x3A, 0x3C, 0x3E, 0x3F, 0x5C, 0x7C].contains(&unit)
}

/// Checks one twelve-sector boot region with `sector`-byte sectors. `base` is
/// the region's volume byte offset. Section 3.1 makes the backup's VolumeFlags
/// stale, so `main` alone checks them.
///
/// Panics when `region` is shorter than twelve sectors or `sector` is not 512,
/// 1024, 2048 or 4096.
pub fn boot_region_deviations(
    region: &[u8],
    sector: usize,
    base: u64,
    main: bool,
    out: &mut DeviationReport,
) {
    let region = &region[..sector * 12];
    let at = |offset: usize| base + offset as u64;
    if region[..3] != JUMP_BOOT {
        out.report(Rule::JumpBoot, at(0), 3, || {
            format!("{}, not EB 76 90", hex(&region[..3]))
        });
    }
    if &region[3..11] != b"EXFAT   " {
        out.report(Rule::FileSystemName, at(3), 8, || {
            format!(
                "{:?}, not \"EXFAT   \"",
                String::from_utf8_lossy(&region[3..11])
            )
        });
    }
    if let Some(position) = nonzero(&region[11..64]) {
        out.report(Rule::MustBeZero, at(11), 53, || {
            format!("byte {} is {:02X}h", 11 + position, region[11 + position])
        });
    }
    let revision = u16_at(region, 104);
    if revision >> 8 != 1 || revision & 0xFF > 99 {
        out.report(Rule::Revision, at(104), 2, || {
            format!("revision {}.{:02}", revision >> 8, revision & 0xFF)
        });
    }
    let flags = u16_at(region, VOLUME_FLAGS);
    let fats = region[110];
    if main && flags & 1 != 0 && fats != 2 {
        out.report(Rule::ActiveFat, at(VOLUME_FLAGS), 2, || {
            format!("ActiveFat is 1 with NumberOfFats {fats}")
        });
    }
    if main && flags & 0xFFF0 != 0 {
        out.report(Rule::VolumeFlagsReserved, at(VOLUME_FLAGS), 2, || {
            format!("VolumeFlags {flags:04X}h sets reserved bits")
        });
    }
    let shift = region[108];
    let valid_shift = (9..=12).contains(&shift);
    if !valid_shift {
        out.report(Rule::BytesPerSectorShift, at(108), 1, || {
            format!("{shift}, not 9 to 12")
        });
    }
    let cluster_shift = region[109];
    if valid_shift && cluster_shift > 25 - shift {
        out.report(Rule::SectorsPerClusterShift, at(109), 1, || {
            format!("{cluster_shift}, more than {}", 25 - shift)
        });
    }
    if !matches!(fats, 1 | 2) {
        out.report(Rule::NumberOfFats, at(110), 1, || {
            format!("{fats}, not 1 or 2")
        });
    }
    let percent = region[PERCENT_IN_USE];
    if percent > 100 && percent != 0xFF {
        out.report(Rule::PercentInUseRange, at(PERCENT_IN_USE), 1, || {
            format!("{percent}, not 0 to 100 or FFh")
        });
    }
    if let Some(position) = nonzero(&region[113..120]) {
        out.report(Rule::BootReserved, at(113), 7, || {
            format!("byte {} is {:02X}h", 113 + position, region[113 + position])
        });
    }
    if region[510..512] != [0x55, 0xAA] {
        out.report(Rule::BootSignature, at(510), 2, || {
            format!("{:04X}h, not AA55h", u16_at(region, 510))
        });
    }
    if valid_shift && cluster_shift <= 25 - shift {
        geometry_deviations(region, shift, at, out);
    }
    for index in 1..=8 {
        let position = (index + 1) * sector - 4;
        let signature = u32_at(region, position);
        if signature != EXTENDED_BOOT_SIGNATURE {
            out.report(Rule::ExtendedBootSignature, at(position), 4, || {
                format!("extended boot sector {index} ends with {signature:08X}h")
            });
        }
    }
    let parameters = &region[PARAMETERS_OFFSET * sector..(PARAMETERS_OFFSET + 1) * sector];
    for index in 0..PARAMETERS {
        let structure = &parameters[index * PARAMETERS_LENGTH..(index + 1) * PARAMETERS_LENGTH];
        if nonzero(&structure[..16]).is_none() && nonzero(&structure[16..]).is_some() {
            let position = PARAMETERS_OFFSET * sector + index * PARAMETERS_LENGTH + 16;
            out.report(Rule::NullParameters, at(position), 32, || {
                format!("Parameters[{index}] has the null GUID and non-zero reserved bytes")
            });
        }
    }
    if let Some(position) = nonzero(&parameters[OEM_RESERVED..]) {
        out.report(
            Rule::OemReserved,
            at(PARAMETERS_OFFSET * sector + OEM_RESERVED),
            (sector - OEM_RESERVED) as u64,
            || {
                format!(
                    "byte {} of the OEM Parameters sector is not zero",
                    OEM_RESERVED + position
                )
            },
        );
    }
    let reserved = &region[RESERVED_SECTOR * sector..(RESERVED_SECTOR + 1) * sector];
    if let Some(position) = nonzero(reserved) {
        out.report(
            Rule::BootReservedSector,
            at(RESERVED_SECTOR * sector),
            sector as u64,
            || format!("byte {position} of the Reserved sector is not zero"),
        );
    }
    let computed = boot_checksum(region, sector);
    let words = &region[CHECKSUM_SECTOR * sector..(CHECKSUM_SECTOR + 1) * sector];
    let wrong: Vec<_> = (0..sector / 4)
        .filter(|index| u32_at(words, index * 4) != computed)
        .collect();
    if let Some(first) = wrong.first() {
        out.report(Rule::BootChecksum, at(CHECKSUM_SECTOR * sector + first * 4), 4, || {
            format!(
                "{} of {} checksum words differ from the computed {computed:08X}h; the first holds {:08X}h",
                wrong.len(),
                sector / 4,
                u32_at(words, first * 4)
            )
        });
    }
}

fn geometry_deviations(
    region: &[u8],
    shift: u8,
    at: impl Fn(usize) -> u64,
    out: &mut DeviationReport,
) {
    let sector = 1u64 << shift;
    let cluster_shift = u32::from(region[109]);
    let volume_length = u64_at(region, 72);
    let fat_offset = u64::from(u32_at(region, 80));
    let fat_length = u64::from(u32_at(region, 84));
    let heap = u64::from(u32_at(region, 88));
    let clusters = u64::from(u32_at(region, 92));
    let root = u64::from(u32_at(region, 96));
    let fats = u64::from(region[110]);
    if volume_length < (1 << 20) >> shift {
        out.report(Rule::VolumeLength, at(72), 8, || {
            format!("{volume_length} sectors, fewer than 1 MB")
        });
    }
    // NumberOfFats outside 1 and 2 is its own deviation; the bounds that
    // depend on it are then not checked.
    let fats_valid = matches!(fats, 1 | 2);
    let fats_end = fat_length * fats;
    if fat_offset < 24
        || fats_valid
            && heap
                .checked_sub(fats_end)
                .is_none_or(|limit| fat_offset > limit)
    {
        out.report(Rule::FatOffset, at(80), 4, || {
            format!("{fat_offset}, not 24 to ClusterHeapOffset {heap} - FatLength {fat_length} x NumberOfFats")
        });
    }
    let minimum = ((clusters + 2) * 4).div_ceil(sector);
    if fats_valid
        && (fat_length < minimum
            || heap
                .checked_sub(fat_offset)
                .is_none_or(|room| fat_length > room / fats))
    {
        out.report(Rule::FatLength, at(84), 4, || {
            format!("{fat_length} sectors, not {minimum} to (ClusterHeapOffset - FatOffset) / NumberOfFats")
        });
    }
    let heap_sectors = clusters << cluster_shift;
    if fats_valid && heap < fat_offset + fats_end
        || volume_length
            .checked_sub(heap_sectors)
            .is_none_or(|limit| heap > limit)
    {
        out.report(Rule::ClusterHeapOffset, at(88), 4, || {
            format!("{heap}, outside the FATs' end and VolumeLength - ClusterCount x sectors per cluster")
        });
    }
    if let Some(room) = volume_length.checked_sub(heap) {
        let expected = (room >> cluster_shift).min(MAX_CLUSTER_COUNT);
        if clusters != expected {
            out.report(Rule::ClusterCount, at(92), 4, || {
                format!("{clusters}, not the {expected} clusters that fit")
            });
        }
    }
    if root < 2 || root > clusters + 1 {
        out.report(Rule::RootCluster, at(96), 4, || {
            format!("{root}, not 2 to ClusterCount + 1 ({})", clusters + 1)
        });
    }
}

/// Reports each run of bytes where the backup boot region differs from the
/// main one, outside VolumeFlags and PercentInUse. `backup_base` is the
/// backup's volume byte offset.
pub fn backup_boot_deviations(
    main: &[u8],
    backup: &[u8],
    backup_base: u64,
    out: &mut DeviationReport,
) {
    let mut run: Option<usize> = None;
    let length = main.len().min(backup.len());
    for position in 0..=length {
        let differs = position < length
            && !matches!(position, 106 | 107 | PERCENT_IN_USE)
            && main[position] != backup[position];
        match (differs, run) {
            (true, None) => run = Some(position),
            (false, Some(start)) => {
                out.report(
                    Rule::BackupBootRegion,
                    backup_base + start as u64,
                    (position - start) as u64,
                    || {
                        format!(
                            "backup bytes {start} to {} differ from the main region",
                            position - 1
                        )
                    },
                );
                run = None;
            }
            _ => {}
        }
    }
}

/// Checks the main boot sector's PercentInUse against the active Allocation
/// Bitmap's count of allocated clusters. `offset` is the field's location.
pub fn percent_in_use_deviation(
    percent: u8,
    allocated: u64,
    cluster_count: u32,
    offset: u64,
    out: &mut DeviationReport,
) {
    if percent == 0xFF || cluster_count == 0 {
        return;
    }
    let expected = allocated * 100 / u64::from(cluster_count);
    if u64::from(percent) != expected {
        out.report(Rule::PercentInUse, offset, 1, || {
            format!("{percent}%, but {allocated} of {cluster_count} clusters are allocated ({expected}%)")
        });
    }
}

/// Checks FatEntry[0] and FatEntry[1]. `head` is the first eight bytes of the
/// FAT and `base` their volume byte offset.
pub fn fat_head_deviations(head: &[u8; 8], base: u64, out: &mut DeviationReport) {
    if head[1..4] != [0xFF; 3] {
        out.report(Rule::FatEntry0, base + 1, 3, || {
            format!(
                "FatEntry[0] is {:08X}h; its high bytes must be FFh",
                u32_at(head, 0)
            )
        });
    }
    if head[0] != MEDIA_TYPE {
        out.report(Rule::FatMediaType, base, 1, || {
            format!("media type {:02X}h, not F8h", head[0])
        });
    }
    if u32_at(head, 4) != u32::MAX {
        out.report(Rule::FatEntry1, base + 4, 4, || {
            format!("FatEntry[1] is {:08X}h, not FFFFFFFFh", u32_at(head, 4))
        });
    }
}

/// Finds the first non-zero byte of the bitmap's Reserved bits, the bits after
/// BitmapEntry[ClusterCount + 1]. `bitmap` is the start of the stream.
pub fn bitmap_reserved_byte(bitmap: &[u8], cluster_count: u32) -> Option<usize> {
    let clusters = cluster_count as usize;
    let partial = clusters / 8;
    if !clusters.is_multiple_of(8) {
        let mask = 0xFFu8 << (clusters % 8);
        if bitmap.get(partial).is_some_and(|byte| byte & mask != 0) {
            return Some(partial);
        }
    }
    let first = clusters.div_ceil(8);
    bitmap
        .get(first..)
        .and_then(nonzero)
        .map(|position| first + position)
}

/// Days in a month of a year, by the Gregorian calendar.
fn days_in(year: u32, month: u32) -> u32 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Section 7.4.8: whether a Timestamp field's parts are all in range.
pub fn timestamp_valid(value: u32) -> bool {
    let year = 1980 + (value >> 25);
    let month = (value >> 21) & 0xF;
    let day = (value >> 16) & 0x1F;
    value & 0x1F <= 29
        && (value >> 5) & 0x3F <= 59
        && (value >> 11) & 0x1F <= 23
        && (1..=12).contains(&month)
        && day >= 1
        && day <= days_in(year, month)
}

/// What the directory rules need to know about the volume.
#[derive(Clone, Copy, Debug)]
pub struct VolumeRules<'a> {
    pub cluster_count: u32,
    pub cluster_size: u64,
    pub number_of_fats: u8,
    pub revision: u16,
    /// A complete, checksummed Up-case Table; without one, name hashes and
    /// duplicate names are not checked.
    pub upcase: Option<&'a [u16]>,
}

impl VolumeRules<'_> {
    fn in_heap(&self, cluster: u32) -> bool {
        (2..u64::from(self.cluster_count) + 2).contains(&u64::from(cluster))
    }

    fn revision_one_zero(&self) -> bool {
        self.revision == 0x0100
    }
}

/// A raw cluster allocation a directory entry describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryAllocation {
    /// Volume byte offset of the entry that describes it.
    pub entry: u64,
    pub first_cluster: u32,
    pub data_length: u64,
    pub no_fat_chain: bool,
}

impl EntryAllocation {
    fn read(entry: &[u8; ENTRY_SIZE], offset: u64) -> Self {
        Self {
            entry: offset,
            first_cluster: u32_at(entry, 20),
            data_length: u64_at(entry, 24),
            no_fat_chain: entry[1] & 2 != 0,
        }
    }
}

/// The root's Allocation Bitmap and Up-case Table entries, as first found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RootStreams {
    pub bitmaps: [Option<EntryAllocation>; 2],
    /// The Up-case Table's allocation and its TableChecksum.
    pub upcase: Option<(EntryAllocation, u32)>,
}

impl RootStreams {
    /// Notes the root entry at volume byte `offset` when it is the first
    /// Allocation Bitmap of its identifier or the first Up-case Table. Breaks
    /// at the end-of-directory marker, after which nothing counts.
    pub fn observe(&mut self, offset: u64, entry: &[u8; ENTRY_SIZE]) -> ControlFlow<()> {
        match entry[0] {
            0 => return ControlFlow::Break(()),
            0x81 => {
                self.bitmaps[usize::from(entry[1] & 1)]
                    .get_or_insert(EntryAllocation::read(entry, offset));
            }
            0x82 if self.upcase.is_none() => {
                self.upcase = Some((EntryAllocation::read(entry, offset), u32_at(entry, 4)));
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

/// Names one directory may hold before duplicate names stop being checked.
const MAX_NAME_UNITS: usize = 4 * 1024 * 1024;

/// Checks a directory's entries one at a time, in order, so a caller can feed
/// it cluster by cluster. Entry sets are gathered across clusters.
pub struct DirectoryRules<'a> {
    volume: VolumeRules<'a>,
    root: bool,
    ended: bool,
    set: Vec<([u8; ENTRY_SIZE], u64)>,
    expected: usize,
    skipping_critical: bool,
    first_entry: Option<u64>,
    names: HashSet<Vec<u16>>,
    name_units: usize,
    names_skipped: bool,
    streams: RootStreams,
    labels: usize,
    guids: usize,
    subdirectories: Vec<EntryAllocation>,
}

/// What a directory's entries described, for the caller to walk next.
#[derive(Debug, Default)]
pub struct DirectoryOutcome {
    pub subdirectories: Vec<EntryAllocation>,
    pub streams: RootStreams,
    /// Duplicate names stopped being checked at the name budget.
    pub names_skipped: bool,
}

impl<'a> DirectoryRules<'a> {
    pub fn new(volume: VolumeRules<'a>, root: bool) -> Self {
        Self {
            volume,
            root,
            ended: false,
            set: Vec::new(),
            expected: 0,
            skipping_critical: false,
            first_entry: None,
            names: HashSet::new(),
            name_units: 0,
            names_skipped: false,
            streams: RootStreams::default(),
            labels: 0,
            guids: 0,
            subdirectories: Vec::new(),
        }
    }

    /// Checks the entry at volume byte `offset`.
    pub fn entry(&mut self, offset: u64, entry: &[u8; ENTRY_SIZE], out: &mut DeviationReport) {
        self.first_entry.get_or_insert(offset);
        let kind = entry[0];
        if self.ended {
            if kind != 0 {
                out.report(Rule::EndOfDirectory, offset, ENTRY_SIZE as u64, || {
                    format!("EntryType {kind:02X}h after an end-of-directory marker")
                });
            }
            return;
        }
        if !self.set.is_empty() {
            if kind == 0 {
                out.report(Rule::SetInUse, offset, 1, || {
                    "an end-of-directory marker inside an entry set".into()
                });
                self.evaluate(false, out);
                self.ended = true;
                return;
            }
            if kind & 0xC0 == 0x80 {
                out.report(Rule::SetSecondary, offset, 1, || {
                    format!(
                        "primary EntryType {kind:02X}h inside the set at byte {}",
                        self.set[0].1
                    )
                });
                self.evaluate(false, out);
            } else {
                if kind & 0x80 == 0 {
                    out.report(Rule::SetInUse, offset, 1, || {
                        format!("unused EntryType {kind:02X}h inside an entry set")
                    });
                }
                self.set.push((*entry, offset));
                if self.set.len() == self.expected {
                    self.evaluate(true, out);
                }
                return;
            }
        }
        if self.skipping_critical && kind & 0xC0 == 0xC0 {
            return;
        }
        self.skipping_critical = false;
        match kind {
            0 => self.ended = true,
            0x01..=0x7F => {}
            0x80 => out.report(Rule::EntryType80, offset, 1, || "EntryType 80h".into()),
            _ if kind & 0x40 != 0 => out.report(Rule::OrphanSecondary, offset, 1, || {
                format!("secondary EntryType {kind:02X}h outside any entry set")
            }),
            0x85 | 0xA0..=0xBF => {
                self.set.push((*entry, offset));
                self.expected = usize::from(entry[1]) + 1;
                if self.expected == 1 {
                    self.evaluate(true, out);
                }
            }
            0x81..=0x83 if self.root => self.system_entry(kind, offset, entry, out),
            _ => {
                if self.root {
                    out.report(Rule::RootCriticalPrimary, offset, 1, || {
                        format!("critical primary EntryType {kind:02X}h in the root")
                    });
                } else {
                    out.report(Rule::CriticalPrimaryOutsideRoot, offset, 1, || {
                        format!("critical primary EntryType {kind:02X}h outside the root")
                    });
                }
                self.skipping_critical = true;
            }
        }
    }

    /// Ends the directory: a set still open is truncated, and the root's
    /// required entries are counted.
    pub fn finish(mut self, out: &mut DeviationReport) -> DirectoryOutcome {
        if let Some((primary, offset)) = self.set.first().copied() {
            out.report(Rule::SetTruncated, offset + 1, 1, || {
                format!(
                    "SecondaryCount {} runs past the directory's end after {} entries",
                    primary[1],
                    self.set.len()
                )
            });
            self.evaluate(false, out);
        }
        if self.root {
            let at = self.first_entry.unwrap_or(0);
            if matches!(self.volume.number_of_fats, 1 | 2) {
                for (identifier, bitmap) in self
                    .streams
                    .bitmaps
                    .iter()
                    .take(usize::from(self.volume.number_of_fats))
                    .enumerate()
                {
                    if bitmap.is_none() {
                        out.report(Rule::BitmapEntryCount, at, ENTRY_SIZE as u64, || {
                            format!("no Allocation Bitmap entry describes bitmap {identifier}")
                        });
                    }
                }
            }
            if self.streams.upcase.is_none() {
                out.report(Rule::UpcaseEntryCount, at, ENTRY_SIZE as u64, || {
                    "no Up-case Table entry".into()
                });
            }
        }
        DirectoryOutcome {
            subdirectories: self.subdirectories,
            streams: self.streams,
            names_skipped: self.names_skipped,
        }
    }

    fn system_entry(
        &mut self,
        kind: u8,
        offset: u64,
        entry: &[u8; ENTRY_SIZE],
        out: &mut DeviationReport,
    ) {
        let reserved = |out: &mut DeviationReport, range: std::ops::Range<usize>| {
            if nonzero(&entry[range.clone()]).is_some() {
                out.report(
                    Rule::SystemEntryReserved,
                    offset + range.start as u64,
                    range.len() as u64,
                    || {
                        format!(
                            "reserved bytes {} to {} are not zero",
                            range.start,
                            range.end - 1
                        )
                    },
                );
            }
        };
        match kind {
            0x81 => {
                let flags = entry[1];
                let identifier = usize::from(flags & 1);
                if identifier == 1 && self.volume.number_of_fats != 2 {
                    out.report(Rule::BitmapIdentifier, offset + 1, 1, || {
                        format!(
                            "BitmapIdentifier 1 with NumberOfFats {}",
                            self.volume.number_of_fats
                        )
                    });
                }
                if flags & 0xFE != 0 {
                    out.report(Rule::BitmapFlagsReserved, offset + 1, 1, || {
                        format!("BitmapFlags {flags:02X}h sets reserved bits")
                    });
                }
                reserved(out, 2..20);
                let slot = &mut self.streams.bitmaps[identifier];
                if slot.is_some() {
                    out.report(Rule::BitmapEntryCount, offset, ENTRY_SIZE as u64, || {
                        format!("a second entry describes bitmap {identifier}")
                    });
                } else {
                    *slot = Some(EntryAllocation::read(entry, offset));
                }
            }
            0x82 => {
                reserved(out, 1..4);
                reserved(out, 8..20);
                if self.streams.upcase.is_some() {
                    out.report(Rule::UpcaseEntryCount, offset, ENTRY_SIZE as u64, || {
                        "a second Up-case Table entry".into()
                    });
                } else {
                    self.streams.upcase =
                        Some((EntryAllocation::read(entry, offset), u32_at(entry, 4)));
                }
            }
            _ => {
                self.labels += 1;
                if self.labels > 1 {
                    out.report(Rule::LabelCount, offset, ENTRY_SIZE as u64, || {
                        "a second Volume Label entry".into()
                    });
                }
                let count = entry[1];
                if count > 11 {
                    out.report(Rule::LabelLength, offset + 1, 1, || {
                        format!("CharacterCount {count}, more than 11")
                    });
                }
                let invalid = (0..usize::from(count).min(11))
                    .find(|index| invalid_name_character(u16_at(entry, 2 + index * 2)));
                if let Some(index) = invalid {
                    out.report(
                        Rule::LabelCharacters,
                        offset + 2 + index as u64 * 2,
                        2,
                        || format!("character {index} is {:04X}h", u16_at(entry, 2 + index * 2)),
                    );
                }
                reserved(out, 24..32);
            }
        }
    }

    fn evaluate(&mut self, framed: bool, out: &mut DeviationReport) {
        let set = std::mem::take(&mut self.set);
        let (primary, offset) = set[0];
        if framed {
            let bytes: Vec<u8> = set.iter().flat_map(|(entry, _)| *entry).collect();
            let computed = entry_set_checksum(&bytes);
            let stored = u16_at(&primary, 2);
            if computed != stored {
                out.report(Rule::SetChecksum, offset + 2, 2, || {
                    format!("SetChecksum {stored:04X}h, computed {computed:04X}h")
                });
            }
        }
        match primary[0] {
            0x85 => self.file_set(&set, framed, out),
            0xA0 => self.guid(&set, out),
            kind => {
                if kind != 0xA1 && self.volume.revision_one_zero() {
                    out.report(Rule::RevisionEntryType, offset, 1, || {
                        format!(
                            "benign primary EntryType {kind:02X}h is not defined by revision 1.00"
                        )
                    });
                }
            }
        }
    }

    fn guid(&mut self, set: &[([u8; ENTRY_SIZE], u64)], out: &mut DeviationReport) {
        let (entry, offset) = set[0];
        if !self.root {
            out.report(Rule::GuidOutsideRoot, offset, ENTRY_SIZE as u64, || {
                "a Volume GUID entry outside the root".into()
            });
            return;
        }
        self.guids += 1;
        if self.guids > 1 {
            out.report(Rule::GuidCount, offset, ENTRY_SIZE as u64, || {
                "a second Volume GUID entry".into()
            });
        }
        if entry[1] != 0 {
            out.report(Rule::GuidFields, offset + 1, 1, || {
                format!("SecondaryCount {}, not 0", entry[1])
            });
        }
        let flags = u16_at(&entry, 4);
        if flags & 3 != 0 {
            out.report(Rule::GuidFields, offset + 4, 2, || {
                format!("GeneralPrimaryFlags {flags:04X}h sets AllocationPossible or NoFatChain")
            });
        }
        if flags & !3 != 0 {
            out.report(Rule::SystemEntryReserved, offset + 4, 2, || {
                format!("GeneralPrimaryFlags {flags:04X}h sets reserved bits")
            });
        }
        if nonzero(&entry[6..22]).is_none() {
            out.report(Rule::GuidNull, offset + 6, 16, || "the null GUID".into());
        }
        if nonzero(&entry[22..32]).is_some() {
            out.report(Rule::SystemEntryReserved, offset + 22, 10, || {
                "reserved bytes 22 to 31 are not zero".into()
            });
        }
    }

    fn file_set(
        &mut self,
        set: &[([u8; ENTRY_SIZE], u64)],
        framed: bool,
        out: &mut DeviationReport,
    ) {
        let (primary, offset) = set[0];
        file_primary(&primary, offset, out);
        let directory = u16_at(&primary, 4) & ATTRIBUTE_DIRECTORY != 0;
        let members = &set[1..];
        let stream = members.first().filter(|(entry, _)| entry[0] == 0xC0);
        match members.first() {
            None if framed => out.report(Rule::FileStream, offset + 1, 1, || {
                "SecondaryCount 0 leaves no Stream Extension".into()
            }),
            Some((entry, at)) if entry[0] != 0xC0 => {
                misplaced(Rule::FileStream, entry[0], *at, "the Stream Extension", out)
            }
            _ => {}
        }
        let Some(&(stream, stream_offset)) = stream else {
            return;
        };
        self.stream(&stream, stream_offset, directory, out);
        let name_length = usize::from(stream[3]);
        let name_entries = name_length.div_ceil(15);
        let mut names = Vec::with_capacity(name_entries);
        for index in 0..name_entries {
            match members.get(1 + index) {
                None => {
                    if framed {
                        out.report(Rule::FileNames, offset + 1, 1, || {
                            format!(
                                "SecondaryCount {} leaves room for {index} of {name_entries} File Name entries",
                                primary[1]
                            )
                        });
                    }
                    break;
                }
                Some((entry, at)) if entry[0] != 0xC1 => {
                    misplaced(Rule::FileNames, entry[0], *at, "a File Name entry", out);
                }
                Some((entry, at)) => {
                    name_flags(entry, *at, out);
                    names.push((*entry, *at));
                }
            }
        }
        for (entry, at) in members.iter().skip(1 + name_entries) {
            self.trailing_secondary(entry, *at, out);
        }
        if names.len() == name_entries && name_length > 0 {
            self.name(&names, name_length, &stream, stream_offset, offset, out);
        }
        if directory && self.volume.in_heap(u32_at(&stream, 20)) {
            self.subdirectories
                .push(EntryAllocation::read(&stream, stream_offset));
        }
    }

    fn stream(
        &self,
        stream: &[u8; ENTRY_SIZE],
        offset: u64,
        directory: bool,
        out: &mut DeviationReport,
    ) {
        let flags = stream[1];
        if flags & 1 == 0 {
            out.report(Rule::StreamAllocationPossible, offset + 1, 1, || {
                format!("GeneralSecondaryFlags {flags:02X}h clears AllocationPossible")
            });
        }
        for (start, length) in [(2, 1), (6, 2), (16, 4)] {
            if nonzero(&stream[start..start + length]).is_some() {
                out.report(
                    Rule::StreamReserved,
                    offset + start as u64,
                    length as u64,
                    || {
                        format!(
                            "reserved bytes {start} to {} are not zero",
                            start + length - 1
                        )
                    },
                );
            }
        }
        if flags & 0xFC != 0 {
            out.report(Rule::StreamReserved, offset + 1, 1, || {
                format!("GeneralSecondaryFlags {flags:02X}h sets reserved bits")
            });
        }
        if stream[3] == 0 {
            out.report(Rule::NameLength, offset + 3, 1, || "NameLength 0".into());
        }
        allocation_deviations(&self.volume, stream, offset, out);
        let valid = u64_at(stream, 8);
        let length = u64_at(stream, 24);
        if valid > length || directory && valid != length {
            out.report(Rule::ValidDataLength, offset + 8, 8, || {
                format!("ValidDataLength {valid} with DataLength {length}")
            });
        }
        if directory
            && (!length.is_multiple_of(self.volume.cluster_size) || length > MAX_DIRECTORY_BYTES)
        {
            out.report(Rule::DirectoryLength, offset + 24, 8, || {
                format!("a directory's DataLength {length} is not whole clusters of at most 256 MB")
            });
        }
    }

    fn trailing_secondary(&self, entry: &[u8; ENTRY_SIZE], offset: u64, out: &mut DeviationReport) {
        match entry[0] {
            0xC0 => out.report(Rule::FileStream, offset, 1, || {
                "a second Stream Extension".into()
            }),
            0xC1 => out.report(Rule::FileNames, offset, 1, || {
                "a File Name entry past NameLength".into()
            }),
            0xE0 | 0xE1 => vendor_deviations(&self.volume, entry, offset, out),
            kind if kind & 0x80 != 0 && self.volume.revision_one_zero() => {
                out.report(Rule::RevisionEntryType, offset, 1, || {
                    format!("secondary EntryType {kind:02X}h is not defined by revision 1.00")
                });
            }
            _ => {}
        }
    }

    fn name(
        &mut self,
        names: &[([u8; ENTRY_SIZE], u64)],
        length: usize,
        stream: &[u8; ENTRY_SIZE],
        stream_offset: u64,
        primary_offset: u64,
        out: &mut DeviationReport,
    ) {
        let units: Vec<(u16, u64)> = names
            .iter()
            .flat_map(|(entry, at)| {
                (0..15).map(move |index| (u16_at(entry, 2 + index * 2), at + 2 + index as u64 * 2))
            })
            .collect();
        let (name, unused) = units.split_at(length);
        if let Some((unit, at)) = name.iter().find(|(unit, _)| invalid_name_character(*unit)) {
            out.report(Rule::NameCharacters, *at, 2, || {
                format!("character {unit:04X}h")
            });
        }
        let text: Vec<u16> = name.iter().map(|(unit, _)| *unit).collect();
        if text == [0x2E] || text == [0x2E, 0x2E] {
            out.report(Rule::NameReserved, name[0].1, length as u64 * 2, || {
                "the reserved name \".\" or \"..\"".into()
            });
        }
        if let Some((unit, at)) = unused.iter().find(|(unit, _)| *unit != 0) {
            out.report(Rule::NameUnusedZero, *at, 2, || {
                format!("{unit:04X}h past NameLength {length}")
            });
        }
        let Some(upcase) = self.volume.upcase else {
            return;
        };
        let upcased = |unit: u16| upcase[usize::from(unit)];
        let computed = name_hash(&text, upcased);
        let stored = u16_at(stream, 4);
        if computed != stored {
            out.report(Rule::NameHash, stream_offset + 4, 2, || {
                format!("NameHash {stored:04X}h, computed {computed:04X}h")
            });
        }
        if self.names_skipped {
            return;
        }
        if self.name_units + text.len() > MAX_NAME_UNITS {
            self.names_skipped = true;
            return;
        }
        self.name_units += text.len();
        if !self
            .names
            .insert(text.iter().map(|unit| upcased(*unit)).collect())
        {
            out.report(
                Rule::NameDuplicate,
                primary_offset,
                ENTRY_SIZE as u64,
                || {
                    format!(
                        "{:?} repeats an up-cased name",
                        String::from_utf16_lossy(&text)
                    )
                },
            );
        }
    }
}

fn misplaced(rule: Rule, kind: u8, offset: u64, wanted: &str, out: &mut DeviationReport) {
    out.report(rule, offset, 1, || {
        format!("EntryType {kind:02X}h where {wanted} belongs")
    });
    if matches!(kind, 0xE0 | 0xE1) {
        out.report(Rule::VendorOrder, offset, 1, || {
            format!(
                "vendor EntryType {kind:02X}h before the Stream Extension and File Name entries"
            )
        });
    }
}

fn name_flags(entry: &[u8; ENTRY_SIZE], offset: u64, out: &mut DeviationReport) {
    let flags = entry[1];
    if flags & 3 != 0 {
        out.report(Rule::NameEntryFlags, offset + 1, 1, || {
            format!("GeneralSecondaryFlags {flags:02X}h sets AllocationPossible or NoFatChain")
        });
    }
    if flags & 0xFC != 0 {
        out.report(Rule::NameFlagsReserved, offset + 1, 1, || {
            format!("GeneralSecondaryFlags {flags:02X}h sets reserved bits")
        });
    }
}

fn file_primary(primary: &[u8; ENTRY_SIZE], offset: u64, out: &mut DeviationReport) {
    let attributes = u16_at(primary, 4);
    if attributes & !ATTRIBUTES_DEFINED != 0 {
        out.report(Rule::FileAttributesReserved, offset + 4, 2, || {
            format!("FileAttributes {attributes:04X}h sets reserved bits")
        });
    }
    for (start, length) in [(6, 2), (25, 7)] {
        if nonzero(&primary[start..start + length]).is_some() {
            out.report(
                Rule::FileReserved,
                offset + start as u64,
                length as u64,
                || {
                    format!(
                        "reserved bytes {start} to {} are not zero",
                        start + length - 1
                    )
                },
            );
        }
    }
    for (field, increment, utc, name) in [
        (8, Some(20), 22, "Create"),
        (12, Some(21), 23, "LastModified"),
        (16, None, 24, "LastAccessed"),
    ] {
        let value = u32_at(primary, field);
        if !timestamp_valid(value) {
            out.report(Rule::Timestamp, offset + field as u64, 4, || {
                format!("{name}Timestamp {value:08X}h is not a valid date and time")
            });
        }
        if let Some(increment) = increment {
            let value = primary[increment];
            if value > 199 {
                out.report(Rule::TenMsIncrement, offset + increment as u64, 1, || {
                    format!("{name}10msIncrement {value}, more than 199")
                });
            }
        }
        let value = primary[utc];
        if value & 0x80 == 0 && value != 0 {
            out.report(Rule::UtcOffset, offset + utc as u64, 1, || {
                format!("{name}UtcOffset {value:02X}h has an offset but OffsetValid 0")
            });
        }
    }
}

/// Sections 6.2.2, 6.2.3 and 6.3.4.2 on an entry whose AllocationPossible is
/// set: a Stream Extension or a Vendor Allocation.
fn allocation_deviations(
    volume: &VolumeRules<'_>,
    entry: &[u8; ENTRY_SIZE],
    offset: u64,
    out: &mut DeviationReport,
) {
    let allocation = EntryAllocation::read(entry, offset);
    let first = allocation.first_cluster;
    let length = allocation.data_length;
    if first != 0 && !volume.in_heap(first) {
        out.report(Rule::FirstCluster, offset + 20, 4, || {
            format!(
                "FirstCluster {first}, not 0 or 2 to {}",
                u64::from(volume.cluster_count) + 1
            )
        });
    }
    let capacity = u64::from(volume.cluster_count) * volume.cluster_size;
    if first == 0 && length != 0 || length > capacity {
        out.report(Rule::DataLength, offset + 24, 8, || {
            format!("DataLength {length} with FirstCluster {first} and a {capacity}-byte heap")
        });
    }
    if allocation.no_fat_chain && (entry[1] & 1 == 0 || first == 0 || length == 0) {
        out.report(Rule::NoFatChain, offset + 1, 1, || {
            format!("NoFatChain with FirstCluster {first} and DataLength {length}")
        });
    }
    if allocation.no_fat_chain
        && volume.in_heap(first)
        && u64::from(first) + length.div_ceil(volume.cluster_size)
            > u64::from(volume.cluster_count) + 2
    {
        out.report(Rule::ContiguousRange, offset + 24, 8, || {
            format!("{length} contiguous bytes from cluster {first} pass the heap's end")
        });
    }
}

fn vendor_deviations(
    volume: &VolumeRules<'_>,
    entry: &[u8; ENTRY_SIZE],
    offset: u64,
    out: &mut DeviationReport,
) {
    let allocation = entry[0] == 0xE1;
    if (entry[1] & 1 != 0) != allocation {
        out.report(Rule::VendorFlags, offset + 1, 1, || {
            format!(
                "EntryType {:02X}h with GeneralSecondaryFlags {:02X}h",
                entry[0], entry[1]
            )
        });
    }
    if nonzero(&entry[2..18]).is_none() {
        out.report(Rule::VendorGuid, offset + 2, 16, || {
            "the null VendorGuid".into()
        });
    }
    if allocation {
        allocation_deviations(volume, entry, offset, out);
    }
}

/// Sections 6.2.2 and 6.2.3 on a root system entry's allocation. Its
/// GeneralPrimaryFlags are redefined, so NoFatChain does not apply.
pub fn system_allocation_deviations(
    volume: &VolumeRules<'_>,
    allocation: &EntryAllocation,
    out: &mut DeviationReport,
) {
    let mut entry = [0u8; ENTRY_SIZE];
    entry[1] = 1;
    entry[20..24].copy_from_slice(&allocation.first_cluster.to_le_bytes());
    entry[24..32].copy_from_slice(&allocation.data_length.to_le_bytes());
    allocation_deviations(volume, &entry, allocation.entry, out);
}
