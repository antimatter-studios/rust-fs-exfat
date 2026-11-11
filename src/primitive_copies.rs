//! The two FAT and Allocation Bitmap pairs of a TexFAT volume.
//!
//! Microsoft's exFAT specification, section 2 "Volume Structure" (Table 3),
//! places the First FAT at FatOffset and the Second FAT at FatOffset +
//! FatLength, a sub-region FatLength × (NumberOfFats − 1) sectors long;
//! section 3.1.7 "FatLength Field" makes FatLength the length of each FAT.
//! Section 3.1.16 "NumberOfFats Field" gives a volume a Second FAT and a
//! Second Allocation Bitmap only when it holds 2, a value "only valid for
//! TexFAT volumes", and section 1.5 "Windows CE and TexFAT" says TexFAT uses
//! both pairs for its transactions. Section 7.1.2.1 "BitmapIdentifier Field"
//! pairs the First Allocation Bitmap with the First FAT and the Second with
//! the Second.
//!
//! Section 3.1.13.1 "ActiveFat Field" names the pair implementations shall
//! use and says they "shall consider the inactive FAT and Allocation Bitmap
//! as stale". Section 4 "File Allocation Table Region" adds that only the
//! Main Boot Sector's VolumeFlags is current and that "use of the inactive FAT
//! and switching between FATs is implementation specific". No rule requires
//! the two pairs to agree, so a difference between them is evidence of what
//! one of them held, not a fault: these helpers locate and compare the copies
//! without judging either, and the strict reader uses the active pair alone.

use super::{u32_at, BootSector, FIRST_CLUSTER};
use serde::{Deserialize, Serialize};
use std::ops::ControlFlow;

/// One FAT and the Allocation Bitmap used with it (sections 3.1.13.1 and
/// 7.1.2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FatCopy {
    /// The First FAT and First Allocation Bitmap: ActiveFat 0 and
    /// BitmapIdentifier 0.
    First,
    /// The Second FAT and Second Allocation Bitmap: ActiveFat 1 and
    /// BitmapIdentifier 1.
    Second,
}

impl FatCopy {
    /// The pair an ActiveFat or BitmapIdentifier value names, 0 or 1.
    pub fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::First),
            1 => Some(Self::Second),
            _ => None,
        }
    }

    /// The ActiveFat and BitmapIdentifier value of this pair.
    pub fn index(self) -> usize {
        match self {
            Self::First => 0,
            Self::Second => 1,
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::First => Self::Second,
            Self::Second => Self::First,
        }
    }
}

/// Where the FATs lie and which pair is active, as one boot sector's fields
/// say. Nothing is validated: the offsets may lie past the volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FatLayout {
    /// The First FAT's byte offset: FatOffset sectors (section 3.1.6).
    pub first: u64,
    /// Each FAT's length in bytes: FatLength sectors (section 3.1.7).
    pub length: u64,
    /// NumberOfFats is 2, so the Second FAT follows the First (Table 3). An
    /// invalid larger value is read the same way; 0 and 1 describe no
    /// Second FAT.
    pub second: bool,
    /// The pair ActiveFat names. ActiveFat 1 on a volume without a Second
    /// FAT, which section 3.1.13.1 rules out, leaves the First active.
    pub active: FatCopy,
}

impl FatLayout {
    /// The layout `boot` describes. Only the Main Boot Sector's VolumeFlags
    /// is current (section 4); a backup's ActiveFat may be stale.
    pub fn from_boot(boot: &BootSector) -> Self {
        let sector = boot.bytes_per_sector();
        let second = boot.number_of_fats >= 2;
        Self {
            first: u64::from(boot.fat_offset) * sector,
            length: u64::from(boot.fat_length) * sector,
            second,
            active: if second && boot.volume_flags & 1 != 0 {
                FatCopy::Second
            } else {
                FatCopy::First
            },
        }
    }

