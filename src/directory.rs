use crate::boot::BootSector;
use crate::metadata::{set_checksum, UpcaseTable};
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

fn word(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap())
}

fn entry_set(data: &[u8]) -> Result<&[u8]> {
    let length = (usize::from(data[1]) + 1) * 32;
    let set = data
        .get(..length)
        .ok_or(Error::Invalid("truncated directory entry set"))?;
    if set_checksum(set) != word(set, 2)
        || set[32..]
            .chunks_exact(32)
            .any(|entry| entry[0] & 0xc0 != 0xc0)
    {
        return Err(Error::Invalid("directory set checksum or secondary type"));
    }
    Ok(set)
}

fn file_entry(set: &[u8], boot: &BootSector, upcase: &UpcaseTable) -> Result<DirectoryEntry> {
    if set.len() < 96 || set[32] != 0xc0 || set[33] & 1 == 0 {
        return Err(Error::Invalid("file stream extension missing or invalid"));
    }
    let name_length = usize::from(set[35]);
    let name_entries = name_length.div_ceil(15);
    if name_length == 0 || set.len() < (name_entries + 2) * 32 {
        return Err(Error::Invalid("file name length or secondary count"));
    }
    let mut name_utf16 = Vec::with_capacity(name_entries * 15);
    for entry in set[64..(name_entries + 2) * 32].chunks_exact(32) {
        if entry[0] != 0xc1 || entry[1] & 3 != 0 {
            return Err(Error::Invalid("file name entry order or flags"));
        }
        name_utf16.extend((0..15).map(|index| word(entry, 2 + index * 2)));
    }
    for entry in set[(name_entries + 2) * 32..].chunks_exact(32) {
        if entry[0] & 0xe0 != 0xe0 {
            return Err(Error::Unsupported("unknown critical file secondary"));
        }
    }
    name_utf16.truncate(name_length);
    if name_utf16
        .iter()
        .any(|unit| *unit < 32 || [34, 42, 47, 58, 60, 62, 63, 92, 124].contains(unit))
        || upcase.name_hash(&name_utf16) != word(set, 36)
    {
        return Err(Error::Invalid("file name character or hash"));
    }
    let name = String::from_utf16(&name_utf16).map_err(|_| Error::Invalid("file name UTF-16"))?;
    if matches!(name.as_str(), "." | "..") {
        return Err(Error::Invalid("reserved directory name"));
    }
    let attributes = word(set, 4);
    let stream = Stream {
        first_cluster: u32::from_le_bytes(set[52..56].try_into().unwrap()),
        data_length: u64::from_le_bytes(set[56..64].try_into().unwrap()),
        valid_data_length: u64::from_le_bytes(set[40..48].try_into().unwrap()),
        no_fat_chain: set[33] & 2 != 0,
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
        primary: set[..32].try_into().unwrap(),
        timestamps: Timestamps::parse(&set[..32])?,
    })
}

pub fn parse(
    data: &[u8],
    boot: &BootSector,
    upcase: &UpcaseTable,
    root: bool,
) -> Result<Vec<DirectoryEntry>> {
    if !data.len().is_multiple_of(32) {
        return Err(Error::Invalid("directory is not entry aligned"));
    }
    let mut entries = Vec::new();
    let mut names = HashSet::new();
    let mut position = 0;
    while position < data.len() {
        let remaining = &data[position..];
        let kind = remaining[0];
        if kind == 0 {
            break;
        }
        if kind & 0x80 == 0 {
            position += 32;
            continue;
        }
        if kind == 0x85 {
            let set = entry_set(remaining)?;
            let entry = file_entry(set, boot, upcase)?;
            let folded: Vec<_> = entry
                .name_utf16
                .iter()
                .map(|unit| upcase.map(*unit))
                .collect();
            if !names.insert(folded) {
                return Err(Error::Invalid("duplicate case-insensitive directory name"));
            }
            entries.push(entry);
            position += set.len();
        } else if root && matches!(kind, 0x81..=0x83) {
            position += 32;
        } else if !root && kind == 0xa0 {
            return Err(Error::Invalid("volume GUID outside root directory"));
        } else if kind & 0xe0 == 0xa0 {
            position += entry_set(remaining)?.len();
        } else {
            return Err(Error::Unsupported(
                "critical directory entry or orphan secondary",
            ));
        }
    }
    Ok(entries)
}
