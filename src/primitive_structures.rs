use super::{
    boot_check, decode_fat, find_bitmap_entry, root_chain, BootCheck, BootSector,
    BOOT_REGION_SECTORS, FIRST_CLUSTER,
};
use crate::Error;
use std::{fmt, io};

/// The smallest sector size the format allows (section 3.1.14). Every offset
/// is computed from the volume's own sector size; this only sizes the first
/// read, which holds the boot sector that says what that size is.
const MIN_SECTOR: u64 = 512;
const MAX_SECTOR: u64 = 4096;
/// Where the boot sector holds BytesPerSectorShift (section 3.1.13).
const BYTES_PER_SECTOR_SHIFT: usize = 108;

/// Raw structures in read order, without interpreting or repairing their bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructureCopy {
    pub main: Vec<u8>,
    pub backup: Vec<u8>,
    pub fat: Vec<u8>,
    pub bitmap: Vec<u8>,
    pub root: Vec<u8>,
}

/// Decoded geometry and boot diagnostics for an existing snapshot, at the
/// volume's own sector size.
///
/// Invalid trailers, checksums, reserved fields and broken FAT links are not
/// rejected. `boot.signature_ok`, `backup_boot.signature_ok` and `boot_check`
/// report damage. FAT words are padded and root chains are partial, exactly as
/// [`decode_fat`] and [`root_chain`] specify.
#[derive(Debug)]
pub struct StructureGeometry {
    pub boot: BootSector,
    pub backup_boot: BootSector,
    pub boot_check: BootCheck,
    pub fat: Vec<u32>,
    pub root_clusters: Vec<u32>,
}

/// The first bitmap entry's raw location, declared size and volume minimum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BitmapLocation {
    pub first_cluster: u32,
    pub length: u64,
    pub required_length: u64,
}

/// A structure-loading diagnostic, with complete read or boot-sector context.
#[derive(Debug)]
pub enum StructureError {
    Read {
        operation: &'static str,
        source: io::Error,
    },
    Boot {
        region: &'static str,
        source: Error,
    },
    UnsupportedSectorSize(u64),
    MissingBitmap,
    BitmapTooShort {
        length: u64,
        required_length: u64,
    },
    RootLengthMismatch,
    Bounds,
}

impl fmt::Display for StructureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { operation, source } => write!(formatter, "{operation}: {source}"),
            Self::Boot { region, source } => {
                write!(formatter, "{region} boot sector: ")?;
                match source {
                    Error::Invalid(reason) => formatter.write_str(reason),
                    error => error.fmt(formatter),
                }
            }
            Self::UnsupportedSectorSize(size) => {
                write!(formatter, "unsupported sector size {size}")
            }
            Self::MissingBitmap => {
                formatter.write_str("root directory has no allocation bitmap entry")
            }
            Self::BitmapTooShort {
                length,
                required_length,
            } => write!(
                formatter,
                "allocation bitmap holds {length} bytes, volume needs {required_length}"
            ),
            Self::RootLengthMismatch => {
                formatter.write_str("the root directory copy does not match the FAT's root chain")
            }
            Self::Bounds => formatter.write_str("structure size or offset is not representable"),
        }
    }
}

impl std::error::Error for StructureError {}

fn parse_boot(bytes: &[u8], region: &'static str) -> Result<BootSector, StructureError> {
    BootSector::parse(bytes).map_err(|source| StructureError::Boot { region, source })
}

/// The volume's sector size, from its boot sector. The boot parser accepts
/// 512, 1024, 2048 and 4096; a size outside that range is refused here too, so
/// no offset is ever computed from a size the format does not allow.
fn sector_size(boot: &BootSector) -> Result<u64, StructureError> {
    let sector = 1u64
        .checked_shl(u32::from(boot.bytes_per_sector_shift))
        .ok_or(StructureError::Bounds)?;
    if !(MIN_SECTOR..=MAX_SECTOR).contains(&sector) {
        return Err(StructureError::UnsupportedSectorSize(sector));
    }
    Ok(sector)
}

