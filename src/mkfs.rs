//! The formatter: lays down a new, empty exFAT volume on a [`BlockDevice`].
//!
//! Everything here is written from Microsoft's exFAT specification and this
//! crate's own decoders, which read the result back in the tests. A formatted
//! volume holds, in this order on the device:
//!
//! - the Main and Backup Boot regions (section 3): boot sector, eight extended
//!   boot sectors, OEM parameters, a reserved sector and the boot checksum;
//! - the First FAT, and the Second FAT when two are asked for (section 4);
//! - in the cluster heap, from cluster 2: the Allocation Bitmap (two on a
//!   two-FAT volume), the recommended up-case table of section 7.2.5.1 and
//!   one cluster of root directory holding the optional Volume Label, the
//!   Allocation Bitmap and Up-case Table entries and an optional Volume GUID.
//!
//! The sector size defaults to 512 bytes. The cluster size defaults by volume
//! size to the sizes Microsoft documents for Windows ([`default_cluster_size`]),
//! raised while the volume would hold more than the 2^24 - 2 clusters section
//! 3.1.9 recommends. The FAT and the cluster heap each start on a multiple of
//! the alignment, which defaults to the cluster size so every cluster is
//! aligned on the device as it is in the heap (sections 3.1.6 to 3.1.8). Each
//! can be overridden through [`FormatOptions`].
//!
//! [`format`] writes the boot sector last, after everything it describes, so
//! an interrupted format never leaves a boot sector whose checksum matches a
//! volume that is not there.

use crate::primitives::{boot_checksum, entry_set_checksum, FAT_END, FIRST_CLUSTER};
use fs_core::BlockDevice;
use std::fmt;

#[path = "mkfs_upcase.rs"]
mod upcase;

pub use upcase::{RECOMMENDED_UPCASE, RECOMMENDED_UPCASE_CHECKSUM};

/// The sector size used when [`FormatOptions::bytes_per_sector`] is `None`.
pub const DEFAULT_BYTES_PER_SECTOR: u32 = 512;
/// The largest cluster, 32 MiB (section 3.1.15).
pub const MAX_CLUSTER_SIZE: u32 = 32 << 20;
/// The most clusters a FAT can describe, 2^32 - 11 (section 3.1.9).
pub const MAX_CLUSTER_COUNT: u32 = 0xFFFF_FFF5;
/// The most clusters section 3.1.9 recommends a heap hold, 2^24 - 2. The
/// default cluster size grows until a volume fits within it.
pub const RECOMMENDED_MAX_CLUSTERS: u64 = (1 << 24) - 2;
/// The smallest volume, 1 MiB (section 3.1.5).
pub const MIN_VOLUME_BYTES: u64 = 1 << 20;
/// The longest volume label, in UTF-16 code units (section 7.3.2).
pub const MAX_LABEL_UNITS: usize = 11;

/// Sectors in one boot region (section 3, Table 3).
const BOOT_REGION_SECTORS: u64 = 12;
/// The first sector a FAT may start at: past both boot regions (section 3.1.6).
const FIRST_FAT_SECTOR: u64 = 2 * BOOT_REGION_SECTORS;
/// FatEntry[0]: media type F8h, then FFh (section 4.1.1).
const MEDIA_FAT_ENTRY: u32 = 0xFFFF_FFF8;
/// The jump instruction (section 3.1.1).
const JUMP_BOOT: [u8; 3] = [0xEB, 0x76, 0x90];
/// FileSystemName (section 3.1.2).
const FILE_SYSTEM_NAME: &[u8; 8] = b"EXFAT   ";
/// Revision 1.00 (section 3.1.12).
const REVISION: u16 = 0x0100;
/// The INT 13h drive number earlier FAT volumes held (section 3.1.17).
const DRIVE_SELECT: u8 = 0x80;
/// BootCode without boot-strapping instructions: halt (section 3.1.19).
const BOOT_CODE_FILL: u8 = 0xF4;
/// ExtendedBootSignature, AA550000h (section 3.2.2).
const EXTENDED_BOOT_SIGNATURE: u32 = 0xAA55_0000;
/// The ten Parameters structures of the OEM Parameters sector (section 3.3).
const OEM_PARAMETERS_BYTES: usize = 480;
/// Directory entry types this formatter writes (sections 7.1, 7.2, 7.3, 7.5).
const BITMAP_ENTRY: u8 = 0x81;
const UPCASE_ENTRY: u8 = 0x82;
const LABEL_ENTRY: u8 = 0x83;
const GUID_ENTRY: u8 = 0xA0;
const ENTRY_BYTES: usize = 32;
/// The most bytes one device write carries.
const WRITE_CHUNK: u64 = 1 << 20;

