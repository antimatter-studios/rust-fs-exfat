//! Materialization plans: the output a caller asks for, as plain data, and
//! how a plan resolves onto source bytes before anything is read.

use super::{append, Extent, Feature, MaterializedRange, SalvageAdapter, SalvageError};
use fs_core::BlockRead;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

/// The prefix of [`Plan::canonical_bytes`]: a name and encoding version 1.
const PLAN_ENCODING: &[u8; 16] = b"fs-salvage-plan\x01";

/// The span a failed source read is retried in, aligned on the source, to
/// narrow what is lost.
const RETRY_SPAN: u64 = 512;

/// One operation of a materialization plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlanStep {
    /// Copy these source bytes.
    Extent(Extent),
    /// Emit this many zero bytes.
    Zeros(u64),
    /// Emit these bytes verbatim.
    Bytes(Vec<u8>),
    /// Copy `count` whole clusters, numbered from `first`, through the
    /// adapter's [`ClusterLayout`]. A cluster is the format's allocation
    /// unit, whatever the format calls it.
    Clusters { first: u64, count: u64 },
}

/// How a format numbers its allocation units on the source: cluster `first`
/// starts at byte `offset`, and `count` clusters of `size` bytes follow it in
/// order. Cluster steps in a plan map through it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterLayout {
    pub first: u64,
    pub count: u64,
    pub size: u64,
    pub offset: u64,
}

impl ClusterLayout {
    /// The source bytes of `count` clusters from `first`, or `None` when
    /// `first` or a cluster of the run lies outside the layout, or the range
    /// is not representable.
    pub fn extent(&self, first: u64, count: u64) -> Option<Extent> {
        let index = first.checked_sub(self.first)?;
        if index >= self.count || index.checked_add(count)? > self.count {
            return None;
        }
        let offset = self.offset.checked_add(index.checked_mul(self.size)?)?;
        let length = count.checked_mul(self.size)?;
        offset.checked_add(length)?;
        Some(Extent::new(offset, length))
    }

    /// The cluster holding a source byte, if the layout covers that byte.
    pub fn cluster_at(&self, offset: u64) -> Option<u64> {
        let index = offset.checked_sub(self.offset)? / self.size.max(1);
        (index < self.count).then(|| self.first + index)
    }
}

/// What a plan says about the file it rebuilds. The driver carries it with
/// the plan and does not interpret it; times are text, as a candidate's
/// format-specific detail gives them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlanMetadata {
    pub name: Option<String>,
    pub created: Option<String>,
    pub modified: Option<String>,
    pub accessed: Option<String>,
}

impl PlanMetadata {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// An explicit, ordered recipe for output bytes. The analysis that chose it
/// lives outside the driver; the driver executes it faithfully.
///
/// Without `length`, the output is as long as its steps. With it, the output
/// is exactly that long: it may cut off part of the last step that produces
/// bytes, as a file's length cuts off the end of its last cluster, but no
/// more, and it may not run past the steps. Output bytes at and past
/// `valid_length` read as zeros, and the source bytes behind them are never
/// read.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub steps: Vec<PlanStep>,
    /// The output's final length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u64>,
    /// The output bytes before this offset come from the steps; the rest are
    /// zeros. It may not exceed the final length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_length: Option<u64>,
    #[serde(default, skip_serializing_if = "PlanMetadata::is_empty")]
    pub metadata: PlanMetadata,
}

/// Why a plan cannot be executed as written. Each names what disagrees, so
/// the analysis that wrote the plan can correct it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlanFault {
    /// The step at this index names clusters outside the adapter's
    /// [`ClusterLayout`].
    ClustersOutOfRange { step: usize },
    /// The declared final length runs past the steps, or cuts off more than
    /// part of the last step that produces bytes.
    LengthMismatch { length: u64, steps: u64 },
    /// The valid length lies past the output's final length.
    ValidLengthPastEnd { valid_length: u64, length: u64 },
}

impl fmt::Display for PlanFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClustersOutOfRange { step } => {
                write!(
                    formatter,
                    "plan step {step} names clusters outside the volume"
                )
            }
            Self::LengthMismatch { length, steps } => write!(
                formatter,
                "plan length {length} disagrees with the {steps} byte(s) its steps produce"
            ),
            Self::ValidLengthPastEnd {
                valid_length,
                length,
            } => write!(
                formatter,
                "plan valid length {valid_length} is past its length {length}"
            ),
        }
    }
}

impl Plan {
    /// A plan of these steps, with no declared lengths or metadata.
    pub fn new(steps: Vec<PlanStep>) -> Self {
        Self {
            steps,
            ..Self::default()
        }
    }