fn cluster_size(boot: &BootSector, sector: u64) -> Result<u64, StructureError> {
    boot.cluster_count
        .checked_add(FIRST_CLUSTER)
        .ok_or(StructureError::Bounds)?;
    let sectors = 1u64
        .checked_shl(u32::from(boot.sectors_per_cluster_shift))
        .ok_or(StructureError::Bounds)?;
    sector.checked_mul(sectors).ok_or(StructureError::Bounds)
}

fn cluster_position(
    boot: &BootSector,
    cluster: u32,
    sector: u64,
    cluster_size: u64,
) -> Result<u64, StructureError> {
    let index = cluster
        .checked_sub(FIRST_CLUSTER)
        .ok_or(StructureError::Bounds)?;
    u64::from(index)
        .checked_mul(cluster_size)
        .and_then(|offset| offset.checked_add(u64::from(boot.cluster_heap_offset) * sector))
        .ok_or(StructureError::Bounds)
}

fn buffer(length: u64) -> Result<Vec<u8>, StructureError> {
    let length = usize::try_from(length).map_err(|_| StructureError::Bounds)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| StructureError::Bounds)?;
    bytes.resize(length, 0);
    Ok(bytes)
}

/// Finds the first raw bitmap entry and checks its declared minimum byte size.
///
/// The required length is `ceil(cluster_count / 8)`. Oversized declarations are
/// preserved, not clamped; cluster placement, FAT links and flags are not
/// validated. Missing entries and undersized declarations return diagnostics.
pub fn bitmap_location(boot: &BootSector, root: &[u8]) -> Result<BitmapLocation, StructureError> {
    let (first_cluster, length) = find_bitmap_entry(root).ok_or(StructureError::MissingBitmap)?;
    let required_length = u64::from(boot.cluster_count).div_ceil(8);
    if length < required_length {
        return Err(StructureError::BitmapTooShort {
            length,
            required_length,
        });
    }
    Ok(BitmapLocation {
        first_cluster,
        length,
        required_length,
    })
}

/// Reads both boot regions, FAT, partial root chain and bitmap, at the sector
/// size the main boot sector declares (512, 1024, 2048 or 4096 bytes).
///
/// The callback reads exactly the requested bytes, with no device alignment,
/// geometry policy or strict-reader validation added around it. The first read
/// is the 6144 bytes that hold twelve 512-byte sectors, which always contain the
/// boot sector; a volume with larger sectors then has the rest of its main boot
/// region read, so a callback over a raw device with larger logical sectors
/// must itself align the first read. The main boot
/// name, sector size and bitmap's minimum declared size must be usable;
/// damaged boot trailers/checksums, backup contents and incomplete root chains
/// remain available for inspection. The bitmap is read contiguously at its
/// declared location and length, without walking its FAT or clamping its size.
/// I/O failures retain their operation context. Unrepresentable arithmetic or
/// allocation sizes return [`StructureError::Bounds`], without a volume-size cap.
pub fn read_structures(
    mut read_exact_at: impl FnMut(u64, &mut [u8]) -> io::Result<()>,
) -> Result<StructureCopy, StructureError> {
    let mut read = |offset, output: &mut [u8], operation| {
        read_exact_at(offset, output).map_err(|source| StructureError::Read { operation, source })
    };
    let first_bytes = BOOT_REGION_SECTORS * MIN_SECTOR;
    let mut main = buffer(first_bytes)?;
    read(0, &mut main, "reading main boot region")?;
    // The boot sector says how large the sectors are. A shift it cannot hold
    // is left to the parse below, which names it.
    let declared = main
        .get(BYTES_PER_SECTOR_SHIFT)
        .map_or(MIN_SECTOR, |&shift| match shift {
            9..=12 => 1 << shift,
            _ => MIN_SECTOR,
        });
    let region_bytes = BOOT_REGION_SECTORS * declared;
    if region_bytes > first_bytes {
        main.resize(region_bytes as usize, 0);
        read(
            first_bytes,
            &mut main[first_bytes as usize..],
            "reading main boot region",
        )?;
    }
    let mut backup = buffer(region_bytes)?;
    read(region_bytes, &mut backup, "reading backup boot region")?;
    let boot = parse_boot(&main, "main")?;
    let sector = sector_size(&boot)?;
    let cluster_size = cluster_size(&boot, sector)?;
    let mut fat = buffer(u64::from(boot.fat_length) * sector)?;
    read(u64::from(boot.fat_offset) * sector, &mut fat, "reading FAT")?;
    let root_clusters = root_chain(&boot, &decode_fat(&fat, &boot));
    let root_length = (root_clusters.len() as u64)
        .checked_mul(cluster_size)
        .ok_or(StructureError::Bounds)?;
    let mut root = buffer(root_length)?;
    let cluster_bytes = usize::try_from(cluster_size).map_err(|_| StructureError::Bounds)?;
    for (&cluster, output) in root_clusters.iter().zip(root.chunks_mut(cluster_bytes)) {
        read(
            cluster_position(&boot, cluster, sector, cluster_size)?,
            output,
            "reading root directory",
        )?;
    }
    let location = bitmap_location(&boot, &root)?;
    let mut bitmap = buffer(location.length)?;
    read(
        cluster_position(&boot, location.first_cluster, sector, cluster_size)?,
        &mut bitmap,
        "reading allocation bitmap",
    )?;
    Ok(StructureCopy {
        main,
        backup,
        fat,
        bitmap,
        root,
    })
}