/// What to format. `Default` formats the whole device with 512-byte sectors,
/// one FAT, the default cluster size and alignment, no label, no GUID and a
/// serial number taken from the clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatOptions {
    /// Bytes to format from the start of the device; `None` is all of it.
    /// Rounded down to a whole sector.
    pub size_bytes: Option<u64>,
    /// 512, 1024, 2048 or 4096 (section 3.1.14); `None` is 512.
    pub bytes_per_sector: Option<u32>,
    /// A power of two from the sector size to 32 MiB; `None` chooses by
    /// volume size ([`default_cluster_size`]).
    pub cluster_size: Option<u32>,
    /// The FAT and the cluster heap start on multiples of this many bytes
    /// from the start of the volume, a multiple of the sector size; `None` is
    /// the cluster size.
    pub alignment: Option<u64>,
    /// 1, or 2 for a TexFAT volume with a Second FAT and Second Allocation
    /// Bitmap (sections 1.5 and 3.1.16).
    pub number_of_fats: u8,
    /// At most eleven UTF-16 code units, none of them a character Table 35
    /// forbids. `None` writes no Volume Label entry; an empty label writes
    /// one with no characters.
    pub label: Option<String>,
    /// `None` combines the current date and time (section 3.1.11).
    pub volume_serial: Option<u32>,
    /// Writes a Volume GUID entry (section 7.5) when present.
    pub guid: Option<[u8; 16]>,
    /// PartitionOffset, in sectors; 0 tells readers to ignore it (section
    /// 3.1.4).
    pub partition_offset: u64,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            size_bytes: None,
            bytes_per_sector: None,
            cluster_size: None,
            alignment: None,
            number_of_fats: 1,
            label: None,
            volume_serial: None,
            guid: None,
            partition_offset: 0,
        }
    }
}

/// A run of contiguous heap clusters holding one system stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    pub first_cluster: u32,
    pub clusters: u32,
    /// The stream's DataLength in bytes.
    pub length: u64,
}

impl Extent {
    /// The cluster after this extent's last.
    fn end(&self) -> u32 {
        self.first_cluster + self.clusters
    }
}

/// Every value a format writes, decided before anything is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub bytes_per_sector_shift: u8,
    pub sectors_per_cluster_shift: u8,
    pub partition_offset: u64,
    /// VolumeLength, in sectors.
    pub volume_length: u64,
    /// FatOffset, in sectors.
    pub fat_offset: u32,
    /// FatLength, in sectors.
    pub fat_length: u32,
    pub number_of_fats: u8,
    /// ClusterHeapOffset, in sectors.
    pub cluster_heap_offset: u32,
    pub cluster_count: u32,
    pub volume_serial: u32,
    /// The First Allocation Bitmap, then the Second on a two-FAT volume.
    pub bitmaps: Vec<Extent>,
    pub upcase: Extent,
    pub root: Extent,
    /// The label's UTF-16 code units, when a Volume Label entry is written.
    pub label: Option<Vec<u16>>,
    pub guid: Option<[u8; 16]>,
    /// PercentInUse: the system streams' share of the heap, rounded down.
    pub percent_in_use: u8,
    /// The device held a valid exFAT boot region whose OEM Parameters this
    /// format kept, as section 3.3 requires. Always false from [`plan`].
    pub oem_parameters_preserved: bool,
}