    /// The output length: the declared final length when there is one,
    /// otherwise the steps' total. `None` when that is not representable, or
    /// when cluster steps make it depend on an adapter's [`ClusterLayout`].
    pub fn length(&self) -> Option<u64> {
        if self.length.is_some() {
            return self.length;
        }
        self.steps.iter().try_fold(0u64, |total, step| {
            total.checked_add(match step {
                PlanStep::Extent(extent) => extent.length,
                PlanStep::Zeros(length) => *length,
                PlanStep::Bytes(bytes) => bytes.len() as u64,
                PlanStep::Clusters { .. } => return None,
            })
        })
    }

    /// Checks the plan against a source size without reading and returns the
    /// output length: every extent must lie inside the source, the length
    /// must be representable and the declared lengths must agree with the
    /// steps. A plan with cluster steps needs an adapter's layout, so this
    /// refuses it with `Unsupported`; [`SalvageAdapter::materialize`] maps
    /// them.
    ///
    /// [`SalvageAdapter::materialize`]: super::SalvageAdapter::materialize
    pub fn validate(&self, source_size: u64) -> Result<u64, SalvageError> {
        self.resolve(source_size, None)
            .map(|resolved| resolved.length)
    }

    /// The plan's canonical encoding, version 1, which [`Self::digest`]
    /// hashes. It is fixed: a release that changed it would be a new
    /// version with a new prefix, never a new meaning for these bytes.
    ///
    /// Integers are little-endian `u64`, and text is UTF-8. In order:
    ///
    /// 1. The 15 bytes `fs-salvage-plan`, then a version byte, 1.
    /// 2. The step count, then each step as a tag byte and its fields:
    ///    `1` extent (offset, length), `2` zeros (length), `3` bytes (length,
    ///    then the bytes), `4` clusters (first, count).
    /// 3. The final length, then the valid length, each as `0` when absent
    ///    or `1` and the value.
    /// 4. The name, created, modified and accessed metadata, each as `0` when
    ///    absent or `1`, the byte length and the bytes.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        fn number(out: &mut Vec<u8>, value: u64) {
            out.extend_from_slice(&value.to_le_bytes());
        }
        fn optional(out: &mut Vec<u8>, value: Option<u64>) {
            match value {
                None => out.push(0),
                Some(value) => {
                    out.push(1);
                    number(out, value);
                }
            }
        }
        fn text(out: &mut Vec<u8>, value: &Option<String>) {
            match value {
                None => out.push(0),
                Some(value) => {
                    out.push(1);
                    number(out, value.len() as u64);
                    out.extend_from_slice(value.as_bytes());
                }
            }
        }
        let mut out = Vec::new();
        out.extend_from_slice(PLAN_ENCODING);
        number(&mut out, self.steps.len() as u64);
        for step in &self.steps {
            match step {
                PlanStep::Extent(extent) => {
                    out.push(1);
                    number(&mut out, extent.offset);
                    number(&mut out, extent.length);
                }
                PlanStep::Zeros(length) => {
                    out.push(2);
                    number(&mut out, *length);
                }
                PlanStep::Bytes(bytes) => {
                    out.push(3);
                    number(&mut out, bytes.len() as u64);
                    out.extend_from_slice(bytes);
                }
                PlanStep::Clusters { first, count } => {
                    out.push(4);
                    number(&mut out, *first);
                    number(&mut out, *count);
                }
            }
        }
        optional(&mut out, self.length);
        optional(&mut out, self.valid_length);
        let metadata = &self.metadata;
        for value in [
            &metadata.name,
            &metadata.created,
            &metadata.modified,
            &metadata.accessed,
        ] {
            text(&mut out, value);
        }
        out
    }

    /// A stable identity for the plan: the SHA-256 of its
    /// [canonical encoding](Self::canonical_bytes), as 64 lowercase hex
    /// digits. It is the same on every platform and in every release, and
    /// changes with any step, length or metadata, so a consumer can tell when
    /// a file must be materialized again. Two plans with the same digest give
    /// the same bytes from the same source.
    pub fn digest(&self) -> String {
        Sha256::digest(self.canonical_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub(crate) fn has_clusters(&self) -> bool {
        self.steps
            .iter()
            .any(|step| matches!(step, PlanStep::Clusters { .. }))
    }

    /// Maps every step onto the source and applies the declared lengths,
    /// without reading. Cluster steps need `clusters`.
    pub(crate) fn resolve(
        &self,
        source_size: u64,
        clusters: Option<&ClusterLayout>,
    ) -> Result<Resolved, SalvageError> {
        let mut spans = Vec::with_capacity(self.steps.len());
        let mut total = 0u64;
        let mut last_start = 0;
        for (index, step) in self.steps.iter().enumerate() {
            let (from, length) = match step {
                PlanStep::Bytes(bytes) => (Origin::Literal { step: index }, bytes.len() as u64),
                PlanStep::Zeros(length) => (Origin::Zeros, *length),
                PlanStep::Extent(extent) => (Origin::Source(extent.offset), extent.length),
                PlanStep::Clusters { first, count } => {
                    let layout =
                        clusters.ok_or(SalvageError::Unsupported(Feature::ClusterSteps))?;
                    let extent = layout.extent(*first, *count).ok_or(SalvageError::Plan(
                        PlanFault::ClustersOutOfRange { step: index },
                    ))?;
                    (Origin::Source(extent.offset), extent.length)
                }
            };
            if let Origin::Source(offset) = from {
                if offset
                    .checked_add(length)
                    .is_none_or(|end| end > source_size)
                {
                    return Err(SalvageError::Bounds);
                }
            }
            if length > 0 {
                last_start = total;
            }
            spans.push((from, length, index));
            total = total.checked_add(length).ok_or(SalvageError::Bounds)?;
        }
        let length = match self.length {
            None => total,
            Some(length) if length == total || (length < total && length > last_start) => length,
            Some(length) => {
                return Err(SalvageError::Plan(PlanFault::LengthMismatch {
                    length,
                    steps: total,
                }))
            }
        };
        let valid = match self.valid_length {
            None => length,
            Some(valid_length) if valid_length <= length => valid_length,
            Some(valid_length) => {
                return Err(SalvageError::Plan(PlanFault::ValidLengthPastEnd {
                    valid_length,
                    length,
                }))
            }
        };
        let mut pieces = Vec::with_capacity(spans.len() + 1);
        let mut output = 0;
        for (from, span, step) in spans {
            let kept = span.min(length - output);
            let valid_part = kept.min(valid.saturating_sub(output));
            if valid_part > 0 {
                pieces.push(Piece {
                    output,
                    length: valid_part,
                    from,
                    step,
                });
            }
            if kept > valid_part {
                pieces.push(Piece {
                    output: output + valid_part,
                    length: kept - valid_part,
                    from: Origin::Zeros,
                    step,
                });
            }
            output += kept;
        }
        Ok(Resolved {
            pieces,
            length,
            valid,
        })
    }
}

/// What checking a plan found without reading: its lengths, and the source
/// bytes more than one step reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanCheck {
    /// The output length.
    pub length: u64,
    /// Where the output's zeros begin: the declared valid length, or the
    /// output length.
    pub valid_length: u64,
    /// Source bytes that two steps both read: one finding per pair of
    /// steps, in source order. A plan may repeat bytes on
    /// purpose, so these are findings, not refusals. Only bytes the plan reads
    /// count, so a step past the valid length overlaps nothing.
    pub overlaps: Vec<PlanOverlap>,
}

