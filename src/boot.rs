use crate::device::Reader;
use crate::{primitives, Error, Result};
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

impl BootSector {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let header = primitives::BootSectorHeader::parse(data)
            .map_err(|_| Error::Invalid("short boot sector"))?;
        Self::from_primitive(header)
    }

    fn from_primitive(header: primitives::BootSectorHeader) -> Result<Self> {
        if !header.jump_valid
            || !header.file_system_name_valid
            || !header.must_be_zero_valid
            || !header.sector.signature_ok
        {
            return Err(Error::Invalid(
                "boot identity, jump, zero field or signature",
            ));
        }
        let fields = header.sector;
        let boot = Self {
            partition_offset: fields.partition_offset,
            volume_length: fields.volume_length,
            fat_offset: fields.fat_offset,
            fat_length: fields.fat_length,
            cluster_heap_offset: fields.cluster_heap_offset,
            cluster_count: fields.cluster_count,
            root_cluster: fields.root_cluster,
            volume_serial: fields.volume_serial,
            revision: fields.revision,
            volume_flags: fields.volume_flags,
            bytes_per_sector_shift: fields.bytes_per_sector_shift,
            sectors_per_cluster_shift: fields.sectors_per_cluster_shift,
            number_of_fats: fields.number_of_fats,
            percent_in_use: fields.percent_in_use,
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
        // Section 3.1.12: the high byte of FileSystemRevision is the major
        // revision and the low byte the minor revision, 0 to 99. Major
        // revision 1 is the only one the specification describes, and
        // implementations should mount any minor revision of it, so 1.01 to
        // 1.99 mount like 1.00. Any other major revision is refused.
        if self.revision >> 8 != 1 {
            return Err(Error::Unsupported("filesystem revision"));
        }
        if self.revision & 0xFF > 99 {
            return Err(Error::Invalid("filesystem revision"));
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
    Ok(primitives::boot_checksum(data, bytes_per_sector))
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
        let raw = primitives::BootRegion::parse(data, sector_size)?;
        let sector = BootSector::from_primitive(raw.header);
        let fits_device = sector.as_ref().is_ok_and(|boot| {
            boot.bytes_per_sector() == sector_size as u64
                && boot
                    .volume_length
                    .checked_mul(boot.bytes_per_sector())
                    .is_some_and(|bytes| bytes <= device_size)
        });
        Ok(Self {
            sector,
            computed_checksum: raw.computed_checksum,
            stored_checksum: raw.stored_checksum,
            checksum_valid: raw.checksum_valid,
            extended_signatures_valid: raw.extended_signatures_valid,
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
        let shift = primitives::BootSectorHeader::parse(&prefix)?
            .sector
            .bytes_per_sector_shift;
        let initial = if (9..=12).contains(&shift) {
            Some(Self::read_with_shift(reader, shift)?)
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