impl Layout {
    pub fn bytes_per_sector(&self) -> u64 {
        1 << self.bytes_per_sector_shift
    }

    pub fn cluster_size(&self) -> u64 {
        self.bytes_per_sector() << self.sectors_per_cluster_shift
    }

    /// Volume bytes this layout covers.
    pub fn volume_bytes(&self) -> u64 {
        self.volume_length * self.bytes_per_sector()
    }

    /// Clusters the system streams take, all allocated from cluster 2 on.
    pub fn allocated_clusters(&self) -> u32 {
        self.root.end() - FIRST_CLUSTER
    }

    fn cluster_offset(&self, cluster: u32) -> u64 {
        u64::from(self.cluster_heap_offset) * self.bytes_per_sector()
            + u64::from(cluster - FIRST_CLUSTER) * self.cluster_size()
    }
}

/// Why a format did not happen, or did not finish.
#[derive(Debug)]
#[non_exhaustive]
pub enum FormatError {
    /// A layout the specification or the device does not allow. Nothing was
    /// written.
    Refused(String),
    /// The device failed a read, write or flush.
    Device(fs_core::Error),
}

impl fmt::Display for FormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(reason) => write!(formatter, "exFAT format refused: {reason}"),
            Self::Device(error) => write!(formatter, "exFAT format: device: {error}"),
        }
    }
}

impl std::error::Error for FormatError {}

impl From<fs_core::Error> for FormatError {
    fn from(error: fs_core::Error) -> Self {
        Self::Device(error)
    }
}

pub type Result<T> = std::result::Result<T, FormatError>;

fn invalid(reason: impl Into<String>) -> FormatError {
    FormatError::Refused(reason.into())
}

/// The cluster size [`format`] uses for a volume of `volume_bytes` when none
/// is given: 4 KiB below 256 MiB, 32 KiB below 32 GiB and 128 KiB from there,
/// the defaults Microsoft documents for exFAT under Windows, then doubled
/// while the volume holds more than [`RECOMMENDED_MAX_CLUSTERS`] clusters'
/// worth of bytes (section 3.1.9), up to [`MAX_CLUSTER_SIZE`]. Never smaller
/// than the sector.
pub fn default_cluster_size(volume_bytes: u64, bytes_per_sector: u32) -> u32 {
    const MIB: u64 = 1 << 20;
    let mut size: u32 = if volume_bytes < 256 * MIB {
        4 << 10
    } else if volume_bytes < 32 << 30 {
        32 << 10
    } else {
        128 << 10
    };
    while size < MAX_CLUSTER_SIZE && volume_bytes / u64::from(size) > RECOMMENDED_MAX_CLUSTERS {
        size *= 2;
    }
    size.max(bytes_per_sector)
}

/// The UTF-16 code units of `label`, checked against sections 7.3.2 and 7.3.3.
fn label_units(label: &str) -> Result<Vec<u16>> {
    let units: Vec<u16> = label.encode_utf16().collect();
    if units.len() > MAX_LABEL_UNITS {
        return Err(invalid(format!(
            "the volume label is {} UTF-16 code units; at most {MAX_LABEL_UNITS} fit",
            units.len()
        )));
    }
    // Table 35: control codes and " * / : < > ? \ |.
    if let Some(unit) = units
        .iter()
        .find(|&&unit| unit < 0x20 || (unit < 0x80 && b"\"*/:<>?\\|".contains(&(unit as u8))))
    {
        return Err(invalid(format!(
            "the volume label holds character {unit:04X}h, which section 7.7.3 forbids"
        )));
    }
    Ok(units)
}

fn round_up(value: u64, multiple: u64) -> Option<u64> {
    value.div_ceil(multiple).checked_mul(multiple)
}