    /// The byte offset of `copy`'s FAT, or `None` for the Second FAT of a
    /// volume that describes none.
    pub fn offset(&self, copy: FatCopy) -> Option<u64> {
        match copy {
            FatCopy::First => Some(self.first),
            FatCopy::Second => self.second.then(|| self.first + self.length),
        }
    }

    /// The pairs the volume describes, First first.
    pub fn copies(&self) -> impl Iterator<Item = FatCopy> {
        [FatCopy::First, FatCopy::Second]
            .into_iter()
            .take(1 + usize::from(self.second))
    }
}

/// How much of two copies a comparison covered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Compared {
    /// Heap clusters whose records both ranges held.
    pub clusters: u64,
    /// Those whose records differ, each passed to the caller.
    pub differences: u64,
    /// The caller stopped the comparison; the counts cover what came before.
    pub stopped: bool,
}

/// Compares the heap clusters' entries in two FAT ranges that start at the
/// same entry, `first_entry`, of the First and the Second FAT, and calls
/// `differ` with each cluster whose entries differ and its raw entry in each.
///
/// FatEntry\[0\] and FatEntry\[1\] describe no cluster (sections 4.1.1 and
/// 4.1.2), and bytes past FatEntry\[ClusterCount+1\] are undefined (section
/// 3.1.7), so neither is compared; nor is an entry that either range holds
/// only in part.
pub fn fat_differences(
    first: &[u8],
    second: &[u8],
    first_entry: u32,
    cluster_count: u32,
    mut differ: impl FnMut(u32, u32, u32) -> ControlFlow<()>,
) -> Compared {
    let held = (first.len().min(second.len()) / 4) as u64;
    let start = u64::from(first_entry);
    let heap_end = u64::from(cluster_count) + u64::from(FIRST_CLUSTER);
    let from = start.max(u64::from(FIRST_CLUSTER));
    let to = (start + held).min(heap_end);
    let mut compared = Compared::default();
    if from >= to {
        return compared;
    }
    let bytes = ((from - start) * 4) as usize..((to - start) * 4) as usize;
    if first[bytes.clone()] == second[bytes] {
        compared.clusters = to - from;
        return compared;
    }
    for entry in from..to {
        compared.clusters += 1;
        let at = ((entry - start) * 4) as usize;
        let (old, new) = (u32_at(first, at), u32_at(second, at));
        if old != new {
            compared.differences += 1;
            if differ(entry as u32, old, new).is_break() {
                compared.stopped = true;
                break;
            }
        }
    }
    compared
}

/// Compares the heap clusters' bits in two Allocation Bitmap ranges that
/// start at the same byte, `first_byte`, of the First and the Second bitmap,
/// and calls `differ` with each cluster whose bits differ and its bit in
/// each, set meaning not available for allocation (section 7.1.5.1).
///
/// The bits past BitmapEntry\[ClusterCount+1\] are Reserved (section 7.1.5,
/// Table 22) and are not compared.
pub fn bitmap_differences(
    first: &[u8],
    second: &[u8],
    first_byte: u64,
    cluster_count: u32,
    mut differ: impl FnMut(u32, bool, bool) -> ControlFlow<()>,
) -> Compared {
    let mut compared = Compared::default();
    let heap_bits = u64::from(cluster_count);
    for (index, (&old, &new)) in first.iter().zip(second).enumerate() {
        let base = (first_byte + index as u64) * 8;
        if base >= heap_bits {
            break;
        }
        // A final partial byte holds fewer heap bits.
        let bits = (heap_bits - base).min(8);
        if old == new {
            compared.clusters += bits;
            continue;
        }
        for bit in 0..bits {
            compared.clusters += 1;
            let (was, is) = (old >> bit & 1 == 1, new >> bit & 1 == 1);
            if was != is {
                compared.differences += 1;
                let cluster = (base + bit + u64::from(FIRST_CLUSTER)) as u32;
                if differ(cluster, was, is).is_break() {
                    compared.stopped = true;
                    return compared;
                }
            }
        }
    }
    compared
}