/// Source bytes that two plan steps both read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanOverlap {
    /// The lower step index.
    pub first: usize,
    /// The higher step index.
    pub second: usize,
    /// The source bytes both read.
    pub source: Extent,
}

/// A plan mapped onto the source: output pieces in order, covering the whole
/// output.
#[derive(Clone, Debug)]
pub(crate) struct Resolved {
    pub(crate) pieces: Vec<Piece>,
    pub(crate) length: u64,
    /// Where the output's zeros begin.
    pub(crate) valid: u64,
}

impl Resolved {
    /// The check's findings for this mapping.
    pub(crate) fn check(&self) -> PlanCheck {
        let mut reads: Vec<(u64, u64, usize)> = self
            .pieces
            .iter()
            .filter_map(|piece| match piece.from {
                Origin::Source(offset) => Some((offset, offset + piece.length, piece.step)),
                _ => None,
            })
            .collect();
        reads.sort_unstable();
        let mut open: Vec<(u64, usize)> = Vec::new();
        let mut overlaps = Vec::new();
        for (start, end, step) in reads {
            open.retain(|(open_end, _)| *open_end > start);
            for &(open_end, open_step) in &open {
                overlaps.push(PlanOverlap {
                    first: open_step.min(step),
                    second: open_step.max(step),
                    source: Extent::new(start, open_end.min(end) - start),
                });
            }
            open.push((end, step));
        }
        overlaps
            .sort_unstable_by_key(|overlap| (overlap.source.offset, overlap.first, overlap.second));
        PlanCheck {
            length: self.length,
            valid_length: self.valid,
            overlaps,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Piece {
    /// Where the piece starts in the output.
    pub(crate) output: u64,
    pub(crate) length: u64,
    pub(crate) from: Origin,
    /// The plan step it comes from.
    pub(crate) step: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Origin {
    /// Source bytes, starting at this offset.
    Source(u64),
    Zeros,
    /// The plan step holding the bytes.
    Literal {
        step: usize,
    },
}

/// An executed plan: positioned reads over its output.
///
/// It holds the adapter, the source and the plan by reference, and the plan
/// mapped onto the source, never any output bytes.
pub struct PlanReader<'a, A: ?Sized> {
    adapter: &'a A,
    source: &'a dyn BlockRead,
    plan: &'a Plan,
    resolved: Resolved,
}

/// What one positioned read produced and where its bytes came from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRead {
    /// The output offset the read started at.
    pub offset: u64,
    /// Bytes read.
    pub length: u64,
    /// Contiguous, in output order, covering exactly the bytes read, as in
    /// [`super::MaterializeReport::ranges`].
    pub ranges: Vec<MaterializedRange>,
}

impl PlanRead {
    /// Output ranges of this read whose source bytes could not be read.
    pub fn unreadable(&self) -> impl Iterator<Item = Extent> + '_ {
        self.ranges
            .iter()
            .filter(|range| !range.readable)
            .map(|range| range.output)
    }
}

impl<'a, A: SalvageAdapter + ?Sized> PlanReader<'a, A> {
    pub(crate) fn new(
        adapter: &'a A,
        source: &'a dyn BlockRead,
        plan: &'a Plan,
        resolved: Resolved,
    ) -> Self {
        Self {
            adapter,
            source,
            plan,
            resolved,
        }
    }