/// The serial number from the current date and time: seconds since 1970,
/// combined with the second's fraction (section 3.1.11 leaves how to
/// combine them to the implementation).
fn serial_from_clock() -> u32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (now.as_secs() as u32) ^ now.subsec_nanos().rotate_left(16)
}

/// Decides the layout for a volume of `volume_bytes` without touching a
/// device. [`format`] writes exactly this, apart from OEM parameters it keeps.
pub fn plan(volume_bytes: u64, options: &FormatOptions) -> Result<Layout> {
    let bytes_per_sector = options.bytes_per_sector.unwrap_or(DEFAULT_BYTES_PER_SECTOR);
    if !matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096) {
        return Err(invalid(format!(
            "{bytes_per_sector} bytes per sector; section 3.1.14 allows 512, 1024, 2048 or 4096"
        )));
    }
    let sector = u64::from(bytes_per_sector);
    let volume_length = volume_bytes / sector;
    if volume_length * sector < MIN_VOLUME_BYTES {
        return Err(invalid(format!(
            "{volume_bytes} bytes is smaller than the 1 MiB section 3.1.5 requires"
        )));
    }
    let cluster_size = options
        .cluster_size
        .unwrap_or_else(|| default_cluster_size(volume_length * sector, bytes_per_sector));
    if !cluster_size.is_power_of_two()
        || cluster_size < bytes_per_sector
        || cluster_size > MAX_CLUSTER_SIZE
    {
        return Err(invalid(format!(
            "a {cluster_size}-byte cluster; it must be a power of two from the sector size \
             to 32 MiB (section 3.1.15)"
        )));
    }
    let sectors_per_cluster = u64::from(cluster_size / bytes_per_sector);
    let alignment = options.alignment.unwrap_or(u64::from(cluster_size));
    if alignment == 0 || !alignment.is_multiple_of(sector) {
        return Err(invalid(format!(
            "a {alignment}-byte alignment; it must be a non-zero multiple of the sector size"
        )));
    }
    let alignment = alignment / sector;
    let number_of_fats = options.number_of_fats;
    if !matches!(number_of_fats, 1 | 2) {
        return Err(invalid(format!(
            "{number_of_fats} FATs; section 3.1.16 allows 1, or 2 on a TexFAT volume"
        )));
    }
    let label = options.label.as_deref().map(label_units).transpose()?;
    let too_small = || invalid(format!("{volume_bytes} bytes is too small for this layout"));
    let too_large =
        || invalid("the FAT region does not fit the 32-bit sector offsets of section 3.1");

    // Section 3.1.6: the First FAT follows both boot regions. Each FAT is
    // rounded up to the alignment, as section 3.1.7 allows, so the Second FAT
    // and the cluster heap after them stay aligned.
    let fat_offset = round_up(FIRST_FAT_SECTOR, alignment).ok_or_else(too_large)?;
    let fat_sectors =
        |clusters: u64| round_up((clusters + 2) * 4, sector).map(|bytes| bytes / sector);
    let fat_length_for =
        |clusters: u64| fat_sectors(clusters).and_then(|length| round_up(length, alignment));
    // A FAT long enough for every cluster the volume could hold without one
    // places the heap; the heap then holds fewer clusters, whose FAT is no
    // longer than this one (section 3.1.9).
    let upper_bound = (volume_length.saturating_sub(fat_offset) / sectors_per_cluster)
        .min(u64::from(MAX_CLUSTER_COUNT));
    let heap = fat_length_for(upper_bound)
        .and_then(|length| length.checked_mul(u64::from(number_of_fats)))
        .and_then(|length| length.checked_add(fat_offset))
        .ok_or_else(too_large)?;
    if heap >= volume_length {
        return Err(too_small());
    }
    let cluster_count =
        ((volume_length - heap) / sectors_per_cluster).min(u64::from(MAX_CLUSTER_COUNT));
    let fat_length = fat_length_for(cluster_count).ok_or_else(too_large)?;
    let fat_offset = u32::try_from(fat_offset).map_err(|_| too_large())?;
    let fat_length = u32::try_from(fat_length).map_err(|_| too_large())?;
    let cluster_heap_offset = u32::try_from(heap).map_err(|_| too_large())?;
    let cluster_count = cluster_count as u32;

    let cluster_bytes = u64::from(cluster_size);
    let clusters_for = |bytes: u64| bytes.div_ceil(cluster_bytes) as u32;
    let mut next = FIRST_CLUSTER;
    let mut extent = |length: u64| {
        let extent = Extent {
            first_cluster: next,
            clusters: clusters_for(length).max(1),
            length,
        };
        next = next.saturating_add(extent.clusters);
        extent
    };
    let bitmap_length = u64::from(cluster_count).div_ceil(8);
    let bitmaps: Vec<Extent> = (0..number_of_fats).map(|_| extent(bitmap_length)).collect();
    let upcase = extent(RECOMMENDED_UPCASE.len() as u64 * 2);
    let root = extent(cluster_bytes);
    if u64::from(root.end() - FIRST_CLUSTER) > u64::from(cluster_count) {
        return Err(too_small());
    }
    let allocated = u64::from(root.end() - FIRST_CLUSTER);
    Ok(Layout {
        bytes_per_sector_shift: bytes_per_sector.trailing_zeros() as u8,
        sectors_per_cluster_shift: sectors_per_cluster.trailing_zeros() as u8,
        partition_offset: options.partition_offset,
        volume_length,
        fat_offset,
        fat_length,
        number_of_fats,
        cluster_heap_offset,
        cluster_count,
        volume_serial: options.volume_serial.unwrap_or_else(serial_from_clock),
        bitmaps,
        upcase,
        root,
        label,
        guid: options.guid,
        percent_in_use: (allocated * 100 / u64::from(cluster_count)) as u8,
        oem_parameters_preserved: false,
    })
}

fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// The recommended up-case table as it is stored: Table 25's words, little
/// endian.
pub fn recommended_upcase_bytes() -> Vec<u8> {
    RECOMMENDED_UPCASE
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect()
}

/// One boot region, twelve sectors (sections 3.1 to 3.4), with `oem` as its
/// OEM Parameters.
pub fn boot_region(layout: &Layout, oem: &[u8; OEM_PARAMETERS_BYTES]) -> Vec<u8> {
    let sector = layout.bytes_per_sector() as usize;
    let mut region = vec![0; sector * BOOT_REGION_SECTORS as usize];
    let boot = &mut region[..sector];
    boot[..3].copy_from_slice(&JUMP_BOOT);
    boot[3..11].copy_from_slice(FILE_SYSTEM_NAME);
    put64(boot, 64, layout.partition_offset);
    put64(boot, 72, layout.volume_length);
    put32(boot, 80, layout.fat_offset);
    put32(boot, 84, layout.fat_length);
    put32(boot, 88, layout.cluster_heap_offset);
    put32(boot, 92, layout.cluster_count);
    put32(boot, 96, layout.root.first_cluster);
    put32(boot, 100, layout.volume_serial);
    put16(boot, 104, REVISION);
    // VolumeFlags: First FAT active, clean, no media failure (section 3.1.13).
    put16(boot, 106, 0);
    boot[108] = layout.bytes_per_sector_shift;
    boot[109] = layout.sectors_per_cluster_shift;
    boot[110] = layout.number_of_fats;
    boot[111] = DRIVE_SELECT;
    boot[112] = layout.percent_in_use;
    boot[120..510].fill(BOOT_CODE_FILL);
    boot[510] = 0x55;
    boot[511] = 0xAA;
    for index in 1..=8 {
        put32(
            &mut region,
            (index + 1) * sector - 4,
            EXTENDED_BOOT_SIGNATURE,
        );
    }
    region[9 * sector..9 * sector + OEM_PARAMETERS_BYTES].copy_from_slice(oem);
    let checksum = boot_checksum(&region, sector);
    for word in region[11 * sector..].chunks_exact_mut(4) {
        word.copy_from_slice(&checksum.to_le_bytes());
    }
    region
}

