//! rust-fs-core's three API layers over this crate: [`ExfatVolume`]
//! implements `VolumeAPI` and `OpenVolume`, `PrimitiveAPI` and `FileAPI`,
//! and [`ExfatSalvage`] is its `SalvageAdapter`.
//!
//! Nothing here decodes the format. Every layer maps onto the crate's own
//! APIs: the boot regions ([`BootRegions`]) for the volume layer, the salvage
//! adapter for the salvage verbs, and the strict [`Volume`] for files. The
//! exFAT primitives themselves -- boot regions, FAT entries, the allocation
//! bitmap, entry sets -- stay this crate's own, in [`crate::primitives`], as
//! `docs/three-layers.md` in rust-fs-core requires.
//!
//! ```
//! # #[cfg(feature = "test-support")] {
//! use fs_core::layers::{FileAPI, OpenVolume, VolumeAPI};
//! use fs_core::BlockRead;
//! use fs_exfat::layers::ExfatVolume;
//! use std::sync::Arc;
//!
//! # fn list(source: Arc<dyn BlockRead>) -> Result<(), fs_exfat::layers::LayerError> {
//! let volume = ExfatVolume::open(source)?;
//! for entry in volume.read_dir(&volume.root()?)? {
//!     println!("{} ({} bytes)", entry.name, entry.size);
//! }
//! # Ok(())
//! # }
//! # }
//! ```
//!
//! exFAT records local times with an optional offset from UTC. A time that
//! carries its offset converts exactly; one without is read as UTC, as the
//! shared `Timestamp` has no local form.

use crate::boot::{BootCopy, BootRegions};
use crate::device::{AlignedDevice, Reader};
use crate::directory::DirectoryEntry;
use crate::path::Node;
use crate::primitives::{BootSector, BootSectorHeader};
use crate::salvage::ExfatSalvage;
use crate::timestamp::Timestamp as ExfatTimestamp;
use crate::volume::Volume;
use fs_core::layers::file::{DirEntry, FileAPI, Metadata, NodeKind, Timestamp};
use fs_core::layers::primitive::{PrimitiveAPI, Structure, StructureRole};
use fs_core::layers::salvage::{
    self as shared, Damage, DamageKind, Extent, Integrity, SalvageError, Severity,
};
use fs_core::layers::volume::{
    check_bounds, ApiError, Geometry, OpenVolume, Probe, Recognition, SuperblockCopy, VolumeAPI,
    VolumeHealth, VolumeSupport,
};
use fs_core::BlockRead;
use std::fmt;
use std::sync::Arc;

/// The format name the layers report.
pub const FORMAT: &str = "exfat";

/// Sectors in each boot region: the boot sector, eight extended boot
/// sectors, the OEM parameters, a reserved sector and the checksum sector.
const BOOT_REGION_SECTORS: u64 = 12;

/// The device every layer reads: aligned, so a raw device with larger
/// logical sectors serves the strict reader's smaller reads.
type Source = Arc<dyn BlockRead>;

/// What the shared layers return when they fail.
#[derive(Debug)]
pub enum LayerError {
    /// A shared refusal: bounds, not found, not a directory.
    Api(ApiError),
    /// The device failed.
    Device(fs_core::Error),
    /// The exFAT reader refused.
    Exfat(crate::Error),
}

impl fmt::Display for LayerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api(error) => error.fmt(formatter),
            Self::Device(error) => write!(formatter, "device: {error}"),
            Self::Exfat(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LayerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Api(error) => Some(error),
            Self::Device(error) => Some(error),
            Self::Exfat(error) => Some(error),
        }
    }
}

impl From<ApiError> for LayerError {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl From<fs_core::Error> for LayerError {
    fn from(error: fs_core::Error) -> Self {
        Self::Device(error)
    }
}

impl From<crate::Error> for LayerError {
    fn from(error: crate::Error) -> Self {
        match error {
            crate::Error::Device(error) => Self::Device(error),
            error => Self::Exfat(error),
        }
    }
}

/// An exFAT volume on the shared layers.
///
/// Opened with [`OpenVolume::open`] it is ready for files and refuses a
/// volume the strict reader refuses. Opened with
/// [`OpenVolume::open_for_salvage`] it needs only a usable geometry from
/// either boot region, and offers files only when the strict reader accepts
/// the volume.
pub struct ExfatVolume {
    source: Source,
    adapter: ExfatSalvage,
    regions: Result<BootRegions, String>,
    files: Result<Volume<Source>, crate::Error>,
}

impl ExfatVolume {
    /// The strict reader underneath, when it accepted the volume.
    pub fn strict(&self) -> Option<&Volume<Source>> {
        self.files.as_ref().ok()
    }

