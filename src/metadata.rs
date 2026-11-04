use crate::{Error, Result};

pub const END_OF_CHAIN: u32 = 0xffff_ffff;

pub fn checksum32(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |sum, byte| {
        sum.rotate_right(1).wrapping_add(u32::from(*byte))
    })
}

pub fn set_checksum(data: &[u8]) -> u16 {
    data.iter()
        .enumerate()
        .filter(|(index, _)| !matches!(index, 2 | 3))
        .fold(0u16, |sum, (_, byte)| {
            sum.rotate_right(1).wrapping_add(u16::from(*byte))
        })
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
        match value {
            0 => Ok(Self::Free),
            0xffff_fff7 => Ok(Self::Bad),
            END_OF_CHAIN => Ok(Self::End),
            next if next >= 2 && u64::from(next) < u64::from(cluster_count) + 2 => {
                Ok(Self::Next(next))
            }
            _ => Err(Error::Invalid("reserved or out-of-range FAT entry")),
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
        if bytes.len() as u64 != u64::from(cluster_count).div_ceil(8) {
            return Err(Error::Invalid("allocation bitmap length"));
        }
        Ok(Self {
            bytes,
            cluster_count,
        })
    }

    pub fn allocated(&self, cluster: u32) -> Result<bool> {
        if cluster < 2 || u64::from(cluster) >= u64::from(self.cluster_count) + 2 {
            return Err(Error::Invalid("bitmap cluster outside heap"));
        }
        let index = (cluster - 2) as usize;
        Ok(self.bytes[index / 8] & (1 << (index % 8)) != 0)
    }
}

#[derive(Debug)]
pub struct UpcaseTable {
    mappings: Vec<u16>,
}

impl UpcaseTable {
    pub fn parse(data: &[u8], expected_checksum: u32) -> Result<Self> {
        if data.is_empty()
            || !data.len().is_multiple_of(2)
            || data.len() > 131072
            || checksum32(data) != expected_checksum
        {
            return Err(Error::Invalid("upcase length or checksum"));
        }
        let mut mappings = Vec::with_capacity(65536);
        let mut words = data
            .chunks_exact(2)
            .map(|word| u16::from_le_bytes([word[0], word[1]]));
        while let Some(value) = words.next() {
            if value == 0xffff && mappings.len() < 65535 {
                let count =
                    usize::from(words.next().ok_or(Error::Invalid("truncated upcase run"))?);
                let end = mappings.len().checked_add(count).ok_or(Error::Bounds)?;
                if count == 0 || end > 65536 {
                    return Err(Error::Invalid("upcase run outside Unicode range"));
                }
                mappings.extend((mappings.len()..end).map(|unit| unit as u16));
            } else {
                if mappings.len() == 65536 {
                    return Err(Error::Invalid("extra upcase mappings"));
                }
                mappings.push(value);
            }
        }
        if mappings.len() != 65536 {
            return Err(Error::Invalid("incomplete upcase table"));
        }
        for unit in 0u16..128 {
            let expected = if (97..=122).contains(&unit) {
                unit - 32
            } else {
                unit
            };
            if mappings[usize::from(unit)] != expected {
                return Err(Error::Invalid("mandatory ASCII upcase mapping"));
            }
        }
        Ok(Self { mappings })
    }

    pub fn map(&self, unit: u16) -> u16 {
        self.mappings[usize::from(unit)]
    }

    pub fn name_hash(&self, name: &[u16]) -> u16 {
        name.iter()
            .flat_map(|unit| self.map(*unit).to_le_bytes())
            .fold(0u16, |sum, byte| {
                sum.rotate_right(1).wrapping_add(u16::from(byte))
            })
    }

    pub fn equal(&self, first: &[u16], second: &[u16]) -> bool {
        first.len() == second.len()
            && first
                .iter()
                .zip(second)
                .all(|(first, second)| self.map(*first) == self.map(*second))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SystemStream {
    pub first_cluster: u32,
    pub length: u64,
}

#[derive(Debug)]
pub struct RootMetadata {
    pub bitmaps: [Option<SystemStream>; 2],
    pub upcase: SystemStream,
    pub upcase_checksum: u32,
    pub label: Option<String>,
    pub guid: Option<[u8; 16]>,
}

fn word(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap())
}

fn stream(data: &[u8]) -> SystemStream {
    SystemStream {
        first_cluster: u32::from_le_bytes(data[20..24].try_into().unwrap()),
        length: u64::from_le_bytes(data[24..32].try_into().unwrap()),
    }
}

impl RootMetadata {
    pub fn parse(data: &[u8], number_of_fats: u8) -> Result<Self> {
        if !data.len().is_multiple_of(32) || !matches!(number_of_fats, 1 | 2) {
            return Err(Error::Invalid("root directory size or FAT count"));
        }
        let mut bitmaps = [None, None];
        let mut upcase = None;
        let mut label = None;
        let mut guid = None;
        let mut position = 0;
        while position < data.len() {
            let entry = &data[position..position + 32];
            if entry[0] == 0 {
                break;
            }
            position += 32;
            if entry[0] & 0x80 == 0 {
                continue;
            }
            match entry[0] {
                0x81 => {
                    let id = usize::from(entry[1] & 1);
                    if entry[1] & !1 != 0
                        || id >= usize::from(number_of_fats)
                        || bitmaps[id].is_some()
                    {
                        return Err(Error::Invalid("allocation bitmap identity or duplicate"));
                    }
                    bitmaps[id] = Some(stream(entry));
                }
                0x82 => {
                    if upcase.is_some() {
                        return Err(Error::Invalid("duplicate upcase entry"));
                    }
                    upcase = Some((
                        stream(entry),
                        u32::from_le_bytes(entry[4..8].try_into().unwrap()),
                    ));
                }
                0x83 => {
                    let length = usize::from(entry[1]);
                    if length > 11 || label.is_some() {
                        return Err(Error::Invalid("volume label length or duplicate"));
                    }
                    let units: Vec<_> = (0..length)
                        .map(|index| word(entry, 2 + index * 2))
                        .collect();
                    if units.iter().any(|unit| {
                        *unit < 32 || [34, 42, 47, 58, 60, 62, 63, 92, 124].contains(unit)
                    }) {
                        return Err(Error::Invalid("volume label character"));
                    }
                    label = Some(
                        String::from_utf16(&units)
                            .map_err(|_| Error::Invalid("volume label UTF-16"))?,
                    );
                }
                0xa0 => {
                    let value: [u8; 16] = entry[6..22].try_into().unwrap();
                    if guid.is_some()
                        || entry[1] != 0
                        || word(entry, 4) & 3 != 0
                        || value == [0; 16]
                        || set_checksum(entry) != word(entry, 2)
                    {
                        return Err(Error::Invalid("volume GUID entry"));
                    }
                    guid = Some(value);
                }
                0x85 => {
                    if entry[1] < 2 {
                        return Err(Error::Invalid("file secondary count"));
                    }
                    position = position
                        .checked_add(usize::from(entry[1]) * 32)
                        .ok_or(Error::Bounds)?;
                    if position > data.len() {
                        return Err(Error::Invalid("truncated root entry set"));
                    }
                }
                kind if kind & 0x60 == 0x20 => {
                    position = position
                        .checked_add(usize::from(entry[1]) * 32)
                        .ok_or(Error::Bounds)?;
                    if position > data.len() {
                        return Err(Error::Invalid("truncated benign entry set"));
                    }
                }
                _ => return Err(Error::Unsupported("critical root directory entry")),
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
