//! Materialization plans: the output a caller asks for, as plain data, and
//! how a plan resolves onto source bytes before anything is read.

use super::{Extent, Feature, SalvageError};
use serde::{Deserialize, Serialize};
use std::fmt;

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
            spans.push((from, length));
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
        for (from, span) in spans {
            let kept = span.min(length - output);
            let valid_part = kept.min(valid.saturating_sub(output));
            if valid_part > 0 {
                pieces.push(Piece {
                    length: valid_part,
                    from,
                });
            }
            if kept > valid_part {
                pieces.push(Piece {
                    length: kept - valid_part,
                    from: Origin::Zeros,
                });
            }
            output += kept;
        }
        Ok(Resolved { pieces, length })
    }
}

/// A plan mapped onto the source: output pieces in order, covering the whole
/// output.
#[derive(Clone, Debug)]
pub(crate) struct Resolved {
    pub(crate) pieces: Vec<Piece>,
    pub(crate) length: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Piece {
    pub(crate) length: u64,
    pub(crate) from: Origin,
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
