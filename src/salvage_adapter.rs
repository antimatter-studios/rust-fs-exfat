//! The format-neutral salvage adapter trait and its evidence types.
//!
//! The trait carries the six common verbs of the driver convention
//! (`docs/convention.md`, section 4):
//!
//! | Verb | Method |
//! |---|---|
//! | Find candidates | [`SalvageAdapter::find_candidates`] |
//! | Walk chains or extents | [`SalvageAdapter::walk_extents`] |
//! | Rebuild the tree | [`SalvageAdapter::rebuild_tree`] |
//! | Report damage | [`SalvageAdapter::report_damage`] |
//! | Read by extent | [`SalvageAdapter::read_extent`] |
//! | Materialize from a plan | [`SalvageAdapter::materialize`], or [`SalvageAdapter::plan_reader`] for positioned reads |
//!
//! [`SalvageAdapter::capabilities`] says which of them, and which
//! deletion-specific parts, a driver lacks.
//!
//! # Read-only by type
//!
//! Every method that touches the source takes it as `&dyn BlockRead`, a
//! positioned-read trait with no write path. No method takes a device that can
//! be written, a path, or a handle that could become one. A runtime flag is not
//! the guarantee; the parameter type is.
//!
//! Materialization writes only to a caller-supplied [`io::Write`] sink. No
//! device type in rust-fs-core implements `io::Write`, so a source device
//! cannot be passed as the sink, and the borrow rules refuse one value as both
//! `source` and `sink`. A second handle to the same file cannot be detected by
//! type, so callers must not direct output at the source's own path.
//!
//! # Evidence, not trusted files
//!
//! Results are plain serializable data without handles: [`Candidate`],
//! [`ExtentWalk`], [`Tree`], [`Damage`], [`Plan`] and the reports. Damaged
//! records are returned with their damage flagged, never dropped for it.
//! Large results stream to a caller-provided [`Sink`]. An empty successful
//! result always means nothing was found; a missing capability is reported as
//! [`SalvageError::Unsupported`] naming the [`Feature`].
//!
//! # Vocabulary across drivers
//!
//! For a filesystem, a candidate is a directory record, inode or similar
//! object description, and the tree is the directory hierarchy. For a block
//! device container (qcow2, vhd, vhdx, vmdk), a candidate is a header, table
//! entry or footer that may be valid, the tree is the mapping from virtual
//! offsets to physical ones, and a plan rebuilds the virtual disk from
//! surviving extents. Extents are always byte ranges on the source the adapter
//! was given.
//!
//! # Read-only formats
//!
//! An inherently read-only format, such as squashfs or erofs, lists
//! [`Feature::DeletedCandidates`] and [`Feature::DeletedExtents`] as missing.
//! Its scans then return extant and damaged candidates only, and
//! [`SalvageAdapter::walk_extents`] refuses a deleted candidate with
//! `Unsupported`. The absence is visible in the flags, never implied by an
//! empty scan.
//!
//! The trait lives in this crate until ext4 and NTFS also implement it; only
//! then does it move to rust-fs-core.

use fs_core::BlockRead;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io;
use std::ops::ControlFlow;

#[path = "salvage_plan.rs"]
mod plan;

use plan::Resolved;
pub use plan::{
    ClusterLayout, Plan, PlanCheck, PlanFault, PlanMetadata, PlanOverlap, PlanRead, PlanReader,
    PlanStep,
};

/// Bytes [`SalvageAdapter::materialize`] copies per source read.
const MATERIALIZE_CHUNK: u64 = 64 * 1024;

/// A byte range on the source device.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct Extent {
    pub offset: u64,
    pub length: u64,
}

impl Extent {
    pub const fn new(offset: u64, length: u64) -> Self {
        Self { offset, length }
    }

    /// The first byte past the range, or `None` when it is not representable.
    pub fn end(&self) -> Option<u64> {
        self.offset.checked_add(self.length)
    }
}