    /// The salvage adapter, with whatever geometry survived.
    pub fn salvage_adapter(&self) -> &ExfatSalvage {
        &self.adapter
    }

    fn files(&self) -> Result<&Volume<Source>, LayerError> {
        self.files.as_ref().map_err(|_| {
            LayerError::Api(ApiError::Unsupported(
                "files: the strict reader refuses this volume",
            ))
        })
    }

    /// The boot fields in use: the salvage adapter's, from the first boot
    /// region whose checksum holds, else the first that names exFAT.
    fn boot(&self) -> Option<&BootSector> {
        self.adapter.boot()
    }

    fn sector_size(&self) -> u64 {
        self.boot().map_or(512, BootSector::bytes_per_sector)
    }

    fn assemble(source: Source) -> Self {
        let source: Source = Arc::new(AlignedDevice::new(source));
        let adapter = ExfatSalvage::probe(&*source);
        let regions = BootRegions::read(&Reader::new(source.clone())).map_err(|e| e.to_string());
        let files = Volume::open(source.clone());
        Self {
            source,
            adapter,
            regions,
            files,
        }
    }
}

fn integrity(copy: &BootCopy) -> Integrity {
    if copy.valid() {
        Integrity::Verified
    } else {
        Integrity::Failed
    }
}

/// Why a boot copy failed, as damage at `location`.
fn copy_damage(copy: &BootCopy, location: Extent, name: &str, severity: Severity) -> Damage {
    let (kind, detail) = if let Err(error) = &copy.sector {
        (DamageKind::Malformed, error.to_string())
    } else if !copy.checksum_valid {
        (
            DamageKind::Checksum,
            format!(
                "boot checksum {:#010x}, computed {:#010x}",
                copy.stored_checksum, copy.computed_checksum
            ),
        )
    } else if !copy.extended_signatures_valid {
        (
            DamageKind::Signature,
            "an extended boot signature is not AA550000h".to_owned(),
        )
    } else {
        (
            DamageKind::Malformed,
            "the sector size or volume length does not fit the device".to_owned(),
        )
    };
    Damage {
        kind,
        severity,
        location,
        structure: name.to_owned(),
        detail,
    }
}

impl VolumeAPI for ExfatVolume {
    type Error = LayerError;
    /// The boot sector's fields, decoded permissively, so a damaged copy
    /// still shows what it holds.
    type Superblock = BootSector;

    fn format(&self) -> &'static str {
        FORMAT
    }

    fn size_bytes(&self) -> u64 {
        self.source.size_bytes()
    }

    fn read_volume_bytes(&self, offset: u64, output: &mut [u8]) -> Result<(), LayerError> {
        check_bounds(offset, output.len(), self.size_bytes())?;
        self.source.read_at(offset, output)?;
        Ok(())
    }

    fn geometry(&self) -> Result<Geometry, LayerError> {
        let boot = self.boot().ok_or(LayerError::Exfat(crate::Error::Invalid(
            "neither boot region gives a usable geometry",
        )))?;
        let serial = boot.volume_serial;
        Ok(Geometry {
            sector_size: boot.bytes_per_sector() as u32,
            allocation_unit: boot.cluster_size(),
            allocation_units: u64::from(boot.cluster_count),
            declared_bytes: boot.volume_length.saturating_mul(boot.bytes_per_sector()),
            layout: shared::SalvageAdapter::cluster_layout(&self.adapter).ok(),
            label: self
                .strict()
                .and_then(|volume| volume.metadata().label.clone()),
            serial: Some(format!("{:04X}-{:04X}", serial >> 16, serial & 0xffff)),
        })
    }

    fn superblock(&self) -> Result<BootSector, LayerError> {
        self.boot()
            .cloned()
            .ok_or(LayerError::Exfat(crate::Error::Invalid(
                "neither boot region gives a usable geometry",
            )))
    }

    /// The main boot region, then the backup, each twelve sectors. Each
    /// copy's boot sector is decoded when it names exFAT; its integrity is
    /// the whole region's: checksum, signatures and fields.
    fn superblocks(&self) -> Result<Vec<SuperblockCopy<BootSector>>, LayerError> {
        let region = self.sector_size() * BOOT_REGION_SECTORS;
        let checked = |index: u32| match &self.regions {
            Ok(regions) if index == 0 => integrity(&regions.main),
            Ok(regions) => integrity(&regions.backup),
            Err(_) => Integrity::Failed,
        };
        Ok((0u32..2)
            .map(|index| {
                let offset = u64::from(index) * region;
                let mut sector = [0u8; 512];
                let superblock = self
                    .read_volume_bytes(offset, &mut sector)
                    .ok()
                    .and_then(|()| BootSectorHeader::parse(&sector).ok())
                    .filter(|header| header.file_system_name_valid)
                    .map(|header| header.sector);
                SuperblockCopy {
                    copy: index,
                    location: Extent::new(offset, region),
                    superblock,
                    integrity: checked(index),
                }
            })
            .collect())
    }