/// The root directory's entries, in the order written: Volume Label, the
/// Allocation Bitmaps, the Up-case Table, then the Volume GUID.
pub fn root_entries(layout: &Layout) -> Vec<u8> {
    let mut entries = Vec::new();
    let mut entry = |fill: &dyn Fn(&mut [u8])| {
        let mut bytes = [0; ENTRY_BYTES];
        fill(&mut bytes);
        entries.extend_from_slice(&bytes);
    };
    if let Some(units) = &layout.label {
        entry(&|bytes| {
            bytes[0] = LABEL_ENTRY;
            bytes[1] = units.len() as u8;
            for (index, unit) in units.iter().enumerate() {
                put16(bytes, 2 + index * 2, *unit);
            }
        });
    }
    for (identifier, bitmap) in layout.bitmaps.iter().enumerate() {
        entry(&|bytes| {
            bytes[0] = BITMAP_ENTRY;
            // BitmapFlags: BitmapIdentifier (section 7.1.2.1).
            bytes[1] = identifier as u8;
            put32(bytes, 20, bitmap.first_cluster);
            put64(bytes, 24, bitmap.length);
        });
    }
    entry(&|bytes| {
        bytes[0] = UPCASE_ENTRY;
        put32(bytes, 4, RECOMMENDED_UPCASE_CHECKSUM);
        put32(bytes, 20, layout.upcase.first_cluster);
        put64(bytes, 24, layout.upcase.length);
    });
    if let Some(guid) = layout.guid {
        entry(&|bytes| {
            bytes[0] = GUID_ENTRY;
            bytes[6..22].copy_from_slice(&guid);
            put16(bytes, 2, entry_set_checksum(bytes));
        });
    }
    entries
}

/// The start of a FAT: FatEntry[0] and [1], then each system stream's chain.
/// Every later entry is zero.
pub fn fat_head(layout: &Layout) -> Vec<u8> {
    let mut entries = vec![MEDIA_FAT_ENTRY, FAT_END];
    for extent in layout.bitmaps.iter().chain([&layout.upcase, &layout.root]) {
        for cluster in extent.first_cluster..extent.end() {
            entries.push(if cluster + 1 == extent.end() {
                FAT_END
            } else {
                cluster + 1
            });
        }
    }
    entries
        .iter()
        .flat_map(|entry| entry.to_le_bytes())
        .collect()
}

/// The start of an Allocation Bitmap: a set bit for each allocated cluster,
/// all at the start of the heap. Every later byte is zero.
pub fn bitmap_head(layout: &Layout) -> Vec<u8> {
    let allocated = layout.allocated_clusters() as usize;
    let mut bytes = vec![0xFF; allocated / 8];
    if !allocated.is_multiple_of(8) {
        bytes.push((1u8 << (allocated % 8)) - 1);
    }
    bytes
}

/// Writes `head` at `offset`, then zeros to `length` bytes, in chunks of at
/// most [`WRITE_CHUNK`].
fn write_region(device: &dyn BlockDevice, offset: u64, length: u64, head: &[u8]) -> Result<()> {
    debug_assert!(head.len() as u64 <= length);
    let chunk = WRITE_CHUNK.min(length) as usize;
    let mut buffer = vec![0; chunk];
    let mut position = 0;
    while position < length {
        let count = (length - position).min(chunk as u64) as usize;
        buffer[..count].fill(0);
        if let Some(part) = head.get(position as usize..) {
            let part = &part[..part.len().min(count)];
            buffer[..part.len()].copy_from_slice(part);
        }
        device.write_at(offset + position, &buffer[..count])?;
        position += count as u64;
    }
    Ok(())
}