/// A salvage operation or deletion-specific part that a driver may lack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Feature {
    FindCandidates,
    WalkExtents,
    RebuildTree,
    ReportDamage,
    ReadExtent,
    Materialize,
    /// Finding candidates that the format records as deleted.
    DeletedCandidates,
    /// Following the storage claims of deleted candidates.
    DeletedExtents,
    /// Materializing plan steps that name clusters, through
    /// [`SalvageAdapter::cluster_layout`].
    ClusterSteps,
}

impl Feature {
    /// A stable name for messages and serialized capability lists.
    pub const fn name(self) -> &'static str {
        match self {
            Self::FindCandidates => "find-candidates",
            Self::WalkExtents => "walk-extents",
            Self::RebuildTree => "rebuild-tree",
            Self::ReportDamage => "report-damage",
            Self::ReadExtent => "read-extent",
            Self::Materialize => "materialize",
            Self::DeletedCandidates => "deleted-candidates",
            Self::DeletedExtents => "deleted-extents",
            Self::ClusterSteps => "cluster-steps",
        }
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// What a driver lacks. Flags name absences, so an empty list means the
/// adapter supports every [`Feature`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub missing: Vec<Feature>,
}

impl Capabilities {
    pub fn supports(&self, feature: Feature) -> bool {
        !self.missing.contains(&feature)
    }

    /// `Ok` when supported, otherwise [`SalvageError::Unsupported`] naming it.
    pub fn require(&self, feature: Feature) -> Result<(), SalvageError> {
        if self.supports(feature) {
            Ok(())
        } else {
            Err(SalvageError::Unsupported(feature))
        }
    }
}

/// Why a salvage operation could not run. Damage is never one of these: it is
/// returned as evidence. These are operational failures and refusals.
#[derive(Debug)]
pub enum SalvageError {
    /// The adapter lacks this feature.
    Unsupported(Feature),
    /// No copy of the format's layout is usable, so storage claims cannot be
    /// mapped onto the source.
    NoGeometry,
    /// The request names bytes outside the source, or an unrepresentable range.
    Bounds,
    /// The source failed a read that the operation could not report as evidence.
    Device(fs_core::Error),
    /// The caller's output sink failed.
    Sink(io::Error),
    /// The plan cannot be executed as written. Nothing was read or written.
    Plan(PlanFault),
}

impl fmt::Display for SalvageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(feature) => {
                write!(formatter, "salvage feature unsupported: {feature}")
            }
            Self::NoGeometry => formatter.write_str("no usable layout copy on the source"),
            Self::Bounds => formatter.write_str("range outside the source or not representable"),
            Self::Device(error) => write!(formatter, "source read failed: {error}"),
            Self::Sink(error) => write!(formatter, "output sink failed: {error}"),
            Self::Plan(fault) => fault.fmt(formatter),
        }
    }
}

impl std::error::Error for SalvageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Device(error) => Some(error),
            Self::Sink(error) => Some(error),
            _ => None,
        }
    }
}

/// Receives streamed results. Returning [`ControlFlow::Break`] stops the
/// operation; its report says where, so the caller can resume.
pub trait Sink<T> {
    fn accept(&mut self, item: T) -> ControlFlow<()>;
}

impl<T> Sink<T> for Vec<T> {
    fn accept(&mut self, item: T) -> ControlFlow<()> {
        self.push(item);
        ControlFlow::Continue(())
    }
}

impl<T, F: FnMut(T) -> ControlFlow<()>> Sink<T> for F {
    fn accept(&mut self, item: T) -> ControlFlow<()> {
        self(item)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CandidateKind {
    File,
    Directory,
    /// A format structure, such as a boot sector, superblock, container header,
    /// table entry or footer.
    Structure,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CandidateState {
    Live,
    /// The format records the object as deleted.
    Deleted,
    /// The format has no deleted state for this record, or its bytes do not say.
    Unknown,
}

/// The result of the format's own check on a candidate's record bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Integrity {
    /// A checksum or equivalent check over the record passed.
    Verified,
    /// The check failed; the record is damaged evidence, not noise.
    Failed,
    /// The format has no check for this record.
    Unchecked,
}

/// A possible object found in raw bytes, never a trusted file.
///
/// `location.offset` identifies the candidate within one source; trees and
/// callers refer to candidates by it. `detail` holds the format-specific
/// fields the driver needs to walk the candidate again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate<D> {
    /// The record bytes that describe the candidate.
    pub location: Extent,
    pub kind: CandidateKind,
    pub state: CandidateState,
    pub integrity: Integrity,
    pub name: Option<String>,
    /// Logical size in bytes, as the record claims it.
    pub size: Option<u64>,
    pub detail: D,
}

/// The outcome of a candidate scan whose candidates went to a sink.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    /// Candidates passed to the sink.
    pub candidates: u64,
    /// Ranges inside the scan that could not be read. Nothing in them was
    /// decoded, and no candidate overlapping them was reported.
    pub unreadable: Vec<Extent>,
    /// Set when the sink stopped the scan: the first byte not yet scanned.
    pub stopped_at: Option<u64>,
}

