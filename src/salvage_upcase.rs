//! An exFAT extension outside the common trait: the volume's Up-case Table,
//! decoded for salvage name lookups (#42).
//!
//! exFAT names are case-insensitive through the volume's own table, not a
//! host's case rules (Microsoft's exFAT specification, section 7.2 "Up-case
//! Table Directory Entry"). Section 7.2.5 "Up-case Table" stores that table
//! either uncompressed or compressed, with identity runs, and makes reading
//! both mandatory; [`primitives::RawUpcaseTable`] decodes either. A salvage
//! lookup compares a candidate's name with this report rather than with a
//! built-in table, so a volume that maps a character differently matches as
//! the volume would. This listing is evidence, not damage: damage in the
//! table is [`SalvageAdapter::report_damage`](super::SalvageAdapter)'s.

use super::stale::{merged, read_tolerant};
use super::{ExfatSalvage, Mapper, DIRECTORY_LIMIT, UPCASE_LIMIT};
use crate::primitives::{self, RawRootMetadata, RawUpcaseTable, RootEntryKind};
use crate::salvage::adapter::{Extent, SalvageError};
use fs_core::BlockRead;
use serde::{Deserialize, Serialize};

/// The root directory's Up-case Table as salvage reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpcaseReport {
    /// The first in-use Up-case Table entry in the root, or `None` when the
    /// part of the root that could be read holds none.
    pub entry: Option<Extent>,
    /// The table's bytes on the source, in chain order, as far as its chain
    /// reaches.
    pub extents: Vec<Extent>,
    /// The table uses the compressed form of section 7.2.5.
    pub compressed: bool,
    /// Every byte was read and the table passed its own checks: length,
    /// TableChecksum, encoding, coverage of 0000h to FFFFh and Table 24.
    /// Lookups use its mappings only then.
    pub usable: bool,
    /// Table bytes that could not be read; zeros stood in for them.
    pub unreadable: Vec<Extent>,
    /// All 65536 mappings when `usable`, otherwise none.
    pub mappings: Vec<u16>,
}

impl UpcaseReport {
    /// A UTF-16 unit up-cased: by the volume's table when it is usable,
    /// otherwise by Table 24 alone, the partial up-casing section 7.2.5 allows
    /// an implementation that supports only those 128 characters.
    pub fn upcase(&self, unit: u16) -> u16 {
        self.mappings
            .get(usize::from(unit))
            .copied()
            .unwrap_or_else(|| primitives::mandatory_upcase(unit))
    }

    /// Whether two names may be the same name on this volume, as a lookup
    /// decides: they have the same number of UTF-16 units, and each pair of
    /// units up-cases alike. Without a usable table, section 7.2.5 has an
    /// implementation that knows only Table 24 treat names that differ only
    /// in characters outside it as equivalent, so a pair with a unit outside
    /// 0000h to 007Fh matches too.
    pub fn same_name(&self, first: &str, second: &str) -> bool {
        let first: Vec<u16> = first.encode_utf16().collect();
        let second: Vec<u16> = second.encode_utf16().collect();
        let mandatory_only = !self.usable;
        first.len() == second.len()
            && first.iter().zip(&second).all(|(&one, &other)| {
                self.upcase(one) == self.upcase(other)
                    || (mandatory_only && (one >= 0x80 || other >= 0x80))
            })
    }
}

impl ExfatSalvage {
    /// Reads the volume's Up-case Table for name lookups: the first in-use
    /// Up-case Table entry in the root, and the table its chain holds, in
    /// either form section 7.2.5 allows.
    ///
    /// This is an exFAT extension, not a verb of the common trait. Nothing is
    /// rejected: a missing entry, a broken chain, unreadable bytes or a table
    /// that fails its own checks leave [`UpcaseReport::usable`] false, and the
    /// report's lookups then up-case by Table 24 alone. A table longer than
    /// 128 KiB, an uncompressed table's size, is not read.
    ///
    /// Fails only with [`SalvageError::NoGeometry`], when no boot copy gave
    /// geometry.
    pub fn upcase_table(&self, source: &dyn BlockRead) -> Result<UpcaseReport, SalvageError> {
        let geometry = self.geometry()?;
        let mut report = UpcaseReport::default();
        let mut mapper = Mapper::new(geometry, source);
        let root = mapper.chain(
            geometry.boot.root_cluster,
            DIRECTORY_LIMIT / geometry.cluster,
            false,
        );
        let mut root_bytes = Vec::new();
        for extent in geometry.extents(&root.runs, u64::MAX) {
            read_tolerant(source, extent, &mut root_bytes, &mut Vec::new());
        }
        let metadata = RawRootMetadata::parse(&root_bytes);
        let Some((position, stream, checksum)) = metadata
            .entries
            .iter()
            .filter(|entry| entry.in_use)
            .find_map(|entry| match entry.kind {
                RootEntryKind::Upcase { stream, checksum } => {
                    Some((entry.offset, stream, checksum))
                }
                _ => None,
            })
        else {
            return Ok(report);
        };
        let clusters = root.clusters();
        let cluster = clusters[position / geometry.cluster as usize];
        report.entry = Some(Extent::new(
            geometry.cluster_offset(cluster) + position as u64 % geometry.cluster,
            primitives::ENTRY_SIZE as u64,
        ));
        if stream.length == 0 || stream.length > UPCASE_LIMIT {
            return Ok(report);
        }
        let claim = mapper.chain(
            stream.first_cluster,
            stream.length.div_ceil(geometry.cluster),
            false,
        );
        report.extents = geometry.extents(&claim.runs, stream.length);
        let mut bytes = Vec::new();
        let mut unreadable = Vec::new();
        let mut missing = false;
        for extent in &report.extents {
            missing |= !read_tolerant(source, *extent, &mut bytes, &mut unreadable).is_empty();
        }
        report.unreadable = merged(unreadable);
        let table = RawUpcaseTable::parse(&bytes, checksum);
        report.compressed = table.compressed;
        // Zeros stand in for missing bytes, so a table with any is not used.
        report.usable = table.usable() && !missing && bytes.len() as u64 == stream.length;
        if report.usable {
            report.mappings = table.mappings;
        }
        Ok(report)
    }
}
