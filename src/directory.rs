use crate::boot::BootSector;
use crate::metadata::UpcaseTable;
use crate::primitives::{EntryType, RawEntrySet, ENTRY_SIZE};
use crate::timestamp::Timestamps;
use crate::{Error, Result};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stream {
    pub(crate) first_cluster: u32,
    pub(crate) data_length: u64,
    pub(crate) valid_data_length: u64,
    pub(crate) no_fat_chain: bool,
}

impl Stream {
    pub fn first_cluster(&self) -> u32 {
        self.first_cluster
    }

    pub fn data_length(&self) -> u64 {
        self.data_length
    }

    pub fn valid_data_length(&self) -> u64 {
        self.valid_data_length
    }

    pub fn no_fat_chain(&self) -> bool {
        self.no_fat_chain
    }
}

#[derive(Clone, Debug)]
pub struct DirectoryEntry {
    name: String,
    name_utf16: Vec<u16>,
    attributes: u16,
    stream: Stream,
    primary: [u8; 32],
    timestamps: Timestamps,
}

impl DirectoryEntry {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn name_utf16(&self) -> &[u16] {
        &self.name_utf16
    }

    pub fn attributes(&self) -> u16 {
        self.attributes
    }

    pub fn is_directory(&self) -> bool {
        self.attributes & 0x10 != 0
    }

    pub fn stream(&self) -> &Stream {
        &self.stream
    }

    pub fn raw_primary(&self) -> &[u8; 32] {
        &self.primary
    }

    pub fn timestamps(&self) -> &Timestamps {
        &self.timestamps
    }
}

fn entry_set(data: &[u8]) -> Result<RawEntrySet<'_>> {
    let set = RawEntrySet::parse(data).ok_or(Error::Invalid("truncated directory entry set"))?;
    if !set.checksum_ok
        || set
            .secondaries
            .iter()
            .any(|entry| !entry.in_use || !entry.secondary)
    {
        return Err(Error::Invalid("directory set checksum or secondary type"));
    }
    Ok(set)
}

fn file_entry(
    set: &RawEntrySet<'_>,
    boot: &BootSector,
    upcase: &UpcaseTable,
) -> Result<DirectoryEntry> {
    let stream = set
        .stream
        .as_ref()
        .ok_or(Error::Invalid("file stream extension missing or invalid"))?;
    if set.bytes.len() < 3 * ENTRY_SIZE
        || stream.entry_type.value != 0xc0
        || !stream.allocation_possible
    {
        return Err(Error::Invalid("file stream extension missing or invalid"));
    }
    if stream.name_length == 0 || !set.name_complete {
        return Err(Error::Invalid("file name length or secondary count"));
    }
    for entry in &set.names {
        if entry.entry_type.value != 0xc1 || entry.flags & 3 != 0 {
            return Err(Error::Invalid("file name entry order or flags"));
        }
    }
    for entry in &set.secondaries[set.names.len() + 1..] {
        if !entry.in_use || !entry.secondary || !entry.benign {
            return Err(Error::Unsupported("unknown critical file secondary"));
        }
    }
    let decoded_name = set.name();
    let name_utf16 = decoded_name.utf16;
    if name_utf16
        .iter()
        .any(|unit| *unit < 32 || [34, 42, 47, 58, 60, 62, 63, 92, 124].contains(unit))
        || upcase.name_hash(&name_utf16) != stream.name_hash
    {
        return Err(Error::Invalid("file name character or hash"));
    }
    if !decoded_name.utf16_ok {
        return Err(Error::Invalid("file name UTF-16"));
    }
    let name = decoded_name.text;
    if matches!(name.as_str(), "." | "..") {
        return Err(Error::Invalid("reserved directory name"));
    }
    let attributes = set.primary.attributes;
    let stream = Stream {
        first_cluster: stream.first_cluster,
        data_length: stream.data_length,
        valid_data_length: stream.valid_data_length,
        no_fat_chain: stream.no_fat_chain,
    };
    if stream.valid_data_length > stream.data_length {
        return Err(Error::Invalid("valid data exceeds stream length"));
    }
    if stream.data_length == 0 {
        if stream.first_cluster != 0 || stream.no_fat_chain {
            return Err(Error::Invalid("empty stream allocation"));
        }
    } else {
        boot.cluster_offset(stream.first_cluster)?;
        let clusters = stream.data_length.div_ceil(boot.cluster_size());
        if clusters > u64::from(boot.cluster_count)
            || (stream.no_fat_chain
                && u64::from(stream.first_cluster) + clusters > u64::from(boot.cluster_count) + 2)
        {
            return Err(Error::Invalid("stream exceeds cluster heap"));
        }
    }
    if attributes & 0x10 != 0
        && (stream.valid_data_length != stream.data_length
            || !stream.data_length.is_multiple_of(boot.cluster_size())
            || stream.data_length > 256 * 1024 * 1024)
    {
        return Err(Error::Invalid("directory stream length"));
    }
    Ok(DirectoryEntry {
        name,
        name_utf16,
        attributes,
        stream,
        primary: set.primary.raw,
        timestamps: Timestamps::parse(&set.primary.raw)?,
    })
}

pub fn parse(
    data: &[u8],
    boot: &BootSector,
    upcase: &UpcaseTable,
    root: bool,
) -> Result<Vec<DirectoryEntry>> {
    if !data.len().is_multiple_of(ENTRY_SIZE) {
        return Err(Error::Invalid("directory is not entry aligned"));
    }
    let mut entries = Vec::new();
    let mut names = HashSet::new();
    let mut position = 0;
    while position < data.len() {
        let remaining = &data[position..];
        let entry_type = EntryType::parse(remaining).unwrap();
        let kind = entry_type.value;
        if kind == 0 {
            break;
        }
        if !entry_type.in_use {
            position += ENTRY_SIZE;
            continue;
        }
        if kind == 0x85 {
            let set = entry_set(remaining)?;
            let entry = file_entry(&set, boot, upcase)?;
            let folded: Vec<_> = entry
                .name_utf16
                .iter()
                .map(|unit| upcase.map(*unit))
                .collect();
            if !names.insert(folded) {
                return Err(Error::Invalid("duplicate case-insensitive directory name"));
            }
            entries.push(entry);
            position += set.bytes.len();
        } else if root && matches!(kind, 0x81..=0x83) {
            position += ENTRY_SIZE;
        } else if !root && kind == 0xa0 {
            return Err(Error::Invalid("volume GUID outside root directory"));
        } else if entry_type.benign && !entry_type.secondary {
            position += entry_set(remaining)?.bytes.len();
        } else {
            return Err(Error::Unsupported(
                "critical directory entry or orphan secondary",
            ));
        }
    }
    Ok(entries)
}