/// Why an extent walk ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WalkEnd {
    /// Every claimed byte was mapped.
    Complete,
    /// The claim's map ended, or held a free, bad or reserved marker, before
    /// the claim was covered.
    Broken,
    /// The next storage unit lies outside the area the map may address.
    OutOfRange,
    /// The claim returned to a storage unit it had already used.
    Cycle,
    /// The map bytes needed to continue could not be read.
    Unreadable(Extent),
}

/// The storage a candidate claims, as far as its map could be followed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtentWalk {
    /// Source byte ranges in claim order, merged where adjacent and trimmed to
    /// the claimed size. A broken walk keeps every extent it reached.
    pub extents: Vec<Extent>,
    /// Bytes the candidate claims.
    pub claimed: u64,
    pub end: WalkEnd,
}

impl ExtentWalk {
    /// Bytes the walk mapped onto the source.
    pub fn mapped(&self) -> u64 {
        self.extents.iter().map(|extent| extent.length).sum()
    }
}

/// A claim that a candidate sits under another one, or under the root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Parent {
    Root,
    /// The parent candidate's `location.offset`.
    Candidate(u64),
}

/// One candidate's place in the rebuilt tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    /// The candidate's `location.offset`.
    pub candidate: u64,
    /// Every parent claim found. None makes an orphan; more than one is a
    /// conflicting parent claim. Claims are evidence and may form cycles.
    pub parents: Vec<Parent>,
}

/// Relationships rebuilt from candidates, one node per candidate in input order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tree {
    pub nodes: Vec<TreeNode>,
}

impl Tree {
    pub fn orphans(&self) -> impl Iterator<Item = &TreeNode> {
        self.nodes.iter().filter(|node| node.parents.is_empty())
    }

    pub fn conflicts(&self) -> impl Iterator<Item = &TreeNode> {
        self.nodes.iter().filter(|node| node.parents.len() > 1)
    }
}

/// The shared core of damage kinds. Format specifics go in [`Damage::detail`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DamageKind {
    /// A signature, magic number or identifying name is wrong.
    Signature,
    /// A stored checksum disagrees with the bytes it covers.
    Checksum,
    /// Copies or structures that must agree do not.
    Mismatch,
    /// A required structure is absent.
    Missing,
    /// A field holds a value the format does not allow.
    Malformed,
    /// A chain or map stops before the structure it describes is covered.
    Broken,
    /// The format itself records a fault, such as a media-failure flag.
    Recorded,
    /// The bytes could not be read.
    Unreadable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// State that may explain other damage, such as an unclean unmount.
    Notice,
    /// Damage that a redundant copy or the format's tolerance covers.
    Warning,
    /// Damage that leaves a structure unusable as stored.
    Error,
}

/// One observation of damage: the shared core of kind, location, severity and
/// source structure, plus format-specific detail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Damage {
    pub kind: DamageKind,
    pub severity: Severity,
    /// The damaged bytes, or for a missing structure where it should be.
    pub location: Extent,
    /// The format structure holding the damage, in the format's own terms.
    pub structure: String,
    /// What was observed, such as stored and computed values.
    pub detail: String,
}

/// The outcome of a damage report whose findings went to a sink.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DamageReport {
    /// Findings passed to the sink.
    pub reported: u64,
    /// Set when the sink stopped the report before every check ran.
    pub stopped: bool,
}