    /// The output length.
    pub fn len(&self) -> u64 {
        self.resolved.length
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The plan being executed.
    pub fn plan(&self) -> &'a Plan {
        self.plan
    }

    /// The source byte that produces an output byte, if one does, without
    /// reading. Plan zeros, literals and bytes past the valid length have
    /// none.
    pub fn source_of(&self, output: u64) -> Option<u64> {
        let piece = self.resolved.pieces.get(self.piece_at(output))?;
        match piece.from {
            Origin::Source(start) if output >= piece.output => {
                Some(start + (output - piece.output))
            }
            _ => None,
        }
    }

    /// Reads output bytes from `offset`, up to the output's end or the
    /// buffer's length, reading only the source bytes they come from.
    ///
    /// A failed source read is retried in 512-byte spans aligned on the
    /// source; spans that still fail read as zeros and are flagged
    /// unreadable, and the rest of the request is still read. Fails with
    /// [`SalvageError::Bounds`] only when `offset` is past the output's end.
    pub fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<PlanRead, SalvageError> {
        if offset > self.len() {
            return Err(SalvageError::Bounds);
        }
        let length = (self.len() - offset).min(output.len() as u64);
        let mut read = PlanRead {
            offset,
            length,
            ranges: Vec::new(),
        };
        let end = offset + length;
        let mut position = offset;
        for piece in &self.resolved.pieces[self.piece_at(offset)..] {
            if position >= end {
                break;
            }
            let within = position - piece.output;
            let count = (piece.length - within).min(end - position);
            let bytes =
                &mut output[(position - offset) as usize..(position - offset + count) as usize];
            match piece.from {
                Origin::Zeros => {
                    bytes.fill(0);
                    append(&mut read.ranges, position, count, None, true);
                }
                Origin::Literal { step } => {
                    let PlanStep::Bytes(literal) = &self.plan.steps[step] else {
                        unreachable!("a literal piece comes from a bytes step")
                    };
                    bytes.copy_from_slice(&literal[within as usize..(within + count) as usize]);
                    append(&mut read.ranges, position, count, None, true);
                }
                Origin::Source(start) => {
                    self.read_source(start + within, bytes, position, &mut read.ranges);
                }
            }
            position += count;
        }
        Ok(read)
    }

    /// The index of the piece holding an output byte, or the piece count when
    /// the byte is past the output.
    fn piece_at(&self, output: u64) -> usize {
        self.resolved
            .pieces
            .partition_point(|piece| piece.output + piece.length <= output)
    }

    fn read_source(
        &self,
        start: u64,
        bytes: &mut [u8],
        at: u64,
        ranges: &mut Vec<MaterializedRange>,
    ) {
        let length = bytes.len() as u64;
        let whole = Extent::new(start, length);
        if self
            .adapter
            .read_extent(self.source, whole, 0, bytes)
            .is_ok()
        {
            append(ranges, at, length, Some(whole), true);
            return;
        }
        let mut done = 0;
        while done < length {
            let offset = start + done;
            let span = (RETRY_SPAN - offset % RETRY_SPAN).min(length - done);
            let part = Extent::new(offset, span);
            let piece = &mut bytes[done as usize..(done + span) as usize];
            let readable = self
                .adapter
                .read_extent(self.source, part, 0, piece)
                .is_ok();
            if !readable {
                piece.fill(0);
            }
            append(ranges, at + done, span, Some(part), readable);
            done += span;
        }
    }
}