    fn support(&self) -> Result<VolumeSupport, LayerError> {
        Ok(VolumeSupport {
            files: self.files.is_ok(),
            salvage: shared::SalvageAdapter::capabilities(&self.adapter),
            unsupported: Vec::new(),
        })
    }

    /// The boot regions -- each copy's checksum, signatures and fields, and
    /// whether they agree -- the VolumeDirty flag, and whether the strict
    /// reader accepts the volume. The allocation audit and the entry-set
    /// scan are the salvage adapter's `report_damage`, not this verdict.
    fn health(&self) -> Result<VolumeHealth, LayerError> {
        let region = self.sector_size() * BOOT_REGION_SECTORS;
        let mut findings = Vec::new();
        match &self.regions {
            Ok(regions) => {
                if !regions.main.valid() {
                    findings.push(copy_damage(
                        &regions.main,
                        Extent::new(0, region),
                        "main boot region",
                        Severity::Error,
                    ));
                }
                if !regions.backup.valid() {
                    findings.push(copy_damage(
                        &regions.backup,
                        Extent::new(region, region),
                        "backup boot region",
                        Severity::Warning,
                    ));
                }
                let fixed: Vec<_> = regions
                    .differences
                    .iter()
                    .filter(|difference| !difference.is_mutable())
                    .collect();
                if let Some(first) = fixed.first() {
                    findings.push(Damage {
                        kind: DamageKind::Mismatch,
                        severity: Severity::Warning,
                        location: Extent::new(region + first.offset as u64, 1),
                        structure: "backup boot region".to_owned(),
                        detail: format!(
                            "{} byte(s) differ from the main boot region, first at {}",
                            fixed.len(),
                            first.offset
                        ),
                    });
                }
            }
            Err(error) => findings.push(Damage {
                kind: DamageKind::Malformed,
                severity: Severity::Error,
                location: Extent::new(0, 2 * region),
                structure: "boot regions".to_owned(),
                detail: error.clone(),
            }),
        }
        if let Some(boot) = self.boot() {
            if boot.volume_dirty() {
                findings.push(Damage {
                    kind: DamageKind::Mismatch,
                    severity: Severity::Notice,
                    location: Extent::new(106, 2),
                    structure: "main boot sector".to_owned(),
                    detail: "VolumeDirty is set: the volume was not cleanly unmounted".to_owned(),
                });
            }
        }
        if let Err(error) = &self.files {
            findings.push(Damage {
                kind: DamageKind::Broken,
                severity: Severity::Error,
                location: Extent::new(0, self.size_bytes()),
                structure: "volume".to_owned(),
                detail: format!("the strict reader refuses the volume: {error}"),
            });
        }
        Ok(VolumeHealth::from_findings(findings))
    }
}

impl OpenVolume for ExfatVolume {
    type Source = Source;

    /// Reads the main boot sector, then the backup at each sector size the
    /// format allows. `EXFAT   ` in either with a valid checksum is a match;
    /// the name alone, or only in the backup, is damaged.
    fn probe(source: &dyn BlockRead) -> Result<Probe, fs_core::Error> {
        let no = |detail: &str| Probe {
            format: FORMAT,
            recognition: Recognition::No,
            declared_bytes: None,
            detail: detail.to_owned(),
        };
        if source.size_bytes() < 512 {
            return Ok(no("shorter than a boot sector"));
        }
        let mut sector = [0u8; 512];
        source.read_at(0, &mut sector)?;
        let named = |bytes: &[u8]| bytes.get(3..11) == Some(b"EXFAT   ");
        let adapter = ExfatSalvage::probe(source);
        let declared = adapter
            .boot()
            .map(|boot| boot.volume_length.saturating_mul(boot.bytes_per_sector()));
        let regions = BootRegions::read(&Reader::new(source));
        let (recognition, detail) = match &regions {
            Ok(regions) if regions.main.valid() && regions.backup.valid() => (
                Recognition::Yes,
                "exFAT, both boot regions valid".to_owned(),
            ),
            Ok(regions) if regions.main.valid() || regions.backup.valid() => (
                Recognition::Damaged,
                format!(
                    "exFAT, {} boot region valid only",
                    if regions.main.valid() {
                        "the main"
                    } else {
                        "the backup"
                    }
                ),
            ),
            _ if named(&sector) || adapter.boot().is_some() => (
                Recognition::Damaged,
                "the exFAT name or a usable geometry, with no valid boot region".to_owned(),
            ),
            _ => return Ok(no("no exFAT boot sector")),
        };
        Ok(Probe {
            format: FORMAT,
            recognition,
            declared_bytes: declared,
            detail,
        })
    }