/// Where a run of output bytes came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterializedRange {
    /// The bytes' position in the output.
    pub output: Extent,
    /// The source bytes they came from; `None` for plan zeros and literals.
    pub source: Option<Extent>,
    /// False when the source bytes could not be read. The output holds zeros
    /// there to keep later offsets in place; this flag, not the bytes, is the
    /// evidence.
    pub readable: bool,
}

/// Provenance for every output byte of a materialized plan.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterializeReport {
    pub length: u64,
    /// Contiguous, in output order, covering the whole output.
    pub ranges: Vec<MaterializedRange>,
}

impl MaterializeReport {
    /// The source byte that produced an output byte, if one did.
    pub fn source_of(&self, output: u64) -> Option<u64> {
        let range = self.ranges.iter().find(|range| {
            range.output.offset <= output && range.output.end().is_some_and(|end| output < end)
        })?;
        Some(range.source?.offset + (output - range.output.offset))
    }

    /// Output ranges whose source bytes could not be read.
    pub fn unreadable(&self) -> impl Iterator<Item = Extent> + '_ {
        self.ranges
            .iter()
            .filter(|range| !range.readable)
            .map(|range| range.output)
    }

    fn push(&mut self, length: u64, source: Option<Extent>, readable: bool) {
        append(&mut self.ranges, self.length, length, source, readable);
        self.length += length;
    }
}

/// Appends `length` output bytes at `at` to `ranges`, joining the last range
/// when the source continues it and readability agrees.
fn append(
    ranges: &mut Vec<MaterializedRange>,
    at: u64,
    length: u64,
    source: Option<Extent>,
    readable: bool,
) {
    if length == 0 {
        return;
    }
    if let Some(last) = ranges.last_mut() {
        let joins_source = match (last.source, source) {
            (Some(previous), Some(next)) => previous.end() == Some(next.offset),
            (None, None) => true,
            _ => false,
        };
        if joins_source && last.readable == readable && last.output.end() == Some(at) {
            last.output.length += length;
            if let (Some(previous), Some(next)) = (last.source.as_mut(), source) {
                previous.length += next.length;
            }
            return;
        }
    }
    ranges.push(MaterializedRange {
        output: Extent::new(at, length),
        source,
        readable,
    });
}

/// The common salvage surface each driver implements over its primitives.
///
/// Methods take the source as `&dyn BlockRead` per call, so an adapter value
/// holds only format context, such as geometry, and never a device.
pub trait SalvageAdapter {
    /// Format-specific candidate fields, kept as plain data.
    type Detail: Clone + fmt::Debug + Serialize + DeserializeOwned;

    /// What this adapter lacks. An empty list means nothing.
    fn capabilities(&self) -> Capabilities;

    /// Streams every candidate whose record starts inside `range`, live,
    /// deleted or damaged, to `sink`. A record may extend past the range end.
    /// Unreadable bytes are listed in the report and the scan continues.
    ///
    /// Fails with [`SalvageError::Bounds`] when `range` leaves the source.
    fn find_candidates(
        &self,
        source: &dyn BlockRead,
        range: Extent,
        sink: &mut dyn Sink<Candidate<Self::Detail>>,
    ) -> Result<ScanReport, SalvageError>;

    /// Follows the storage a candidate claims, keeping every extent reached
    /// and saying why the walk ended. A break is evidence, not an error.
    fn walk_extents(
        &self,
        source: &dyn BlockRead,
        candidate: &Candidate<Self::Detail>,
    ) -> Result<ExtentWalk, SalvageError>;

    /// Rebuilds parent claims among `candidates`, keeping orphans and
    /// conflicting parents. Returns one node per candidate, in input order.
    fn rebuild_tree(
        &self,
        source: &dyn BlockRead,
        candidates: &[Candidate<Self::Detail>],
    ) -> Result<Tree, SalvageError>;

    /// Streams observed damage in the source's own structures to `sink`.
    /// Unreadable structures are damage findings, not errors.
    fn report_damage(
        &self,
        source: &dyn BlockRead,
        sink: &mut dyn Sink<Damage>,
    ) -> Result<DamageReport, SalvageError>;