/// Decodes saved structures without re-reading or imposing strict validity.
///
/// Both boot names and minimum lengths, the main sector size and the
/// root-copy length must match. Bitmap sizing is deliberately not rechecked:
/// this is an inspection of an existing snapshot, not a fresh structure read.
/// Boot regions need eleven sectors of the volume's size and the first stored checksum
/// word; a partial checksum sector remains inspectable.
pub fn inspect_structures(copy: &StructureCopy) -> Result<StructureGeometry, StructureError> {
    let boot = parse_boot(&copy.main, "main")?;
    let backup_boot = parse_boot(&copy.backup, "backup")?;
    let sector = sector_size(&boot)?;
    let cluster_size = cluster_size(&boot, sector)?;
    let checksum_bytes = (sector * 11) as usize + std::mem::size_of::<u32>();
    if copy.main.len() < checksum_bytes || copy.backup.len() < checksum_bytes {
        return Err(StructureError::Bounds);
    }
    let boot_check = boot_check(&copy.main, &copy.backup, sector as usize);
    let fat = decode_fat(&copy.fat, &boot);
    let root_clusters = root_chain(&boot, &fat);
    let root_length = (root_clusters.len() as u64)
        .checked_mul(cluster_size)
        .ok_or(StructureError::Bounds)?;
    if copy.root.len() as u64 != root_length {
        return Err(StructureError::RootLengthMismatch);
    }
    Ok(StructureGeometry {
        boot,
        backup_boot,
        boot_check,
        fat,
        root_clusters,
    })
}

/// Returns a complete raw little-endian FAT word, including reserved entries.
///
/// Partial words, absent entries and unrepresentable offsets return `None`.
/// No cluster-number or FAT-value policy is applied.
pub fn checked_fat_entry(fat: &[u8], cluster: usize) -> Option<u32> {
    let start = cluster.checked_mul(std::mem::size_of::<u32>())?;
    let end = start.checked_add(std::mem::size_of::<u32>())?;
    fat.get(start..end)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
}

/// Returns the raw allocation bit, or `false` outside the supplied snapshot.
///
/// Cluster two is bit zero. Reserved clusters below two also return `false`;
/// no volume cluster count is imposed on the bitmap's available bits.
pub fn checked_bitmap_bit(bitmap: &[u8], cluster: u32) -> bool {
    let Some(index) = cluster.checked_sub(FIRST_CLUSTER) else {
        return false;
    };
    bitmap
        .get(index as usize / 8)
        .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
}