    fn open(source: Source) -> Result<Self, LayerError> {
        let Self {
            source,
            adapter,
            regions,
            files,
        } = Self::assemble(source);
        let files = files?;
        Ok(Self {
            source,
            adapter,
            regions,
            files: Ok(files),
        })
    }

    /// Needs a usable geometry from either boot region, nothing more.
    fn open_for_salvage(source: Source) -> Result<Self, LayerError> {
        let volume = Self::assemble(source);
        if volume.adapter.boot().is_none() && volume.files.is_err() {
            return Err(LayerError::Exfat(crate::Error::Invalid(
                "neither boot region gives a usable geometry",
            )));
        }
        Ok(volume)
    }
}

/// Splits clusters into runs of physically adjacent ones.
fn runs(clusters: &[u32]) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &cluster in clusters {
        match runs.last_mut() {
            Some((first, count)) if first.checked_add(*count) == Some(cluster) => *count += 1,
            _ => runs.push((cluster, 1)),
        }
    }
    runs
}

impl ExfatVolume {
    /// A system stream's clusters as structures, one per run.
    fn stream_structures(
        &self,
        volume: &Volume<Source>,
        name: &str,
        role: StructureRole,
        first: u32,
        integrity: Integrity,
        out: &mut Vec<Structure>,
    ) {
        let boot = volume.boot();
        let Ok(chain) = volume.cluster_chain(first) else {
            return;
        };
        let pieces = runs(&chain);
        for (index, (start, count)) in pieces.iter().enumerate() {
            let Ok(offset) = boot.cluster_offset(*start) else {
                continue;
            };
            out.push(Structure {
                name: if pieces.len() == 1 {
                    name.to_owned()
                } else {
                    format!("{name}, run {}", index + 1)
                },
                role,
                location: Extent::new(offset, u64::from(*count) * boot.cluster_size()),
                copy: 0,
                integrity,
            });
        }
    }
}

impl PrimitiveAPI for ExfatVolume {
    type Adapter = ExfatSalvage;
    /// exFAT keeps a backup of its boot region, so it never needs a
    /// geometry hypothesis.
    type Hypothesis = ();

    fn source(&self) -> &dyn BlockRead {
        &*self.source
    }

    fn adapter(&self) -> Result<ExfatSalvage, SalvageError> {
        match self.adapter.boot() {
            Some(_) => Ok(self.adapter.clone()),
            None => Err(SalvageError::NoGeometry),
        }
    }

    /// Both boot regions and each FAT, from the geometry in use; with the
    /// strict reader, also the allocation bitmaps, the up-case table and the
    /// root directory, one structure per run of adjacent clusters.
    fn structures(&self) -> Result<Vec<Structure>, SalvageError> {
        let sector = self.sector_size();
        let region = sector * BOOT_REGION_SECTORS;
        let (main, backup) = match &self.regions {
            Ok(regions) => (integrity(&regions.main), integrity(&regions.backup)),
            Err(_) => (Integrity::Failed, Integrity::Failed),
        };
        let mut out = vec![
            Structure {
                name: "main boot region".to_owned(),
                role: StructureRole::Boot,
                location: Extent::new(0, region),
                copy: 0,
                integrity: main,
            },
            Structure {
                name: "backup boot region".to_owned(),
                role: StructureRole::Boot,
                location: Extent::new(region, region),
                copy: 1,
                integrity: backup,
            },
        ];
        let Some(boot) = self.boot() else {
            return Ok(out);
        };
        let fat_bytes = u64::from(boot.fat_length) * sector;
        for index in 0..u64::from(boot.number_of_fats) {
            out.push(Structure {
                name: format!("FAT {}", index + 1),
                role: StructureRole::AllocationMap,
                location: Extent::new(
                    u64::from(boot.fat_offset) * sector + index * fat_bytes,
                    fat_bytes,
                ),
                copy: index as u32,
                integrity: Integrity::Unchecked,
            });
        }
        if let Some(volume) = self.strict() {
            let metadata = volume.metadata();
            for (index, bitmap) in metadata.bitmaps.iter().enumerate() {
                if let Some(bitmap) = bitmap {
                    self.stream_structures(
                        volume,
                        &format!("allocation bitmap {}", index + 1),
                        StructureRole::AllocationMap,
                        bitmap.first_cluster,
                        Integrity::Unchecked,
                        &mut out,
                    );
                }
            }
            self.stream_structures(
                volume,
                "up-case table",
                StructureRole::Other,
                metadata.upcase.first_cluster,
                Integrity::Verified,
                &mut out,
            );
            self.stream_structures(
                volume,
                "root directory",
                StructureRole::Directory,
                volume.boot().root_cluster,
                Integrity::Unchecked,
                &mut out,
            );
        }
        Ok(out)
    }
}