    /// Reads bytes of `extent` starting `offset` bytes into it, up to the
    /// extent's end or the output's length, and returns how many were read.
    ///
    /// Fails with [`SalvageError::Bounds`] when `offset` exceeds the extent's
    /// length or the extent leaves the source.
    fn read_extent(
        &self,
        source: &dyn BlockRead,
        extent: Extent,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, SalvageError>;

    /// How this adapter numbers clusters on the source, for plan steps that
    /// name them. Fails with `Unsupported` when the format has no such
    /// numbering, and with [`SalvageError::NoGeometry`] when no layout copy is
    /// usable.
    fn cluster_layout(&self) -> Result<ClusterLayout, SalvageError> {
        Err(SalvageError::Unsupported(Feature::ClusterSteps))
    }

    /// Writes the plan's output to `sink`, never to the source, and reports
    /// which source bytes produced each output byte.
    ///
    /// The plan is validated against the source size before anything is read
    /// or written: cluster steps map through [`Self::cluster_layout`], and the
    /// declared lengths must agree with the steps. Bytes past the valid length
    /// are written as zeros without reading their source. A failed source read
    /// is retried in 512-byte spans; spans that still fail are written as
    /// zeros and flagged unreadable, and the rest of the plan still runs. Only
    /// a sink failure stops it part way.
    fn materialize(
        &self,
        source: &dyn BlockRead,
        plan: &Plan,
        sink: &mut dyn io::Write,
    ) -> Result<MaterializeReport, SalvageError> {
        let reader = self.plan_reader(source, plan)?;
        let mut report = MaterializeReport::default();
        let mut buffer = vec![0u8; MATERIALIZE_CHUNK as usize];
        let mut offset = 0;
        while offset < reader.len() {
            let count = (reader.len() - offset).min(MATERIALIZE_CHUNK) as usize;
            let read = reader.read_at(offset, &mut buffer[..count])?;
            sink.write_all(&buffer[..count])
                .map_err(SalvageError::Sink)?;
            for range in read.ranges {
                report.push(range.output.length, range.source, range.readable);
            }
            offset += count as u64;
        }
        Ok(report)
    }

    /// Executes a plan as a positioned reader over its output, so any offset
    /// can be read on demand without materializing what comes before it.
    ///
    /// The plan is validated as [`Self::materialize`] validates it, before
    /// anything is read. Each [`PlanReader::read_at`] reads only the source
    /// bytes its request covers, with the same 512-byte retries, zero fill
    /// and provenance.
    fn plan_reader<'a>(
        &'a self,
        source: &'a dyn BlockRead,
        plan: &'a Plan,
    ) -> Result<PlanReader<'a, Self>, SalvageError> {
        self.capabilities().require(Feature::Materialize)?;
        let resolved = resolve(self, source.size_bytes(), plan)?;
        Ok(PlanReader::new(self, source, plan, resolved))
    }

    /// Checks a plan against a source of `source_size` bytes without reading
    /// anything, as [`Self::materialize`] checks it first.
    ///
    /// Refuses, as materializing would, extents past the source
    /// ([`SalvageError::Bounds`]), cluster runs outside
    /// [`Self::cluster_layout`] and declared lengths that disagree with the
    /// steps ([`SalvageError::Plan`]). Otherwise returns the output and valid
    /// lengths and the source bytes more than one step reads, as findings.
    fn check_plan(&self, plan: &Plan, source_size: u64) -> Result<PlanCheck, SalvageError> {
        self.capabilities().require(Feature::Materialize)?;
        Ok(resolve(self, source_size, plan)?.check())
    }
}

/// Maps a plan onto the source through the adapter's cluster layout, which
/// is asked for only when the plan names clusters.
fn resolve<A: SalvageAdapter + ?Sized>(
    adapter: &A,
    source_size: u64,
    plan: &Plan,
) -> Result<Resolved, SalvageError> {
    let layout = if plan.has_clusters() {
        adapter.capabilities().require(Feature::ClusterSteps)?;
        Some(adapter.cluster_layout()?)
    } else {
        None
    };
    plan.resolve(source_size, layout.as_ref())
}
