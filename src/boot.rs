use crate::device::Reader;
use crate::{Error, Result};
use fs_core::BlockRead;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootSector {
    pub partition_offset: u64,
    pub volume_length: u64,
    pub fat_offset: u32,
    pub fat_length: u32,
    pub cluster_heap_offset: u32,
    pub cluster_count: u32,
    pub root_cluster: u32,
    pub volume_serial: u32,
    pub revision: u16,
    pub volume_flags: u16,
    pub bytes_per_sector_shift: u8,
    pub sectors_per_cluster_shift: u8,
    pub number_of_fats: u8,
    pub percent_in_use: u8,
}

fn word(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap())
}

fn dword(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn qword(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

impl BootSector {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 512 {
            return Err(Error::Invalid("short boot sector"));
        }
        if data[..3] != [0xeb, 0x76, 0x90]
            || &data[3..11] != b"EXFAT   "
            || data[11..64].iter().any(|byte| *byte != 0)
            || word(data, 510) != 0xaa55
        {
            return Err(Error::Invalid(
                "boot identity, jump, zero field or signature",
            ));
        }
        let boot = Self {
            partition_offset: qword(data, 64),
            volume_length: qword(data, 72),
            fat_offset: dword(data, 80),
            fat_length: dword(data, 84),
            cluster_heap_offset: dword(data, 88),
            cluster_count: dword(data, 92),
            root_cluster: dword(data, 96),
            volume_serial: dword(data, 100),
            revision: word(data, 104),
            volume_flags: word(data, 106),
            bytes_per_sector_shift: data[108],
            sectors_per_cluster_shift: data[109],
            number_of_fats: data[110],
            percent_in_use: data[112],
        };
        boot.validate()?;
        Ok(boot)
    }

    fn validate(&self) -> Result<()> {
        if !(9..=12).contains(&self.bytes_per_sector_shift)
            || self.sectors_per_cluster_shift > 25 - self.bytes_per_sector_shift
        {
            return Err(Error::Invalid("sector or cluster shift"));
        }
        if self.revision != 0x0100 {
            return Err(Error::Unsupported("filesystem revision"));
        }
        if !matches!(self.number_of_fats, 1 | 2)
            || self.number_of_fats == 1 && self.volume_flags & 1 != 0
            || self.percent_in_use > 100 && self.percent_in_use != 255
        {
            return Err(Error::Invalid("FAT count, active FAT or percentage"));
        }
        if self.cluster_count == 0 || self.cluster_count > 0xffff_fff5 {
            return Err(Error::Invalid("cluster count"));
        }
        if !(2..u64::from(self.cluster_count) + 2).contains(&u64::from(self.root_cluster)) {
            return Err(Error::Invalid("root cluster"));
        }
        let sector = self.bytes_per_sector();
        let volume_bytes = self
            .volume_length
            .checked_mul(sector)
            .ok_or(Error::Bounds)?;
        let fat_end = u64::from(self.fat_offset)
            + u64::from(self.fat_length) * u64::from(self.number_of_fats);
        let heap_end = u64::from(self.cluster_heap_offset)
            + (u64::from(self.cluster_count) << self.sectors_per_cluster_shift);
        let required_fat_bytes = (u64::from(self.cluster_count) + 2) * 4;
        if volume_bytes < 1024 * 1024
            || self.fat_offset < 24
            || u64::from(self.fat_length) * sector < required_fat_bytes
            || fat_end > u64::from(self.cluster_heap_offset)
            || heap_end > self.volume_length
        {
            return Err(Error::Invalid("volume, FAT or heap geometry"));
        }
        Ok(())
    }

    pub fn bytes_per_sector(&self) -> u64 {
        1u64.checked_shl(u32::from(self.bytes_per_sector_shift))
            .unwrap_or(0)
    }

    pub fn cluster_size(&self) -> u64 {
        self.bytes_per_sector()
            .checked_shl(u32::from(self.sectors_per_cluster_shift))
            .unwrap_or(0)
    }

    pub fn cluster_offset(&self, cluster: u32) -> Result<u64> {
        self.validate()?;
        if !(2..u64::from(self.cluster_count) + 2).contains(&u64::from(cluster)) {
            return Err(Error::Invalid("cluster outside heap"));
        }
        Ok(
            u64::from(self.cluster_heap_offset) * self.bytes_per_sector()
                + u64::from(cluster - 2) * self.cluster_size(),
        )
    }
}

pub fn boot_checksum(region: &[u8], bytes_per_sector: usize) -> Result<u32> {
    if !matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096) {
        return Err(Error::Invalid("checksum sector size"));
    }
    let data = region
        .get(..bytes_per_sector * 11)
        .ok_or(Error::Invalid("short boot region"))?;
    Ok(data
        .iter()
        .enumerate()
        .filter(|(index, _)| !matches!(index, 106 | 107 | 112))
        .fold(0u32, |checksum, (_, byte)| {
            checksum.rotate_right(1).wrapping_add(u32::from(*byte))
        }))
}