fn timestamp(time: &ExfatTimestamp) -> Option<Timestamp> {
    let mut time = *time;
    time.utc_offset_minutes.get_or_insert(0);
    let milliseconds = time.unix_milliseconds()?;
    Some(Timestamp {
        seconds: milliseconds.div_euclid(1000),
        nanoseconds: (milliseconds.rem_euclid(1000) * 1_000_000) as u32,
    })
}

fn kind(entry: &DirectoryEntry) -> NodeKind {
    if entry.is_directory() {
        NodeKind::Directory
    } else {
        NodeKind::File
    }
}

fn metadata_of(entry: &DirectoryEntry, cluster: u64) -> Metadata {
    let length = entry.stream().data_length();
    let mut metadata = Metadata::new(kind(entry), length);
    metadata.allocated = Some(length.div_ceil(cluster).saturating_mul(cluster));
    let times = entry.timestamps();
    metadata.created = timestamp(&times.created);
    metadata.modified = timestamp(&times.modified);
    metadata.accessed = timestamp(&times.accessed);
    metadata.attributes = Some(u64::from(entry.attributes()));
    metadata
}

impl FileAPI for ExfatVolume {
    /// The root, or a directory entry: the strict reader's own path node.
    type Node = Node;

    fn root(&self) -> Result<Node, LayerError> {
        self.files()?;
        Ok(Node::Root)
    }

    fn read_dir(&self, directory: &Node) -> Result<Vec<DirEntry<Node>>, LayerError> {
        let volume = self.files()?;
        let entries = match directory {
            Node::Root => volume.root_directory()?,
            Node::Entry(entry) if entry.is_directory() => volume.read_directory(entry)?,
            Node::Entry(entry) => {
                return Err(ApiError::NotADirectory(entry.name().to_owned()).into())
            }
        };
        Ok(entries
            .into_iter()
            .map(|entry| DirEntry {
                name: entry.name().to_owned(),
                bytes: entry.name().as_bytes().to_vec(),
                lossy: String::from_utf16(entry.name_utf16()).is_err(),
                kind: kind(&entry),
                size: entry.stream().data_length(),
                node: Node::Entry(entry),
            })
            .collect())
    }

    fn metadata(&self, node: &Node) -> Result<Metadata, LayerError> {
        let volume = self.files()?;
        Ok(match node {
            Node::Root => Metadata::new(NodeKind::Directory, volume.root_bytes().len() as u64),
            Node::Entry(entry) => metadata_of(entry, volume.boot().cluster_size()),
        })
    }

    /// Reads to the smaller of the output and the bytes left, as the shared
    /// contract asks; bytes past ValidDataLength read as zeros.
    fn read_at(&self, file: &Node, offset: u64, output: &mut [u8]) -> Result<usize, LayerError> {
        let volume = self.files()?;
        let entry = match file {
            Node::Root => return Err(ApiError::IsADirectory("/".to_owned()).into()),
            Node::Entry(entry) if entry.is_directory() => {
                return Err(ApiError::IsADirectory(entry.name().to_owned()).into())
            }
            Node::Entry(entry) => entry,
        };
        let file = volume.open_file(entry)?;
        let mut done = 0;
        while done < output.len() {
            let read = file.read_at(offset + done as u64, &mut output[done..])?;
            if read == 0 {
                break;
            }
            done += read;
        }
        Ok(done)
    }

    /// exFAT names compare through the volume's up-case table.
    fn names_match(&self, stored: &str, wanted: &str) -> bool {
        let stored: Vec<u16> = stored.encode_utf16().collect();
        let wanted: Vec<u16> = wanted.encode_utf16().collect();
        match self.strict() {
            Some(volume) => volume.upcase().equal(&stored, &wanted),
            None => stored == wanted,
        }
    }
}
