//! Directory entries this driver does not interpret.
//!
//! Microsoft's exFAT specification, section 6.2.1 "EntryType Field": 00h is
//! the end-of-directory marker, and every later entry of the directory is one
//! too; 01h to 7Fh are unused entries, whose InUse bit (section 6.2.1.4) is
//! clear; 81h to FFh are regular entries, the only values valid inside a
//! directory entry set; 80h is invalid. TypeCode (bits 0 to 4), TypeImportance
//! (bit 5, section 6.2.1.2: critical or benign) and TypeCategory (bit 6,
//! section 6.2.1.3: primary or secondary) together name the type.
//!
//! Section 7 "Directory Entry Definitions" defines, for revision 1.00, the
//! critical primaries Allocation Bitmap 81h (7.1), Up-case Table 82h (7.2),
//! Volume Label 83h (7.3) and File 85h (7.4); the benign primaries Volume GUID
//! A0h (7.5) and TexFAT Padding A1h (7.10); the critical secondaries Stream
//! Extension C0h (7.6) and File Name C1h (7.7); and the benign secondaries
//! Vendor Extension E0h (7.8) and Vendor Allocation E1h (7.9). This driver
//! interprets all of them except the two vendor entries and TexFAT Padding,
//! which section 7.10 says to treat as an unrecognized benign primary.
//!
//! The skip rules: sections 6.3.1.2.2 "Benign Primary Directory Entries" and
//! 6.4.1.2.2 "Benign Secondary Directory Entries" make benign entries
//! optional. An unrecognized benign primary renders its whole set
//! unrecognized; unrecognized benign secondaries do not, and "implementations
//! may ignore" them. Sections 7.8 and 7.9 treat a vendor entry whose
//! VendorGuid is not recognized as any other unrecognized benign secondary.
//! Section 8.2 "Implications of Unrecognized Directory Entries": an
//! unrecognized critical primary in the root renders the volume invalid, and
//! an unrecognized critical secondary renders its set unrecognized. Section
//! 6.3.2 "SecondaryCount Field" lets critical primaries redefine SecondaryCount
//! and SetChecksum, so only File and benign primaries frame a set here.
//!
//! Entries are classified by type alone, InUse aside. A secondary of a known
//! type inside a set that an unknown primary heads is not listed again: the
//! primary's own record stands for that set.

use super::{set_checksum, u16_at, RawEntrySet, ENTRY_SIZE, TYPE_FILE, TYPE_NAME, TYPE_STREAM};
use serde::{Deserialize, Serialize};
use std::ops::ControlFlow;

const IN_USE: u8 = 0x80;
const SECONDARY: u8 = 0x40;
const BENIGN: u8 = 0x20;
const END_OF_DIRECTORY: u8 = 0x00;
const VENDOR_EXTENSION: u8 = 0xE0;
const VENDOR_ALLOCATION: u8 = 0xE1;

/// What kind of entry the driver does not interpret, by its EntryType with
/// InUse restored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UninterpretedKind {
    /// E0h, section 7.8: a benign secondary of File sets, named by a
    /// VendorGuid, that may be ignored.
    VendorExtension,
    /// E1h, section 7.9: a vendor entry with a cluster allocation of its own.
    VendorAllocation,
    /// A1h to BFh: a benign primary other than Volume GUID, TexFAT Padding
    /// (A1h, section 7.10) included. Its whole set is unrecognized.
    UnknownBenignPrimary,
    /// 80h, 84h and 86h to 9Fh: a critical primary that revision 1.00 does
    /// not define. One in the root renders the volume invalid (section 8.2).
    UnknownCriticalPrimary,
    /// C2h to DFh: renders its set unrecognized (section 8.2).
    UnknownCriticalSecondary,
    /// E2h to FFh: may be ignored (section 6.4.1.2.2).
    UnknownBenignSecondary,
}