#[derive(Debug)]
pub struct BootCopy {
    pub sector: Result<BootSector>,
    pub computed_checksum: u32,
    pub stored_checksum: u32,
    pub checksum_valid: bool,
    pub extended_signatures_valid: bool,
    pub fits_device: bool,
}

impl BootCopy {
    fn inspect(data: &[u8], sector_size: usize, device_size: u64) -> Result<Self> {
        let computed_checksum = boot_checksum(data, sector_size)?;
        let stored = data
            .get(sector_size * 11..sector_size * 12)
            .ok_or(Error::Invalid("short checksum sector"))?;
        let sector = BootSector::parse(data);
        let fits_device = sector.as_ref().is_ok_and(|boot| {
            boot.bytes_per_sector() == sector_size as u64
                && boot
                    .volume_length
                    .checked_mul(boot.bytes_per_sector())
                    .is_some_and(|bytes| bytes <= device_size)
        });
        Ok(Self {
            sector,
            computed_checksum,
            stored_checksum: dword(stored, 0),
            checksum_valid: stored
                .chunks_exact(4)
                .all(|entry| dword(entry, 0) == computed_checksum),
            extended_signatures_valid: (1..=8)
                .all(|index| dword(data, (index + 1) * sector_size - 4) == 0xaa55_0000),
            fits_device,
        })
    }

    pub fn valid(&self) -> bool {
        self.sector.is_ok()
            && self.checksum_valid
            && self.extended_signatures_valid
            && self.fits_device
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootDifference {
    pub offset: usize,
    pub main: u8,
    pub backup: u8,
}

impl BootDifference {
    pub fn is_mutable(&self) -> bool {
        matches!(self.offset, 106 | 107 | 112)
    }
}

#[derive(Debug)]
pub struct BootRegions {
    pub main: BootCopy,
    pub backup: BootCopy,
    pub differences: Vec<BootDifference>,
}

impl BootRegions {
    pub fn read<Device: BlockRead>(reader: &Reader<Device>) -> Result<Self> {
        let mut prefix = [0; 512];
        reader.read_at(0, &mut prefix)?;
        let initial = if (9..=12).contains(&prefix[108]) {
            Some(Self::read_with_shift(reader, prefix[108])?)
        } else {
            None
        };
        if initial
            .as_ref()
            .is_some_and(|report| report.main.valid() || report.backup.valid())
        {
            return Ok(initial.unwrap());
        }
        let mut recovered = None;
        for shift in 9..=12 {
            if let Ok(report) = Self::read_with_shift(reader, shift) {
                if report.backup.valid() {
                    if recovered.is_some() {
                        return Err(Error::Invalid("ambiguous backup boot sector sizes"));
                    }
                    recovered = Some(report);
                }
            }
        }
        recovered
            .or(initial)
            .ok_or(Error::Invalid("neither boot copy identifies a sector size"))
    }

    fn read_with_shift<Device: BlockRead>(reader: &Reader<Device>, shift: u8) -> Result<Self> {
        let sector_size = 1usize << shift;
        let mut data = vec![0; sector_size * 24];
        reader.read_at(0, &mut data)?;
        let (main, backup) = data.split_at(sector_size * 12);
        Ok(Self {
            main: BootCopy::inspect(main, sector_size, reader.size_bytes())?,
            backup: BootCopy::inspect(backup, sector_size, reader.size_bytes())?,
            differences: main
                .iter()
                .zip(backup)
                .enumerate()
                .filter(|(_, (main, backup))| main != backup)
                .map(|(offset, (main, backup))| BootDifference {
                    offset,
                    main: *main,
                    backup: *backup,
                })
                .collect(),
        })
    }

    pub fn strict(&self) -> Result<&BootSector> {
        if !self.main.valid() || !self.backup.valid() {
            return Err(Error::Invalid(
                "main or backup boot region failed validation",
            ));
        }
        if self
            .differences
            .iter()
            .any(|difference| !difference.is_mutable())
        {
            return Err(Error::Invalid("main and backup boot regions disagree"));
        }
        self.main
            .sector
            .as_ref()
            .map_err(|_| Error::Invalid("main boot sector"))
    }
}