/// The OEM Parameters of a valid exFAT boot region already on the device,
/// main first, then backup. A device that cannot be read, or holds no valid
/// region, has none, and the format writes Null Parameters (section 3.3.3).
fn existing_oem_parameters(device: &dyn BlockDevice) -> Option<[u8; OEM_PARAMETERS_BYTES]> {
    let mut boot = [0; 512];
    device.read_at(0, &mut boot).ok()?;
    let shift = boot[108];
    if &boot[3..11] != FILE_SYSTEM_NAME || !(9..=12).contains(&shift) {
        return None;
    }
    let sector = 1usize << shift;
    let length = sector * BOOT_REGION_SECTORS as usize;
    for copy in 0..2u64 {
        let mut region = vec![0; length];
        if device.read_at(copy * length as u64, &mut region).is_err() {
            continue;
        }
        let Ok(parsed) = crate::primitives::BootRegion::parse(&region, sector) else {
            continue;
        };
        if parsed.checksum_valid
            && parsed.header.file_system_name_valid
            && usize::from(parsed.header.sector.bytes_per_sector_shift) == shift as usize
        {
            let mut oem = [0; OEM_PARAMETERS_BYTES];
            oem.copy_from_slice(&region[9 * sector..9 * sector + OEM_PARAMETERS_BYTES]);
            return Some(oem);
        }
    }
    None
}

/// Formats `device` as an empty exFAT volume and returns what was written.
///
/// The device must be writable and at least as large as
/// [`FormatOptions::size_bytes`]. OEM Parameters of an exFAT volume already on
/// the device are kept, as section 3.3 requires of a later format. Only the
/// boot regions, the FATs and the system streams' clusters are written; the
/// rest of the heap keeps whatever it held, as nothing allocates it.
pub fn format(device: &dyn BlockDevice, options: &FormatOptions) -> Result<Layout> {
    if !device.is_writable() {
        return Err(invalid("the device is not writable"));
    }
    let available = device.size_bytes();
    let volume_bytes = match options.size_bytes {
        Some(size) if size > available => {
            return Err(invalid(format!(
                "{size} bytes asked for; the device holds {available}"
            )))
        }
        Some(size) => size,
        None => available,
    };
    let mut layout = plan(volume_bytes, options)?;
    let oem = existing_oem_parameters(device);
    layout.oem_parameters_preserved = oem.is_some();
    let region = boot_region(&layout, &oem.unwrap_or([0; OEM_PARAMETERS_BYTES]));
    let sector = layout.bytes_per_sector();
    let cluster = layout.cluster_size();

    let fat = fat_head(&layout);
    let fat_bytes = u64::from(layout.fat_length) * sector;
    for copy in 0..u64::from(layout.number_of_fats) {
        let offset = (u64::from(layout.fat_offset) + copy * u64::from(layout.fat_length)) * sector;
        write_region(device, offset, fat_bytes, &fat)?;
    }
    let bitmap = bitmap_head(&layout);
    for extent in &layout.bitmaps {
        let length = u64::from(extent.clusters) * cluster;
        write_region(
            device,
            layout.cluster_offset(extent.first_cluster),
            length,
            &bitmap,
        )?;
    }
    let upcase = &layout.upcase;
    write_region(
        device,
        layout.cluster_offset(upcase.first_cluster),
        u64::from(upcase.clusters) * cluster,
        &recommended_upcase_bytes(),
    )?;
    write_region(
        device,
        layout.cluster_offset(layout.root.first_cluster),
        cluster,
        &root_entries(&layout),
    )?;
    let region_bytes = region.len() as u64;
    device.write_at(region_bytes, &region)?;
    device.write_at(sector, &region[sector as usize..])?;
    device.flush()?;
    device.write_at(0, &region[..sector as usize])?;
    device.flush()?;
    Ok(layout)
}