impl UninterpretedKind {
    /// The kind of a raw EntryType, or `None` for the end-of-directory marker
    /// and the types this driver interprets. An unused entry keeps the kind
    /// of the type it held, so a deleted vendor entry is still listed.
    pub fn of(entry_type: u8) -> Option<Self> {
        if entry_type == END_OF_DIRECTORY {
            return None;
        }
        Some(match entry_type | IN_USE {
            0x81..=0x83 | TYPE_FILE | 0xA0 | TYPE_STREAM | TYPE_NAME => return None,
            VENDOR_EXTENSION => Self::VendorExtension,
            VENDOR_ALLOCATION => Self::VendorAllocation,
            value => match (value & SECONDARY != 0, value & BENIGN != 0) {
                (false, true) => Self::UnknownBenignPrimary,
                (false, false) => Self::UnknownCriticalPrimary,
                (true, false) => Self::UnknownCriticalSecondary,
                (true, true) => Self::UnknownBenignSecondary,
            },
        })
    }

    pub fn is_primary(self) -> bool {
        matches!(
            self,
            Self::UnknownBenignPrimary | Self::UnknownCriticalPrimary
        )
    }
}

/// Uninterpreted entries counted by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UninterpretedCounts {
    pub vendor_extension: u64,
    pub vendor_allocation: u64,
    pub unknown_benign_primary: u64,
    pub unknown_critical_primary: u64,
    pub unknown_critical_secondary: u64,
    pub unknown_benign_secondary: u64,
}

impl UninterpretedCounts {
    pub fn add(&mut self, kind: UninterpretedKind) {
        *self.slot(kind) += 1;
    }

    pub fn get(&self, kind: UninterpretedKind) -> u64 {
        match kind {
            UninterpretedKind::VendorExtension => self.vendor_extension,
            UninterpretedKind::VendorAllocation => self.vendor_allocation,
            UninterpretedKind::UnknownBenignPrimary => self.unknown_benign_primary,
            UninterpretedKind::UnknownCriticalPrimary => self.unknown_critical_primary,
            UninterpretedKind::UnknownCriticalSecondary => self.unknown_critical_secondary,
            UninterpretedKind::UnknownBenignSecondary => self.unknown_benign_secondary,
        }
    }

    pub fn total(&self) -> u64 {
        self.vendor_extension
            + self.vendor_allocation
            + self.unknown_benign_primary
            + self.unknown_critical_primary
            + self.unknown_critical_secondary
            + self.unknown_benign_secondary
    }

    /// Counts the uninterpreted entries among whole 32-byte entries, such as
    /// one framed record. End-of-directory markers are not counted; a
    /// trailing partial entry is ignored.
    pub fn tally(&mut self, entries: &[u8]) {
        for entry in entries.chunks_exact(ENTRY_SIZE) {
            if let Some(kind) = UninterpretedKind::of(entry[0]) {
                self.add(kind);
            }
        }
    }

    fn slot(&mut self, kind: UninterpretedKind) -> &mut u64 {
        match kind {
            UninterpretedKind::VendorExtension => &mut self.vendor_extension,
            UninterpretedKind::VendorAllocation => &mut self.vendor_allocation,
            UninterpretedKind::UnknownBenignPrimary => &mut self.unknown_benign_primary,
            UninterpretedKind::UnknownCriticalPrimary => &mut self.unknown_critical_primary,
            UninterpretedKind::UnknownCriticalSecondary => &mut self.unknown_critical_secondary,
            UninterpretedKind::UnknownBenignSecondary => &mut self.unknown_benign_secondary,
        }
    }
}

/// The entry set an entry lies in: the nearest File or benign primary before
/// it, or the entry itself, whose SecondaryCount reaches it with no other
/// primary between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetMembership {
    /// Offset of the set's primary entry within the walked bytes.
    pub primary: usize,
    /// The primary's raw EntryType.
    pub primary_type: u8,
    /// The set is valid as it stands: all of its 1 + SecondaryCount entries
    /// are present; the primary and every secondary are regular, InUse set,
    /// and every entry after the primary is a secondary (sections 6.2.1 and
    /// 6.3); SetChecksum matches the stored bytes (section 6.3.3); and a File
    /// set opens with its Stream Extension and File Name entries (sections
    /// 7.4, 7.6 and 7.7), so no vendor entry precedes them (sections 7.8 and
    /// 7.9).
    pub valid: bool,
    /// SetChecksum matches with InUse restored on every entry, as it does for
    /// a set deleted by clearing InUse alone. False when entries are missing.
    pub checksum_ok: bool,
}

