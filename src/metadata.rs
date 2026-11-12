use crate::primitives::{
    self, FatEntryReport, FatValue, RawAllocationBitmap, RawRootMetadata, RawUpcaseTable,
    RootEntryKind, UpcaseIssue,
};
use crate::{Error, Result};

pub use crate::primitives::SystemStream;

pub const END_OF_CHAIN: u32 = 0xffff_ffff;

pub fn checksum32(data: &[u8]) -> u32 {
    primitives::checksum32(data)
}

pub fn set_checksum(data: &[u8]) -> u16 {
    primitives::entry_set_checksum(data)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FatEntry {
    Free,
    Next(u32),
    Bad,
    End,
}

impl FatEntry {
    pub fn parse(value: u32, cluster_count: u32) -> Result<Self> {
        Self::from_report(primitives::decode_fat_entry(value, cluster_count))
    }

    pub(crate) fn from_report(entry: FatEntryReport) -> Result<Self> {
        match entry.kind {
            FatValue::Free => Ok(Self::Free),
            FatValue::Bad => Ok(Self::Bad),
            FatValue::End => Ok(Self::End),
            FatValue::Next(next) => Ok(Self::Next(next)),
            FatValue::Reserved => Err(Error::Invalid("reserved or out-of-range FAT entry")),
        }
    }
}

#[derive(Debug)]
pub struct AllocationBitmap {
    bytes: Vec<u8>,
    cluster_count: u32,
}

impl AllocationBitmap {
    pub fn parse(bytes: Vec<u8>, cluster_count: u32) -> Result<Self> {
        if !RawAllocationBitmap::parse(&bytes, cluster_count).length_ok {
            return Err(Error::Invalid("allocation bitmap length"));
        }
        Ok(Self {
            bytes,
            cluster_count,
        })
    }

    pub fn allocated(&self, cluster: u32) -> Result<bool> {
        RawAllocationBitmap::parse(&self.bytes, self.cluster_count)
            .allocated(cluster)
            .ok_or(Error::Invalid("bitmap cluster outside heap"))
    }

    /// The number of clusters the bitmap records as allocated.
    pub fn allocated_count(&self) -> u64 {
        primitives::bitmap_count(&self.bytes, self.cluster_count)
    }

    /// Records a cluster as allocated or free and returns the bitmap byte
    /// that holds it, with its index, for the caller to write.
    pub(crate) fn set(&mut self, cluster: u32, allocated: bool) -> Result<(usize, u8)> {
        if !(2..u64::from(self.cluster_count) + 2).contains(&u64::from(cluster)) {
            return Err(Error::Invalid("bitmap cluster outside heap"));
        }
        let index = (cluster - 2) as usize;
        let byte = &mut self.bytes[index / 8];
        if allocated {
            *byte |= 1 << (index % 8);
        } else {
            *byte &= !(1 << (index % 8));
        }
        Ok((index / 8, *byte))
    }
}

#[derive(Debug)]
pub struct UpcaseTable {
    mappings: Vec<u16>,
}

impl UpcaseTable {
    pub fn parse(data: &[u8], expected_checksum: u32) -> Result<Self> {
        let raw = RawUpcaseTable::parse(data, expected_checksum);
        if !raw.length_ok || !raw.checksum_ok {
            return Err(Error::Invalid("upcase length or checksum"));
        }
        if let Some(issue) = raw.issue {
            return Err(Error::Invalid(match issue {
                UpcaseIssue::TruncatedRun => "truncated upcase run",
                UpcaseIssue::RunOutsideRange => "upcase run outside Unicode range",
                UpcaseIssue::ExtraMappings => "extra upcase mappings",
            }));
        }
        if !raw.complete {
            return Err(Error::Invalid("incomplete upcase table"));
        }
        if !raw.ascii_ok {
            return Err(Error::Invalid("mandatory ASCII upcase mapping"));
        }
        Ok(Self {
            mappings: raw.mappings,
        })
    }

    pub fn map(&self, unit: u16) -> u16 {
        self.mappings[usize::from(unit)]
    }

    pub fn name_hash(&self, name: &[u16]) -> u16 {
        primitives::name_hash(name, |unit| self.map(unit))
    }

    pub fn equal(&self, first: &[u16], second: &[u16]) -> bool {
        first.len() == second.len()
            && first
                .iter()
                .zip(second)
                .all(|(first, second)| self.map(*first) == self.map(*second))
    }
}

#[derive(Debug)]
pub struct RootMetadata {
    pub bitmaps: [Option<SystemStream>; 2],
    pub upcase: SystemStream,
    pub upcase_checksum: u32,
    pub label: Option<String>,
    pub guid: Option<[u8; 16]>,
}

impl RootMetadata {
    pub fn parse(data: &[u8], number_of_fats: u8) -> Result<Self> {
        let raw = RawRootMetadata::parse(data);
        if !raw.aligned || !matches!(number_of_fats, 1 | 2) {
            return Err(Error::Invalid("root directory size or FAT count"));
        }
        let mut bitmaps = [None, None];
        let mut upcase = None;
        let mut label = None;
        let mut guid = None;
        for entry in raw.entries {
            if !entry.in_use {
                continue;
            }
            match entry.kind {
                RootEntryKind::Bitmap {
                    id,
                    flags_ok,
                    stream,
                    ..
                } => {
                    if !flags_ok || id >= usize::from(number_of_fats) || bitmaps[id].is_some() {
                        return Err(Error::Invalid("allocation bitmap identity or duplicate"));
                    }
                    bitmaps[id] = Some(stream);
                }
                RootEntryKind::Upcase { stream, checksum } => {
                    if upcase.is_some() {
                        return Err(Error::Invalid("duplicate upcase entry"));
                    }
                    upcase = Some((stream, checksum));
                }
                RootEntryKind::Label {
                    value,
                    length_ok,
                    characters_ok,
                    utf16_ok,
                    ..
                } => {
                    if !length_ok || label.is_some() {
                        return Err(Error::Invalid("volume label length or duplicate"));
                    }
                    if !characters_ok {
                        return Err(Error::Invalid("volume label character"));
                    }
                    if !utf16_ok {
                        return Err(Error::Invalid("volume label UTF-16"));
                    }
                    label = Some(value);
                }
                RootEntryKind::Guid {
                    secondary_count,
                    flags,
                    value,
                    checksum_ok,
                    ..
                } => {
                    if guid.is_some()
                        || secondary_count != 0
                        || flags & 3 != 0
                        || value == [0; 16]
                        || !checksum_ok
                    {
                        return Err(Error::Invalid("volume GUID entry"));
                    }
                    guid = Some(value);
                }
                RootEntryKind::File {
                    secondary_count,
                    complete,
                } => {
                    if secondary_count < 2 {
                        return Err(Error::Invalid("file secondary count"));
                    }
                    if !complete {
                        return Err(Error::Invalid("truncated root entry set"));
                    }
                }
                RootEntryKind::Benign { complete, .. } => {
                    if !complete {
                        return Err(Error::Invalid("truncated benign entry set"));
                    }
                }
                RootEntryKind::Unknown => {
                    return Err(Error::Unsupported("critical root directory entry"))
                }
            }
        }
        if bitmaps
            .iter()
            .take(usize::from(number_of_fats))
            .any(Option::is_none)
        {
            return Err(Error::Invalid("missing allocation bitmap"));
        }
        let (upcase, upcase_checksum) = upcase.ok_or(Error::Invalid("missing upcase table"))?;
        Ok(Self {
            bitmaps,
            upcase,
            upcase_checksum,
            label,
            guid,
        })
    }
}