/// One entry the driver does not interpret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UninterpretedRecord {
    /// Offset of the entry within the walked bytes.
    pub offset: usize,
    /// The raw EntryType, InUse included.
    pub entry_type: u8,
    pub kind: UninterpretedKind,
    /// The InUse bit: clear on an unused entry.
    pub in_use: bool,
    /// `None` when no File or benign primary's set reaches the entry, as for
    /// an orphan secondary or an unknown critical primary.
    pub set: Option<SetMembership>,
}

/// How a walk of directory bytes ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirectoryWalk {
    /// Offset of the end-of-directory marker, when one was reached. Nothing
    /// after it was walked.
    pub end_marker: Option<usize>,
    /// The caller stopped the walk.
    pub stopped: bool,
}

/// Passes every entry of `directory` that the driver does not interpret to
/// `emit`, in order, up to the end-of-directory marker. Every entry is
/// visited: unused primaries and broken sets hide nothing. A trailing
/// partial entry is ignored.
pub fn uninterpreted_entries(
    directory: &[u8],
    mut emit: impl FnMut(UninterpretedRecord) -> ControlFlow<()>,
) -> DirectoryWalk {
    let mut walk = DirectoryWalk::default();
    // The set being walked and the offset just past its declared end.
    let mut set: Option<(SetMembership, usize)> = None;
    for (index, entry) in directory.chunks_exact(ENTRY_SIZE).enumerate() {
        let offset = index * ENTRY_SIZE;
        let value = entry[0];
        if value == END_OF_DIRECTORY {
            walk.end_marker = Some(offset);
            break;
        }
        if value & SECONDARY == 0 {
            set = frame(directory, offset);
        } else if set.is_some_and(|(_, end)| offset >= end) {
            set = None;
        }
        let Some(kind) = UninterpretedKind::of(value) else {
            continue;
        };
        let record = UninterpretedRecord {
            offset,
            entry_type: value,
            kind,
            in_use: value & IN_USE != 0,
            set: set.map(|(membership, _)| membership),
        };
        if emit(record).is_break() {
            walk.stopped = true;
            break;
        }
    }
    walk
}

/// The set a primary at `offset` heads, when its type has a SecondaryCount.
fn frame(directory: &[u8], offset: usize) -> Option<(SetMembership, usize)> {
    let primary = &directory[offset..offset + ENTRY_SIZE];
    let restored = primary[0] | IN_USE;
    if restored != TYPE_FILE && restored & BENIGN == 0 {
        return None;
    }
    let end = offset + (usize::from(primary[1]) + 1) * ENTRY_SIZE;
    let bytes = directory.get(offset..end);
    let membership = SetMembership {
        primary: offset,
        primary_type: primary[0],
        valid: bytes.is_some_and(valid_set),
        checksum_ok: bytes.is_some_and(|bytes| set_checksum(bytes) == u16_at(bytes, 2)),
    };
    Some((membership, end))
}

fn valid_set(bytes: &[u8]) -> bool {
    let Some(set) = RawEntrySet::parse(bytes) else {
        return false;
    };
    let regular = set.primary.entry_type.in_use
        && set
            .secondaries
            .iter()
            .all(|entry| entry.in_use && entry.secondary);
    let file_order = set.primary.entry_type.value != TYPE_FILE || opens_with_names(&set);
    regular && set.checksum_ok && file_order
}

/// A Stream Extension right after the primary, then every File Name entry
/// its NameLength needs, at least one.
fn opens_with_names(set: &RawEntrySet<'_>) -> bool {
    let stream = set
        .stream
        .as_ref()
        .is_some_and(|stream| stream.entry_type.value == TYPE_STREAM && stream.name_length > 0);
    stream
        && set.name_complete
        && set
            .names
            .iter()
            .all(|name| name.entry_type.value == TYPE_NAME)
}
